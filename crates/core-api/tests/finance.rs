//! M5's money through HTTP: tax codes and settings, invoices from draft to
//! issued (numbered, frozen), part payment, credit notes and voids,
//! refunds, past invoices kept as records, settlements and payouts, and the
//! reports with their CSV downloads. The two-currency deposit-and-balance
//! flow, which needs the worker, is in `core-worker/tests/finance.rs`.

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

async fn get(app: &TestApp, who: &Client, path: &str) -> Value {
    app.get(path).signed_in(who).send().await.ok().body
}

struct Cast {
    owner: Client,
    talent: Uuid,
    organisation: Uuid,
}

async fn cast(app: &TestApp) -> Cast {
    let (_, owner) = app.workspace_with_owner("acme").await;
    let talent = post(
        app,
        &owner,
        "/api/v1/talent",
        json!({ "name": "The Midnight Owls", "commission_percent": 15.0 }),
    )
    .await
    .ok()
    .id("/id");
    let organisation = post(
        app,
        &owner,
        "/api/v1/organisations",
        json!({ "name": "The Grand Hall" }),
    )
    .await
    .ok()
    .id("/id");
    Cast {
        owner,
        talent,
        organisation,
    }
}

async fn draft(app: &TestApp, c: &Cast, lines: Value) -> Value {
    let res = post(
        app,
        &c.owner,
        "/api/v1/invoices",
        json!({ "payer": { "organisation_id": c.organisation }, "line_items": lines }),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    res.body
}

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v["id"].as_str().unwrap()).unwrap()
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn tax_codes_and_settings_start_from_the_country(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let c = cast(&app).await;

    // A UK workspace starts with UK VAT, 20% by default.
    let codes = get(&app, &c.owner, "/api/v1/tax-codes").await;
    let default: Vec<&Value> = codes
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["is_default"] == true)
        .collect();
    assert_eq!(default.len(), 1, "{codes}");
    assert_eq!(default[0]["rate_bps"], 2000);

    // A new default replaces the old one.
    let zero = post(
        &app,
        &c.owner,
        "/api/v1/tax-codes",
        json!({ "name": "No VAT (not registered)", "rate_bps": 0, "kind": "out_of_scope", "is_default": true }),
    )
    .await;
    assert_eq!(zero.status, StatusCode::CREATED, "{}", zero.body);
    let codes = get(&app, &c.owner, "/api/v1/tax-codes").await;
    let defaults: Vec<&str> = codes
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["is_default"] == true)
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(defaults, ["No VAT (not registered)"]);
    post(
        &app,
        &c.owner,
        "/api/v1/tax-codes",
        json!({ "name": "Silly", "rate_bps": 20_000 }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");

    // Settings: the base currency follows the workspace; formats are checked.
    let s = get(&app, &c.owner, "/api/v1/finance/settings").await;
    assert_eq!(s["base_currency"], "GBP");
    assert_eq!(s["invoicing_model"], "agency");
    app.put("/api/v1/finance/settings")
        .signed_in(&c.owner)
        .json(json!({ "invoice_format": "no counter here", "invoicing_model": "barter" }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");
    let saved = app
        .put("/api/v1/finance/settings")
        .signed_in(&c.owner)
        .json(
            json!({ "invoice_format": "OH{YY}-{000}", "payment_terms_days": 30,
                      "legal_name": "Acme Agency Ltd", "tax_number": "GB123456789",
                      "bank_accounts": [{ "currency": "GBP", "name": "Acme Agency Ltd",
                                          "details": "Sort code 00-00-00, account 12345678" }] }),
        )
        .send()
        .await
        .ok()
        .body;
    assert_eq!(saved["invoice_format"], "OH{YY}-{000}");

    // The numbers follow the format.
    let d = draft(
        &app,
        &c,
        json!([{ "description": "Fee", "unit_minor": 10_000 }]),
    )
    .await;
    let issued = post(
        &app,
        &c.owner,
        &format!("/api/v1/invoices/{}/issue", id(&d)),
        json!({}),
    )
    .await
    .ok()
    .body;
    let yy = chrono::Utc::now().format("%y").to_string();
    assert_eq!(issued["reference"], format!("OH{yy}-001"));
    assert_eq!(issued["tax"]["minor"], 0);
    assert_eq!(issued["issuer"]["name"], "Acme Agency Ltd", "{issued}");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_invoice_is_issued_part_paid_credited_and_refunded(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let c = cast(&app).await;

    let d = draft(
        &app,
        &c,
        json!([
            { "description": "Evening performance", "unit_minor": 100_000 },
            { "description": "Travel", "quantity": 2, "unit_minor": 5_000 }
        ]),
    )
    .await;
    assert_eq!(d["status"], "draft");
    assert!(d["reference"].is_null());
    assert_eq!(d["subtotal"]["minor"], 110_000);
    assert_eq!(d["tax"]["minor"], 22_000);
    assert_eq!(d["total"]["minor"], 132_000);
    let inv = id(&d);

    // Drafts change freely and can be deleted; this one is kept.
    let edited = app
        .patch(&format!("/api/v1/invoices/{inv}"))
        .signed_in(&c.owner)
        .json(json!({ "notes": "Thank you!" }))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(edited["notes"], "Thank you!");

    // Issuing numbers and freezes it.
    let issued = post(
        &app,
        &c.owner,
        &format!("/api/v1/invoices/{inv}/issue"),
        json!({}),
    )
    .await
    .ok()
    .body;
    let year = chrono::Utc::now().format("%Y").to_string();
    assert_eq!(issued["reference"], format!("INV-{year}-0001"));
    assert_eq!(issued["status"], "issued");
    assert!(issued["due_date"].is_string());
    app.patch(&format!("/api/v1/invoices/{inv}"))
        .signed_in(&c.owner)
        .json(json!({ "line_items": [{ "description": "Cheaper", "unit_minor": 1 }] }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "invoice_issued");
    app.delete(&format!("/api/v1/invoices/{inv}"))
        .signed_in(&c.owner)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "invoice_issued");
    let pdf = app
        .get(&format!("/api/v1/invoices/{inv}/pdf"))
        .signed_in(&c.owner)
        .send()
        .await
        .ok();
    assert_eq!(pdf.headers["content-type"], "application/pdf");
    assert!(pdf.body.as_str().unwrap().starts_with("%PDF-"));

    // Part paid.
    let pay = post(
        &app,
        &c.owner,
        "/api/v1/payments",
        json!({ "amount": { "minor": 50_000, "currency": "GBP" }, "reference": "BACS 1",
                "allocations": [{ "invoice_id": inv }] }),
    )
    .await;
    assert_eq!(pay.status, StatusCode::CREATED, "{}", pay.body);
    let payment = id(&pay.body);
    assert_eq!(pay.body["organisation_name"], "The Grand Hall");
    let i = get(&app, &c.owner, &format!("/api/v1/invoices/{inv}")).await;
    assert_eq!(i["status"], "part_paid");
    assert_eq!(i["outstanding"]["minor"], 82_000);

    // Overpaying an invoice and paying in the wrong currency are refused.
    post(
        &app,
        &c.owner,
        "/api/v1/payments",
        json!({ "amount": { "minor": 500_000, "currency": "GBP" },
                "allocations": [{ "invoice_id": inv, "amount_minor": 500_000 }] }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
    post(
        &app,
        &c.owner,
        "/api/v1/payments",
        json!({ "amount": { "minor": 100, "currency": "EUR" }, "allocations": [{ "invoice_id": inv }] }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");

    // Paid money means no voiding: a credit note instead, for the travel.
    post(
        &app,
        &c.owner,
        &format!("/api/v1/invoices/{inv}/void"),
        json!({}),
    )
    .await
    .expect(StatusCode::CONFLICT, "invoice_has_payments");
    let credit = post(
        &app,
        &c.owner,
        &format!("/api/v1/invoices/{inv}/credit-notes"),
        json!({ "line_items": [{ "description": "Travel not needed", "quantity": 2, "unit_minor": 5_000 }] }),
    )
    .await;
    assert_eq!(credit.status, StatusCode::CREATED, "{}", credit.body);
    assert_eq!(credit.body["kind"], "credit_note");
    assert_eq!(credit.body["reference"], format!("CN-{year}-0001"));
    assert_eq!(credit.body["total"]["minor"], 12_000);
    let i = get(&app, &c.owner, &format!("/api/v1/invoices/{inv}")).await;
    assert_eq!(i["credited"]["minor"], 12_000);
    assert_eq!(i["outstanding"]["minor"], 70_000);

    // More credit than is left is refused.
    post(
        &app,
        &c.owner,
        &format!("/api/v1/invoices/{inv}/credit-notes"),
        json!({ "line_items": [{ "description": "Everything", "unit_minor": 200_000 }] }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");

    // The rest arrives in one go; a little too much, kept unallocated.
    let rest = post(
        &app,
        &c.owner,
        "/api/v1/payments",
        json!({ "amount": { "minor": 75_000, "currency": "GBP" }, "organisation_id": c.organisation,
                "allocations": [{ "invoice_id": inv }] }),
    )
    .await
    .ok()
    .body;
    assert_eq!(rest["allocated"]["minor"], 70_000);
    assert_eq!(rest["unallocated"]["minor"], 5_000);
    let i = get(&app, &c.owner, &format!("/api/v1/invoices/{inv}")).await;
    assert_eq!(i["status"], "paid");

    // The extra is refunded; no more than is unallocated.
    let rest_id = id(&rest);
    post(
        &app,
        &c.owner,
        &format!("/api/v1/payments/{rest_id}/refunds"),
        json!({ "amount_minor": 6_000 }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
    let refund = post(
        &app,
        &c.owner,
        &format!("/api/v1/payments/{rest_id}/refunds"),
        json!({ "amount_minor": 5_000 }),
    )
    .await;
    assert_eq!(refund.status, StatusCode::CREATED, "{}", refund.body);
    assert_eq!(refund.body["kind"], "refund");

    // Voiding the first payment takes it off the invoice again.
    post(
        &app,
        &c.owner,
        &format!("/api/v1/payments/{payment}/void"),
        json!({ "reason": "Bounced" }),
    )
    .await
    .ok();
    let i = get(&app, &c.owner, &format!("/api/v1/invoices/{inv}")).await;
    assert_eq!(i["status"], "part_paid", "{i}");
    assert_eq!(i["outstanding"]["minor"], 50_000);
    let allocations = get(
        &app,
        &c.owner,
        &format!("/api/v1/invoices/{inv}/allocations"),
    )
    .await;
    assert_eq!(allocations.as_array().unwrap().len(), 1, "{allocations}");

    // An invoice nobody paid can be voided; it keeps its number.
    let other = draft(
        &app,
        &c,
        json!([{ "description": "Fee", "unit_minor": 1_000 }]),
    )
    .await;
    let other = id(&other);
    post(
        &app,
        &c.owner,
        &format!("/api/v1/invoices/{other}/issue"),
        json!({}),
    )
    .await
    .ok();
    let voided = post(
        &app,
        &c.owner,
        &format!("/api/v1/invoices/{other}/void"),
        json!({ "reason": "Duplicate" }),
    )
    .await
    .ok()
    .body;
    assert_eq!(voided["status"], "void");
    assert_eq!(voided["reference"], format!("INV-{year}-0002"));

    // In the database too, an issued invoice cannot change.
    let frozen = sqlx::query("UPDATE invoices SET total_minor = 1 WHERE id = $1")
        .bind(inv)
        .execute(&app.owner)
        .await;
    assert!(frozen.is_err(), "an issued invoice was changed");

    // The lists, the report and the CSV.
    let list = get(&app, &c.owner, "/api/v1/invoices?status=part_paid").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let report = get(&app, &c.owner, "/api/v1/reports/finance").await;
    assert_eq!(
        report["outstanding"][0]["outstanding"]["minor"], 50_000,
        "{report}"
    );
    let csv = app
        .get("/api/v1/reports/invoices/csv")
        .signed_in(&c.owner)
        .send()
        .await
        .ok();
    assert!(
        csv.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/csv")
    );
    let text = csv.body.as_str().unwrap();
    assert!(text.starts_with("reference,kind,status"), "{text}");
    assert!(
        text.contains(&format!("INV-{year}-0001,invoice,part_paid,The Grand Hall")),
        "{text}"
    );
    assert!(text.contains("1320.00"), "{text}");
    app.get("/api/v1/reports/nothing/csv")
        .signed_in(&c.owner)
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn past_invoices_are_kept_as_records_without_taking_a_number(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let c = cast(&app).await;

    let past = post(
        &app,
        &c.owner,
        "/api/v1/invoices/historical",
        json!({ "reference": "OLD-1234", "payer": { "organisation_id": c.organisation },
                "currency": "EUR", "issue_date": "2024-03-01", "total_minor": 120_000,
                "paid": true }),
    )
    .await;
    assert_eq!(past.status, StatusCode::CREATED, "{}", past.body);
    assert_eq!(past.body["reference"], "OLD-1234");
    assert_eq!(past.body["historical"], true);
    assert_eq!(past.body["status"], "paid");
    assert_eq!(past.body["total"]["minor"], 120_000);

    // The same number twice is refused.
    post(
        &app,
        &c.owner,
        "/api/v1/invoices/historical",
        json!({ "reference": "OLD-1234", "payer": { "organisation_id": c.organisation },
                "currency": "EUR", "issue_date": "2024-03-02", "total_minor": 1 }),
    )
    .await
    .expect(StatusCode::CONFLICT, "invoice_number_taken");

    // The first invoice made in OnHold still takes the first number.
    let d = draft(
        &app,
        &c,
        json!([{ "description": "Fee", "unit_minor": 10_000 }]),
    )
    .await;
    let issued = post(
        &app,
        &c.owner,
        &format!("/api/v1/invoices/{}/issue", id(&d)),
        json!({}),
    )
    .await
    .ok()
    .body;
    assert!(
        issued["reference"].as_str().unwrap().ends_with("-0001"),
        "{issued}"
    );

    // A past invoice cannot be sent.
    post(
        &app,
        &c.owner,
        &format!("/api/v1/invoices/{}/send", id(&past.body)),
        json!({ "to": "a@b.test" }),
    )
    .await
    .expect(StatusCode::CONFLICT, "invoice_not_issued");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_settlement_pays_the_talent_their_share(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let c = cast(&app).await;
    let booking = post(
        &app,
        &c.owner,
        "/api/v1/bookings",
        json!({ "title": "Gala", "talent_ids": [c.talent], "status": "confirmed",
                "organisation_id": c.organisation, "fee": { "minor": 200_000, "currency": "GBP" },
                "starts_at": "2031-06-01T19:00:00Z", "ends_at": "2031-06-01T23:00:00Z" }),
    )
    .await
    .ok()
    .id("/id");

    let made = post(
        &app,
        &c.owner,
        &format!("/api/v1/bookings/{booking}/settlements"),
        json!({}),
    )
    .await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    let s = &made.body[0];
    assert_eq!(s["talent_name"], "The Midnight Owls");
    assert_eq!(s["gross"]["minor"], 200_000);
    assert_eq!(s["commission_bps"], 1500);
    assert_eq!(s["commission"]["minor"], 30_000);
    assert_eq!(s["net"]["minor"], 170_000);
    let sid = id(s);

    let patched = app
        .patch(&format!("/api/v1/settlements/{sid}"))
        .signed_in(&c.owner)
        .json(json!({ "expenses": [{ "description": "Van hire", "amount_minor": 20_000 }] }))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(patched["net"]["minor"], 150_000);

    // Finalising makes the statement; then it is fixed until reopened.
    let fin = post(
        &app,
        &c.owner,
        &format!("/api/v1/settlements/{sid}/finalise"),
        json!({}),
    )
    .await
    .ok()
    .body;
    assert_eq!(fin["status"], "final", "{fin}");
    assert!(fin["document_id"].is_string());
    app.patch(&format!("/api/v1/settlements/{sid}"))
        .signed_in(&c.owner)
        .json(json!({ "notes": "late change" }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "settlement_final");

    // The payout: by default what is left to pay.
    let paid = post(
        &app,
        &c.owner,
        &format!("/api/v1/settlements/{sid}/payouts"),
        json!({}),
    )
    .await
    .ok()
    .body;
    assert_eq!(paid["paid_out"]["minor"], 150_000, "{paid}");
    let payouts = get(&app, &c.owner, "/api/v1/payments?kind=payout").await;
    assert_eq!(payouts["items"][0]["amount"]["minor"], 150_000, "{payouts}");
    assert_eq!(payouts["items"][0]["talent_name"], "The Midnight Owls");
    // Nothing more is owed to them.
    post(
        &app,
        &c.owner,
        &format!("/api/v1/settlements/{sid}/payouts"),
        json!({}),
    )
    .await
    .expect(StatusCode::CONFLICT, "settlement_paid");
    let report = get(&app, &c.owner, "/api/v1/reports/finance").await;
    assert_eq!(report["cash"][0]["paid_out_minor"], 150_000, "{report}");
}
