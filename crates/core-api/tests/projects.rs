//! Projects through HTTP: a tour's dates, crew, shared travel and costs,
//! whether each date and the whole run pays, the budget, and the day-by-day
//! itinerary.

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

async fn get(app: &TestApp, who: &Client, path: &str) -> common::Resp {
    app.get(path).signed_in(who).send().await.ok()
}

async fn date(
    app: &TestApp,
    who: &Client,
    talent: Uuid,
    title: &str,
    tz: &str,
    at: &str,
    fee: Value,
) -> Uuid {
    let starts = chrono::DateTime::parse_from_rfc3339(at).unwrap();
    let ends = starts + chrono::Duration::hours(3);
    post(
        app,
        who,
        "/api/v1/bookings",
        json!({ "title": title, "talent_ids": [talent], "status": "pending", "timezone": tz,
                "starts_at": at, "ends_at": ends.to_rfc3339(), "fee": fee, "venue_text": title }),
    )
    .await
    .ok()
    .id("/id")
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_tour_adds_up_across_its_dates(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    // A euro buys 85p.
    sqlx::query(
        "INSERT INTO fx_rates (rate_date, currency, per_eur) VALUES (current_date, 'GBP', 0.85)",
    )
    .execute(&app.owner)
    .await
    .unwrap();
    let talent = post(
        &app,
        &owner,
        "/api/v1/talent",
        json!({ "name": "The Midnight Owls", "commission_percent": 15.0 }),
    )
    .await
    .ok()
    .id("/id");
    let gbp = |minor: i64| json!({ "minor": minor, "currency": "GBP" });
    let london = date(
        &app,
        &owner,
        talent,
        "Electric Ballroom",
        "Europe/London",
        "2031-06-12T19:00:00Z",
        gbp(300_000),
    )
    .await;
    let paris = date(
        &app,
        &owner,
        talent,
        "La Maroquinerie",
        "Europe/Paris",
        "2031-06-14T18:00:00Z",
        json!({ "minor": 200_000, "currency": "EUR" }),
    )
    .await;
    let berlin = date(
        &app,
        &owner,
        talent,
        "Lido",
        "Europe/Berlin",
        "2031-06-15T18:00:00Z",
        gbp(100_000),
    )
    .await;

    let created = post(
        &app,
        &owner,
        "/api/v1/projects",
        json!({ "name": "Summer run 2031", "budget": { "accommodation": 20_000, "travel": 100_000 },
                "booking_ids": [london, paris] }),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let p = &created.body;
    assert_eq!(p["kind"], "tour");
    assert_eq!(p["status"], "planning");
    assert_eq!(p["currency"], "GBP");
    assert_eq!(p["dates"], 2);
    let project = created.id("/id");
    let bad = post(
        &app,
        &owner,
        "/api/v1/projects",
        json!({ "name": "x", "kind": "gig", "budget": { "snacks": 1 } }),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);

    post(
        &app,
        &owner,
        &format!("/api/v1/projects/{project}/dates"),
        json!({ "booking_id": berlin }),
    )
    .await
    .ok();
    // The hotel in Paris belongs to that date; the van to the project, on the day between.
    post(&app, &owner, &format!("/api/v1/bookings/{paris}/travel"),
         json!({ "kind": "hotel", "carrier": "Hôtel Amour", "starts_at": "2031-06-13T13:00:00Z",
                 "ends_at": "2031-06-15T09:00:00Z", "cost": { "minor": 30_000, "currency": "EUR" } }))
        .await
        .ok();
    let van = post(&app, &owner, &format!("/api/v1/projects/{project}/travel"),
         json!({ "kind": "car", "carrier": "Splitter van", "from_place": "London", "to_place": "Paris",
                 "starts_at": "2031-06-13T07:00:00Z", "start_tz": "Europe/London",
                 "ends_at": "2031-06-13T16:00:00Z", "end_tz": "Europe/Paris", "cost": gbp(40_000) }))
        .await
        .ok();
    assert_eq!(van.body["project_id"], project.to_string());
    assert_eq!(van.body["booking_id"], Value::Null);
    post(&app, &owner, &format!("/api/v1/bookings/{berlin}/money"),
         json!({ "direction": "cost", "category": "production", "description": "Backline", "amount": gbp(150_000) }))
        .await
        .ok();
    post(&app, &owner, &format!("/api/v1/projects/{project}/money"),
         json!({ "direction": "cost", "category": "other", "description": "Insurance", "amount": gbp(20_000) }))
        .await
        .ok();
    post(&app, &owner, &format!("/api/v1/projects/{project}/money"),
         json!({ "direction": "income", "category": "other", "description": "Tour support", "amount": gbp(50_000) }))
        .await
        .ok();
    let crew = post(&app, &owner, &format!("/api/v1/projects/{project}/crew"),
                    json!({ "name": "Dev Patel", "role": "FOH engineer", "phone": "+44 7700 900123", "notes": "Vegetarian" }))
        .await;
    assert_eq!(crew.status, StatusCode::CREATED, "{}", crew.body);

    let o = get(
        &app,
        &owner,
        &format!("/api/v1/projects/{project}/overview"),
    )
    .await;
    let o = &o.body;
    let dates = o["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 3);
    assert_eq!(dates[0]["title"], "Electric Ballroom");
    assert_eq!(dates[0]["net"]["minor"], 255_000);
    // Paris works in euros, and in pounds for the totals.
    assert_eq!(
        dates[1]["net"],
        json!({ "minor": 140_000, "currency": "EUR" })
    );
    assert_eq!(
        dates[1]["net_in_currency"],
        json!({ "minor": 119_000, "currency": "GBP" })
    );
    assert_eq!(dates[2]["net"]["minor"], -65_000);
    assert_eq!(dates[2]["warnings"][0]["code"], "loss");
    assert_eq!(o["total_income"]["minor"], 620_000);
    assert_eq!(o["total_commission"]["minor"], 85_500);
    assert_eq!(o["total_costs"]["minor"], 235_500);
    assert_eq!(o["net"]["minor"], 299_000);
    let codes: Vec<&str> = o["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["losing_dates", "over_budget"]);
    assert_eq!(o["warnings"][1]["message"], "Over budget on accommodation.");
    let budget: Vec<(&str, i64, i64)> = o["budget"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["category"].as_str().unwrap(),
                b["budget"]["minor"].as_i64().unwrap(),
                b["actual"]["minor"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        budget,
        [
            ("travel", 100_000, 0),
            ("accommodation", 20_000, 25_500),
            ("ground", 0, 40_000),
            ("production", 0, 150_000),
            ("other", 0, 20_000)
        ]
    );
    assert_eq!(o["crew"][0]["name"], "Dev Patel");
    assert_eq!(o["travel"].as_array().unwrap().len(), 1);
    assert_eq!(o["lines"].as_array().unwrap().len(), 2);

    // Day by day: a show, the drive, Paris, Berlin; the Paris hotel covers two nights.
    let it = get(
        &app,
        &owner,
        &format!("/api/v1/projects/{project}/itinerary"),
    )
    .await;
    let days: Vec<(&str, &str, usize, usize)> = it.body["days"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["date"].as_str().unwrap(),
                d["kind"].as_str().unwrap(),
                d["travel"].as_array().unwrap().len(),
                d["hotels"].as_array().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(
        days,
        [
            ("2031-06-12", "show", 0, 0),
            ("2031-06-13", "travel", 1, 1),
            ("2031-06-14", "show", 0, 1),
            ("2031-06-15", "show", 0, 0)
        ]
    );
    assert_eq!(
        it.body["days"][0]["dates"][0]["talent"][0],
        "The Midnight Owls"
    );

    // Milestone tasks hang off the project.
    let task = post(
        &app,
        &owner,
        "/api/v1/tasks",
        json!({ "title": "Send day sheets", "related_entity": "project", "related_id": project }),
    )
    .await
    .ok();
    assert_eq!(task.body["related_name"], "Summer run 2031");

    // A viewer reads it but cannot change it.
    let viewer = app.add_member(&owner, "viewer@acme.test", "viewer").await;
    get(
        &app,
        &viewer,
        &format!("/api/v1/projects/{project}/overview"),
    )
    .await;
    let denied = post(
        &app,
        &viewer,
        &format!("/api/v1/projects/{project}/crew"),
        json!({ "name": "x" }),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let denied = app
        .delete(&format!("/api/v1/travel/{}", van.id("/id")))
        .signed_in(&viewer)
        .send()
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    // A date moves between projects; taking one out or deleting the project keeps the bookings.
    let other = post(
        &app,
        &owner,
        "/api/v1/projects",
        json!({ "name": "Autumn", "kind": "season" }),
    )
    .await
    .ok()
    .id("/id");
    let moved = post(
        &app,
        &owner,
        &format!("/api/v1/projects/{other}/dates"),
        json!({ "booking_id": berlin }),
    )
    .await
    .ok();
    assert_eq!(moved.body["dates"], 1);
    assert_eq!(
        get(&app, &owner, &format!("/api/v1/projects/{project}"))
            .await
            .body["dates"],
        2
    );
    app.delete(&format!("/api/v1/projects/{project}/dates/{london}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(
        get(&app, &owner, &format!("/api/v1/projects/{project}"))
            .await
            .body["dates"],
        1
    );
    app.delete(&format!("/api/v1/projects/{project}"))
        .signed_in(&owner)
        .send()
        .await;
    get(&app, &owner, &format!("/api/v1/bookings/{paris}")).await;
    let list = get(&app, &owner, "/api/v1/projects").await;
    assert_eq!(list.body["total"], 1);

    let actions = app.audit_actions(ws).await;
    for a in [
        "project.created",
        "project.date_added",
        "project.crew_added",
        "project.date_removed",
        "project.deleted",
    ] {
        assert!(actions.contains(&a.to_owned()), "{a}");
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn cancelled_dates_count_for_nothing_and_long_dates_fill_their_days(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let talent = post(&app, &owner, "/api/v1/talent", json!({ "name": "Ada" }))
        .await
        .ok()
        .id("/id");
    let gbp = |minor: i64| json!({ "minor": minor, "currency": "GBP" });
    // A residency over three days, and a date that falls through.
    let residency = post(
        &app,
        &owner,
        "/api/v1/bookings",
        json!({ "title": "Residency", "talent_ids": [talent], "status": "pending", "timezone": "Europe/London",
                "starts_at": "2031-07-01T09:00:00Z", "ends_at": "2031-07-03T17:00:00Z", "fee": gbp(100_000) }),
    )
    .await
    .ok()
    .id("/id");
    let dropped = date(
        &app,
        &owner,
        talent,
        "Dropped",
        "Europe/London",
        "2031-07-05T19:00:00Z",
        gbp(200_000),
    )
    .await;
    let project = post(
        &app,
        &owner,
        "/api/v1/projects",
        json!({ "name": "July", "budget": { "travel": 1_000 }, "booking_ids": [residency, dropped] }),
    )
    .await
    .ok()
    .id("/id");
    post(
        &app,
        &owner,
        &format!("/api/v1/bookings/{dropped}/status"),
        json!({ "status": "cancelled", "reason": "Venue closed" }),
    )
    .await
    .ok();

    let o = get(
        &app,
        &owner,
        &format!("/api/v1/projects/{project}/overview"),
    )
    .await;
    assert_eq!(o.body["dates"].as_array().unwrap().len(), 2);
    assert_eq!(o.body["total_income"]["minor"], 100_000, "{}", o.body);

    let it = get(
        &app,
        &owner,
        &format!("/api/v1/projects/{project}/itinerary"),
    )
    .await;
    let days: Vec<(&str, &str)> = it.body["days"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["date"].as_str().unwrap(), d["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(
        days,
        [
            ("2031-07-01", "show"),
            ("2031-07-02", "show"),
            ("2031-07-03", "show")
        ]
    );

    // The budget is in the project's currency, so a new currency needs it again.
    let changed = app
        .patch(&format!("/api/v1/projects/{project}"))
        .signed_in(&owner)
        .json(json!({ "currency": "EUR" }))
        .send()
        .await;
    assert_eq!(changed.status, StatusCode::BAD_REQUEST);
}
