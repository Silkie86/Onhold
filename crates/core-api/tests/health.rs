mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::TestApp;
use core_api::{AppState, Config, router};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// A pool pointing at nothing, for proving the probes behave when the database is down.
fn unreachable_state() -> AppState {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgres://nobody:nothing@127.0.0.1:1/none")
        .expect("lazy pool");
    AppState::new(pool, Config::for_development()).expect("state")
}

async fn get(
    app: axum::Router,
    path: &str,
    request_id: Option<&str>,
) -> (StatusCode, serde_json::Value, axum::http::HeaderMap) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let mut req = axum::http::Request::get(path);
    if let Some(id) = request_id {
        req = req.header("x-request-id", id);
    }
    let res = app
        .oneshot(req.body(axum::body::Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or_default(),
        headers,
    )
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn healthz_is_ok(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let res = app.get("/healthz").send().await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["status"], "ok");
}

#[tokio::test]
async fn healthz_stays_ok_when_the_database_is_down() {
    let (status, _, _) = get(router(unreachable_state()), "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn readyz_is_ready_with_a_database(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let res = app.get("/readyz").send().await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["status"], "ready");
}

#[tokio::test]
async fn readyz_is_unavailable_without_a_database() {
    let (status, body, _) = get(router(unreachable_state()), "/readyz", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "unavailable");
}

#[tokio::test]
async fn every_response_carries_a_request_id() {
    let (_, _, headers) = get(router(unreachable_state()), "/healthz", None).await;
    assert!(headers.contains_key("x-request-id"));
}

#[tokio::test]
async fn a_caller_supplied_request_id_is_echoed() {
    let (_, _, headers) = get(
        router(unreachable_state()),
        "/healthz",
        Some("trace-me-123"),
    )
    .await;
    assert_eq!(headers["x-request-id"], "trace-me-123");
}

#[tokio::test]
async fn unknown_routes_are_404() {
    let (status, _, _) = get(router(unreachable_state()), "/api/v1/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_worker_probes_need_only_a_pool() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://nobody:nothing@127.0.0.1:1/none")
        .expect("lazy pool");
    let (status, _, _) = get(core_api::health::standalone(pool), "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
}
