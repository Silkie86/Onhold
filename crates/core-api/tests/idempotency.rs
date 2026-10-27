//! Safe retries: the same write sent twice with one Idempotency-Key acts once.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;
use sqlx::PgPool;

async fn invitation_count(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM invitations")
        .fetch_one(&app.owner)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_retried_write_replays_the_first_response(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let before = invitation_count(&app).await;
    let send = || {
        app.post("/api/v1/invitations")
            .signed_in(&owner)
            .header("idempotency-key", "offline-op-1")
            .json(json!({ "email": "a@acme.test", "role": "agent" }))
            .send()
    };

    let first = send().await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert!(first.headers.get("idempotent-replayed").is_none());
    let second = send().await;
    assert_eq!(second.status, StatusCode::CREATED);
    assert_eq!(second.headers.get("idempotent-replayed").unwrap(), "true");
    assert_eq!(second.body, first.body);
    assert_eq!(invitation_count(&app).await, before + 1);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_key_cannot_be_reused_for_a_different_request(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    app.post("/api/v1/invitations")
        .signed_in(&owner)
        .header("idempotency-key", "k")
        .json(json!({ "email": "a@acme.test", "role": "agent" }))
        .send()
        .await
        .ok();
    let res = app
        .post("/api/v1/invitations")
        .signed_in(&owner)
        .header("idempotency-key", "k")
        .json(json!({ "email": "b@acme.test", "role": "agent" }))
        .send()
        .await;
    res.expect(StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn failures_are_not_remembered(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let body = json!({ "email": "a@acme.test", "role": "agent" });

    // The first attempt fails (no CSRF token); the identical retry must run for real.
    let res = app
        .post("/api/v1/invitations")
        .header("cookie", &owner.cookie)
        .workspace(ws)
        .header("idempotency-key", "k")
        .json(body.clone())
        .send()
        .await;
    res.expect(StatusCode::FORBIDDEN, "csrf_failed");
    let res = app
        .post("/api/v1/invitations")
        .signed_in(&owner)
        .header("idempotency-key", "k")
        .json(body)
        .send()
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    assert!(res.headers.get("idempotent-replayed").is_none());
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn keys_are_private_to_each_credential(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, alpha) = app.workspace_with_owner("alpha").await;
    let (_, bravo) = app.workspace_with_owner("bravo").await;
    let body = json!({ "email": "same@example.test", "role": "agent" });
    let a = app
        .post("/api/v1/invitations")
        .signed_in(&alpha)
        .header("idempotency-key", "shared")
        .json(body.clone())
        .send()
        .await;
    let b = app
        .post("/api/v1/invitations")
        .signed_in(&bravo)
        .header("idempotency-key", "shared")
        .json(body)
        .send()
        .await;
    assert_eq!(
        (a.status, b.status),
        (StatusCode::CREATED, StatusCode::CREATED)
    );
    assert!(b.headers.get("idempotent-replayed").is_none());
    assert_ne!(a.body["id"], b.body["id"]);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn malformed_keys_are_rejected(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let long = "k".repeat(256);
    let res = app
        .post("/api/v1/invitations")
        .signed_in(&owner)
        .header("idempotency-key", &long)
        .json(json!({ "email": "a@acme.test", "role": "agent" }))
        .send()
        .await;
    res.expect(StatusCode::BAD_REQUEST, "invalid_idempotency_key");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn stored_responses_are_invisible_to_the_app(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    app.post("/api/v1/invitations")
        .signed_in(&owner)
        .header("idempotency-key", "k")
        .json(json!({ "email": "a@acme.test", "role": "agent" }))
        .send()
        .await
        .ok();
    let read = sqlx::query("SELECT * FROM idempotency_keys")
        .fetch_all(&app.state.db)
        .await;
    assert!(read.unwrap_err().to_string().contains("permission denied"));
}
