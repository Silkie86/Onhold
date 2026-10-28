//! The talent portal through HTTP: an agent invites a talent from their
//! record; the talent sees their own dates and nothing else, confirms a
//! date and the agent gets a task, keeps their availability, and loses
//! access when the agency removes it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{Client, TestApp};
use core_domain::WorkspaceId;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn post(app: &TestApp, who: &Client, path: &str, body: Value) -> common::Resp {
    app.post(path).signed_in(who).json(body).send().await
}

async fn get(app: &TestApp, who: &Client, path: &str) -> common::Resp {
    app.get(path).signed_in(who).send().await
}

async fn talent(app: &TestApp, who: &Client, name: &str) -> Uuid {
    post(app, who, "/api/v1/talent", json!({ "name": name }))
        .await
        .ok()
        .id("/id")
}

async fn booking(
    app: &TestApp,
    who: &Client,
    talent: Uuid,
    title: &str,
    status: &str,
    day: u32,
) -> Uuid {
    post(
        app,
        who,
        "/api/v1/bookings",
        json!({ "title": title, "talent_ids": [talent], "status": status,
                "timezone": "Europe/London", "starts_at": format!("2031-06-{day:02}T19:00:00Z"),
                "ends_at": format!("2031-06-{day:02}T22:00:00Z"), "venue_text": "Village Underground",
                "schedule": [{ "label": "Load-in", "at": format!("2031-06-{day:02}T15:00:00Z") }],
                "fee": { "minor": 150_000, "currency": "GBP" } }),
    )
    .await
    .ok()
    .id("/id")
}

/// Invites the talent record's person and signs them up through the email's link.
async fn invite_talent(
    app: &TestApp,
    agent: &Client,
    ws: WorkspaceId,
    talent: Uuid,
    email: &str,
) -> Client {
    let access = post(
        app,
        agent,
        &format!("/api/v1/talent/{talent}/portal"),
        json!({ "email": email }),
    )
    .await
    .ok();
    assert_eq!(access.body["invitation"]["email"], email);
    assert!(access.body["user"].is_null());
    let token = app.last_invitation_token(ws).await;
    app.accept_new(&token, "Ada Lovelace")
        .await
        .in_workspace(ws)
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_talent_sees_their_own_dates_and_answers_them(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let agent = app.add_member(&owner, "agent@acme.test", "agent").await;
    let ada = talent(&app, &agent, "Ada").await;
    let other = talent(&app, &agent, "Grace").await;
    let gig = booking(&app, &agent, ada, "Village Underground", "pending", 14).await;
    let hold = booking(&app, &agent, ada, "Pencilled festival", "hold", 20).await;
    let not_hers = booking(&app, &agent, other, "Grace's gig", "confirmed", 15).await;

    // Talent access only comes from a talent record.
    let res = post(
        &app,
        &owner,
        "/api/v1/invitations",
        json!({ "email": "x@y.test", "role": "talent" }),
    )
    .await;
    res.expect(StatusCode::BAD_REQUEST, "role_not_assignable");

    let me = invite_talent(&app, &agent, ws, ada, "ada@artist.test").await;
    let access = get(&app, &agent, &format!("/api/v1/talent/{ada}/portal"))
        .await
        .ok();
    assert_eq!(access.body["user"]["email"], "ada@artist.test");
    assert!(access.body["invitation"].is_null());

    let home = get(&app, &me, "/api/v1/portal").await.ok();
    assert_eq!(home.body["talent"], json!([{ "id": ada, "name": "Ada" }]));
    assert_eq!(home.body["workspace_name"], "acme agency");
    assert_eq!(home.body["awaiting_answer"], 2);
    assert_eq!(home.body["settings"]["show_fees"], false);

    // Their dates only, soonest first, without fees by default.
    let dates = get(&app, &me, "/api/v1/portal/dates").await.ok();
    let ids: Vec<&str> = dates
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [gig.to_string(), hold.to_string()]);
    assert!(dates.body[0]["fee"].is_null());
    assert_eq!(dates.body[0]["venue"], "Village Underground");
    assert_eq!(dates.body[0]["schedule"][0]["label"], "Load-in");
    assert_eq!(dates.body[0]["can_answer"], true);
    get(&app, &me, &format!("/api/v1/portal/dates/{not_hers}"))
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");

    // Nothing of the staff app opens for them.
    for path in [
        "/api/v1/bookings",
        "/api/v1/talent",
        "/api/v1/contacts",
        "/api/v1/members",
        "/api/v1/tasks",
    ] {
        let res = get(&app, &me, path).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{path}: {}", res.body);
    }

    // The agency shows fees, and hides holds.
    let res = app
        .put("/api/v1/workspace/portal")
        .signed_in(&owner)
        .json(json!({ "show_fees": true, "show_holds": false }))
        .send()
        .await
        .ok();
    assert_eq!(
        res.body["answer_dates"], true,
        "missing switches take their defaults"
    );
    let dates = get(&app, &me, "/api/v1/portal/dates").await.ok();
    assert_eq!(dates.body.as_array().unwrap().len(), 1);
    assert_eq!(
        dates.body[0]["fee"],
        json!({ "minor": 150_000, "currency": "GBP" })
    );
    get(&app, &me, &format!("/api/v1/portal/dates/{hold}"))
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
    // Only owners and admins change the switches.
    let res = app
        .put("/api/v1/workspace/portal")
        .signed_in(&agent)
        .json(json!({}))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);

    // She confirms; the agent who made the booking gets a task.
    let res = post(
        &app,
        &me,
        &format!("/api/v1/portal/dates/{gig}/answer"),
        json!({ "answer": "confirmed", "note": "Can't wait" }),
    )
    .await
    .ok();
    assert_eq!(res.body["entries"][0]["answer"], "confirmed");
    assert_eq!(res.body["entries"][0]["answer_note"], "Can't wait");
    let tasks = get(&app, &agent, &format!("/api/v1/tasks?related_id={gig}"))
        .await
        .ok();
    assert_eq!(tasks.body["total"], 1);
    assert_eq!(
        tasks.body["items"][0]["title"],
        "Ada confirmed Village Underground"
    );
    assert_eq!(
        tasks.body["items"][0]["assignee_id"],
        json!(app.user_id("agent@acme.test").await)
    );
    // The booking itself stays the agency's to move.
    let b = get(&app, &agent, &format!("/api/v1/bookings/{gig}"))
        .await
        .ok();
    assert_eq!(b.body["status"], "pending");
    assert!(
        app.audit_actions(ws)
            .await
            .contains(&"booking.talent_answered".to_owned())
    );

    let res = post(
        &app,
        &me,
        &format!("/api/v1/portal/dates/{gig}/answer"),
        json!({ "answer": "maybe" }),
    )
    .await;
    res.expect(StatusCode::BAD_REQUEST, "invalid_input");

    // Moving the date clears her answer: she has not said yes to the new time.
    app.patch(&format!("/api/v1/bookings/{gig}"))
        .signed_in(&agent)
        .json(json!({ "starts_at": "2031-06-14T20:00:00Z" }))
        .send()
        .await
        .ok();
    let d = get(&app, &me, &format!("/api/v1/portal/dates/{gig}"))
        .await
        .ok();
    assert!(d.body["entries"][0]["answer"].is_null());

    // A cancelled date can no longer be answered.
    let res = post(
        &app,
        &agent,
        &format!("/api/v1/bookings/{gig}/status"),
        json!({ "status": "cancelled", "reason": "Venue closed" }),
    )
    .await
    .ok();
    assert_eq!(res.body["status"], "cancelled");
    let res = post(
        &app,
        &me,
        &format!("/api/v1/portal/dates/{gig}/answer"),
        json!({ "answer": "declined" }),
    )
    .await;
    res.expect(StatusCode::CONFLICT, "not_answerable");

    // When the agency answers for them, the button goes.
    app.put("/api/v1/workspace/portal")
        .signed_in(&owner)
        .json(json!({ "answer_dates": false }))
        .send()
        .await
        .ok();
    let res = post(
        &app,
        &me,
        &format!("/api/v1/portal/dates/{hold}/answer"),
        json!({ "answer": "confirmed" }),
    )
    .await;
    res.expect(StatusCode::FORBIDDEN, "portal_switched_off");

    // Statements stay hidden until the agency shares them.
    let money = get(&app, &me, "/api/v1/portal/money").await.ok();
    assert_eq!(money.body, json!([]));

    // "Unlock features" sends them to the website OnHold's team set, unless
    // the agency hides locked features.
    let home = get(&app, &me, "/api/v1/portal").await.ok();
    assert!(home.body["upgrade_url"].is_null());
    sqlx::query(
        "INSERT INTO platform_settings (key, value) VALUES ('price_list', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(
        json!({ "currency": "GBP", "base": 0, "features": {}, "team": {},
                  "upgrade_url": "https://onhold.example/plans" }),
    )
    .execute(&app.owner)
    .await
    .unwrap();
    let home = get(&app, &me, "/api/v1/portal").await.ok();
    assert_eq!(home.body["upgrade_url"], "https://onhold.example/plans");
    app.put("/api/v1/workspace/portal")
        .signed_in(&owner)
        .json(json!({ "hide_locked": true }))
        .send()
        .await
        .ok();
    let home = get(&app, &me, "/api/v1/portal").await.ok();
    assert!(home.body["upgrade_url"].is_null());
    assert_eq!(home.body["settings"]["hide_locked"], true);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_talent_keeps_their_availability_and_access_can_end(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let ada = talent(&app, &owner, "Ada").await;
    let me = invite_talent(&app, &owner, ws, ada, "ada@artist.test").await;

    let mine = post(
        &app,
        &me,
        "/api/v1/portal/availability",
        json!({ "kind": "unavailable", "starts_at": "2031-07-01T00:00:00Z", "ends_at": "2031-07-08T00:00:00Z",
                "all_day": true, "note": "Holiday" }),
    )
    .await;
    assert_eq!(mine.status, StatusCode::CREATED, "{}", mine.body);
    assert_eq!(mine.body["source"], "portal");
    let mine = mine.id("/id");
    // The agency sees it, and adds one of its own.
    let staff = get(
        &app,
        &owner,
        &format!("/api/v1/talent/{ada}/availability?from=2031-06-01T00:00:00Z"),
    )
    .await
    .ok();
    assert_eq!(staff.body[0]["note"], "Holiday");
    let theirs = post(
        &app,
        &owner,
        &format!("/api/v1/talent/{ada}/availability"),
        json!({ "kind": "tentative", "starts_at": "2031-08-01T00:00:00Z", "ends_at": "2031-08-02T00:00:00Z" }),
    )
    .await
    .ok()
    .id("/id");

    // Shown within a month back to a year ahead from today; these are later,
    // so the list reads what it can and the removal rules are what matter.
    let res = app
        .delete(&format!("/api/v1/portal/availability/{theirs}"))
        .signed_in(&me)
        .send()
        .await;
    res.expect(StatusCode::CONFLICT, "agency_entry");
    let res = app
        .delete(&format!("/api/v1/portal/availability/{mine}"))
        .signed_in(&me)
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);

    // A link to connect her own calendar, like the one an agency sends.
    let link = post(&app, &me, "/api/v1/portal/calendar-link", json!({})).await;
    assert_eq!(link.status, StatusCode::CREATED, "{}", link.body);
    assert!(
        link.body["url"]
            .as_str()
            .unwrap()
            .contains("/connect-calendar/")
    );

    // Someone else cannot be invited while her portal is open.
    let res = post(
        &app,
        &owner,
        &format!("/api/v1/talent/{ada}/portal"),
        json!({ "email": "b@b.test" }),
    )
    .await;
    res.expect(StatusCode::CONFLICT, "portal_open");

    // Removing access ends her membership: she belonged here only as Ada.
    let res = app
        .delete(&format!("/api/v1/talent/{ada}/portal"))
        .signed_in(&owner)
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let res = get(&app, &me, "/api/v1/portal").await;
    res.expect(StatusCode::FORBIDDEN, "not_a_member");
    let access = get(&app, &owner, &format!("/api/v1/talent/{ada}/portal"))
        .await
        .ok();
    assert!(access.body["user"].is_null());

    // A staff member is never a portal user unless a record is opened for them.
    let home = get(&app, &owner, "/api/v1/portal").await.ok();
    assert_eq!(home.body["talent"], json!([]));
    let dates = get(&app, &owner, "/api/v1/portal/dates").await.ok();
    assert_eq!(dates.body, json!([]));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_talent_opens_only_the_documents_meant_for_them(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let ada = talent(&app, &owner, "Ada").await;
    let gig = booking(&app, &owner, ada, "Village Underground", "confirmed", 14).await;
    let me = invite_talent(&app, &owner, ws, ada, "ada@artist.test").await;

    let doc = |kind: &'static str, entity: &'static str, id: Uuid| {
        let app = &app;
        let owner = &owner;
        async move {
            let file = app
                .post(&format!("/api/v1/files?filename={kind}.pdf"))
                .signed_in(owner)
                .bytes(
                    "application/pdf",
                    b"%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n".to_vec(),
                )
                .send()
                .await
                .ok()
                .id("/id");
            post(
                app,
                owner,
                "/api/v1/documents",
                json!({ "file_id": file, "kind": kind, "title": kind, "related_entity": entity, "related_id": id }),
            )
            .await
            .ok()
            .id("/id")
        }
    };
    let contract = doc("contract", "booking", gig).await;
    let sheet = doc("advance_sheet", "booking", gig).await;
    let po = doc("purchase_order", "booking", gig).await;
    let rider = doc("rider", "talent", ada).await;

    let kinds_on_date = |body: &Value| -> Vec<String> {
        body["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["kind"].as_str().unwrap().to_owned())
            .collect()
    };
    // Contracts carry the fee, so they stay hidden while fees are.
    let d = get(&app, &me, &format!("/api/v1/portal/dates/{gig}"))
        .await
        .ok();
    assert_eq!(kinds_on_date(&d.body), ["advance_sheet"]);
    let all = get(&app, &me, "/api/v1/portal/documents").await.ok();
    assert_eq!(all.body.as_array().unwrap().len(), 2, "{}", all.body);
    get(
        &app,
        &me,
        &format!("/api/v1/portal/documents/{contract}/download"),
    )
    .await
    .expect(StatusCode::NOT_FOUND, "not_found");
    app.put("/api/v1/workspace/portal")
        .signed_in(&owner)
        .json(json!({ "show_fees": true }))
        .send()
        .await
        .ok();
    let d = get(&app, &me, &format!("/api/v1/portal/dates/{gig}"))
        .await
        .ok();
    let mut kinds = kinds_on_date(&d.body);
    kinds.sort();
    assert_eq!(kinds, ["advance_sheet", "contract"]);

    for id in [contract, sheet, rider] {
        let file = app
            .get(&format!("/api/v1/portal/documents/{id}/download"))
            .signed_in(&me)
            .send()
            .await;
        assert_eq!(file.status, StatusCode::OK);
    }
    get(
        &app,
        &me,
        &format!("/api/v1/portal/documents/{po}/download"),
    )
    .await
    .expect(StatusCode::NOT_FOUND, "not_found");

    // Switched off, documents go.
    app.put("/api/v1/workspace/portal")
        .signed_in(&owner)
        .json(json!({ "show_documents": false }))
        .send()
        .await
        .ok();
    let all = get(&app, &me, "/api/v1/portal/documents").await.ok();
    assert_eq!(all.body, json!([]));
    for id in [contract, rider] {
        get(
            &app,
            &me,
            &format!("/api/v1/portal/documents/{id}/download"),
        )
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn portal_links_are_made_only_by_invitation(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let agent = app.add_member(&owner, "agent@acme.test", "agent").await;
    let ada = talent(&app, &agent, "Ada").await;
    let me = invite_talent(&app, &agent, ws, ada, "ada@artist.test").await;
    let grace = talent(&app, &agent, "Grace").await;

    // Pointing another record at Ada's account would show her its dates.
    let ada_user = get(&app, &agent, &format!("/api/v1/talent/{ada}"))
        .await
        .ok()
        .body["portal_user_id"]
        .clone();
    let res = app
        .patch(&format!("/api/v1/talent/{grace}"))
        .signed_in(&agent)
        .json(json!({ "portal_user_id": ada_user }))
        .send()
        .await;
    res.expect(StatusCode::BAD_REQUEST, "invalid_input");
    let res = app
        .patch(&format!("/api/v1/talent/{ada}"))
        .signed_in(&agent)
        .json(json!({ "portal_user_id": null }))
        .send()
        .await;
    res.expect(StatusCode::BAD_REQUEST, "invalid_input");

    // A colleague's account has no portal to open.
    let res = post(
        &app,
        &agent,
        &format!("/api/v1/talent/{grace}/portal"),
        json!({ "email": "agent@acme.test" }),
    )
    .await;
    res.expect(StatusCode::CONFLICT, "already_member");

    // Deleting the record ends the portal, and the talent member goes.
    app.delete(&format!("/api/v1/talent/{ada}"))
        .signed_in(&agent)
        .send()
        .await
        .ok();
    let members = get(&app, &owner, "/api/v1/members").await.ok();
    assert!(
        !members.body.to_string().contains("ada@artist.test"),
        "{}",
        members.body
    );
    let res = get(&app, &me, "/api/v1/portal").await;
    assert!(!res.status.is_success());
}
