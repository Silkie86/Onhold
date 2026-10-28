//! Pitching, part 1: where each organisation is and where things stand with
//! it, the lists of what it wants, the workspace's own settings for them,
//! and organisations moving along by themselves as deals come in.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{Client, TestApp};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn post(app: &TestApp, who: &Client, path: &str, body: Value) -> common::Resp {
    app.post(path).signed_in(who).json(body).send().await
}

async fn patch(app: &TestApp, who: &Client, path: &str, body: Value) -> common::Resp {
    app.patch(path).signed_in(who).json(body).send().await
}

async fn get(app: &TestApp, who: &Client, path: &str) -> common::Resp {
    app.get(path).signed_in(who).send().await.ok()
}

fn fields(res: &common::Resp) -> Vec<String> {
    res.body["error"]["issues"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|i| i["field"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_else(|| vec![res.body["error"]["field"].as_str().unwrap_or("").to_owned()])
}

async fn organisation(app: &TestApp, who: &Client, body: Value) -> Value {
    let res = post(app, who, "/api/v1/organisations", body).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    res.body
}

async fn talent(app: &TestApp, who: &Client, name: &str) -> Uuid {
    post(app, who, "/api/v1/talent", json!({ "name": name }))
        .await
        .ok()
        .id("/id")
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn organisations_know_where_they_are_and_where_things_stand(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;

    // The country is read however it was typed, or from a well-known city.
    let hall = organisation(
        &app,
        &owner,
        json!({ "name": "Hamer Hall", "addresses": [{ "line1": "100 St Kilda Rd", "city": "Melbourne" }] }),
    )
    .await;
    assert_eq!(hall["country_code"], "AU");
    assert_eq!(hall["country"], "Australia");
    assert_eq!(hall["city"], "Melbourne");
    assert_eq!(hall["region"], "oceania");
    assert_eq!(hall["region_picked"], false);
    assert_eq!(hall["relationship"], Value::Null);
    assert_eq!(hall["status"], Value::Null);
    assert_eq!(hall["do_not_contact"], false);

    let club = organisation(
        &app,
        &owner,
        json!({ "name": "Hyde Park Book Club", "addresses": [{ "city": " Leeds ", "country": "England" }],
                "relationship": "prospect", "status": "talking", "follow_up_on": "2031-01-05" }),
    )
    .await;
    assert_eq!(club["country_code"], "GB");
    assert_eq!(club["city"], "Leeds");
    assert_eq!(club["region"], "europe");
    assert_eq!(club["relationship"], "prospect");
    assert_eq!(club["status"], "talking");
    assert_eq!(club["follow_up_on"], "2031-01-05");
    let club_id = club["id"].as_str().unwrap();
    let path = format!("/api/v1/organisations/{club_id}");

    // A region picked by hand wins over the country's, until cleared.
    let picked = patch(&app, &owner, &path, json!({ "region": "asia" }))
        .await
        .ok();
    assert_eq!(picked.body["region"], "asia");
    assert_eq!(picked.body["region_picked"], true);
    let back = patch(&app, &owner, &path, json!({ "region": null }))
        .await
        .ok();
    assert_eq!(back.body["region"], "europe");
    assert_eq!(back.body["region_picked"], false);

    // Moving the address moves the country.
    let moved = patch(
        &app,
        &owner,
        &path,
        json!({ "addresses": [{ "city": "Lisbon", "country": "Portgual" }] }),
    )
    .await
    .ok();
    assert_eq!(moved.body["country_code"], "PT");
    assert_eq!(moved.body["region"], "europe");

    // Regions and statuses come from the workspace's settings.
    let bad = patch(
        &app,
        &owner,
        &path,
        json!({ "region": "mars", "status": "maybe", "buys_from_others": ["x".repeat(101)] }),
    )
    .await;
    bad.expect(StatusCode::BAD_REQUEST, "invalid_input");
    assert_eq!(fields(&bad), ["region", "status", "buys_from_others"]);

    // What they want: two lists, never mixed, without repeats; similar acts they buy elsewhere.
    let owls = talent(&app, &owner, "The Midnight Owls").await;
    let lark = talent(&app, &owner, "DJ Lark").await;
    let lists = patch(
        &app,
        &owner,
        &path,
        json!({
            "interested_in": [{ "kind": "talent", "id": owls }],
            "potential_matches": [{ "kind": "talent", "id": lark }, { "kind": "talent", "id": lark },
                                  { "kind": "talent", "id": owls }],
            "buys_from_others": ["Bonobo", " bonobo ", "Floating  Points"],
            "do_not_contact": true
        }),
    )
    .await
    .ok();
    let o = &lists.body;
    assert_eq!(
        o["interested_in"],
        json!([{ "kind": "talent", "id": owls, "name": "The Midnight Owls" }])
    );
    assert_eq!(
        o["potential_matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["DJ Lark", "The Midnight Owls"]
    );
    assert_eq!(o["buys_from_others"], json!(["Bonobo", "Floating Points"]));
    assert_eq!(o["do_not_contact"], true);
    // Sending the same lists again changes nothing.
    let version = o["version"].clone();
    let same = patch(
        &app,
        &owner,
        &path,
        json!({ "interested_in": [{ "kind": "talent", "id": owls }] }),
    )
    .await
    .ok();
    assert_eq!(same.body["version"], version);
    let unknown = patch(
        &app,
        &owner,
        &path,
        json!({ "interested_in": [{ "kind": "project", "id": Uuid::now_v7() }] }),
    )
    .await;
    unknown.expect(StatusCode::BAD_REQUEST, "invalid_input");
    assert_eq!(fields(&unknown), ["interested_in"]);

    // Lists carry the new fields too, and sort by them.
    let list = get(&app, &owner, "/api/v1/organisations?sort=country_code").await;
    let names: Vec<&str> = list.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Hamer Hall", "Hyde Park Book Club"]);
    assert_eq!(
        list.body["items"][1]["interested_in"][0]["name"],
        "The Midnight Owls"
    );

    // People: social links as full addresses, and numbers with their country code.
    let person = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Rita Reyes", "organisation_id": hall["id"], "phones": ["412 345 678"],
                "links": ["instagram.com/hpbc", "https://hpbc.example/", "instagram.com/hpbc"] }),
    )
    .await
    .ok();
    assert_eq!(
        person.body["links"],
        json!(["https://instagram.com/hpbc", "https://hpbc.example/"])
    );
    // Not a national number (no leading 0): kept as it is.
    assert_eq!(person.body["phones_international"], json!(["412 345 678"]));
    let person_id = person.id("/id");
    let rang = patch(
        &app,
        &owner,
        &format!("/api/v1/contacts/{person_id}"),
        json!({ "phones": ["0412 345 678", "+1 212 555 0199"] }),
    )
    .await
    .ok();
    assert_eq!(
        rang.body["phones_international"],
        json!(["+61 412 345 678", "+1 212 555 0199"])
    );
    assert_eq!(rang.body["phones"][0], "0412 345 678");
    let bad_link = patch(
        &app,
        &owner,
        &format!("/api/v1/contacts/{person_id}"),
        json!({ "links": ["not a link", "ftp://files.example"] }),
    )
    .await;
    bad_link.expect(StatusCode::BAD_REQUEST, "invalid_input");
    assert_eq!(fields(&bad_link), ["links", "links"]);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn deals_move_organisations_along_by_themselves(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let owls = talent(&app, &owner, "The Midnight Owls").await;
    let hall = organisation(&app, &owner, json!({ "name": "The Grand Hall" })).await;
    let hall_id = hall["id"].as_str().unwrap().to_owned();
    let read = |id: String| {
        let app = &app;
        let owner = &owner;
        async move {
            get(app, owner, &format!("/api/v1/organisations/{id}"))
                .await
                .body
        }
    };

    // An enquiry makes them a prospect, talking.
    post(
        &app,
        &owner,
        "/api/v1/enquiries",
        json!({ "title": "Summer ball", "organisation_id": hall_id, "talent_ids": [owls] }),
    )
    .await
    .ok();
    let h = read(hall_id.clone()).await;
    assert_eq!(h["relationship"], "prospect");
    assert_eq!(h["status"], "talking");

    // A hold changes nothing more; confirming it makes them a client, booked.
    let hold = post(
        &app,
        &owner,
        "/api/v1/bookings",
        json!({ "title": "Summer ball", "talent_ids": [owls], "organisation_id": hall_id, "status": "hold",
                "starts_at": "2031-06-01T19:00:00Z", "ends_at": "2031-06-01T23:00:00Z" }),
    )
    .await
    .ok();
    assert_eq!(read(hall_id.clone()).await["relationship"], "prospect");
    let hold_id = hold.id("/id");
    post(
        &app,
        &owner,
        &format!("/api/v1/bookings/{hold_id}/status"),
        json!({ "status": "confirmed" }),
    )
    .await
    .ok();
    let h = read(hall_id.clone()).await;
    assert_eq!(h["relationship"], "client");
    assert_eq!(h["status"], "booked");

    // Never back: a later enquiry leaves a client a client, and booked.
    post(
        &app,
        &owner,
        "/api/v1/enquiries",
        json!({ "title": "Winter ball", "organisation_id": hall_id }),
    )
    .await
    .ok();
    let h = read(hall_id.clone()).await;
    assert_eq!(h["relationship"], "client");
    assert_eq!(h["status"], "booked");

    // A status set by hand that is not booked yet gives way to a confirmed booking.
    let club = organisation(
        &app,
        &owner,
        json!({ "name": "Night Owl Club", "status": "inactive" }),
    )
    .await;
    let club_id = club["id"].as_str().unwrap().to_owned();
    post(
        &app,
        &owner,
        "/api/v1/bookings",
        json!({ "title": "Residency", "talent_ids": [owls], "organisation_id": club_id, "status": "confirmed",
                "starts_at": "2031-07-01T19:00:00Z", "ends_at": "2031-07-01T23:00:00Z" }),
    )
    .await
    .ok();
    let c = read(club_id).await;
    assert_eq!(c["relationship"], "client");
    assert_eq!(c["status"], "booked");

    // A status the workspace no longer lists counts as none, so it gives way.
    let bar = organisation(
        &app,
        &owner,
        json!({ "name": "Owl Bar", "status": "talking" }),
    )
    .await;
    let bar_id = bar["id"].as_str().unwrap().to_owned();
    app.put("/api/v1/pitching/settings")
        .signed_in(&owner)
        .header("If-Match", "\"0\"")
        .json(json!({
            "statuses": [
                { "key": "booked", "label": "Booked", "colour": "green", "meaning": "booked" },
                { "key": "in_talks", "label": "In talks", "colour": "amber", "meaning": "talking" }
            ]
        }))
        .send()
        .await
        .ok();
    assert_eq!(read(bar_id.clone()).await["status"], Value::Null);
    post(
        &app,
        &owner,
        "/api/v1/enquiries",
        json!({ "title": "Quiz night", "organisation_id": bar_id }),
    )
    .await
    .ok();
    assert_eq!(read(bar_id).await["status"], "in_talks");

    // Each move is an ordinary, audited change.
    let actions = app.audit_actions(ws).await;
    assert!(
        actions
            .iter()
            .filter(|a| *a == "organisation.updated")
            .count()
            >= 3,
        "{actions:?}"
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn settings_are_the_workspaces_own(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let (_, other) = app.workspace_with_owner("rival").await;

    let defaults = get(&app, &owner, "/api/v1/pitching/settings").await;
    let s = &defaults.body;
    assert_eq!(s["version"], 0);
    assert_eq!(
        s["regions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "Europe (incl. UK)",
            "North & Central America",
            "South America",
            "Oceania",
            "Asia",
            "Africa"
        ]
    );
    assert_eq!(s["statuses"][0]["key"], "booked");
    assert_eq!(s["statuses"][0]["colour"], "green");
    assert_eq!(s["items"], json!(["talent"]));
    assert!(s["countries"].as_array().unwrap().len() > 240);

    let club = organisation(
        &app,
        &owner,
        json!({ "name": "Hyde Park Book Club", "addresses": [{ "country": "UK" }], "status": "talking" }),
    )
    .await;
    let club_id = club["id"].as_str().unwrap().to_owned();

    // The UK gets a region of its own; "talking" goes; projects can be pitched.
    let mut regions = s["regions"].as_array().unwrap().clone();
    let europe = regions[0]["countries"].as_array_mut().unwrap();
    europe.retain(|c| c != "GB");
    regions.push(json!({ "key": "uk", "name": "UK", "countries": ["gb"] }));
    let body = json!({
        "regions": regions,
        "statuses": [
            { "key": "booked", "label": "Worked with", "colour": "green", "meaning": "booked" },
            { "key": "cold", "label": "Cold", "colour": "blue", "meaning": "other" }
        ],
        "items": ["talent", "project"]
    });
    let stale = app
        .put("/api/v1/pitching/settings")
        .signed_in(&owner)
        .header("If-Match", "\"7\"")
        .json(body.clone())
        .send()
        .await;
    stale.expect(StatusCode::PRECONDITION_FAILED, "version_conflict");
    let saved = app
        .put("/api/v1/pitching/settings")
        .signed_in(&owner)
        .header("If-Match", "\"0\"")
        .json(body.clone())
        .send()
        .await
        .ok();
    assert_eq!(saved.body["version"], 1);
    assert_eq!(saved.body["items"], json!(["talent", "project"]));

    let c = get(&app, &owner, &format!("/api/v1/organisations/{club_id}")).await;
    assert_eq!(c.body["region"], "uk");
    // A status no longer listed reads as not set.
    assert_eq!(c.body["status"], Value::Null);

    // Forms offer the workspace's own regions and statuses, with colours.
    let schema = get(&app, &owner, "/api/v1/schema/organisation").await;
    let field = |key: &str| {
        schema.body["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["key"] == key)
            .cloned()
    };
    let status = field("status").unwrap();
    assert_eq!(
        status["options"][1],
        json!({ "value": "cold", "label": "Cold", "colour": "blue" })
    );
    assert!(
        field("region").unwrap()["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["value"] == "uk")
    );
    assert_eq!(field("country").unwrap()["read_only"], true);

    // A country in two regions, or an unknown one, is refused.
    let mut twice = body.clone();
    twice["regions"][0]["countries"]
        .as_array_mut()
        .unwrap()
        .push(json!("GB"));
    let refused = app
        .put("/api/v1/pitching/settings")
        .signed_in(&owner)
        .json(twice)
        .send()
        .await;
    refused.expect(StatusCode::BAD_REQUEST, "invalid_input");

    // Only those who change settings may; other workspaces keep their own.
    let agent = app.add_member(&owner, "agent@acme.test", "agent").await;
    app.put("/api/v1/pitching/settings")
        .signed_in(&agent)
        .json(body)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
    let theirs = get(&app, &other, "/api/v1/pitching/settings").await;
    assert_eq!(theirs.body["version"], 0);
    assert_eq!(theirs.body["items"], json!(["talent"]));

    // Without the pitching module, its fields leave the forms.
    app.put("/api/v1/workspace/setup")
        .signed_in(&owner)
        .json(json!({ "answers": { "roles": ["agency"], "features": ["bookings"], "team": "small" } }))
        .send()
        .await
        .ok();
    let schema = get(&app, &owner, "/api/v1/schema/organisation").await;
    let keys: Vec<&str> = schema.body["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["key"].as_str().unwrap())
        .collect();
    assert!(!keys.contains(&"status"), "{keys:?}");
    assert!(!keys.contains(&"follow_up_on"));
    assert!(keys.contains(&"relationship"));
    assert!(keys.contains(&"region"));
}
