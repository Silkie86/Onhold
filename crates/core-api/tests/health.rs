use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use core_api::{AppState, router};
use http_body_util::BodyExt;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

async fn get(
    app: axum::Router,
    path: &str,
) -> (StatusCode, serde_json::Value, axum::http::HeaderMap) {
    let res = app
        .oneshot(Request::get(path).body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).expect("json"),
        headers,
    )
}

/// A pool pointing at nothing, for proving the probes behave when the database is down.
fn unreachable_pool() -> PgPool {
    PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgres://nobody:nothing@127.0.0.1:1/none")
        .expect("lazy pool")
}

#[sqlx::test]
async fn healthz_is_ok(pool: PgPool) {
    let (status, body, _) = get(router(AppState { db: pool }), "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn healthz_stays_ok_when_the_database_is_down() {
    let (status, _, _) = get(
        router(AppState {
            db: unreachable_pool(),
        }),
        "/healthz",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test]
async fn readyz_is_ready_with_a_database(pool: PgPool) {
    let (status, body, _) = get(router(AppState { db: pool }), "/readyz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ready");
}

#[tokio::test]
async fn readyz_is_unavailable_without_a_database() {
    let (status, body, _) = get(
        router(AppState {
            db: unreachable_pool(),
        }),
        "/readyz",
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "unavailable");
}

#[tokio::test]
async fn every_response_carries_a_request_id() {
    let (_, _, headers) = get(
        router(AppState {
            db: unreachable_pool(),
        }),
        "/healthz",
    )
    .await;
    assert!(headers.contains_key("x-request-id"));
}

#[tokio::test]
async fn a_caller_supplied_request_id_is_echoed() {
    let res = router(AppState {
        db: unreachable_pool(),
    })
    .oneshot(
        Request::get("/healthz")
            .header("x-request-id", "trace-me-123")
            .body(Body::empty())
            .expect("request"),
    )
    .await
    .expect("response");
    assert_eq!(res.headers()["x-request-id"], "trace-me-123");
}

#[tokio::test]
async fn unknown_routes_are_404() {
    let res = router(AppState {
        db: unreachable_pool(),
    })
    .oneshot(
        Request::get("/api/v1/nope")
            .body(Body::empty())
            .expect("request"),
    )
    .await
    .expect("response");
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
