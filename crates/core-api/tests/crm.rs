//! M3's CRM through HTTP: talent, organisations and contacts; tags, search,
//! duplicates, the activity timeline, bulk actions, saved views, the schema
//! forms are drawn from, and spreadsheet imports up to the point the worker
//! takes over (the worker side is tested in `core-worker`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{Client, TestApp};
use serde_json::{Value, json};
use sqlx::PgPool;

async fn post(app: &TestApp, who: &Client, path: &str, body: Value) -> common::Resp {
    app.post(path).signed_in(who).json(body).send().await
}

async fn get(app: &TestApp, who: &Client, path: &str) -> common::Resp {
    app.get(path).signed_in(who).send().await.ok()
}

fn names(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect()
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn talent_is_created_checked_edited_and_deleted(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;

    let created = post(
        &app,
        &owner,
        "/api/v1/talent",
        json!({
            "name": "  The Midnight Owls ", "kind": "act", "talent_type": "Band",
            "default_fee": { "minor": 250_000, "currency": "gbp" }, "commission_percent": 15.5,
            "tags": ["Jazz", "jazz", " Touring "]
        }),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let t = &created.body;
    assert_eq!(t["name"], "The Midnight Owls");
    assert_eq!(t["kind"], "act");
    assert_eq!(t["status"], "active");
    assert_eq!(t["commission_percent"], 15.5);
    assert_eq!(
        t["default_fee"],
        json!({ "minor": 250_000, "currency": "GBP" })
    );
    assert_eq!(t["tags"], json!(["jazz", "touring"]));
    let id = created.id("/id");

    // Every problem at once, each named.
    let bad = post(
        &app,
        &owner,
        "/api/v1/talent",
        json!({ "name": " ", "commission_percent": 120, "default_fee": { "minor": 1, "currency": "XX" } }),
    )
    .await;
    bad.expect(StatusCode::BAD_REQUEST, "invalid_input");
    let fields: Vec<&str> = bad.body["error"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["field"].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        ["name", "default_fee.currency", "commission_percent"]
    );

    // Edits go through If-Match; a stale version is refused.
    let path = format!("/api/v1/talent/{id}");
    let edited = app
        .patch(&path)
        .signed_in(&owner)
        .header("if-match", "\"1\"")
        .json(json!({ "status": "inactive", "legal_name": "Owls Ltd", "commission_percent": null }))
        .send()
        .await
        .ok();
    assert_eq!(edited.body["version"], 2);
    assert_eq!(edited.body["status"], "inactive");
    assert_eq!(edited.body["legal_name"], "Owls Ltd");
    assert!(edited.body["commission_percent"].is_null());
    app.patch(&path)
        .signed_in(&owner)
        .header("if-match", "\"1\"")
        .json(json!({ "name": "Late" }))
        .send()
        .await
        .expect(StatusCode::PRECONDITION_FAILED, "version_conflict");

    // Members of the act.
    let members = format!("{path}/members");
    let drummer = post(
        &app,
        &owner,
        &members,
        json!({ "name": "Ana", "role": "Drums", "email": "Ana@Example.com" }),
    )
    .await
    .ok();
    assert_eq!(drummer.body["email"], "ana@example.com");
    assert_eq!(drummer.body["position"], 0);
    let singer = post(
        &app,
        &owner,
        &members,
        json!({ "name": "Bo", "role": "Vocals" }),
    )
    .await
    .ok();
    assert_eq!(singer.body["position"], 1);
    let drummer_id = drummer.id("/id");
    app.put(&format!("{members}/{drummer_id}"))
        .signed_in(&owner)
        .json(json!({ "name": "Ana", "role": "Drums", "notes": "vegetarian", "position": 2 }))
        .send()
        .await
        .ok();
    let list = get(&app, &owner, &members).await;
    assert_eq!(names(&list.body), ["Bo", "Ana"]);
    assert_eq!(list.body[1]["notes"], "vegetarian");
    app.delete(&format!("{members}/{drummer_id}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(names(&get(&app, &owner, &members).await.body), ["Bo"]);

    app.delete(&path).signed_in(&owner).send().await.ok();
    app.get(&path)
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
    let actions = app.audit_actions(owner.workspace.unwrap()).await;
    for action in [
        "talent.created",
        "talent.updated",
        "talent_member.created",
        "talent_member.updated",
        "talent_member.deleted",
        "talent.deleted",
    ] {
        assert!(actions.iter().any(|a| a == action), "{action}: {actions:?}");
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn contacts_belong_to_organisations_and_keep_their_details_tidy(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;

    let venue = post(
        &app,
        &owner,
        "/api/v1/organisations",
        json!({
            "name": "The Blue Room", "org_type": "venue", "tax_id": "GB123456789",
            "addresses": [{ "label": "Stage door", "line1": "1 High St", "city": "Leeds", "country": "GB" }, {}],
            "payment_terms_days": 30, "default_currency": "eur"
        }),
    )
    .await
    .ok();
    let venue_id = venue.id("/id");
    // Empty addresses are dropped.
    assert_eq!(venue.body["addresses"].as_array().unwrap().len(), 1);
    assert_eq!(venue.body["default_currency"], "EUR");

    let jo = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({
            "name": "Jo Booker", "organisation_id": venue_id,
            "emails": [" Jo@BlueRoom.example ", "jo@blueroom.example", "jo.home@example.com"],
            "phones": ["+44 7700 900123"], "preferred_channel": "email", "role": "Booker"
        }),
    )
    .await
    .ok();
    assert_eq!(
        jo.body["emails"],
        json!(["jo@blueroom.example", "jo.home@example.com"])
    );
    assert_eq!(jo.body["email"], "jo@blueroom.example");
    assert_eq!(jo.body["phone"], "+44 7700 900123");
    assert_eq!(jo.body["organisation_name"], "The Blue Room");
    assert_eq!(jo.body["email_opt_in"], false);
    let jo_id = jo.id("/id");

    post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Free Agent", "emails": ["not-an-email"] }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
    post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Nowhere", "organisation_id": uuid::Uuid::now_v7() }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
    post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Sam Solo", "phones": ["07700 900456"] }),
    )
    .await
    .ok();

    // The organisation counts its contacts; contacts filter by organisation.
    let orgs = get(&app, &owner, "/api/v1/organisations").await;
    assert_eq!(orgs.body["items"][0]["contact_count"], 1);
    let at_venue = get(
        &app,
        &owner,
        &format!("/api/v1/contacts?organisation_id={venue_id}"),
    )
    .await;
    assert_eq!(names(&at_venue.body["items"]), ["Jo Booker"]);
    assert_eq!(at_venue.body["total"], 1);

    // Moving Jo out of the organisation.
    let moved = app
        .patch(&format!("/api/v1/contacts/{jo_id}"))
        .signed_in(&owner)
        .json(json!({ "organisation_id": null }))
        .send()
        .await
        .ok();
    assert!(moved.body["organisation_name"].is_null());
    let venue = get(&app, &owner, &format!("/api/v1/organisations/{venue_id}")).await;
    assert_eq!(venue.body["contact_count"], 0);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn lists_search_filter_sort_and_page(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    post(
        &app,
        &owner,
        "/api/v1/custom-fields",
        json!({ "entity": "talent", "key": "genre", "label": "Genre", "field_type": "select", "searchable": true,
                "validation": { "options": [{ "value": "jazz", "label": "Jazz" }, { "value": "rock", "label": "Rock" }] } }),
    )
    .await
    .ok();
    for (name, genre, tags) in [
        ("Ella Sings", "jazz", json!(["vocal"])),
        ("Rock Lobsters", "rock", json!(["band"])),
        ("Miles Ahead", "jazz", json!(["band", "touring"])),
        ("Zed Zero", "rock", json!([])),
    ] {
        post(
            &app,
            &owner,
            "/api/v1/talent",
            json!({ "name": name, "custom": { "genre": genre }, "tags": tags }),
        )
        .await
        .ok();
    }

    let list = |query: &str| {
        let path = format!("/api/v1/talent{query}");
        let app = &app;
        let owner = &owner;
        async move { get(app, owner, &path).await.body }
    };
    let all = list("").await;
    assert_eq!(all["total"], 4);
    assert_eq!(
        names(&all["items"]),
        ["Ella Sings", "Miles Ahead", "Rock Lobsters", "Zed Zero"]
    );
    assert_eq!(
        names(&list("?sort=-name&limit=2&offset=1").await["items"]),
        ["Rock Lobsters", "Miles Ahead"]
    );
    assert_eq!(list("?sort=-name&limit=2&offset=1").await["total"], 4);
    assert_eq!(
        names(&list("?tag=band").await["items"]),
        ["Miles Ahead", "Rock Lobsters"]
    );
    assert_eq!(
        names(&list("?custom=%7B%22genre%22%3A%22jazz%22%7D").await["items"]),
        ["Ella Sings", "Miles Ahead"]
    );
    // Searchable custom fields count; so do close misspellings.
    assert_eq!(
        names(&list("?q=jazz").await["items"]),
        ["Ella Sings", "Miles Ahead"]
    );
    assert_eq!(
        names(&list("?q=lobsterz").await["items"]),
        ["Rock Lobsters"]
    );
    let by_genre = list("?sort=custom.genre").await;
    assert_eq!(by_genre["items"][0]["custom"]["genre"], "jazz");
    assert_eq!(by_genre["items"][3]["custom"]["genre"], "rock");

    for bad in ["?sort=password", "?sort=custom.nope", "?custom=%5B%5D"] {
        app.get(&format!("/api/v1/talent{bad}"))
            .signed_in(&owner)
            .send()
            .await
            .expect(StatusCode::BAD_REQUEST, "invalid_input");
    }

    let tags = get(&app, &owner, "/api/v1/tags?entity=talent").await;
    assert_eq!(tags.body[0], json!({ "tag": "band", "count": 2 }));
    assert_eq!(tags.body.as_array().unwrap().len(), 3);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn search_finds_records_across_the_crm_best_first(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let org = post(
        &app,
        &owner,
        "/api/v1/organisations",
        json!({ "name": "Harbour Arts Centre" }),
    )
    .await
    .id("/id");
    post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Harriet Hall", "organisation_id": org, "emails": ["hh@harbour.example"] }),
    )
    .await
    .ok();
    post(&app, &owner, "/api/v1/talent", json!({ "name": "Harbour" }))
        .await
        .ok();

    let hits = get(&app, &owner, "/api/v1/search?q=harbour").await;
    let found: Vec<(&str, &str)> = hits
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|h| (h["entity"].as_str().unwrap(), h["name"].as_str().unwrap()))
        .collect();
    // The exact name first, then the name that starts with it, then the
    // contact whose email has it.
    assert_eq!(
        found,
        [
            ("talent", "Harbour"),
            ("organisation", "Harbour Arts Centre"),
            ("contact", "Harriet Hall")
        ]
    );
    let only = get(&app, &owner, "/api/v1/search?q=harbour&entities=contact").await;
    assert_eq!(only.body.as_array().unwrap().len(), 1);

    // A key that may read contacts only searches contacts only.
    let key = post(
        &app,
        &owner,
        "/api/v1/api-keys",
        json!({ "name": "contacts", "permissions": ["contact:read"] }),
    )
    .await
    .ok();
    let secret = key.body["secret"].as_str().unwrap();
    let hits = app
        .get("/api/v1/search?q=harbour")
        .bearer(secret)
        .workspace(ws)
        .send()
        .await
        .ok();
    assert_eq!(hits.body.as_array().unwrap().len(), 1);
    assert_eq!(hits.body[0]["entity"], "contact");
    // Entities it may not read are left out, not refused, and nothing
    // typed finds nothing.
    let none = app
        .get("/api/v1/search?q=harbour&entities=talent")
        .bearer(secret)
        .workspace(ws)
        .send()
        .await
        .ok();
    assert_eq!(none.body, json!([]));
    assert_eq!(
        get(&app, &owner, "/api/v1/search?q=%20").await.body,
        json!([])
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn duplicates_are_found_by_email_phone_tax_id_and_name(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let jo = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Joanna Smith", "emails": ["jo@example.com"], "phones": ["+44 7700 900123"] }),
    )
    .await
    .id("/id");

    let check = |body: Value| {
        let app = &app;
        let owner = &owner;
        async move {
            post(app, owner, "/api/v1/contacts/duplicates", body)
                .await
                .ok()
                .body
        }
    };
    let same_email = check(json!({ "name": "Someone Else", "emails": ["JO@example.com"] })).await;
    assert_eq!(same_email[0]["id"], jo.to_string());
    assert_eq!(
        same_email[0]["reasons"],
        json!(["same email jo@example.com"])
    );
    // The same number written another way.
    let same_phone = check(json!({ "name": "X", "phones": ["07700 900123"] })).await;
    assert_eq!(
        same_phone[0]["reasons"][0],
        "same phone number (ending 0123)"
    );
    let similar = check(json!({ "name": "Joanna Smyth" })).await;
    assert!(
        similar[0]["reasons"][0]
            .as_str()
            .unwrap()
            .starts_with("similar name"),
        "{similar}"
    );
    assert_eq!(check(json!({ "name": "Bob Jones" })).await, json!([]));
    assert_eq!(
        check(json!({ "name": "Joanna Smith", "except": jo })).await,
        json!([])
    );

    post(
        &app,
        &owner,
        "/api/v1/organisations",
        json!({ "name": "Northern Lights Ltd", "tax_id": "GB 123 456 789" }),
    )
    .await
    .ok();
    let org = post(
        &app,
        &owner,
        "/api/v1/organisations/duplicates",
        json!({ "name": "Totally Different", "tax_id": "gb123456789" }),
    )
    .await
    .ok();
    assert_eq!(org.body[0]["reasons"], json!(["same tax id"]));
    let talent = post(
        &app,
        &owner,
        "/api/v1/talent/duplicates",
        json!({ "name": "joanna smith" }),
    )
    .await
    .ok();
    assert_eq!(talent.body, json!([]));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_timeline_shows_who_changed_what(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    post(
        &app,
        &owner,
        "/api/v1/custom-fields",
        json!({ "entity": "contact", "key": "source", "label": "Source", "field_type": "text" }),
    )
    .await
    .ok();
    let org = post(
        &app,
        &owner,
        "/api/v1/organisations",
        json!({ "name": "Blue Room" }),
    )
    .await
    .id("/id");
    let jo = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Jo", "organisation_id": org }),
    )
    .await
    .id("/id");
    app.patch(&format!("/api/v1/contacts/{jo}"))
        .signed_in(&owner)
        .json(json!({ "name": "Jo Booker", "custom": { "source": "referral" } }))
        .send()
        .await
        .ok();

    let timeline = get(&app, &owner, &format!("/api/v1/contacts/{jo}/timeline")).await;
    let entries = timeline.body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    // Newest first.
    assert_eq!(entries[0]["action"], "contact.updated");
    assert_eq!(entries[0]["actor"]["name"], "acme owner");
    assert_eq!(
        entries[0]["changes"],
        json!([
            { "field": "custom.source", "before": null, "after": "referral" },
            { "field": "name", "before": "Jo", "after": "Jo Booker" }
        ])
    );
    assert_eq!(entries[1]["action"], "contact.created");
    assert!(timeline.body["next_before"].is_null());

    // The organisation's timeline includes what happened to its contacts.
    let org_timeline = get(
        &app,
        &owner,
        &format!("/api/v1/organisations/{org}/timeline?limit=2"),
    )
    .await;
    let entries = org_timeline.body["entries"].as_array().unwrap();
    assert_eq!(entries[0]["action"], "contact.updated");
    assert_eq!(entries[0]["object_name"], "Jo Booker");
    let next = org_timeline.body["next_before"].as_str().unwrap();
    let older = get(
        &app,
        &owner,
        &format!("/api/v1/organisations/{org}/timeline?before={next}"),
    )
    .await;
    assert_eq!(older.body["entries"][0]["action"], "organisation.created");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn bulk_actions_tag_and_delete_many_records(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let mut ids = Vec::new();
    for name in ["A", "B", "C"] {
        ids.push(
            post(
                &app,
                &owner,
                "/api/v1/contacts",
                json!({ "name": name, "tags": ["old"] }),
            )
            .await
            .id("/id"),
        );
    }
    let missing = uuid::Uuid::now_v7();
    let tagged = post(
        &app,
        &owner,
        "/api/v1/contacts/bulk",
        json!({ "action": "add_tags", "ids": [ids[0], ids[1], ids[1], missing], "tags": ["VIP"] }),
    )
    .await
    .ok();
    assert_eq!(tagged.body["done"], 2);
    assert_eq!(
        tagged.body["failed"],
        json!([{ "id": missing, "message": "not found" }])
    );
    let vip = get(&app, &owner, "/api/v1/contacts?tag=vip").await;
    assert_eq!(names(&vip.body["items"]), ["A", "B"]);
    assert_eq!(vip.body["items"][0]["tags"], json!(["old", "vip"]));

    post(
        &app,
        &owner,
        "/api/v1/contacts/bulk",
        json!({ "action": "remove_tags", "ids": ids, "tags": ["old"] }),
    )
    .await
    .ok();
    assert_eq!(
        get(&app, &owner, "/api/v1/contacts?tag=old").await.body["total"],
        0
    );
    post(
        &app,
        &owner,
        "/api/v1/contacts/bulk",
        json!({ "action": "add_tags", "ids": ids, "tags": [] }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");

    let deleted = post(
        &app,
        &owner,
        "/api/v1/contacts/bulk",
        json!({ "action": "delete", "ids": [ids[0], ids[2]] }),
    )
    .await
    .ok();
    assert_eq!(deleted.body["done"], 2);
    assert_eq!(
        names(&get(&app, &owner, "/api/v1/contacts").await.body["items"]),
        ["B"]
    );

    let viewer = app.add_member(&owner, "viewer@acme.test", "viewer").await;
    post(
        &app,
        &viewer,
        "/api/v1/contacts/bulk",
        json!({ "action": "delete", "ids": [ids[1]] }),
    )
    .await
    .expect(StatusCode::FORBIDDEN, "permission_denied");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn saved_views_are_private_unless_shared(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let agent = app.add_member(&owner, "agent@acme.test", "agent").await;
    let config =
        json!({ "columns": ["name", "email"], "sort": "-created_at", "filters": { "tag": "vip" } });

    let mine = post(
        &app,
        &agent,
        "/api/v1/views",
        json!({ "entity": "contact", "name": "My VIPs", "config": config }),
    )
    .await
    .ok();
    assert_eq!(mine.body["shared"], false);
    assert_eq!(mine.body["config"], config);
    // Sharing with everyone is configuration.
    post(
        &app,
        &agent,
        "/api/v1/views",
        json!({ "entity": "contact", "name": "Everyone's", "shared": true, "config": {} }),
    )
    .await
    .expect(StatusCode::FORBIDDEN, "permission_denied");
    let shared = post(
        &app,
        &owner,
        "/api/v1/views",
        json!({ "entity": "contact", "name": "All bookers", "shared": true, "config": {} }),
    )
    .await
    .ok();

    let agent_sees = get(&app, &agent, "/api/v1/views?entity=contact").await;
    assert_eq!(names(&agent_sees.body), ["All bookers", "My VIPs"]);
    let owner_sees = get(&app, &owner, "/api/v1/views?entity=contact").await;
    assert_eq!(names(&owner_sees.body), ["All bookers"]);
    // Someone else's private view does not exist for the owner.
    app.delete(&format!("/api/v1/views/{}", mine.id("/id")))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
    app.put(&format!("/api/v1/views/{}", shared.id("/id")))
        .signed_in(&agent)
        .json(json!({ "entity": "contact", "name": "Mine now", "shared": true, "config": {} }))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
    let renamed = app
        .put(&format!("/api/v1/views/{}", mine.id("/id")))
        .signed_in(&agent)
        .json(json!({ "entity": "contact", "name": "VIPs", "config": config }))
        .send()
        .await
        .ok();
    assert_eq!(renamed.body["name"], "VIPs");
    post(
        &app,
        &agent,
        "/api/v1/views",
        json!({ "entity": "contact", "name": "Bad", "config": [] }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_schema_lists_core_and_custom_fields_for_forms_and_tables(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    post(
        &app,
        &owner,
        "/api/v1/custom-fields",
        json!({ "entity": "contact", "key": "dietary", "label": "Dietary needs", "field_type": "text",
                "group": "Hospitality", "help_text": "Allergies and the like" }),
    )
    .await
    .ok();

    let schema = get(&app, &owner, "/api/v1/schema/contact").await;
    assert_eq!(schema.body["entity"], "contact");
    assert_eq!(schema.body["label_plural"], "Contacts");
    assert_eq!(schema.body["can_write"], true);
    let fields = schema.body["fields"].as_array().unwrap();
    let field = |key: &str| {
        fields
            .iter()
            .find(|f| f["key"] == key)
            .unwrap_or_else(|| panic!("no {key}"))
    };
    assert_eq!(field("name")["required"], true);
    assert_eq!(field("name")["core"], true);
    assert_eq!(field("emails")["field_type"], "email_list");
    assert_eq!(field("organisation_id")["relation_entity"], "organisation");
    let dietary = field("custom.dietary");
    assert_eq!(dietary["core"], false);
    assert_eq!(dietary["label"], "Dietary needs");
    assert_eq!(dietary["group"], "Hospitality");
    assert_eq!(dietary["sort"], "custom.dietary");

    let viewer = app.add_member(&owner, "viewer@acme.test", "viewer").await;
    let theirs = get(&app, &viewer, "/api/v1/schema/contact").await;
    assert_eq!(theirs.body["can_write"], false);
    app.get("/api/v1/schema/thing-that-is-not")
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");

    // Custom values are checked against their fields.
    post(
        &app,
        &owner,
        "/api/v1/custom-fields",
        json!({ "entity": "contact", "key": "rating", "label": "Rating", "field_type": "number",
                "validation": { "min": 1, "max": 5 } }),
    )
    .await
    .ok();
    let bad = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Jo", "custom": { "rating": 9, "made_up": "x" } }),
    )
    .await;
    bad.expect(StatusCode::BAD_REQUEST, "invalid_input");
    let ok = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Jo", "custom": { "rating": 4 } }),
    )
    .await
    .ok();
    assert_eq!(ok.body["custom"]["rating"], 4);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn imports_suggest_a_mapping_and_dry_run_without_writing(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    post(
        &app,
        &owner,
        "/api/v1/custom-fields",
        json!({ "entity": "contact", "key": "budget", "label": "Budget", "field_type": "money",
                "validation": { "currency": "GBP" } }),
    )
    .await
    .ok();
    post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Existing Person", "emails": ["jo@example.com"] }),
    )
    .await
    .ok();

    let csv = "\u{feff}Full Name;E-mail;Company;Budget;Notes\n\
               Jo Smith;JO@example.com;Blue Room;£1,200.50;\n\
               Sam Jones;sam@example.com;Blue Room;300;likes jazz\n\
               ;nobody@example.com;;;\n\
               Al Bad;not-an-email;;abc;\n";
    let upload = post(
        &app,
        &owner,
        "/api/v1/imports",
        json!({ "entity": "contact", "filename": "people.csv", "csv": csv }),
    )
    .await;
    assert_eq!(upload.status, StatusCode::CREATED, "{}", upload.body);
    let import = upload.id("/import/id");
    assert_eq!(upload.body["import"]["status"], "uploaded");
    assert_eq!(upload.body["import"]["row_count"], 4);
    assert_eq!(
        upload.body["import"]["headers"],
        json!(["Full Name", "E-mail", "Company", "Budget", "Notes"])
    );
    assert_eq!(
        upload.body["suggested"],
        json!(["name", "emails", "organisation", "custom.budget", "notes"])
    );
    assert_eq!(upload.body["sample"][0][0], "Jo Smith");

    let plan = json!({ "columns": upload.body["suggested"], "options": { "on_match": "update" } });
    let dry = post(
        &app,
        &owner,
        &format!("/api/v1/imports/{import}/dry-run"),
        plan.clone(),
    )
    .await
    .ok();
    assert_eq!(dry.body["create"], 1);
    assert_eq!(dry.body["update"], 1);
    assert_eq!(dry.body["fail"], 2);
    assert_eq!(dry.body["new_organisations"], json!(["Blue Room"]));
    let issues: Vec<(u64, &str)> = dry.body["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["row"].as_u64().unwrap(),
                i["field"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(issues, [(4, "name"), (5, "custom.budget"), (5, "emails")]);
    assert_eq!(
        dry.body["sample"][0]["custom"]["budget"],
        json!({ "minor": 120_050, "currency": "GBP" })
    );
    // Nothing was written.
    assert_eq!(get(&app, &owner, "/api/v1/contacts").await.body["total"], 1);
    assert_eq!(
        get(&app, &owner, "/api/v1/organisations").await.body["total"],
        0
    );

    // A mapping that does not fit the sheet is refused up front.
    post(
        &app,
        &owner,
        &format!("/api/v1/imports/{import}/run"),
        json!({ "columns": ["name", "nonsense"] }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
    let queued = post(
        &app,
        &owner,
        &format!("/api/v1/imports/{import}/run"),
        plan.clone(),
    )
    .await
    .ok();
    assert_eq!(queued.body["status"], "queued");
    post(&app, &owner, &format!("/api/v1/imports/{import}/run"), plan)
        .await
        .expect(StatusCode::CONFLICT, "import_started");
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'crm.import'")
        .fetch_one(&app.owner)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    let list = get(&app, &owner, "/api/v1/imports").await;
    assert_eq!(list.body[0]["id"], import.to_string());
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn imports_refuse_files_they_cannot_read(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    for (entity, csv) in [
        ("contact", ""),
        ("contact", "Name\n"),
        ("thing", "Name\nX\n"),
    ] {
        post(
            &app,
            &owner,
            "/api/v1/imports",
            json!({ "entity": entity, "csv": csv }),
        )
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");
    }
    // Bigger than any other request may be, but within the upload limit.
    let row = format!("{},x@example.com\n", "n".repeat(100));
    let big = format!("Name,Email\n{}", row.repeat(20_000));
    assert!(big.len() > 2 * 1024 * 1024);
    post(
        &app,
        &owner,
        "/api/v1/imports",
        json!({ "entity": "contact", "csv": big }),
    )
    .await
    .ok();
    // Past the upload limit.
    let huge = format!("Name\n{}", "n\n".repeat(6 * 1024 * 1024));
    let res = post(
        &app,
        &owner,
        "/api/v1/imports",
        json!({ "entity": "contact", "csv": huge }),
    )
    .await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);
    // Other routes keep the ordinary limit.
    let res = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "x", "notes": "n".repeat(2 * 1024 * 1024) }),
    )
    .await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);

    let viewer = app.add_member(&owner, "viewer@acme.test", "viewer").await;
    post(
        &app,
        &viewer,
        "/api/v1/imports",
        json!({ "entity": "contact", "csv": "Name\nX\n" }),
    )
    .await
    .expect(StatusCode::FORBIDDEN, "permission_denied");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn rules_and_templates_work_on_crm_records(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    post(
        &app,
        &owner,
        "/api/v1/custom-fields",
        json!({ "entity": "contact", "key": "stage", "label": "Stage", "field_type": "text" }),
    )
    .await
    .ok();
    let jo = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Jo", "emails": ["jo@example.com"], "custom": { "stage": "lead" } }),
    )
    .await
    .id("/id");

    let preview = post(
        &app,
        &owner,
        "/api/v1/templates/preview",
        json!({ "kind": "email", "subject": "Hi {{ contact.name }}", "body": "Stage: {{ contact.custom.stage }}",
                "entity": "contact", "record_id": jo }),
    )
    .await
    .ok();
    assert_eq!(preview.body["subject"], "Hi Jo");
    assert_eq!(preview.body["body"], "Stage: lead");

    let vars = get(&app, &owner, "/api/v1/templates/variables?entity=contact").await;
    let paths: Vec<&str> = vars
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"contact.email"));
    assert!(paths.contains(&"contact.custom.stage"));

    let rule = post(
        &app,
        &owner,
        "/api/v1/rules",
        json!({ "name": "Welcome", "trigger": { "kind": "event", "topic": "contact.created" },
                "actions": [{ "type": "set_field", "field": "custom.stage", "value": "welcomed" }] }),
    )
    .await
    .ok();
    let dry = post(
        &app,
        &owner,
        &format!("/api/v1/rules/{}/dry-run", rule.id("/id")),
        json!({ "record_id": jo }),
    )
    .await
    .ok();
    assert_eq!(dry.body["matched"], true, "{}", dry.body);
    // Things-only actions are refused on contacts.
    post(
        &app,
        &owner,
        "/api/v1/rules",
        json!({ "name": "Nope", "trigger": { "kind": "event", "topic": "contact.created" },
                "actions": [{ "type": "move_stage", "stage": "won" }] }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
}
