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

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_write_replayed_after_signing_in_again_still_acts_once(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let before = invitation_count(&app).await;
    let send = |who: &common::Client| {
        app.post("/api/v1/invitations")
            .signed_in(who)
            .header("idempotency-key", "queued-op")
            .json(json!({ "email": "a@acme.test", "role": "agent" }))
            .send()
    };
    let first = send(&owner).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);

    // The session ends and the person signs in again: a new token, same person.
    let again = app.login("owner@acme.test", common::PASSWORD).await;
    let again = common::Client::from_login(&again).in_workspace(ws);
    assert_ne!(again.cookie, owner.cookie);
    let second = send(&again).await;
    assert_eq!(second.status, StatusCode::CREATED);
    assert_eq!(second.headers.get("idempotent-replayed").unwrap(), "true");
    assert_eq!(invitation_count(&app).await, before + 1);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_session_waiting_for_its_second_factor_gets_no_replay(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let setup = app
        .post("/api/v1/auth/totp/setup")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let secret = setup.body["secret"].as_str().unwrap().to_owned();
    app.post("/api/v1/auth/totp/enable")
        .signed_in(&owner)
        .json(json!({ "code": common::totp_code(&secret, -1) }))
        .send()
        .await
        .ok();
    let send = |who: &common::Client| {
        app.post("/api/v1/invitations")
            .signed_in(who)
            .header("idempotency-key", "queued-op")
            .json(json!({ "email": "a@acme.test", "role": "agent" }))
            .send()
    };
    let first = send(&owner).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);

    // The password alone, with no second factor: the same key and request do
    // not hand back the invitation the verified session made.
    let pending = app.login("owner@acme.test", common::PASSWORD).await.ok();
    assert_eq!(pending.body["mfa_required"], true);
    let pending = common::Client::from_login(&pending).in_workspace(ws);
    let second = send(&pending).await;
    second.expect(StatusCode::UNAUTHORIZED, "mfa_required");
    assert!(second.headers.get("idempotent-replayed").is_none());
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_request_cut_off_midway_releases_its_key(pool: PgPool) {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    // A handler that never finishes, behind the real idempotency layer.
    let router = axum::Router::new()
        .route(
            "/hang",
            axum::routing::post(std::future::pending::<StatusCode>),
        )
        .layer(axum::middleware::from_fn_with_state(
            app.state.clone(),
            core_api::idempotency::layer,
        ))
        .with_state(app.state.clone());
    let request = Request::post("/hang")
        .header("cookie", &owner.cookie)
        .header("x-workspace-id", ws.to_string())
        .header("idempotency-key", "cut-off")
        .body(Body::empty())
        .unwrap();
    let keys = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM idempotency_keys WHERE key = 'cut-off'")
            .fetch_one(&app.owner)
            .await
            .unwrap()
    };

    let running = tokio::spawn(router.oneshot(request));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while keys().await == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the key was never claimed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // The timeout fires, or the client goes away: the request is dropped.
    running.abort();
    while keys().await != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the key stayed claimed after the request was dropped"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_abandoned_claim_can_be_claimed_again(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (scope, key, hash) = (b"scope".as_slice(), "k", b"request".as_slice());
    let claim = || core_db::idempotency::claim(&app.state.db, scope, key, hash);
    assert!(claim().await.unwrap().is_none(), "the first claim is ours");
    let running = claim().await.unwrap().expect("still running");
    assert!(running.status.is_none());

    // Its request was cut off without releasing it, long enough ago.
    sqlx::query("UPDATE idempotency_keys SET created_at = now() - interval '5 minutes'")
        .execute(&app.owner)
        .await
        .unwrap();
    assert!(claim().await.unwrap().is_none(), "the retry runs");
}
