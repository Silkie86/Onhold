//! M2's configuration APIs through HTTP: custom fields, pipelines, the
//! `thing` test entity, templates and previews, rules and dry runs, the
//! event feed, webhooks and the job list. The worker side is tested in
//! `core-worker`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{Client, TestApp};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

/// A workspace with a `budget` money field, an `email` field and a pipeline
/// whose `won` stage needs the budget filled in.
async fn setup(app: &TestApp) -> Client {
    let (_, owner) = app.workspace_with_owner("acme").await;
    for body in [
        json!({ "entity": "thing", "key": "budget", "label": "Budget", "field_type": "money",
                "validation": { "currency": "GBP", "min": 0 } }),
        json!({ "entity": "thing", "key": "client_email", "label": "Client email", "field_type": "email" }),
        json!({ "entity": "thing", "key": "size", "label": "Size", "field_type": "select",
                "validation": { "options": [{ "value": "s", "label": "Small" }, { "value": "l", "label": "Large" }] },
                "default_value": "s" }),
    ] {
        app.post("/api/v1/custom-fields")
            .signed_in(&owner)
            .json(body)
            .send()
            .await
            .ok();
    }
    app.post("/api/v1/pipelines")
        .signed_in(&owner)
        .json(json!({
            "entity": "thing", "name": "Sales",
            "stages": [
                { "key": "lead", "name": "Lead", "allowed_next": ["quoted", "lost"] },
                { "key": "quoted", "name": "Quoted" },
                { "key": "won", "name": "Won", "outcome": "won", "required_fields": ["budget"] },
                { "key": "lost", "name": "Lost", "outcome": "lost" }
            ]
        }))
        .send()
        .await
        .ok();
    owner
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn custom_fields_are_validated_and_versioned(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let owner = setup(&app).await;

    app.post("/api/v1/custom-fields")
        .signed_in(&owner)
        .json(json!({ "entity": "thing", "key": "budget", "label": "Again", "field_type": "text" }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "field_key_taken");
    for bad in [
        json!({ "entity": "thing", "key": "Bad Key", "label": "x", "field_type": "text" }),
        json!({ "entity": "planet", "key": "ok", "label": "x", "field_type": "text" }),
        json!({ "entity": "thing", "key": "ok", "label": "x", "field_type": "select" }),
        json!({ "entity": "thing", "key": "ok", "label": "x", "field_type": "text", "validation": { "regex": "(" } }),
        json!({ "entity": "thing", "key": "ok", "label": "x", "field_type": "number", "validation": { "currency": "GBP" } }),
    ] {
        app.post("/api/v1/custom-fields")
            .signed_in(&owner)
            .json(bad.clone())
            .send()
            .await
            .expect(StatusCode::BAD_REQUEST, "invalid_input");
    }

    let list = app
        .get("/api/v1/custom-fields?entity=thing")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let keys: Vec<&str> = list
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["budget", "client_email", "size"]);
    let id = list.body[0]["id"].as_str().unwrap().to_owned();

    let field = app
        .get(&format!("/api/v1/custom-fields/{id}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(field.headers["etag"], "\"1\"");
    app.patch(&format!("/api/v1/custom-fields/{id}"))
        .signed_in(&owner)
        .header("if-match", "\"7\"")
        .json(json!({ "label": "Lost update" }))
        .send()
        .await
        .expect(StatusCode::PRECONDITION_FAILED, "version_conflict");
    let updated = app
        .patch(&format!("/api/v1/custom-fields/{id}"))
        .signed_in(&owner)
        .header("if-match", "\"1\"")
        .json(json!({ "label": "Budget (GBP)", "help_text": "Before VAT" }))
        .send()
        .await
        .ok();
    assert_eq!(updated.body["label"], "Budget (GBP)");
    assert_eq!(updated.body["field_type"], "money");
    assert_eq!(updated.body["version"], 2);

    app.delete(&format!("/api/v1/custom-fields/{id}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let list = app
        .get("/api/v1/custom-fields")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(list.body.as_array().unwrap().len(), 2);
    let all = app
        .get("/api/v1/custom-fields?include_archived=true")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(all.body.as_array().unwrap().len(), 3);
    assert_eq!(all.body[0]["archived"], true);
    // An archived key stays taken.
    app.post("/api/v1/custom-fields")
        .signed_in(&owner)
        .json(json!({ "entity": "thing", "key": "budget", "label": "Again", "field_type": "text" }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "field_key_taken");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn things_follow_their_fields_and_pipeline(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let owner = setup(&app).await;

    // Values are checked against the definitions, every problem at once.
    let res = app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "Bad", "custom": { "client_email": "nope", "size": "xl", "mystery": 1 } }))
        .send()
        .await;
    res.expect(StatusCode::BAD_REQUEST, "invalid_input");
    let fields: Vec<&str> = res.body["error"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["field"].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        ["custom.client_email", "custom.mystery", "custom.size"]
    );

    let thing = app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "Spring campaign", "custom": { "client_email": "Client@Example.com" } }))
        .send()
        .await
        .ok();
    assert_eq!(thing.status, StatusCode::CREATED);
    assert_eq!(
        thing.body["stage"], "lead",
        "the default pipeline's first stage"
    );
    assert_eq!(thing.body["custom"]["size"], "s", "defaults fill in");
    assert_eq!(thing.body["custom"]["client_email"], "Client@Example.com");
    let id = thing.body["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v1/things/{id}");

    // `lead` may only move to `quoted` or `lost`.
    app.patch(&path)
        .signed_in(&owner)
        .json(json!({ "stage": "won" }))
        .send()
        .await
        .expect(StatusCode::UNPROCESSABLE_ENTITY, "stage_move_refused");
    app.patch(&path)
        .signed_in(&owner)
        .json(json!({ "stage": "quoted" }))
        .send()
        .await
        .ok();
    // `won` needs a budget.
    let res = app
        .patch(&path)
        .signed_in(&owner)
        .json(json!({ "stage": "won" }))
        .send()
        .await;
    res.expect(StatusCode::UNPROCESSABLE_ENTITY, "stage_move_refused");
    assert!(
        res.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("budget")
    );
    app.patch(&path)
        .signed_in(&owner)
        .json(json!({ "custom": { "budget": { "minor": -1, "currency": "GBP" } } }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");
    let won = app
        .patch(&path)
        .signed_in(&owner)
        .json(json!({ "stage": "won", "custom": { "budget": { "minor": 1_500_000, "currency": "GBP" } } }))
        .send()
        .await
        .ok();
    assert_eq!(won.body["stage"], "won");
    assert_eq!(won.body["custom"]["budget"]["minor"], 1_500_000);
    assert_eq!(won.body["version"], 3);

    let list = app
        .get("/api/v1/things?stage=won")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(list.body["things"].as_array().unwrap().len(), 1);
    let list = app
        .get("/api/v1/things?q=autumn")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert!(list.body["things"].as_array().unwrap().is_empty());

    // A pipeline in use cannot be deleted.
    let pipelines = app
        .get("/api/v1/pipelines")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let pipeline = pipelines.body[0]["id"].as_str().unwrap();
    app.delete(&format!("/api/v1/pipelines/{pipeline}"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "pipeline_in_use");

    app.delete(&path).signed_in(&owner).send().await.ok();
    app.get(&path)
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");

    let topics: Vec<String> = sqlx::query_scalar(
        "SELECT topic FROM outbox WHERE topic LIKE 'thing.%' ORDER BY created_at, id",
    )
    .fetch_all(&app.owner)
    .await
    .unwrap();
    assert_eq!(
        topics,
        [
            "thing.created",
            "thing.updated",
            "thing.stage_changed",
            "thing.updated",
            "thing.stage_changed",
            "thing.deleted"
        ]
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn pipelines_keep_removed_stages_archived(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let owner = setup(&app).await;
    let list = app
        .get("/api/v1/pipelines?entity=thing")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let p = &list.body[0];
    assert_eq!(p["is_default"], true);
    let id = p["id"].as_str().unwrap();

    app.patch(&format!("/api/v1/pipelines/{id}"))
        .signed_in(&owner)
        .json(json!({ "stages": [{ "key": "lead", "name": "Lead", "allowed_next": ["nowhere"] }] }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");
    let updated = app
        .patch(&format!("/api/v1/pipelines/{id}"))
        .signed_in(&owner)
        .json(json!({ "name": "Deals", "stages": [{ "key": "lead", "name": "New lead" }, { "key": "won", "name": "Won", "outcome": "won" }] }))
        .send()
        .await
        .ok();
    assert_eq!(updated.body["name"], "Deals");
    let stages: Vec<(String, bool)> = updated.body["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["key"].as_str().unwrap().to_owned(),
                s["archived"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        stages,
        [
            ("lead".into(), false),
            ("won".into(), false),
            ("quoted".into(), true),
            ("lost".into(), true)
        ]
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn templates_are_checked_and_previewed(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let owner = setup(&app).await;

    let res = app
        .post("/api/v1/templates")
        .signed_in(&owner)
        .json(json!({ "kind": "email", "key": "quote.sent", "name": "Quote", "subject": "Hi", "body": "line one\n{% if %}" }))
        .send()
        .await;
    res.expect(StatusCode::BAD_REQUEST, "invalid_input");
    assert!(
        res.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("line 2"),
        "{}",
        res.body
    );
    app.post("/api/v1/templates")
        .signed_in(&owner)
        .json(
            json!({ "kind": "email", "key": "quote.sent", "name": "Quote", "body": "no subject" }),
        )
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");

    app.post("/api/v1/templates")
        .signed_in(&owner)
        .json(json!({ "kind": "partial", "key": "signature", "name": "Signature", "body": "<p>The {{ workspace.name }} team</p>" }))
        .send()
        .await
        .ok();
    let created = app
        .post("/api/v1/templates")
        .signed_in(&owner)
        .json(json!({
            "kind": "email", "key": "quote.sent", "name": "Quote sent",
            "subject": "Your quote for {{ thing.name }}",
            "body": "<p>Budget: {{ thing.custom.budget | money }}</p><p>{{ thing.custom.missing }}</p>{% include \"signature\" %}<script>{{ thing.name }}</script>"
        }))
        .send()
        .await
        .ok();
    let id = created.body["id"].as_str().unwrap().to_owned();
    assert_eq!(
        created.body["locale"], "en",
        "serves every English locale without its own"
    );

    let thing = app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "<b>Spring</b>", "custom": { "budget": { "minor": 1_250_050, "currency": "GBP" } } }))
        .send()
        .await
        .ok();
    let preview = app
        .post(&format!("/api/v1/templates/{id}/preview"))
        .signed_in(&owner)
        .json(json!({ "thing_id": thing.body["id"] }))
        .send()
        .await
        .ok();
    assert_eq!(preview.body["subject"], "Your quote for <b>Spring</b>");
    let body = preview.body["body"].as_str().unwrap();
    assert!(body.contains("£12,500.50"), "{body}");
    assert!(body.contains("The acme agency team"), "{body}");
    assert!(body.contains("&lt;b&gt;Spring"), "HTML is escaped: {body}");
    assert!(
        preview.body["text"]
            .as_str()
            .unwrap()
            .contains("£12,500.50")
    );
    assert_eq!(preview.body["warnings"], json!(["thing.custom.missing"]));

    // A draft, with sample data and an override.
    let draft = app
        .post("/api/v1/templates/preview")
        .signed_in(&owner)
        .json(json!({ "kind": "sms", "body": "Hi {{ thing.name }} at {{ workspace.name }}", "overrides": { "thing": { "name": "Override" } } }))
        .send()
        .await
        .ok();
    assert_eq!(draft.body["body"], "Hi Override at acme agency");

    let vars = app
        .get("/api/v1/templates/variables?entity=thing")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let paths: Vec<&str> = vars
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"thing.custom.budget"));
    assert!(paths.contains(&"workspace.name"));

    // In use by a rule, a template cannot be archived.
    let rule = app
        .post("/api/v1/rules")
        .signed_in(&owner)
        .json(json!({ "name": "Quote mail", "trigger": { "kind": "event", "topic": "thing.created" },
                      "actions": [{ "type": "send_email", "template": "quote.sent", "to": { "field": "thing.custom.client_email" } }] }))
        .send()
        .await
        .ok();
    app.delete(&format!("/api/v1/templates/{id}"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "template_in_use");
    app.delete(&format!(
        "/api/v1/rules/{}",
        rule.body["id"].as_str().unwrap()
    ))
    .signed_in(&owner)
    .send()
    .await
    .ok();
    app.delete(&format!("/api/v1/templates/{id}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn rules_are_checked_on_save_and_explain_themselves(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let owner = setup(&app).await;
    app.post("/api/v1/templates")
        .signed_in(&owner)
        .json(json!({ "kind": "email", "key": "big.deal", "name": "Big deal", "subject": "{{ thing.name }}", "body": "Big!" }))
        .send()
        .await
        .ok();

    for (bad, why) in [
        (
            json!({ "kind": "event", "topic": "thing.created" }),
            json!([{ "type": "send_email", "template": "nope", "to": { "address": "a@b.test" } }]),
        ),
        (
            json!({ "kind": "event", "topic": "Thing Created" }),
            json!([{ "type": "move_stage", "stage": "won" }]),
        ),
        (
            json!({ "kind": "schedule", "cron": "* * * * *" }),
            json!([{ "type": "send_email", "template": "big.deal", "to": { "address": "a@b.test" } }]),
        ),
        (
            json!({ "kind": "schedule", "cron": "0 9 * * 1" }),
            json!([{ "type": "set_field", "field": "name", "value": "x" }]),
        ),
        (
            json!({ "kind": "event", "topic": "thing.created" }),
            json!([{ "type": "set_field", "field": "custom.unknown", "value": 1 }]),
        ),
        (
            json!({ "kind": "date_offset", "entity": "thing", "field": "custom.budget", "offset_minutes": 0 }),
            json!([{ "type": "move_stage", "stage": "won" }]),
        ),
    ] {
        let res = app
            .post("/api/v1/rules")
            .signed_in(&owner)
            .json(json!({ "name": "Bad", "trigger": bad, "actions": why }))
            .send()
            .await;
        res.expect(StatusCode::BAD_REQUEST, "invalid_input");
    }

    let rule = app
        .post("/api/v1/rules")
        .signed_in(&owner)
        .json(json!({
            "name": "Big deals",
            "trigger": { "kind": "event", "topic": "thing.created" },
            "conditions": { "all": [{ "field": "thing.custom.budget", "op": "gte", "value": { "minor": 1_000_000, "currency": "GBP" } }] },
            "actions": [
                { "type": "send_email", "template": "big.deal", "to": { "role": "owner" } },
                { "type": "move_stage", "stage": "quoted" }
            ]
        }))
        .send()
        .await
        .ok();
    let id = rule.body["id"].as_str().unwrap().to_owned();
    assert_eq!(rule.body["enabled"], true);
    assert!(rule.body["next_run_at"].is_null());

    let small = app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(
            json!({ "name": "Small", "custom": { "budget": { "minor": 100, "currency": "GBP" } } }),
        )
        .send()
        .await
        .ok();
    let dry = app
        .post(&format!("/api/v1/rules/{id}/dry-run"))
        .signed_in(&owner)
        .json(json!({ "thing_id": small.body["id"] }))
        .send()
        .await
        .ok();
    assert_eq!(dry.body["matched"], false);
    assert_eq!(dry.body["checks"][0]["passed"], false);
    assert_eq!(dry.body["checks"][0]["actual"]["minor"], 100);

    let big = app
        .post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "Big", "custom": { "budget": { "minor": 2_000_000, "currency": "GBP" } } }))
        .send()
        .await
        .ok();
    let dry = app
        .post(&format!("/api/v1/rules/{id}/dry-run"))
        .signed_in(&owner)
        .json(json!({ "thing_id": big.body["id"] }))
        .send()
        .await
        .ok();
    assert_eq!(dry.body["matched"], true, "{}", dry.body);
    let kinds: Vec<&str> = dry.body["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["send_email", "move_stage"]);
    assert_eq!(
        dry.body["actions"][0]["detail"]["to"],
        json!(["owner@acme.test"])
    );
    // Nothing was done.
    let after = app
        .get(&format!(
            "/api/v1/things/{}",
            big.body["id"].as_str().unwrap()
        ))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(after.body["stage"], "lead");
    let log = app
        .get(&format!("/api/v1/rules/{id}/log"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert!(log.body["runs"].as_array().unwrap().is_empty());

    // A schedule rule gets its next run.
    let cron = app
        .post("/api/v1/rules")
        .signed_in(&owner)
        .json(json!({ "name": "Mondays", "trigger": { "kind": "schedule", "cron": "0 9 * * 1" },
                      "actions": [{ "type": "send_email", "template": "big.deal", "to": { "address": "ops@acme.test" } }] }))
        .send()
        .await
        .ok();
    assert!(cron.body["next_run_at"].is_string());

    // The kill switch.
    let state = app
        .get("/api/v1/automations")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(state.body["paused"], false);
    let state = app
        .put("/api/v1/automations")
        .signed_in(&owner)
        .json(json!({ "paused": true }))
        .send()
        .await
        .ok();
    assert_eq!(state.body["paused"], true);
    assert!(
        app.audit_actions(owner.workspace.unwrap())
            .await
            .contains(&"automations.paused".to_owned())
    );

    // Disabling and editing go through If-Match like everything else.
    let updated = app
        .patch(&format!("/api/v1/rules/{id}"))
        .signed_in(&owner)
        .header("if-match", "\"1\"")
        .json(json!({ "enabled": false }))
        .send()
        .await
        .ok();
    assert_eq!(updated.body["enabled"], false);
    assert_eq!(updated.body["version"], 2);
}

async fn events(app: &TestApp, owner: &Client, query: &str) -> common::Resp {
    app.get(&format!("/api/v1/events{query}"))
        .signed_in(owner)
        .send()
        .await
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_event_feed_is_ordered_filtered_and_expires(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let owner = setup(&app).await;
    for name in ["one", "two", "three"] {
        app.post("/api/v1/things")
            .signed_in(&owner)
            .json(json!({ "name": name }))
            .send()
            .await
            .ok();
    }
    // Nothing is in the feed until the worker publishes it.
    let page = events(&app, &owner, "").await.ok();
    assert!(page.body["events"].as_array().unwrap().is_empty());
    app.publish_all().await;

    let page = events(&app, &owner, "?topics=thing.*&limit=2").await.ok();
    let evs = page.body["events"].as_array().unwrap();
    assert_eq!(evs.len(), 2);
    assert_eq!(page.body["has_more"], true);
    assert!(evs[0]["seq"].as_i64() < evs[1]["seq"].as_i64());
    assert_eq!(evs[0]["topic"], "thing.created");
    assert_eq!(evs[0]["resource"], "thing");
    assert!(evs[0]["object_id"].is_string());
    // Events say what changed, never the record itself.
    assert!(evs[0].get("payload").is_none() && evs[0].get("after").is_none());
    assert!(!page.body.to_string().contains("\"one\""));

    let next = page.body["next_after"].as_i64().unwrap();
    let rest = events(&app, &owner, &format!("?topics=thing.*&after={next}"))
        .await
        .ok();
    assert_eq!(rest.body["events"].as_array().unwrap().len(), 1);
    assert_eq!(rest.body["has_more"], false);
    let latest = rest.body["latest"].as_i64().unwrap();
    assert!(latest >= rest.body["next_after"].as_i64().unwrap());

    events(&app, &owner, "?topics=Not%20A%20Topic")
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");

    // Once events are pruned, an old cursor must resync.
    sqlx::query("INSERT INTO outbox_pruned (singleton, through_seq) VALUES (true, $1)")
        .bind(next)
        .execute(&app.owner)
        .await
        .unwrap();
    events(&app, &owner, "?after=1")
        .await
        .expect(StatusCode::GONE, "cursor_expired");
    events(&app, &owner, &format!("?after={next}")).await.ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_live_stream_sends_events_with_their_position(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let owner = setup(&app).await;
    app.post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "streamed" }))
        .send()
        .await
        .ok();
    app.publish_all().await;
    let seq: i64 = sqlx::query_scalar("SELECT seq FROM outbox WHERE topic = 'thing.created'")
        .fetch_one(&app.owner)
        .await
        .unwrap();

    let req = Request::get("/api/v1/events/stream?topics=thing.created")
        .header("cookie", &owner.cookie)
        .header("x-workspace-id", owner.workspace.unwrap().to_string())
        .header("last-event-id", (seq - 1).to_string())
        .body(Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    let mut body = res.into_body();
    let mut text = String::new();
    while !text.contains("\n\n") {
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("an event arrives")
            .expect("stream open")
            .unwrap();
        if let Ok(data) = frame.into_data() {
            text.push_str(std::str::from_utf8(&data).unwrap());
        }
    }
    assert!(text.contains(&format!("id: {seq}\n")), "{text}");
    let data = text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
    let event: Value = serde_json::from_str(data).unwrap();
    assert_eq!(event["topic"], "thing.created");

    // A stale cursor is refused before the stream starts.
    sqlx::query("INSERT INTO outbox_pruned (singleton, through_seq) VALUES (true, $1)")
        .bind(seq)
        .execute(&app.owner)
        .await
        .unwrap();
    app.get("/api/v1/events/stream?after=1")
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::GONE, "cursor_expired");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn webhooks_show_their_secret_once(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let owner = setup(&app).await;

    let created = app
        .post("/api/v1/webhooks")
        .signed_in(&owner)
        .json(json!({ "url": "https://hooks.example/onhold", "events": ["thing.*", "thing.*", "pipeline.updated"] }))
        .send()
        .await
        .ok();
    let secret = created.body["secret"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("whsec_"));
    assert_eq!(
        created.body["webhook"]["events"],
        json!(["pipeline.updated", "thing.*"])
    );
    let id = created.body["webhook"]["id"].as_str().unwrap().to_owned();

    let got = app
        .get(&format!("/api/v1/webhooks/{id}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert!(!got.body.to_string().contains(&secret));
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT secret_enc FROM webhook_endpoints WHERE id = $1::uuid")
            .bind(&id)
            .fetch_one(&app.owner)
            .await
            .unwrap();
    assert!(
        !String::from_utf8_lossy(&stored).contains(&secret),
        "stored encrypted"
    );

    let rotated = app
        .post(&format!("/api/v1/webhooks/{id}/rotate-secret"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_ne!(rotated.body["secret"].as_str().unwrap(), secret);

    for bad in [
        json!({ "url": "ftp://hooks.example/", "events": ["*"] }),
        json!({ "url": "https://hooks.example/", "events": [] }),
        json!({ "url": "https://hooks.example/", "events": ["Thing!"] }),
    ] {
        app.post("/api/v1/webhooks")
            .signed_in(&owner)
            .json(bad)
            .send()
            .await
            .expect(StatusCode::BAD_REQUEST, "invalid_input");
    }

    // Ping makes an event aimed at this endpoint; replay queues a job.
    app.post(&format!("/api/v1/webhooks/{id}/ping"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    app.publish_all().await;
    let ping: uuid::Uuid = sqlx::query_scalar("SELECT id FROM outbox WHERE topic = 'webhook.ping'")
        .fetch_one(&app.owner)
        .await
        .unwrap();
    let replay = app
        .post(&format!("/api/v1/webhooks/{id}/replay"))
        .signed_in(&owner)
        .json(json!({ "event_id": ping }))
        .send()
        .await;
    assert_eq!(replay.status, StatusCode::ACCEPTED);
    let job = replay.body["job_id"].as_str().unwrap();
    let detail = app
        .get(&format!("/api/v1/jobs/{job}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(detail.body["job"]["kind"], "webhook.deliver");
    assert_eq!(detail.body["job"]["status"], "queued");
    app.post(&format!("/api/v1/jobs/{job}/retry"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "job_not_dead");
    let jobs = app
        .get("/api/v1/jobs?status=queued")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(jobs.body["jobs"].as_array().unwrap().len(), 1);
    let deliveries = app
        .get(&format!("/api/v1/webhooks/{id}/deliveries"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert!(deliveries.body["deliveries"].as_array().unwrap().is_empty());
    let messages = app
        .get("/api/v1/messages")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert!(messages.body["messages"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn webhooks_cannot_target_private_addresses(pool: PgPool) {
    let config = core_api::Config {
        webhooks_allow_private: false,
        ..core_api::Config::for_development()
    };
    let app = TestApp::with_config(pool, config).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    for url in [
        "http://127.0.0.1:8080/hook",
        "http://10.0.0.5/hook",
        "http://[::1]/hook",
        "http://169.254.169.254/latest/meta-data",
        "http://localhost/hook",
    ] {
        app.post("/api/v1/webhooks")
            .signed_in(&owner)
            .json(json!({ "url": url, "events": ["*"] }))
            .send()
            .await
            .expect(StatusCode::BAD_REQUEST, "invalid_input");
    }
}
