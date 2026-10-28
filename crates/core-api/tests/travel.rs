//! Travel on a booking and the money check through HTTP: flights read in
//! their own zones, codes and numbers are tidied, costs in other currencies
//! are converted, and the check warns when costs eat the fee.

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

/// A confirmed-enough booking in Barcelona for an act on 15% commission,
/// with a £2,000 fee.
async fn gig(app: &TestApp, who: &Client) -> Uuid {
    let talent = post(
        app,
        who,
        "/api/v1/talent",
        json!({ "name": "The Midnight Owls", "commission_percent": 15.0 }),
    )
    .await
    .ok()
    .id("/id");
    post(
        app,
        who,
        "/api/v1/bookings",
        json!({ "title": "Sala Apolo", "talent_ids": [talent], "status": "pending",
                "timezone": "Europe/Madrid", "starts_at": "2031-06-14T19:00:00Z",
                "ends_at": "2031-06-14T22:00:00Z", "fee": { "minor": 200_000, "currency": "GBP" } }),
    )
    .await
    .ok()
    .id("/id")
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn flights_and_hotels_sit_on_a_booking(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let booking = gig(&app, &owner).await;

    let flight = post(
        &app,
        &owner,
        &format!("/api/v1/bookings/{booking}/travel"),
        json!({ "kind": "flight", "carrier": "British Airways", "number": "ba1442", "reference": "X7YQ2P",
                "from_code": "lhr", "from_place": "London Heathrow", "to_code": "BCN",
                "starts_at": "2031-06-14T06:10:00Z", "start_tz": "Europe/London",
                "ends_at": "2031-06-14T08:25:00Z", "travellers": ["Ada", " Ada ", "Grace  Hopper"],
                "cost": { "minor": 46_000, "currency": "eur" } }),
    )
    .await;
    assert_eq!(flight.status, StatusCode::CREATED, "{}", flight.body);
    let f = &flight.body;
    assert_eq!(f["number"], "BA 1442");
    assert_eq!(f["from_code"], "LHR");
    assert_eq!(f["status"], "planned");
    assert_eq!(f["paid_by"], "us");
    // The arrival takes the departure's zone unless told otherwise.
    assert_eq!(f["end_tz"], "Europe/London");
    assert_eq!(f["travellers"], json!(["Ada", "Grace Hopper"]));
    assert_eq!(f["cost"], json!({ "minor": 46_000, "currency": "EUR" }));
    let flight_id = flight.id("/id");

    // The hotel is the promoter's: no zone given, so the booking's.
    let hotel = post(
        &app,
        &owner,
        &format!("/api/v1/bookings/{booking}/travel"),
        json!({ "kind": "hotel", "carrier": "Hotel Arts", "to_place": "Carrer de la Marina 19",
                "starts_at": "2031-06-14T13:00:00Z", "ends_at": "2031-06-15T10:00:00Z", "paid_by": "buyer",
                "cost": { "minor": 30_000, "currency": "EUR" } }),
    )
    .await
    .ok();
    assert_eq!(hotel.body["start_tz"], "Europe/Madrid");

    let bad = post(
        &app,
        &owner,
        &format!("/api/v1/bookings/{booking}/travel"),
        json!({ "kind": "rocket", "from_code": "Heathrow", "starts_at": "2031-06-14T06:10:00Z",
                "start_tz": "Mars/Olympus", "ends_at": "2031-06-13T06:10:00Z" }),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let fields: Vec<&str> = bad.body["error"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["field"].as_str().unwrap())
        .collect();
    for f in ["kind", "from_code", "start_tz", "ends_at"] {
        assert!(fields.contains(&f), "{f}: {}", bad.body);
    }

    // Listed in time order; changing replaces the whole item.
    let list = get(&app, &owner, &format!("/api/v1/bookings/{booking}/travel")).await;
    assert_eq!(list.body[0]["kind"], "flight");
    assert_eq!(list.body[1]["kind"], "hotel");
    let changed = app
        .put(&format!("/api/v1/travel/{flight_id}"))
        .signed_in(&owner)
        .header("If-Match", "\"1\"")
        .json(json!({ "kind": "flight", "number": "BA 1442", "status": "booked", "from_code": "LHR",
                      "to_code": "BCN", "starts_at": "2031-06-14T06:10:00Z", "start_tz": "Europe/London",
                      "ends_at": "2031-06-14T08:25:00Z", "end_tz": "Europe/Madrid",
                      "cost": { "minor": 46_000, "currency": "EUR" } }))
        .send()
        .await
        .ok();
    assert_eq!(changed.body["status"], "booked");
    assert_eq!(changed.body["end_tz"], "Europe/Madrid");
    assert_eq!(changed.body["carrier"], Value::Null);
    let stale = app
        .put(&format!("/api/v1/travel/{flight_id}"))
        .signed_in(&owner)
        .header("If-Match", "\"1\"")
        .json(json!({ "kind": "flight", "starts_at": "2031-06-14T06:10:00Z" }))
        .send()
        .await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);

    // Viewers read travel but cannot change it.
    let viewer = app.add_member(&owner, "viewer@acme.test", "viewer").await;
    get(&app, &viewer, &format!("/api/v1/bookings/{booking}/travel")).await;
    let denied = app
        .delete(&format!("/api/v1/travel/{flight_id}"))
        .signed_in(&viewer)
        .send()
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    // The day sheet is a document of its own, made from the booking.
    let doc = post(
        &app,
        &owner,
        "/api/v1/documents/generate",
        json!({ "template_key": "document.day_sheet", "related_entity": "booking", "related_id": booking }),
    )
    .await;
    assert_eq!(doc.status, StatusCode::CREATED, "{}", doc.body);
    assert_eq!(doc.body["kind"], "day_sheet");

    app.delete(&format!("/api/v1/travel/{flight_id}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let list = get(&app, &owner, &format!("/api/v1/bookings/{booking}/travel")).await;
    assert_eq!(list.body.as_array().unwrap().len(), 1);
    let ws = app.workspace_with_owner("other").await.0;
    let actions = app.audit_actions(ws).await;
    assert!(!actions.contains(&"travel.created".to_owned()));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_money_check_weighs_costs_against_the_fee(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let booking = gig(&app, &owner).await;
    // A euro buys 85p on the day.
    sqlx::query(
        "INSERT INTO fx_rates (rate_date, currency, per_eur) VALUES ('2031-06-13', 'GBP', 0.85)",
    )
    .execute(&app.owner)
    .await
    .unwrap();
    let travel_path = format!("/api/v1/bookings/{booking}/travel");
    let money_path = format!("/api/v1/bookings/{booking}/money");
    let travel = |body: Value| post(&app, &owner, &travel_path, body);
    let line = |body: Value| post(&app, &owner, &money_path, body);
    travel(json!({ "kind": "flight", "starts_at": "2031-06-14T06:10:00Z", "from_code": "LHR", "to_code": "BCN",
                   "number": "BA1442", "cost": { "minor": 46_000, "currency": "EUR" } }))
        .await
        .ok();
    // The buyer's hotel and a cancelled train cost nothing here.
    travel(
        json!({ "kind": "hotel", "starts_at": "2031-06-14T13:00:00Z", "paid_by": "buyer",
                   "cost": { "minor": 30_000, "currency": "EUR" } }),
    )
    .await
    .ok();
    travel(
        json!({ "kind": "train", "status": "cancelled", "starts_at": "2031-06-14T13:00:00Z",
                   "cost": { "minor": 9_000, "currency": "GBP" } }),
    )
    .await
    .ok();
    line(
        json!({ "direction": "income", "category": "buyout", "description": "Catering buyout",
                 "amount": { "minor": 30_000, "currency": "GBP" } }),
    )
    .await
    .ok();
    line(
        json!({ "direction": "cost", "category": "per_diem", "description": "Per diems",
                 "amount": { "minor": 15_000, "currency": "GBP" } }),
    )
    .await
    .ok();
    let wrong = line(
        json!({ "direction": "income", "category": "per_diem", "description": "x",
                             "amount": { "minor": 1, "currency": "GBP" } }),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);

    let m = get(&app, &owner, &format!("/api/v1/bookings/{booking}/money")).await;
    let m = &m.body;
    assert_eq!(m["currency"], "GBP");
    // £2,000 fee + £300 buyout; 15% of the fee only; €460 is £391 + £150.
    assert_eq!(m["total_income"]["minor"], 230_000);
    assert_eq!(m["total_commission"]["minor"], 30_000);
    assert_eq!(m["total_costs"]["minor"], 54_100);
    assert_eq!(m["net"]["minor"], 145_900);
    assert_eq!(m["warnings"], json!([]));
    assert_eq!(
        m["costs"][0]["description"],
        "Flight · BA 1442 · LHR to BCN"
    );
    assert_eq!(
        m["costs"][0]["original"],
        json!({ "minor": 46_000, "currency": "EUR" })
    );
    assert_eq!(m["lines"].as_array().unwrap().len(), 2);

    // A big production bill turns it into a loss; a dollar cost with no
    // rate is left out and said so.
    let production = line(json!({ "direction": "cost", "category": "production",
                                  "description": "Backline hire", "amount": { "minor": 160_000, "currency": "GBP" } }))
        .await
        .ok()
        .id("/id");
    line(
        json!({ "direction": "cost", "category": "visa", "description": "Visa",
                 "amount": { "minor": 19_000, "currency": "USD" } }),
    )
    .await
    .ok();
    let m = get(&app, &owner, &format!("/api/v1/bookings/{booking}/money")).await;
    assert_eq!(m.body["net"]["minor"], -14_100);
    let codes: Vec<&str> = m.body["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["loss", "unconverted"]);

    // Smaller, it is only a caution.
    app.put(&format!("/api/v1/money-lines/{production}"))
        .signed_in(&owner)
        .json(
            json!({ "direction": "cost", "category": "production", "description": "Backline hire",
                      "amount": { "minor": 80_000, "currency": "GBP" } }),
        )
        .send()
        .await
        .ok();
    let m = get(&app, &owner, &format!("/api/v1/bookings/{booking}/money")).await;
    assert_eq!(m.body["warnings"][0]["code"], "high_costs");
    assert_eq!(m.body["warnings"][0]["level"], "caution");

    let actions = app.audit_actions(ws).await;
    for a in ["travel.created", "money_line.created", "money_line.updated"] {
        assert!(actions.contains(&a.to_owned()), "{a}");
    }
}
