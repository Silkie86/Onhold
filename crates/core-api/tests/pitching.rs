//! Pitching, part 1: where each organisation is and where things stand with
//! it, the lists of what it wants, the workspace's own settings for them,
//! and organisations moving along by themselves as deals come in; and the
//! pitch log of what was done and the answers given.

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

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn pitches_and_answers_are_logged_by_hand(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let viewer = app.add_member(&owner, "viewer@acme.test", "viewer").await;
    let club = organisation(&app, &owner, json!({ "name": "Hyde Park Book Club" })).await;
    let club_id = club["id"].as_str().unwrap().to_owned();
    let other = organisation(&app, &owner, json!({ "name": "Brudenell" })).await;
    let jo = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Jo Booker", "organisation_id": club_id }),
    )
    .await
    .ok()
    .id("/id");
    let elsewhere = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Sam", "organisation_id": other["id"] }),
    )
    .await
    .ok()
    .id("/id");
    let owls = talent(&app, &owner, "The Midnight Owls").await;

    // The workspace's own actions and answers, OnHold's to start with.
    let s = get(&app, &owner, "/api/v1/pitching/settings").await.body;
    assert_eq!(
        s["actions"][0],
        json!({ "key": "pitched", "label": "Pitched" })
    );
    assert_eq!(
        s["outcomes"][0],
        json!({ "key": "interested", "label": "Interested or yes", "tag": "good" })
    );

    // Pitched by email last week, then the answer today.
    let pitched = post(
        &app,
        &owner,
        "/api/v1/pitches",
        json!({ "organisation_id": club_id, "contact_id": jo, "kind": "action", "type_key": "pitched",
                "channel": "email", "item": { "kind": "talent", "id": owls }, "note": "Sent the EPK",
                "happened_at": "2026-10-21T10:00:00Z" }),
    )
    .await;
    assert_eq!(pitched.status, StatusCode::CREATED, "{}", pitched.body);
    assert_eq!(pitched.body["type_label"], "Pitched");
    assert_eq!(pitched.body["contact_name"], "Jo Booker");
    assert_eq!(pitched.body["item"]["name"], "The Midnight Owls");
    assert_eq!(pitched.body["source"], "manual");
    let answer = post(
        &app,
        &owner,
        "/api/v1/pitches",
        json!({ "organisation_id": club_id, "kind": "outcome", "type_key": "not_now", "channel": "phone" }),
    )
    .await
    .ok();
    assert_eq!(answer.body["tag"], "wait");

    // What they cannot be, all said at once.
    let bad = post(
        &app,
        &owner,
        "/api/v1/pitches",
        json!({ "organisation_id": club_id, "contact_id": elsewhere, "kind": "outcome",
                "type_key": "pitched", "item": { "kind": "talent", "id": Uuid::now_v7() },
                "happened_at": "2099-01-01T00:00:00Z", "note": "x".repeat(2001) }),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
    let mut f = fields(&bad);
    f.sort();
    assert_eq!(f, ["contact_id", "happened_at", "item", "note", "type_key"]);
    let tomorrow = (chrono::Utc::now() + chrono::Duration::hours(20)).to_rfc3339();
    let res = post(
        &app,
        &owner,
        "/api/v1/pitches",
        json!({ "organisation_id": club_id, "kind": "action", "type_key": "called", "happened_at": tomorrow }),
    )
    .await;
    assert_eq!(fields(&res), ["happened_at"]);
    post(
        &app,
        &viewer,
        "/api/v1/pitches",
        json!({ "organisation_id": club_id, "kind": "action", "type_key": "called" }),
    )
    .await
    .expect(StatusCode::FORBIDDEN, "permission_denied");

    // Latest first; viewers can read it.
    let log = get(
        &app,
        &viewer,
        &format!("/api/v1/organisations/{club_id}/pitches"),
    )
    .await;
    assert_eq!(log.body["total"], 2);
    assert_eq!(log.body["items"][0]["type_key"], "not_now");
    assert_eq!(log.body["items"][1]["type_key"], "pitched");
    let none = get(
        &app,
        &owner,
        &format!(
            "/api/v1/organisations/{}/pitches",
            other["id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(none.body["total"], 0);

    // Edited: the answer was really "interested", and with Jo.
    let id = answer.id("/id");
    let edited = app
        .patch(&format!("/api/v1/pitches/{id}"))
        .signed_in(&owner)
        .header("If-Match", "\"1\"")
        .json(json!({ "type_key": "interested", "contact_id": jo, "note": "Wants a date in May" }))
        .send()
        .await
        .ok();
    assert_eq!(edited.body["tag"], "good");
    assert_eq!(edited.body["contact_name"], "Jo Booker");
    assert_eq!(edited.body["version"], 2);
    // An action's type is one of the actions, not the answers.
    patch(
        &app,
        &owner,
        &format!("/api/v1/pitches/{}", pitched.id("/id")),
        json!({ "type_key": "interested" }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");

    // A removed answer type still shows, unlabelled, and can be edited.
    let mut body = s.clone();
    body["outcomes"] = json!([{ "key": "yes", "label": "Yes", "tag": "good" }]);
    body["actions"] = json!([{ "key": "pitched", "label": "Sent it" }]);
    // Templates and follow-ups left out, those using an action no longer
    // listed go too.
    body.as_object_mut().unwrap().remove("templates");
    body.as_object_mut().unwrap().remove("follow_ups");
    app.put("/api/v1/pitching/settings")
        .signed_in(&owner)
        .json(body)
        .send()
        .await
        .ok();
    // Lists left out, by a client from before they existed, stay.
    let older = json!({ "regions": s["regions"], "statuses": s["statuses"], "items": s["items"] });
    let saved = app
        .put("/api/v1/pitching/settings")
        .signed_in(&owner)
        .json(older)
        .send()
        .await
        .ok();
    assert_eq!(saved.body["outcomes"][0]["key"], "yes");
    assert_eq!(saved.body["actions"][0]["label"], "Sent it");
    let log = get(
        &app,
        &owner,
        &format!("/api/v1/organisations/{club_id}/pitches"),
    )
    .await;
    assert_eq!(log.body["items"][0]["type_label"], Value::Null);
    assert_eq!(log.body["items"][0]["tag"], Value::Null);
    patch(
        &app,
        &owner,
        &format!("/api/v1/pitches/{id}"),
        json!({ "note": "Still keen" }),
    )
    .await
    .ok();

    // Removed.
    let gone = app
        .delete(&format!("/api/v1/pitches/{id}"))
        .signed_in(&owner)
        .send()
        .await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT, "{}", gone.body);
    app.delete(&format!("/api/v1/pitches/{id}"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
    let log = get(
        &app,
        &owner,
        &format!("/api/v1/organisations/{club_id}/pitches"),
    )
    .await;
    assert_eq!(log.body["total"], 1);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn pitches_are_drafted_and_sent(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let viewer = app.add_member(&owner, "viewer@acme.test", "viewer").await;
    let club = organisation(
        &app,
        &owner,
        json!({ "name": "Hyde Park Book Club", "addresses": [{ "city": "Leeds", "country": "England" }] }),
    )
    .await;
    let club_id = club["id"].as_str().unwrap().to_owned();
    let jo = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Jo Booker", "organisation_id": club_id, "emails": ["jo@club.test"],
                "phones": ["07700 900123"] }),
    )
    .await
    .ok()
    .id("/id");
    let owls = talent(&app, &owner, "The Midnight Owls").await;

    // OnHold's own templates and default to start with.
    let s = get(&app, &owner, "/api/v1/pitching/settings").await.body;
    assert_eq!(s["send_with"], "onhold");
    assert_eq!(s["templates"][0]["key"], "first_pitch");
    assert_eq!(s["templates"][1]["action"], "followed_up");

    // Filled in for Jo and the Owls; nothing gives a link.
    let draft = post(
        &app,
        &viewer,
        &format!("/api/v1/organisations/{club_id}/pitches/draft"),
        json!({ "template_key": "first_pitch", "contact_id": jo, "item": { "kind": "talent", "id": owls } }),
    )
    .await
    .ok();
    assert_eq!(
        draft.body["subject"],
        "The Midnight Owls for Hyde Park Book Club"
    );
    let body = draft.body["body"].as_str().unwrap().to_owned();
    assert!(
        body.starts_with("Hi Jo,\n\nI look after The Midnight Owls"),
        "{body}"
    );
    assert!(body.contains("{link}"), "{body}");
    assert_eq!(draft.body["missing"], json!(["link"]));
    assert_eq!(draft.body["email"], "jo@club.test");
    assert!(
        draft.body["phone"].as_str().unwrap().starts_with("+44"),
        "{}",
        draft.body
    );
    assert_eq!(draft.body["action"], "pitched");

    // What is pitched fills in only for those who may read it.
    let key = post(
        &app,
        &owner,
        "/api/v1/api-keys",
        json!({ "name": "crm", "permissions": ["organisation:read", "contact:read"] }),
    )
    .await
    .ok();
    let secret = key.body["secret"].as_str().unwrap().to_owned();
    let blind = app
        .post(&format!("/api/v1/organisations/{club_id}/pitches/draft"))
        .bearer(&secret)
        .workspace(ws)
        .json(json!({ "template_key": "first_pitch", "contact_id": jo, "item": { "kind": "talent", "id": owls } }))
        .send()
        .await
        .ok();
    assert_eq!(blind.body["missing"], json!(["item", "link", "yourName"]));
    assert!(!blind.body["body"].as_str().unwrap().contains("Owls"));

    // A name that only stands in for one (their email) is no name.
    let nameless = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "sam@club.test", "organisation_id": club_id, "emails": ["sam@club.test"] }),
    )
    .await
    .ok()
    .id("/id");
    let draft_sam = post(
        &app,
        &owner,
        &format!("/api/v1/organisations/{club_id}/pitches/draft"),
        json!({ "template_key": "first_pitch", "contact_id": nameless }),
    )
    .await
    .ok();
    assert!(
        draft_sam.body["body"]
            .as_str()
            .unwrap()
            .starts_with("Hi {firstName},"),
        "{}",
        draft_sam.body
    );

    // Not sent while a placeholder is left, nor by a viewer.
    let send = |body: &str| {
        json!({ "organisation_id": club_id, "contact_id": jo, "item": { "kind": "talent", "id": owls },
                "template_key": "first_pitch", "subject": draft.body["subject"], "body": body })
    };
    let left = post(&app, &owner, "/api/v1/pitches/send", send(&body)).await;
    assert_eq!(left.status, StatusCode::BAD_REQUEST, "{}", left.body);
    assert!(
        left.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("{link}")
    );
    let filled = body.replace("{link}", "https://owls.test");
    post(&app, &viewer, "/api/v1/pitches/send", send(&filled))
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");

    // Sent from OnHold (no mailbox here, so through OnHold's own sender),
    // logged as pitched by email, with its message in the inbox.
    let sent = post(&app, &owner, "/api/v1/pitches/send", send(&filled)).await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.body);
    assert_eq!(sent.body["source"], "sent");
    assert_eq!(sent.body["type_key"], "pitched");
    assert_eq!(sent.body["channel"], "email");
    assert_eq!(sent.body["template_key"], "first_pitch");
    assert_eq!(sent.body["item"]["name"], "The Midnight Owls");
    assert_eq!(sent.body["message"]["status"], "queued");
    let thread = sent.id("/message/thread_id");
    let t = get(&app, &owner, &format!("/api/v1/inbox/threads/{thread}")).await;
    assert_eq!(t.body["thread"]["contact_id"], json!(jo));
    let m = &t.body["messages"][0];
    assert_eq!(m["to"], json!(["jo@club.test"]));
    assert!(m["text"].as_str().unwrap().contains("https://owls.test"));

    // Sent another way, then logged with one tap.
    let tapped = post(
        &app,
        &owner,
        "/api/v1/pitches",
        json!({ "organisation_id": club_id, "contact_id": jo, "kind": "action", "type_key": "followed_up",
                "channel": "whatsapp", "template_key": "follow_up" }),
    )
    .await;
    assert_eq!(tapped.status, StatusCode::CREATED, "{}", tapped.body);
    assert_eq!(tapped.body["source"], "manual");
    assert_eq!(tapped.body["message"], Value::Null);
    let bad = post(
        &app,
        &owner,
        "/api/v1/pitches",
        json!({ "organisation_id": club_id, "kind": "action", "type_key": "pitched", "template_key": "nope" }),
    )
    .await;
    assert_eq!(fields(&bad), ["template_key"]);
    let log = get(
        &app,
        &owner,
        &format!("/api/v1/organisations/{club_id}/pitches"),
    )
    .await;
    assert_eq!(log.body["total"], 2);
    assert_eq!(log.body["items"][1]["message"]["thread_id"], json!(thread));
    // Without the inbox, the conversation is left out.
    let log = app
        .get(&format!("/api/v1/organisations/{club_id}/pitches"))
        .bearer(&secret)
        .workspace(ws)
        .send()
        .await
        .ok();
    assert_eq!(log.body["items"][1]["source"], "sent");
    assert_eq!(log.body["items"][1]["message"], Value::Null);

    // Without a template, it logs the workspace's first action.
    let mut body = s.clone();
    body["actions"] = json!([{ "key": "emailed", "label": "Emailed" }, { "key": "pitched", "label": "Pitched" },
                             { "key": "followed_up", "label": "Followed up" }]);
    body.as_object_mut().unwrap().remove("follow_ups");
    app.put("/api/v1/pitching/settings")
        .signed_in(&owner)
        .json(body)
        .send()
        .await
        .ok();
    let blank = post(
        &app,
        &owner,
        "/api/v1/pitches/send",
        json!({ "organisation_id": club_id, "contact_id": jo, "subject": "Hello", "body": "Hi Jo" }),
    )
    .await;
    assert_eq!(blank.status, StatusCode::CREATED, "{}", blank.body);
    assert_eq!(blank.body["type_key"], "emailed");

    // Templates name one of the actions.
    let mut body = s.clone();
    body["templates"][0]["action"] = json!("nope");
    body["templates"][1]["body"] = json!("  ");
    let res = app
        .put("/api/v1/pitching/settings")
        .signed_in(&owner)
        .json(body)
        .send()
        .await;
    let mut f = fields(&res);
    f.sort();
    assert_eq!(f, ["templates[0].action", "templates[1].body"]);

    // Each member picks how they send, over the workspace's default.
    let me = get(&app, &viewer, "/api/v1/pitching/me").await.body;
    assert_eq!(me["send_with"], Value::Null);
    assert_eq!(me["workspace_send_with"], "onhold");
    assert_eq!(me["mailbox"], Value::Null);
    let me = app
        .put("/api/v1/pitching/me")
        .signed_in(&viewer)
        .json(json!({ "send_with": "copy" }))
        .send()
        .await
        .ok();
    assert_eq!(me.body["send_with"], "copy");
    let me = get(&app, &owner, "/api/v1/pitching/me").await.body;
    assert_eq!(me["send_with"], Value::Null);

    // Not to an organisation marked do not contact.
    patch(
        &app,
        &owner,
        &format!("/api/v1/organisations/{club_id}"),
        json!({ "do_not_contact": true }),
    )
    .await
    .ok();
    post(
        &app,
        &owner,
        &format!("/api/v1/organisations/{club_id}/pitches/draft"),
        json!({ "template_key": "first_pitch" }),
    )
    .await
    .expect(StatusCode::CONFLICT, "do_not_contact");
    post(&app, &owner, "/api/v1/pitches/send", send(&filled))
        .await
        .expect(StatusCode::CONFLICT, "do_not_contact");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn follow_ups_come_due_until_an_answer(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let club = organisation(&app, &owner, json!({ "name": "Hyde Park Book Club" })).await;
    let club_id = club["id"].as_str().unwrap().to_owned();
    let quiet = organisation(
        &app,
        &owner,
        json!({ "name": "Quiet Bar", "do_not_contact": true }),
    )
    .await;
    let owls = talent(&app, &owner, "The Midnight Owls").await;
    let due = |q: &str| {
        let path = format!("/api/v1/pitching/follow-ups{q}");
        let app = &app;
        let owner = &owner;
        async move { get(app, owner, &path).await.ok().body }
    };
    assert_eq!(due("").await, json!([]));

    // Pitched eight days ago: the email follow-up was due yesterday.
    let ago = |days: i64| (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
    for org in [&club_id, quiet["id"].as_str().unwrap()] {
        post(
            &app,
            &owner,
            "/api/v1/pitches",
            json!({ "organisation_id": org, "kind": "action", "type_key": "pitched", "channel": "email",
                    "item": { "kind": "talent", "id": owls }, "happened_at": ago(8) }),
        )
        .await
        .ok();
    }
    let d = due("").await;
    assert_eq!(d.as_array().unwrap().len(), 1, "{d}");
    assert_eq!(d[0]["organisation_name"], "Hyde Park Book Club");
    assert_eq!(d[0]["kind"], "step");
    assert_eq!(d[0]["step"], 1);
    assert_eq!(d[0]["steps"], 2);
    assert_eq!(d[0]["channel"], "email");
    assert_eq!(d[0]["template_key"], "follow_up");
    assert_eq!(d[0]["action"], "followed_up");
    assert_eq!(d[0]["item"]["name"], "The Midnight Owls");

    // Followed up today: the call is a week off.
    post(
        &app,
        &owner,
        "/api/v1/pitches",
        json!({ "organisation_id": club_id, "kind": "action", "type_key": "followed_up", "channel": "email" }),
    )
    .await
    .ok();
    assert_eq!(due("").await, json!([]));
    let week = due("?days_ahead=7").await;
    assert_eq!(week[0]["step"], 2, "{week}");
    assert_eq!(week[0]["channel"], "phone");
    assert_eq!(week[0]["template_key"], Value::Null);
    assert_eq!(week[0]["action_label"], "Called");
    // On the organisation, its next however far off.
    let next = due(&format!("?organisation_id={club_id}")).await;
    assert_eq!(next[0]["step"], 2);

    // An answer ends them.
    post(
        &app,
        &owner,
        "/api/v1/pitches",
        json!({ "organisation_id": club_id, "kind": "outcome", "type_key": "not_now" }),
    )
    .await
    .ok();
    assert_eq!(due(&format!("?organisation_id={club_id}")).await, json!([]));

    // A date set by hand comes due too.
    let today = chrono::Utc::now().date_naive().to_string();
    patch(
        &app,
        &owner,
        &format!("/api/v1/organisations/{club_id}"),
        json!({ "follow_up_on": today }),
    )
    .await
    .ok();
    let d = due("?days_ahead=1").await;
    assert_eq!(d[0]["kind"], "date", "{d}");

    app.get("/api/v1/pitching/follow-ups?days_ahead=61")
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");

    // Follow-ups name one of the actions, and a template if any.
    let mut s = get(&app, &owner, "/api/v1/pitching/settings").await.body;
    s["follow_ups"][0]["action"] = json!("nope");
    s["follow_ups"][1]["template_key"] = json!("nope");
    s["follow_ups"][1]["after_days"] = json!(0);
    let res = app
        .put("/api/v1/pitching/settings")
        .signed_in(&owner)
        .json(s)
        .send()
        .await;
    let mut f = fields(&res);
    f.sort();
    assert_eq!(
        f,
        [
            "follow_ups[0].action",
            "follow_ups[1].after_days",
            "follow_ups[1].template_key"
        ]
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn follow_ups_forget_people_who_moved_on(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let bar = organisation(&app, &owner, json!({ "name": "Belgrave" })).await;
    let bar = bar["id"].as_str().unwrap().to_owned();
    let ann = post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Ann", "organisation_id": bar }),
    )
    .await
    .ok();
    let ann_id = ann.id("/id");
    let ago = (chrono::Utc::now() - chrono::Duration::days(8)).to_rfc3339();
    post(
        &app,
        &owner,
        "/api/v1/pitches",
        json!({ "organisation_id": bar, "contact_id": ann_id, "kind": "action", "type_key": "pitched",
                "happened_at": ago }),
    )
    .await
    .ok();
    let due = get(&app, &owner, "/api/v1/pitching/follow-ups").await.body;
    assert_eq!(due[0]["contact_id"], ann_id.to_string(), "{due}");
    assert_eq!(due[0]["due_in_days"], -1);

    // Ann moves on: the follow-up is no longer for her.
    app.patch(&format!("/api/v1/contacts/{ann_id}"))
        .signed_in(&owner)
        .header("If-Match", &format!("\"{}\"", ann.body["version"]))
        .json(json!({ "organisation_id": null }))
        .send()
        .await
        .ok();
    let due = get(&app, &owner, "/api/v1/pitching/follow-ups").await.body;
    assert_eq!(due[0]["contact_id"], Value::Null, "{due}");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_list_ranks_who_to_pitch_first_with_reasons(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let owls = post(
        &app,
        &owner,
        "/api/v1/talent",
        json!({ "name": "The Midnight Owls", "tags": ["Techno"] }),
    )
    .await
    .ok()
    .id("/id");
    let id = |o: &Value| o["id"].as_str().unwrap().to_owned();
    let leeds = json!([{ "city": "Leeds", "country": "GB" }]);
    // Asked about the Owls, in a city they play soon.
    let asks = organisation(
        &app,
        &owner,
        json!({ "name": "Brudenell", "addresses": leeds, "interested_in": [{ "kind": "talent", "id": owls }] }),
    )
    .await;
    post(
        &app,
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Nat", "organisation_id": id(&asks), "emails": ["nat@brudenell.test"] }),
    )
    .await
    .ok();
    let venue = organisation(&app, &owner, json!({ "name": "Belgrave", "addresses": leeds })).await;
    let soon = chrono::Utc::now() + chrono::Duration::days(20);
    post(
        &app,
        &owner,
        "/api/v1/bookings",
        json!({ "title": "Leeds", "talent_ids": [owls], "venue_id": id(&venue),
                "starts_at": soon.to_rfc3339(), "ends_at": (soon + chrono::Duration::hours(2)).to_rfc3339() }),
    )
    .await
    .ok();
    // Into techno, but nobody to reach there.
    let techno = organisation(
        &app,
        &owner,
        json!({ "name": "Wire", "tags": ["techno", "festival"], "addresses": [{ "city": "Berlin", "country": "DE" }] }),
    )
    .await;
    // Said "not now" today: resting.
    let resting = organisation(&app, &owner, json!({ "name": "Albert Hall" })).await;
    post(
        &app,
        &owner,
        "/api/v1/pitches",
        json!({ "organisation_id": id(&resting), "kind": "outcome", "type_key": "not_now",
                "item": { "kind": "talent", "id": owls } }),
    )
    .await
    .ok();
    // Never listed.
    organisation(
        &app,
        &owner,
        json!({ "name": "Quiet Bar", "do_not_contact": true }),
    )
    .await;

    let list = |q: String| {
        let app = &app;
        let owner = &owner;
        async move {
            get(
                app,
                owner,
                &format!("/api/v1/pitching/list?item_kind=talent&item_id={owls}{q}"),
            )
            .await
            .body
        }
    };
    let l = list(String::new()).await;
    let names: Vec<&str> = l["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Brudenell", "Wire", "Belgrave", "Albert Hall"], "{l}");
    assert_eq!(l["total"], 4);
    assert_eq!(l["item"]["name"], "The Midnight Owls");
    let top = &l["items"][0];
    assert_eq!(top["pitch_status"], "interested");
    assert_eq!(top["reasons"][0]["label"], "Asked about The Midnight Owls");
    assert!(
        top["reasons"][1]["label"]
            .as_str()
            .unwrap()
            .starts_with("The Midnight Owls is in Leeds on "),
        "{top}"
    );
    assert_eq!(top["but"], Value::Null);
    assert_eq!(top["region"], "europe");
    let wire = &l["items"][1];
    assert_eq!(wire["reasons"][0]["label"], "Into techno");
    assert_eq!(wire["but"]["reason"], "no_way_to_reach");
    assert_eq!(wire["score"], 15 - 30);
    let last = &l["items"][3];
    assert_eq!(last["resting"], true);
    assert_eq!(last["pitch_status"], "tried");
    assert!(
        last["rest_reason"]
            .as_str()
            .unwrap()
            .starts_with("Said “Not now” on"),
        "{last}"
    );

    // A to Z; without the resting; by what stands; by region; by words.
    let az = list("&sort=name".into()).await;
    assert_eq!(az["items"][0]["name"], "Albert Hall");
    assert_eq!(list("&hide_resting=true".into()).await["total"], 3);
    let tried = list("&pitch_status=tried".into()).await;
    assert_eq!(tried["items"][0]["name"], "Albert Hall");
    assert_eq!(tried["total"], 1);
    assert_eq!(list("&region=europe".into()).await["total"], 3);
    assert_eq!(list("&q=wire".into()).await["items"][0]["name"], "Wire");
    let page = list("&limit=1&offset=1".into()).await;
    assert_eq!(page["items"][0]["name"], "Wire");
    assert_eq!(page["total"], 4);

    // The workspace's own weights and reasons.
    let mut s = get(&app, &owner, "/api/v1/pitching/settings").await.body;
    for w in s["ranking"].as_array_mut().unwrap() {
        if w["reason"] == "no_way_to_reach" {
            w["points"] = json!(0);
        }
    }
    s["own_reasons"] = json!([{ "tag": "Festival", "points": 50 }]);
    app.put("/api/v1/pitching/settings")
        .signed_in(&owner)
        .header("If-Match", &format!("\"{}\"", s["version"]))
        .json(s.clone())
        .send()
        .await
        .ok();
    let l = list(String::new()).await;
    assert_eq!(l["items"][0]["name"], "Wire", "{l}");
    assert_eq!(l["items"][0]["score"], 65);
    assert_eq!(l["items"][0]["reasons"][0]["label"], "Tagged Festival");
    assert_eq!(l["items"][0]["but"], Value::Null);

    // Weights and rests are checked.
    s["own_reasons"] = json!([{ "tag": "x", "points": 101 }]);
    s["outcomes"][0]["rest_days"] = json!(-1);
    let bad = app
        .put("/api/v1/pitching/settings")
        .signed_in(&owner)
        .json(s.clone())
        .send()
        .await;
    bad.expect(StatusCode::BAD_REQUEST, "invalid_input");
    assert_eq!(fields(&bad), ["outcomes[0].rest_days", "own_reasons[0].points"]);

    // What is pitched comes as a pair, and must exist.
    app.get("/api/v1/pitching/list?item_kind=talent")
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");
    app.get(&format!(
        "/api/v1/pitching/list?item_kind=talent&item_id={}",
        Uuid::now_v7()
    ))
    .signed_in(&owner)
    .send()
    .await
    .expect(StatusCode::NOT_FOUND, "not_found");
    // With nothing picked, the whole list.
    assert_eq!(
        get(&app, &owner, "/api/v1/pitching/list").await.body["total"],
        4
    );
}
