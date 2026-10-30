//! M5's demonstrable, end to end: two bookings, one in pounds and one in
//! euros, each invoiced for a deposit that is emailed with its PDF, paid,
//! and (by a rule) confirms the booking and makes its confirmation; then the
//! balance is invoiced, falls overdue, and a date rule chases it. Also the
//! signing email and the daily exchange rate fetch being off.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core-api/tests/common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use common::{Client, TestApp};
use core_integrations::http::WebhookClient;
use core_integrations::mail::MemoryMailer;
use core_worker::{Settings, Worker};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

struct Harness {
    app: TestApp,
    worker: Worker,
    mailer: Arc<MemoryMailer>,
}

async fn harness(pool: PgPool) -> Harness {
    let app = TestApp::new(pool).await;
    let options = app.owner.connect_options().as_ref().clone();
    let queue = core_db::worker_pool_options(PgPoolOptions::new().max_connections(3))
        .connect_with(options)
        .await
        .unwrap();
    let mailer = Arc::new(MemoryMailer::default());
    let worker = Worker {
        app: app.state.clone(),
        queue,
        mailer: mailer.clone(),
        webhooks: WebhookClient::new(true).unwrap(),
        settings: Settings {
            concurrency: 4,
            backoff: |_| Duration::ZERO,
            // No network in tests: rates are stored by hand below.
            fx_rates: None,
            ..Settings::default()
        },
    };
    Harness {
        app,
        worker,
        mailer,
    }
}

impl Harness {
    async fn drain(&self) {
        for _ in 0..50 {
            let published = self.worker.publish_once().await.unwrap();
            let ran = self.worker.run_jobs_once().await.unwrap();
            if published == 0 && ran == 0 {
                return;
            }
        }
        panic!("the queue did not settle");
    }

    async fn tick(&self) {
        self.worker.schedule_once().await.unwrap();
        self.drain().await;
    }

    async fn post(&self, who: &Client, path: &str, body: Value) -> common::Resp {
        let res = self.app.post(path).signed_in(who).json(body).send().await;
        assert!(
            res.status.is_success(),
            "{path}: {} {}",
            res.status,
            res.body
        );
        res
    }

    async fn get(&self, who: &Client, path: &str) -> Value {
        self.app.get(path).signed_in(who).send().await.ok().body
    }

    async fn topics(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT topic FROM outbox ORDER BY created_at, id")
            .fetch_all(&self.app.owner)
            .await
            .unwrap()
    }
}

struct Cast {
    owner: Client,
    talent: Uuid,
    organisation: Uuid,
    contact: Uuid,
}

async fn cast(h: &Harness) -> Cast {
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    let talent = h
        .post(
            &owner,
            "/api/v1/talent",
            json!({ "name": "The Midnight Owls" }),
        )
        .await
        .id("/id");
    let organisation = h
        .post(
            &owner,
            "/api/v1/organisations",
            json!({ "name": "Le Grand Hall", "address": { "line1": "1 Rue de la Paix", "city": "Paris", "country": "FR" } }),
        )
        .await
        .id("/id");
    let contact = h
        .post(
            &owner,
            "/api/v1/contacts",
            json!({ "name": "Bea Booker", "organisation_id": organisation, "emails": ["bea@grand.test"] }),
        )
        .await
        .id("/id");
    Cast {
        owner,
        talent,
        organisation,
        contact,
    }
}

async fn booking(h: &Harness, c: &Cast, title: &str, fee: Value, days: i64) -> Uuid {
    let starts = Utc::now() + chrono::Duration::days(days);
    h.post(
        &c.owner,
        "/api/v1/bookings",
        json!({ "title": title, "talent_ids": [c.talent], "status": "hold",
                "organisation_id": c.organisation, "contact_id": c.contact,
                "fee": fee, "deposit_percent": 25.0,
                "starts_at": starts, "ends_at": starts + chrono::Duration::hours(4) }),
    )
    .await
    .id("/id")
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn deposit_paid_and_balance_chased_in_two_currencies(pool: PgPool) {
    let h = harness(pool).await;
    let c = cast(&h).await;
    let today = Utc::now().date_naive();

    // The day's reference rates: one euro buys 0.85 pounds.
    let mut conn = h.app.owner.acquire().await.unwrap();
    core_db::finance::store_rates(
        &mut conn,
        today,
        &[("GBP".into(), "0.85".into()), ("USD".into(), "1.08".into())],
        "ecb",
    )
    .await
    .unwrap();
    drop(conn);

    // Paying a deposit confirms the booking and makes its confirmation.
    h.post(
        &c.owner,
        "/api/v1/rules",
        json!({
            "name": "Deposit in: confirm",
            "trigger": { "kind": "event", "topic": "booking.deposit_paid" },
            "actions": [
                { "type": "set_booking_status", "status": "confirmed" },
                { "type": "generate_document", "template": "document.confirmation" }
            ]
        }),
    )
    .await;
    // A day after an invoice falls due, someone is asked to chase it.
    let chase = h
        .post(
            &c.owner,
            "/api/v1/rules",
            json!({
                "name": "Chase late invoices",
                "trigger": { "kind": "date_offset", "entity": "invoice", "field": "due_date",
                             "offset_minutes": 1440 },
                // Not yet paid (the overdue mark may land in the same minute).
                "conditions": { "field": "invoice.outstanding.minor", "op": "gt", "value": 0 },
                "actions": [{ "type": "create_task", "title": "Chase {{ invoice.reference }}" }]
            }),
        )
        .await
        .id("/id");
    // The rule has been watching for a while (dates before a rule exists are not chased).
    sqlx::query("UPDATE rules SET created_at = now() - interval '30 days', scanned_until = now() - interval '30 days' WHERE id = $1")
        .bind(chase)
        .execute(&h.app.owner)
        .await
        .unwrap();

    let gbp = booking(
        &h,
        &c,
        "London gala",
        json!({ "minor": 200_000, "currency": "GBP" }),
        60,
    )
    .await;
    let eur = booking(
        &h,
        &c,
        "Paris gala",
        json!({ "minor": 300_000, "currency": "EUR" }),
        61,
    )
    .await;
    h.drain().await;

    let mut deposits = Vec::new();
    for (booking, currency) in [(gbp, "GBP"), (eur, "EUR")] {
        // The deposit: 25% of the fee, plus the default 20% VAT, numbered on issue.
        let inv = h
            .post(
                &c.owner,
                &format!("/api/v1/bookings/{booking}/invoices"),
                json!({ "purpose": "deposit", "issue": true }),
            )
            .await
            .body;
        assert_eq!(inv["status"], "issued", "{inv}");
        assert_eq!(inv["total"]["currency"], currency);
        let net = if currency == "GBP" { 50_000 } else { 75_000 };
        assert_eq!(inv["subtotal"]["minor"], net);
        assert_eq!(inv["tax"]["minor"], net / 5);
        assert_eq!(inv["total"]["minor"], net * 6 / 5);
        assert_eq!(inv["base_total"]["currency"], "GBP");
        if currency == "EUR" {
            // Converted at the day's rate, which is kept with the invoice.
            assert_eq!(inv["fx_source"], "ecb", "{inv}");
            assert_eq!(inv["base_total"]["minor"], 76_500);
        } else {
            assert_eq!(inv["base_total"]["minor"], 60_000);
        }
        let id = Uuid::parse_str(inv["id"].as_str().unwrap()).unwrap();
        deposits.push((
            id,
            inv["reference"].as_str().unwrap().to_owned(),
            net * 6 / 5,
        ));

        // Sent to the booker with a link and the PDF attached.
        let sent = h
            .post(&c.owner, &format!("/api/v1/invoices/{id}/send"), json!({}))
            .await
            .body;
        let token = sent["link"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned();
        h.drain().await;
        let mail = h.mailer.sent().pop().unwrap();
        assert_eq!(mail.to, "bea@grand.test");
        assert!(
            mail.subject.contains(inv["reference"].as_str().unwrap()),
            "{}",
            mail.subject
        );
        assert_eq!(mail.attachments.len(), 1);
        assert_eq!(mail.attachments[0].content_type, "application/pdf");
        assert!(mail.attachments[0].bytes.starts_with(b"%PDF-"));

        // The booker can open it without signing in.
        let public = h
            .app
            .get(&format!("/api/v1/public/invoices/{token}"))
            .send()
            .await
            .ok()
            .body;
        assert_eq!(public["reference"], inv["reference"], "{public}");
    }
    // One sequence across currencies: INV-0001, INV-0002 (format permitting).
    assert_ne!(deposits[0].1, deposits[1].1);

    // The deposits are paid; each booking is confirmed and gets a confirmation.
    for ((invoice, _, amount), (booking, currency)) in
        deposits.iter().zip([(gbp, "GBP"), (eur, "EUR")])
    {
        h.post(
            &c.owner,
            "/api/v1/payments",
            json!({ "amount": { "minor": amount, "currency": currency }, "method": "bank_transfer",
                    "allocations": [{ "invoice_id": invoice }] }),
        )
        .await;
        h.drain().await;
        let inv = h
            .get(&c.owner, &format!("/api/v1/invoices/{invoice}"))
            .await;
        assert_eq!(inv["status"], "paid", "{inv}");
        let b = h
            .get(&c.owner, &format!("/api/v1/bookings/{booking}"))
            .await;
        assert_eq!(b["status"], "confirmed", "{b}");
        let docs = h
            .get(
                &c.owner,
                &format!("/api/v1/documents?related_entity=booking&related_id={booking}"),
            )
            .await;
        let has_confirmation = docs["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["kind"].as_str().unwrap() == "confirmation");
        assert!(has_confirmation, "{docs}");
    }
    let topics = h.topics().await;
    assert_eq!(
        topics
            .iter()
            .filter(|t| *t == "booking.deposit_paid")
            .count(),
        2
    );

    // The balances: the fee less the deposit, issued with terms already run out.
    let mut balances = Vec::new();
    for (booking, currency) in [(gbp, "GBP"), (eur, "EUR")] {
        let draft = h
            .post(
                &c.owner,
                &format!("/api/v1/bookings/{booking}/invoices"),
                json!({ "purpose": "balance" }),
            )
            .await
            .body;
        let id = Uuid::parse_str(draft["id"].as_str().unwrap()).unwrap();
        let net = if currency == "GBP" { 150_000 } else { 225_000 };
        assert_eq!(draft["subtotal"]["minor"], net, "{draft}");
        let issued = h
            .post(
                &c.owner,
                &format!("/api/v1/invoices/{id}/issue"),
                // No reference rate that long ago: the euro one is given by hand.
                json!({ "issue_date": today - chrono::Duration::days(40),
                        "due_date": today - chrono::Duration::days(10),
                        "fx_rate": (currency == "EUR").then_some("0.85") }),
            )
            .await
            .body;
        assert_eq!(issued["status"], "issued");
        if currency == "EUR" {
            assert_eq!(issued["fx_source"], "manual", "{issued}");
        }
        balances.push(id);
    }
    // A second balance invoice is refused.
    h.app
        .post(&format!("/api/v1/bookings/{gbp}/invoices"))
        .signed_in(&c.owner)
        .json(json!({ "purpose": "balance" }))
        .send()
        .await
        .expect(axum::http::StatusCode::CONFLICT, "already_invoiced");

    // The scheduler notices they are late, and the rule chases each one.
    tokio::time::sleep(Duration::from_millis(20)).await;
    h.tick().await;
    for id in &balances {
        let inv = h.get(&c.owner, &format!("/api/v1/invoices/{id}")).await;
        assert_eq!(inv["status"], "overdue", "{inv}");
    }
    let tasks = h.get(&c.owner, "/api/v1/tasks?status=all").await;
    let titles: Vec<String> = tasks["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        titles.iter().filter(|t| t.starts_with("Chase ")).count(),
        2,
        "{titles:?}"
    );
    // Chased once, however often the scheduler looks.
    h.tick().await;
    let tasks = h.get(&c.owner, "/api/v1/tasks?status=all").await;
    assert_eq!(tasks["items"].as_array().unwrap().len(), 2);

    // The report shows what is owed, in pounds.
    let report = h.get(&c.owner, "/api/v1/reports/finance").await;
    assert_eq!(report["base_currency"], "GBP");
    assert_eq!(
        report["outstanding"].as_array().unwrap().len(),
        2,
        "{report}"
    );
    // 180,000 + 270,000 × 0.85 = 409,500 owed, 1 to 30 days late.
    assert_eq!(report["ageing"]["days_1_30"]["minor"], 409_500, "{report}");
    let cash: i64 = report["cash"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["received_minor"].as_i64().unwrap())
        .sum();
    assert_eq!(cash, 60_000 + 90_000);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn signers_are_emailed_their_own_link(pool: PgPool) {
    let h = harness(pool).await;
    let c = cast(&h).await;
    let b = booking(
        &h,
        &c,
        "London gala",
        json!({ "minor": 200_000, "currency": "GBP" }),
        60,
    )
    .await;
    let doc = h
        .post(
            &c.owner,
            "/api/v1/documents/generate",
            json!({ "template_key": "document.contract", "related_entity": "booking", "related_id": b }),
        )
        .await
        .id("/id");
    h.drain().await;
    let before = h.mailer.sent().len();
    h.post(
        &c.owner,
        &format!("/api/v1/documents/{doc}/signatures"),
        json!({ "signers": [
            { "name": "Bea Booker", "email": "bea@grand.test" },
            { "name": "Tom Talent", "email": "tom@owls.test" }
        ] }),
    )
    .await;
    h.drain().await;
    let mut to: Vec<String> = h.mailer.sent()[before..]
        .iter()
        .map(|m| m.to.clone())
        .collect();
    to.sort();
    assert_eq!(to, ["bea@grand.test", "tom@owls.test"]);
    let mail = &h.mailer.sent()[before];
    assert!(mail.subject.starts_with("Please sign"), "{}", mail.subject);
    let html = mail.html.clone().unwrap();
    // The link is put in after the template renders, whole.
    assert!(
        html.contains("<a href=\"http://localhost:5173/sign/"),
        "{html}"
    );
    // The links are not kept in the stored copies of the emails.
    let stored: Vec<String> = sqlx::query_scalar("SELECT coalesce(body_html, '') FROM outbound_messages WHERE template_key = 'system.signature_request'")
        .fetch_all(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(stored.len(), 2);
    assert!(
        stored.iter().all(|s| !s.contains("sign&#x2f;")),
        "{stored:?}"
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn rates_are_not_fetched_when_turned_off(pool: PgPool) {
    let h = harness(pool).await;
    assert_eq!(h.worker.fetch_rates_once().await.unwrap(), 0);
}
