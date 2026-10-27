//! M4 through HTTP: enquiries in their pipeline, holds and bookings with the
//! double-booking guard, availability, quotes up to the buyer's answer,
//! tasks, the calendar and its iCal links. The worker's side (hold expiry,
//! completion, quote expiry, the quote email) is tested in `core-worker`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::{StatusCode, header};
use common::{Client, TestApp};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn post(app: &TestApp, who: &Client, path: &str, body: Value) -> common::Resp {
    app.post(path).signed_in(who).json(body).send().await
}

async fn get(app: &TestApp, who: &Client, path: &str) -> common::Resp {
    app.get(path).signed_in(who).send().await.ok()
}

/// A talent, and a booker with an email at a venue.
struct Cast {
    talent: Uuid,
    other_talent: Uuid,
    organisation: Uuid,
    contact: Uuid,
}

async fn cast(app: &TestApp, who: &Client) -> Cast {
    let id = |res: common::Resp| res.ok().id("/id");
    let talent = id(post(
        app,
        who,
        "/api/v1/talent",
        json!({ "name": "The Midnight Owls", "default_fee": { "minor": 200_000, "currency": "GBP" } }),
    )
    .await);
    let other_talent = id(post(app, who, "/api/v1/talent", json!({ "name": "DJ Lark" })).await);
    let organisation = id(post(
        app,
        who,
        "/api/v1/organisations",
        json!({ "name": "The Grand Hall" }),
    )
    .await);
    let contact = id(post(
        app,
        who,
        "/api/v1/contacts",
        json!({ "name": "Bea Booker", "organisation_id": organisation, "emails": ["bea@grand.test"] }),
    )
    .await);
    Cast {
        talent,
        other_talent,
        organisation,
        contact,
    }
}

async fn booking(
    app: &TestApp,
    who: &Client,
    talent: Uuid,
    status: &str,
    from: &str,
    to: &str,
) -> common::Resp {
    post(
        app,
        who,
        "/api/v1/bookings",
        json!({ "title": format!("{status} {from}"), "talent_ids": [talent], "status": status,
                "starts_at": from, "ends_at": to }),
    )
    .await
}

async fn set_status(app: &TestApp, who: &Client, id: Uuid, body: Value) -> common::Resp {
    post(app, who, &format!("/api/v1/bookings/{id}/status"), body).await
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn enquiries_start_in_a_default_pipeline_and_move_through_it(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let c = cast(&app, &owner).await;

    let created = post(
        &app,
        &owner,
        "/api/v1/enquiries",
        json!({ "title": "  Summer ball ", "talent_ids": [c.talent, c.talent], "contact_id": c.contact,
                "organisation_id": c.organisation, "starts_on": "2031-06-01",
                "budget": { "minor": 300_000, "currency": "gbp" }, "tags": ["Wedding"] }),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let e = &created.body;
    assert_eq!(e["title"], "Summer ball");
    assert_eq!(e["source"], "manual");
    assert_eq!(e["stage"], "new");
    assert_eq!(e["stage_name"], "New");
    assert_eq!(e["talent"][0]["name"], "The Midnight Owls");
    assert_eq!(e["talent"].as_array().unwrap().len(), 1);
    assert_eq!(e["contact_name"], "Bea Booker");
    assert_eq!(e["organisation_name"], "The Grand Hall");
    assert_eq!(e["budget"], json!({ "minor": 300_000, "currency": "GBP" }));
    let id = created.id("/id");

    // The pipeline was made on first use and is an ordinary pipeline.
    let pipelines = get(&app, &owner, "/api/v1/pipelines?entity=enquiry").await;
    let stages: Vec<&str> = pipelines.body[0]["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["key"].as_str().unwrap())
        .collect();
    assert_eq!(stages, ["new", "checking", "quoted", "won", "lost"]);

    // Problems are named; links must exist in this workspace.
    let bad = post(
        &app,
        &owner,
        "/api/v1/enquiries",
        json!({ "title": "", "talent_ids": [Uuid::now_v7()], "stage": "nowhere",
                "starts_on": "2031-06-02", "ends_on": "2031-06-01" }),
    )
    .await;
    bad.expect(StatusCode::BAD_REQUEST, "invalid_input");
    let fields: Vec<&str> = bad.body["error"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["field"].as_str().unwrap())
        .collect();
    for f in ["title", "talent_ids[0]", "ends_on"] {
        assert!(fields.contains(&f), "{f} in {fields:?}");
    }

    // Moving stage needs the current version.
    let path = format!("/api/v1/enquiries/{id}");
    let moved = app
        .patch(&path)
        .signed_in(&owner)
        .header("if-match", "\"1\"")
        .json(json!({ "stage": "checking", "notes": "Asked the band" }))
        .send()
        .await
        .ok();
    assert_eq!(moved.body["stage"], "checking");
    assert_eq!(moved.body["version"], 2);
    app.patch(&path)
        .signed_in(&owner)
        .header("if-match", "\"1\"")
        .json(json!({ "stage": "lost" }))
        .send()
        .await
        .expect(StatusCode::PRECONDITION_FAILED, "version_conflict");

    // Lists filter by talent and stage, and search by title.
    post(
        &app,
        &owner,
        "/api/v1/enquiries",
        json!({ "title": "Office party" }),
    )
    .await
    .ok();
    let by_talent = get(
        &app,
        &owner,
        &format!("/api/v1/enquiries?talent_id={}", c.talent),
    )
    .await;
    assert_eq!(by_talent.body["total"], 1);
    assert_eq!(by_talent.body["items"][0]["title"], "Summer ball");
    let found = get(&app, &owner, "/api/v1/enquiries?q=office").await;
    assert_eq!(found.body["items"][0]["title"], "Office party");
    let all = get(&app, &owner, "/api/v1/enquiries").await;
    assert_eq!(all.body["total"], 2);

    // The timeline shows the stage change.
    let timeline = get(&app, &owner, &format!("/api/v1/enquiries/{id}/timeline")).await;
    assert!(
        timeline.body.to_string().contains("enquiry.stage_changed"),
        "{}",
        timeline.body
    );

    // Deleting hides it.
    app.delete(&path)
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::NO_CONTENT, "");
    app.get(&path)
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn confirmed_bookings_cannot_double_book_talent(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let agent = app.add_member(&owner, "agent@acme.test", "agent").await;
    let c = cast(&app, &owner).await;

    let first = booking(
        &app,
        &agent,
        c.talent,
        "confirmed",
        "2031-06-01T19:00:00Z",
        "2031-06-01T23:00:00Z",
    )
    .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let first = first.id("/id");

    // Holds may overlap anything: they are pencil marks.
    let hold = booking(
        &app,
        &agent,
        c.talent,
        "hold",
        "2031-06-01T21:00:00Z",
        "2031-06-02T01:00:00Z",
    )
    .await;
    assert_eq!(hold.status, StatusCode::CREATED, "{}", hold.body);
    assert_eq!(hold.body["status"], "hold");
    assert!(hold.body["hold_expires_at"].is_string());
    let hold = hold.id("/id");

    // The check says what is in the way.
    let report = post(
        &app,
        &agent,
        "/api/v1/bookings/clash-check",
        json!({ "talent_ids": [c.talent], "starts_at": "2031-06-01T21:00:00Z",
                "ends_at": "2031-06-02T01:00:00Z", "except_id": hold }),
    )
    .await
    .ok();
    assert_eq!(report.body["blocking"], true);
    assert_eq!(report.body["clashes"][0]["booking_id"], first.to_string());
    assert_eq!(
        report.body["clashes"][0]["talent_name"],
        "The Midnight Owls"
    );

    // Confirming the hold is refused, and so is a confirmed booking straight in.
    set_status(&app, &agent, hold, json!({ "status": "confirmed" }))
        .await
        .expect(StatusCode::CONFLICT, "booking_clash");
    booking(
        &app,
        &agent,
        c.talent,
        "confirmed",
        "2031-06-01T22:00:00Z",
        "2031-06-01T23:30:00Z",
    )
    .await
    .expect(StatusCode::CONFLICT, "booking_clash");

    // Back to back is fine; another talent at the same time is fine.
    booking(
        &app,
        &agent,
        c.talent,
        "confirmed",
        "2031-06-01T23:00:00Z",
        "2031-06-02T00:00:00Z",
    )
    .await
    .ok();
    booking(
        &app,
        &agent,
        c.other_talent,
        "confirmed",
        "2031-06-01T19:00:00Z",
        "2031-06-01T23:00:00Z",
    )
    .await
    .ok();

    // Only someone allowed to override may, and only with a reason.
    set_status(
        &app,
        &agent,
        hold,
        json!({ "status": "confirmed", "override_reason": "Two sets" }),
    )
    .await
    .expect(StatusCode::FORBIDDEN, "permission_denied");
    // A blank reason is no reason.
    set_status(
        &app,
        &owner,
        hold,
        json!({ "status": "confirmed", "override_reason": " " }),
    )
    .await
    .expect(StatusCode::CONFLICT, "booking_clash");
    let overridden = set_status(
        &app,
        &owner,
        hold,
        json!({ "status": "confirmed", "override_reason": "Two sets, agreed with the band" }),
    )
    .await
    .ok();
    assert_eq!(overridden.body["status"], "confirmed");
    assert_eq!(overridden.body["clash_override"], true);
    assert_eq!(
        overridden.body["override_reason"],
        "Two sets, agreed with the band"
    );
    assert!(overridden.body["hold_expires_at"].is_null());

    // The override is in the audit log.
    let ws = owner.workspace.unwrap();
    assert!(
        app.audit_actions(ws)
            .await
            .contains(&"booking.clash_overridden".to_owned())
    );

    // Marked-unavailable time blocks too; tentative time only warns.
    post(
        &app,
        &owner,
        &format!("/api/v1/talent/{}/availability", c.other_talent),
        json!({ "kind": "unavailable", "starts_at": "2031-07-01T00:00:00Z", "ends_at": "2031-07-08T00:00:00Z",
                "note": "Holiday" }),
    )
    .await
    .ok();
    post(
        &app,
        &owner,
        &format!("/api/v1/talent/{}/availability", c.other_talent),
        json!({ "kind": "tentative", "starts_at": "2031-08-01T00:00:00Z", "ends_at": "2031-08-02T00:00:00Z" }),
    )
    .await
    .ok();
    let blocked = booking(
        &app,
        &agent,
        c.other_talent,
        "confirmed",
        "2031-07-03T19:00:00Z",
        "2031-07-03T23:00:00Z",
    )
    .await;
    blocked.expect(StatusCode::CONFLICT, "booking_clash");
    assert!(
        blocked.body.to_string().contains("unavailable"),
        "{}",
        blocked.body
    );
    let warned = post(
        &app,
        &agent,
        "/api/v1/bookings/clash-check",
        json!({ "talent_ids": [c.other_talent], "starts_at": "2031-08-01T19:00:00Z",
                "ends_at": "2031-08-01T23:00:00Z" }),
    )
    .await
    .ok();
    assert_eq!(warned.body["blocking"], false);
    assert_eq!(warned.body["clashes"][0]["kind"], "availability");
    assert_eq!(warned.body["clashes"][0]["status"], "tentative");
    booking(
        &app,
        &agent,
        c.other_talent,
        "confirmed",
        "2031-08-01T19:00:00Z",
        "2031-08-01T23:00:00Z",
    )
    .await
    .ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn two_confirmations_racing_for_one_slot_let_only_one_through(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let c = cast(&app, &owner).await;
    let mut holds = Vec::new();
    for _ in 0..2 {
        holds.push(
            booking(
                &app,
                &owner,
                c.talent,
                "hold",
                "2031-06-01T19:00:00Z",
                "2031-06-01T23:00:00Z",
            )
            .await
            .ok()
            .id("/id"),
        );
    }
    let (a, b) = tokio::join!(
        set_status(&app, &owner, holds[0], json!({ "status": "confirmed" })),
        set_status(&app, &owner, holds[1], json!({ "status": "confirmed" })),
    );
    let mut outcomes = [
        (a.status, a.code().to_owned()),
        (b.status, b.code().to_owned()),
    ];
    outcomes.sort();
    assert_eq!(outcomes[0].0, StatusCode::OK, "{outcomes:?}");
    assert_eq!(
        outcomes[1],
        (StatusCode::CONFLICT, "booking_clash".to_owned())
    );

    // The database itself refuses a second firm booking, whatever the API does.
    let ws = owner.workspace.unwrap().0;
    let refused =
        sqlx::query("UPDATE booking_talent SET firm = true WHERE workspace_id = $1 AND NOT firm")
            .bind(ws)
            .execute(&app.owner)
            .await;
    assert!(
        refused.is_err(),
        "the exclusion constraint let a double booking in"
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn bookings_follow_their_status_arrows_and_reschedule_with_a_check(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let c = cast(&app, &owner).await;

    let enquiry = post(
        &app,
        &owner,
        "/api/v1/enquiries",
        json!({ "title": "Gala", "talent_ids": [c.talent], "contact_id": c.contact,
                "organisation_id": c.organisation, "venue_text": "Main hall" }),
    )
    .await
    .ok()
    .id("/id");
    // A booking from an enquiry takes its people and place.
    let b = post(
        &app,
        &owner,
        "/api/v1/bookings",
        json!({ "title": "Gala night", "enquiry_id": enquiry,
                "starts_at": "2031-06-01T19:00:00Z", "ends_at": "2031-06-01T23:00:00Z",
                "fee": { "minor": 250_000, "currency": "GBP" }, "deposit_percent": 25,
                "schedule": [{ "at": "2031-06-01T18:00:00Z", "label": "Load in" }] }),
    )
    .await;
    assert_eq!(b.status, StatusCode::CREATED, "{}", b.body);
    assert_eq!(b.body["talent"][0]["id"], c.talent.to_string());
    assert_eq!(b.body["contact_name"], "Bea Booker");
    assert_eq!(b.body["venue_text"], "Main hall");
    assert_eq!(b.body["status"], "hold");
    assert_eq!(b.body["schedule"][0]["label"], "Load in");
    let id = b.id("/id");

    // Ends must follow starts.
    post(
        &app,
        &owner,
        "/api/v1/bookings",
        json!({ "title": "Backwards", "starts_at": "2031-06-01T19:00:00Z", "ends_at": "2031-06-01T18:00:00Z" }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");

    // Arrows: hold → confirmed → completed; not back to hold.
    set_status(&app, &owner, id, json!({ "status": "confirmed" }))
        .await
        .ok();
    set_status(&app, &owner, id, json!({ "status": "hold" }))
        .await
        .expect(StatusCode::CONFLICT, "status_change_refused");

    // Moving a confirmed booking onto another confirmed one is refused.
    let other = booking(
        &app,
        &owner,
        c.talent,
        "confirmed",
        "2031-06-02T19:00:00Z",
        "2031-06-02T23:00:00Z",
    )
    .await
    .ok()
    .id("/id");
    let path = format!("/api/v1/bookings/{id}");
    app.patch(&path)
        .signed_in(&owner)
        .json(json!({ "starts_at": "2031-06-02T20:00:00Z", "ends_at": "2031-06-02T22:00:00Z" }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "booking_clash");
    let moved = app
        .patch(&path)
        .signed_in(&owner)
        .header("if-match", "\"2\"")
        .json(json!({ "starts_at": "2031-06-03T19:00:00Z", "ends_at": "2031-06-03T23:00:00Z" }))
        .send()
        .await
        .ok();
    assert_eq!(moved.body["starts_at"], "2031-06-03T19:00:00Z");

    // Cancelling keeps the reason; cancelled is final.
    let cancelled = set_status(
        &app,
        &owner,
        other,
        json!({ "status": "cancelled", "reason": "Venue flooded" }),
    )
    .await
    .ok();
    assert_eq!(cancelled.body["cancel_reason"], "Venue flooded");
    set_status(&app, &owner, other, json!({ "status": "confirmed" }))
        .await
        .expect(StatusCode::CONFLICT, "status_change_refused");

    // Lists filter by status and window, soonest first.
    let list = get(&app, &owner, "/api/v1/bookings?status=confirmed").await;
    assert_eq!(list.body["total"], 1);
    assert_eq!(list.body["items"][0]["id"], id.to_string());
    let window = get(
        &app,
        &owner,
        "/api/v1/bookings?from=2031-06-02T00:00:00Z&to=2031-06-03T00:00:00Z",
    )
    .await;
    assert_eq!(window.body["total"], 1);
    assert_eq!(window.body["items"][0]["id"], other.to_string());

    // The enquiry's timeline includes its booking; the booking's its own changes.
    let timeline = get(&app, &owner, &format!("/api/v1/bookings/{id}/timeline")).await;
    let text = timeline.body.to_string();
    assert!(
        text.contains("booking.confirmed") && text.contains("booking.rescheduled"),
        "{text}"
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_quote_is_sent_viewed_and_accepted_by_the_buyer(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let c = cast(&app, &owner).await;
    let enquiry = post(
        &app,
        &owner,
        "/api/v1/enquiries",
        json!({ "title": "Gala", "talent_ids": [c.talent], "contact_id": c.contact }),
    )
    .await
    .ok()
    .id("/id");
    let booking = post(
        &app,
        &owner,
        "/api/v1/bookings",
        json!({ "title": "Gala night", "enquiry_id": enquiry,
                "starts_at": "2031-06-01T19:00:00Z", "ends_at": "2031-06-01T23:00:00Z" }),
    )
    .await
    .ok()
    .id("/id");

    // With no lines given, the talent's default fee is used.
    let draft = post(
        &app,
        &owner,
        &format!("/api/v1/bookings/{booking}/quotes"),
        json!({}),
    )
    .await;
    assert_eq!(draft.status, StatusCode::CREATED, "{}", draft.body);
    assert_eq!(draft.body["status"], "draft");
    assert_eq!(draft.body["reference"], "Q-0001");
    assert_eq!(
        draft.body["total"],
        json!({ "minor": 200_000, "currency": "GBP" })
    );
    let quote = draft.id("/id");

    // Drafts can be edited; the total follows the lines.
    let edited = app
        .patch(&format!("/api/v1/quotes/{quote}"))
        .signed_in(&owner)
        .header("if-match", "\"1\"")
        .json(json!({ "line_items": [
            { "description": "Evening performance", "unit_minor": 200_000 },
            { "description": "Travel", "quantity": 2, "unit_minor": 15_000 }
        ], "message": "Looking forward to it", "valid_until": "2031-05-01" }))
        .send()
        .await
        .ok();
    assert_eq!(edited.body["total"]["minor"], 230_000);
    assert_eq!(edited.body["line_items"][1]["amount_minor"], 30_000);

    // Sending goes to the booking's contact unless told otherwise.
    let sent = post(
        &app,
        &owner,
        &format!("/api/v1/quotes/{quote}/send"),
        json!({}),
    )
    .await;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
    assert_eq!(sent.body["quote"]["status"], "sent");
    assert_eq!(sent.body["quote"]["sent_to"], "bea@grand.test");
    let link = sent.body["link"].as_str().unwrap().to_owned();
    let token = link.rsplit('/').next().unwrap().to_owned();
    assert!(link.contains("/quote/"), "{link}");

    // Sending moved the enquiry to Quoted; a sent quote is no longer editable.
    let e = get(&app, &owner, &format!("/api/v1/enquiries/{enquiry}")).await;
    assert_eq!(e.body["stage"], "quoted");
    app.patch(&format!("/api/v1/quotes/{quote}"))
        .signed_in(&owner)
        .json(json!({ "message": "Changed my mind" }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "quote_not_draft");

    // The buyer opens the link without signing in.
    let public = app
        .get(&format!("/api/v1/public/quotes/{token}"))
        .send()
        .await
        .ok();
    assert_eq!(public.body["workspace_name"], "acme agency");
    assert_eq!(public.body["reference"], "Q-0001");
    assert_eq!(public.body["total"]["minor"], 230_000);
    assert_eq!(public.body["booking_title"], "Gala night");
    let text = public.body.to_string();
    assert!(
        !text.contains(&booking.to_string()) && !text.contains("bea@grand.test"),
        "{text}"
    );
    let viewed = get(&app, &owner, &format!("/api/v1/quotes/{quote}")).await;
    assert_eq!(viewed.body["status"], "viewed");

    // A wrong token finds nothing.
    app.get("/api/v1/public/quotes/not-a-real-token")
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");

    // Accepting needs a name, then moves the hold and the enquiry along.
    app.post(&format!("/api/v1/public/quotes/{token}/accept"))
        .json(json!({ "name": " " }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");
    let accepted = app
        .post(&format!("/api/v1/public/quotes/{token}/accept"))
        .json(json!({ "name": "Bea Booker", "email": "bea@grand.test" }))
        .send()
        .await
        .ok();
    assert_eq!(accepted.body["status"], "accepted");
    let q = get(&app, &owner, &format!("/api/v1/quotes/{quote}")).await;
    assert_eq!(q.body["status"], "accepted");
    assert_eq!(q.body["decided_name"], "Bea Booker");
    assert_eq!(q.body["snapshot"]["quote"]["total"]["minor"], 230_000);
    assert_eq!(q.body["snapshot"]["accepted_by"]["name"], "Bea Booker");
    let b = get(&app, &owner, &format!("/api/v1/bookings/{booking}")).await;
    assert_eq!(b.body["status"], "pending");
    let e = get(&app, &owner, &format!("/api/v1/enquiries/{enquiry}")).await;
    assert_eq!(e.body["stage"], "won");

    // Accepted is final: no second answer, no edits, even in the database.
    app.post(&format!("/api/v1/public/quotes/{token}/decline"))
        .json(json!({ "name": "Someone else" }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "quote_closed");
    app.post(&format!("/api/v1/quotes/{quote}/withdraw"))
        .signed_in(&owner)
        .json(json!({}))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "quote_closed");
    let frozen = sqlx::query("UPDATE quotes SET total_minor = 1 WHERE id = $1")
        .bind(quote)
        .execute(&app.owner)
        .await;
    assert!(frozen.is_err(), "an accepted quote was changed");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn quotes_are_revised_withdrawn_and_declined(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let c = cast(&app, &owner).await;
    let booking = booking(
        &app,
        &owner,
        c.talent,
        "hold",
        "2031-06-01T19:00:00Z",
        "2031-06-01T23:00:00Z",
    )
    .await
    .ok()
    .id("/id");
    let lines = json!({ "line_items": [{ "description": "Set", "unit_minor": 100_000 }] });

    let first = post(
        &app,
        &owner,
        &format!("/api/v1/bookings/{booking}/quotes"),
        lines.clone(),
    )
    .await
    .ok()
    .id("/id");
    // No contact on the booking: an address must be given, and be one.
    post(
        &app,
        &owner,
        &format!("/api/v1/quotes/{first}/send"),
        json!({}),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
    post(
        &app,
        &owner,
        &format!("/api/v1/quotes/{first}/send"),
        json!({ "to": "nope" }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
    let sent = post(
        &app,
        &owner,
        &format!("/api/v1/quotes/{first}/send"),
        json!({ "to": "buyer@x.test" }),
    )
    .await
    .ok();
    let old_token = sent.body["link"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();

    // A revision is a new draft with the same number; sending it retires the first.
    let revision = post(
        &app,
        &owner,
        &format!("/api/v1/quotes/{first}/revise"),
        json!({}),
    )
    .await;
    assert_eq!(revision.status, StatusCode::CREATED, "{}", revision.body);
    assert_eq!(revision.body["reference"], "Q-0001-R2");
    assert_eq!(revision.body["supersedes_id"], first.to_string());
    let second = revision.id("/id");
    let sent = post(
        &app,
        &owner,
        &format!("/api/v1/quotes/{second}/send"),
        json!({ "to": "buyer@x.test" }),
    )
    .await
    .ok();
    let token = sent.body["link"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    let old = get(&app, &owner, &format!("/api/v1/quotes/{first}")).await;
    assert_eq!(old.body["status"], "withdrawn");
    app.post(&format!("/api/v1/public/quotes/{old_token}/accept"))
        .json(json!({ "name": "Buyer" }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "quote_closed");

    // The buyer declines the revision, with a reason.
    let declined = app
        .post(&format!("/api/v1/public/quotes/{token}/decline"))
        .json(json!({ "name": "Buyer", "reason": "Over budget" }))
        .send()
        .await
        .ok();
    assert_eq!(declined.body["status"], "declined");
    let q = get(&app, &owner, &format!("/api/v1/quotes/{second}")).await;
    assert_eq!(q.body["decline_reason"], "Over budget");
    let b = get(&app, &owner, &format!("/api/v1/bookings/{booking}")).await;
    assert_eq!(b.body["status"], "hold");

    // A fresh quote can be withdrawn after sending, and a draft deleted.
    let third = post(
        &app,
        &owner,
        &format!("/api/v1/bookings/{booking}/quotes"),
        lines.clone(),
    )
    .await
    .ok()
    .id("/id");
    assert_eq!(
        get(&app, &owner, &format!("/api/v1/quotes/{third}"))
            .await
            .body["reference"],
        "Q-0002"
    );
    post(
        &app,
        &owner,
        &format!("/api/v1/quotes/{third}/send"),
        json!({ "to": "buyer@x.test" }),
    )
    .await
    .ok();
    let withdrawn = post(
        &app,
        &owner,
        &format!("/api/v1/quotes/{third}/withdraw"),
        json!({}),
    )
    .await
    .ok();
    assert_eq!(withdrawn.body["status"], "withdrawn");
    let fourth = post(
        &app,
        &owner,
        &format!("/api/v1/bookings/{booking}/quotes"),
        lines,
    )
    .await
    .ok()
    .id("/id");
    app.delete(&format!("/api/v1/quotes/{fourth}"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::NO_CONTENT, "");

    // Cancelling the booking withdraws anything still open.
    let fifth = post(
        &app,
        &owner,
        &format!("/api/v1/bookings/{booking}/quotes"),
        json!({ "line_items": [{ "description": "Set", "unit_minor": 90_000 }] }),
    )
    .await
    .ok()
    .id("/id");
    post(
        &app,
        &owner,
        &format!("/api/v1/quotes/{fifth}/send"),
        json!({ "to": "buyer@x.test" }),
    )
    .await
    .ok();
    set_status(&app, &owner, booking, json!({ "status": "cancelled" }))
        .await
        .ok();
    let q = get(&app, &owner, &format!("/api/v1/quotes/{fifth}")).await;
    assert_eq!(q.body["status"], "withdrawn");
    let list = get(&app, &owner, &format!("/api/v1/bookings/{booking}/quotes")).await;
    assert_eq!(list.body["total"], 4);
    let open = get(&app, &owner, "/api/v1/quotes?status=sent,viewed").await;
    assert_eq!(open.body["total"], 0);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn tasks_are_assigned_completed_and_reopened(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let agent = app.add_member(&owner, "agent@acme.test", "agent").await;
    let agent_id = app.user_id("agent@acme.test").await.0;
    let c = cast(&app, &owner).await;

    let task = post(
        &app,
        &owner,
        "/api/v1/tasks",
        json!({ "title": "Send rider", "assignee_id": agent_id, "due_at": "2031-05-01T09:00:00Z",
                "related_entity": "talent", "related_id": c.talent }),
    )
    .await;
    assert_eq!(task.status, StatusCode::CREATED, "{}", task.body);
    assert_eq!(task.body["status"], "open");
    assert_eq!(task.body["origin"], "manual");
    assert_eq!(task.body["related_name"], "The Midnight Owls");
    let id = task.id("/id");

    // Links and assignees must be real.
    post(
        &app,
        &owner,
        "/api/v1/tasks",
        json!({ "title": "Ghost", "related_entity": "talent", "related_id": Uuid::now_v7() }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
    post(
        &app,
        &owner,
        "/api/v1/tasks",
        json!({ "title": "Ghost", "assignee_id": Uuid::now_v7() }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
    post(
        &app,
        &owner,
        "/api/v1/tasks",
        json!({ "title": "Unassigned" }),
    )
    .await
    .ok();

    // "Mine" is the agent's.
    let mine = get(&app, &agent, "/api/v1/tasks?assignee=me").await;
    assert_eq!(mine.body["total"], 1);
    assert_eq!(mine.body["items"][0]["title"], "Send rider");

    // Done, then reopened.
    let path = format!("/api/v1/tasks/{id}");
    let done = app
        .patch(&path)
        .signed_in(&agent)
        .json(json!({ "status": "done" }))
        .send()
        .await
        .ok();
    assert_eq!(done.body["status"], "done");
    assert_eq!(done.body["done_by"], agent_id.to_string());
    let open = get(&app, &owner, "/api/v1/tasks").await;
    assert_eq!(open.body["total"], 1);
    let all = get(&app, &owner, "/api/v1/tasks?status=all").await;
    assert_eq!(all.body["total"], 2);
    let reopened = app
        .patch(&path)
        .signed_in(&agent)
        .json(json!({ "status": "open" }))
        .send()
        .await
        .ok();
    assert!(reopened.body["done_at"].is_null());
    let ws = owner.workspace.unwrap();
    let actions = app.audit_actions(ws).await;
    assert!(
        actions.contains(&"task.completed".to_owned())
            && actions.contains(&"task.reopened".to_owned())
    );

    // Viewers can read but not write.
    let viewer = app.add_member(&owner, "viewer@acme.test", "viewer").await;
    get(&app, &viewer, "/api/v1/tasks").await;
    post(&app, &viewer, "/api/v1/tasks", json!({ "title": "Nope" }))
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_calendar_shows_bookings_and_availability_and_publishes_ical(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let c = cast(&app, &owner).await;
    post(
        &app,
        &owner,
        "/api/v1/bookings",
        json!({ "title": "Gala night", "talent_ids": [c.talent], "status": "confirmed",
                "starts_at": "2031-06-01T19:00:00Z", "ends_at": "2031-06-01T23:00:00Z" }),
    )
    .await
    .ok();
    booking(
        &app,
        &owner,
        c.other_talent,
        "hold",
        "2031-06-10T19:00:00Z",
        "2031-06-10T23:00:00Z",
    )
    .await
    .ok();
    let cancelled = booking(
        &app,
        &owner,
        c.talent,
        "hold",
        "2031-06-12T19:00:00Z",
        "2031-06-12T23:00:00Z",
    )
    .await
    .ok()
    .id("/id");
    set_status(&app, &owner, cancelled, json!({ "status": "cancelled" }))
        .await
        .ok();
    let away = post(
        &app,
        &owner,
        &format!("/api/v1/talent/{}/availability", c.talent),
        json!({ "starts_at": "2031-06-20T00:00:00Z", "ends_at": "2031-06-22T00:00:00Z", "all_day": true,
                "note": "Holiday" }),
    )
    .await;
    assert_eq!(away.status, StatusCode::CREATED, "{}", away.body);
    assert_eq!(away.body["kind"], "unavailable");
    let away = away.id("/id");

    let june = "from=2031-06-01T00:00:00Z&to=2031-07-01T00:00:00Z";
    let cal = get(&app, &owner, &format!("/api/v1/calendar?{june}")).await;
    let titles: Vec<&str> = cal.body["bookings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["status"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["confirmed", "hold"], "cancelled ones are left out");
    assert_eq!(cal.body["availability"][0]["note"], "Holiday");
    let lane = get(
        &app,
        &owner,
        &format!("/api/v1/calendar?{june}&talent_id={}", c.other_talent),
    )
    .await;
    assert_eq!(lane.body["bookings"].as_array().unwrap().len(), 1);
    assert!(lane.body["availability"].as_array().unwrap().is_empty());
    app.get("/api/v1/calendar?from=2031-01-01T00:00:00Z&to=2033-01-01T00:00:00Z")
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");

    // Availability is edited and removed like anything else.
    let edited = app
        .patch(&format!("/api/v1/availability/{away}"))
        .signed_in(&owner)
        .json(json!({ "kind": "tentative" }))
        .send()
        .await
        .ok();
    assert_eq!(edited.body["kind"], "tentative");
    let listed = get(
        &app,
        &owner,
        &format!("/api/v1/talent/{}/availability?{june}", c.talent),
    )
    .await;
    assert_eq!(listed.body.as_array().unwrap().len(), 1);
    app.delete(&format!("/api/v1/availability/{away}"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::NO_CONTENT, "");

    // A private iCal link, shown once. It covers the coming two years.
    let soon = chrono::Utc::now() + chrono::Duration::days(30);
    let confirmed = post(
        &app,
        &owner,
        "/api/v1/bookings",
        json!({ "title": "Spring launch", "talent_ids": [c.talent], "status": "confirmed",
                "starts_at": soon, "ends_at": soon + chrono::Duration::hours(3),
                "fee": { "minor": 987_654, "currency": "GBP" }, "venue_text": "Main hall" }),
    )
    .await
    .ok()
    .id("/id");
    let feed = post(
        &app,
        &owner,
        "/api/v1/calendar/feeds",
        json!({ "name": "My phone" }),
    )
    .await;
    assert_eq!(feed.status, StatusCode::CREATED, "{}", feed.body);
    let url = feed.body["url"].as_str().unwrap().to_owned();
    let feed_id = feed.id("/feed/id");
    let path = &url[url.find("/api/v1/public/").unwrap()..];
    let ics = app.get(path).send().await.ok();
    assert!(
        ics.headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/calendar")
    );
    let text = ics.body.as_str().unwrap_or_default().to_owned();
    assert!(text.starts_with("BEGIN:VCALENDAR"), "{text}");
    assert!(
        text.contains("Spring launch") && text.contains(&confirmed.to_string()),
        "{text}"
    );
    assert!(!text.contains("Gala night"), "beyond two years: {text}");
    assert!(
        !text.contains("9876") && !text.contains("cancelled"),
        "fees and cancelled bookings stay out: {text}"
    );
    let listed = get(&app, &owner, "/api/v1/calendar/feeds").await;
    assert!(listed.body[0]["last_used_at"].is_string());
    assert!(!listed.body.to_string().contains(&url));

    // Revoking stops the link.
    app.delete(&format!("/api/v1/calendar/feeds/{feed_id}"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::NO_CONTENT, "");
    app.get(path)
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_hold_period_is_a_workspace_setting(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let c = cast(&app, &owner).await;
    let ws = get(&app, &owner, "/api/v1/workspace").await;
    assert_eq!(ws.body["hold_days"], 7);

    let hold = booking(
        &app,
        &owner,
        c.talent,
        "hold",
        "2031-06-01T19:00:00Z",
        "2031-06-01T23:00:00Z",
    )
    .await
    .ok();
    let expires: chrono::DateTime<chrono::Utc> = hold.body["hold_expires_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let days = (expires - chrono::Utc::now()).num_hours();
    assert!((7 * 24 - 1..=7 * 24).contains(&days), "{days} hours");

    app.patch("/api/v1/workspace")
        .signed_in(&owner)
        .json(json!({ "hold_days": 400 }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");
    let ws = app
        .patch("/api/v1/workspace")
        .signed_in(&owner)
        .json(json!({ "hold_days": 0 }))
        .send()
        .await
        .ok();
    assert_eq!(ws.body["hold_days"], 0);
    let forever = booking(
        &app,
        &owner,
        c.talent,
        "hold",
        "2031-06-02T19:00:00Z",
        "2031-06-02T23:00:00Z",
    )
    .await
    .ok();
    assert!(forever.body["hold_expires_at"].is_null());

    // A hold can be given its own expiry.
    let own = post(
        &app,
        &owner,
        "/api/v1/bookings",
        json!({ "title": "Short hold", "talent_ids": [c.talent],
                "starts_at": "2031-06-03T19:00:00Z", "ends_at": "2031-06-03T23:00:00Z",
                "hold_expires_at": "2031-05-01T12:00:00Z" }),
    )
    .await
    .ok();
    assert_eq!(own.body["hold_expires_at"], "2031-05-01T12:00:00Z");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn moving_an_overridden_booking_clear_of_its_clash_ends_the_override(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let c = cast(&app, &owner).await;
    booking(
        &app,
        &owner,
        c.talent,
        "confirmed",
        "2031-06-01T19:00:00Z",
        "2031-06-01T23:00:00Z",
    )
    .await
    .ok();
    let second = booking(
        &app,
        &owner,
        c.talent,
        "hold",
        "2031-06-01T21:00:00Z",
        "2031-06-02T01:00:00Z",
    )
    .await
    .ok()
    .id("/id");
    let overridden = set_status(
        &app,
        &owner,
        second,
        json!({ "status": "confirmed", "override_reason": "Two sets" }),
    )
    .await
    .ok();
    assert_eq!(overridden.body["clash_override"], true);

    // Moved to a free night, it no longer clashes, so it is firm again.
    let moved = app
        .patch(&format!("/api/v1/bookings/{second}"))
        .signed_in(&owner)
        .json(json!({ "starts_at": "2031-06-08T19:00:00Z", "ends_at": "2031-06-08T23:00:00Z" }))
        .send()
        .await
        .ok();
    assert_eq!(moved.body["clash_override"], false);
    assert!(moved.body["override_reason"].is_null());
    // So the double-booking constraint guards it again.
    let third = booking(
        &app,
        &owner,
        c.talent,
        "hold",
        "2031-06-08T20:00:00Z",
        "2031-06-08T22:00:00Z",
    )
    .await
    .ok()
    .id("/id");
    let forced = sqlx::query("UPDATE bookings SET status = 'confirmed' WHERE id = $1")
        .bind(third)
        .execute(&app.owner)
        .await;
    assert!(
        forced.is_err(),
        "a second firm booking slipped past the constraint"
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_booking_never_has_two_accepted_quotes(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let c = cast(&app, &owner).await;
    let booking = booking(
        &app,
        &owner,
        c.talent,
        "hold",
        "2031-06-01T19:00:00Z",
        "2031-06-01T23:00:00Z",
    )
    .await
    .ok()
    .id("/id");
    let quotes = format!("/api/v1/bookings/{booking}/quotes");
    let draft = || async { post(&app, &owner, &quotes, json!({})).await.ok().id("/id") };
    let send = |quote: Uuid| {
        let path = format!("/api/v1/quotes/{quote}/send");
        let (app, owner) = (&app, &owner);
        async move { post(app, owner, &path, json!({ "to": "bea@grand.test" })).await }
    };
    let token = |res: common::Resp| {
        res.ok().body["link"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned()
    };
    let accept = |token: String| {
        let path = format!("/api/v1/public/quotes/{token}/accept");
        let app = &app;
        async move {
            app.post(&path)
                .json(json!({ "name": "Bea Booker" }))
                .send()
                .await
        }
    };
    let revise = |quote: Uuid| {
        let path = format!("/api/v1/quotes/{quote}/revise");
        let (app, owner) = (&app, &owner);
        async move { post(app, owner, &path, json!({})).await.ok().id("/id") }
    };

    // Two quotes out with the buyer at once, as when two sends race: Q-0002
    // is sent, Q-0001 withdraws it, and the race puts it back.
    let first = draft().await;
    let second = draft().await;
    let second_token = token(send(second).await);
    let first_token = token(send(first).await);
    sqlx::query("UPDATE quotes SET status = 'sent' WHERE id = $1")
        .bind(second)
        .execute(&app.owner)
        .await
        .unwrap();
    accept(first_token).await.ok();
    accept(second_token)
        .await
        .expect(StatusCode::CONFLICT, "quote_already_accepted");

    // Once one is accepted, another quote for the booking cannot be sent.
    let third = draft().await;
    send(third)
        .await
        .expect(StatusCode::CONFLICT, "quote_already_accepted");

    // A revision of the accepted quote replaces it, and can be revised again.
    let revision = revise(first).await;
    accept(token(send(revision).await)).await.ok();
    let again = revise(revision).await;
    send(again).await.ok();
    send(third)
        .await
        .expect(StatusCode::CONFLICT, "quote_already_accepted");

    // The buyer asks for a change instead, so the sent revision is revised
    // unaccepted: the new one still goes out, can be accepted, and revised.
    let changed = revise(again).await;
    accept(token(send(changed).await)).await.ok();
    let later = revise(changed).await;
    send(later).await.ok();
    send(third)
        .await
        .expect(StatusCode::CONFLICT, "quote_already_accepted");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn links_stop_working_when_the_workspace_is_suspended_or_closed(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let c = cast(&app, &owner).await;
    let booking = booking(
        &app,
        &owner,
        c.talent,
        "hold",
        "2031-06-01T19:00:00Z",
        "2031-06-01T23:00:00Z",
    )
    .await
    .ok()
    .id("/id");
    let quote = post(
        &app,
        &owner,
        &format!("/api/v1/bookings/{booking}/quotes"),
        json!({}),
    )
    .await
    .ok()
    .id("/id");
    let sent = post(
        &app,
        &owner,
        &format!("/api/v1/quotes/{quote}/send"),
        json!({ "to": "bea@grand.test" }),
    )
    .await
    .ok();
    let quote_path = format!(
        "/api/v1/public/quotes/{}",
        sent.body["link"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
    );
    let feed = post(
        &app,
        &owner,
        "/api/v1/calendar/feeds",
        json!({ "name": "My phone" }),
    )
    .await
    .ok();
    let url = feed.body["url"].as_str().unwrap();
    let feed_path = url[url.find("/api/v1/public/").unwrap()..].to_owned();
    let set_workspace_status = |status: &'static str| {
        let pool = &app.owner;
        async move {
            sqlx::query("UPDATE workspaces SET status = $2::workspace_status WHERE id = $1")
                .bind(ws.0)
                .bind(status)
                .execute(pool)
                .await
                .unwrap();
        }
    };
    let all_gone = || async {
        app.get(&feed_path)
            .send()
            .await
            .expect(StatusCode::NOT_FOUND, "not_found");
        app.get(&quote_path)
            .send()
            .await
            .expect(StatusCode::NOT_FOUND, "not_found");
        for answer in ["accept", "decline"] {
            app.post(&format!("{quote_path}/{answer}"))
                .json(json!({ "name": "Bea Booker" }))
                .send()
                .await
                .expect(StatusCode::NOT_FOUND, "not_found");
        }
    };

    set_workspace_status("suspended").await;
    all_gone().await;
    set_workspace_status("active").await;
    app.get(&feed_path).send().await.ok();
    app.get(&quote_path).send().await.ok();

    app.post("/api/v1/workspace/close")
        .signed_in(&owner)
        .json(json!({ "confirm_slug": "acme" }))
        .send()
        .await
        .ok();
    all_gone().await;
    let status: String = sqlx::query_scalar("SELECT status::text FROM quotes WHERE id = $1")
        .bind(quote)
        .fetch_one(&app.owner)
        .await
        .unwrap();
    assert_eq!(
        status, "viewed",
        "nothing was decided in a closed workspace"
    );
}
