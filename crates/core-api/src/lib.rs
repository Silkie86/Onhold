//! The HTTP API: routing, middleware and (from M1) auth, OpenAPI and SSE.

pub mod health;

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderName, Request};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower::ServiceBuilder;
use tower_http::LatencyUnit;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};

const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Largest request body the API accepts. Uploads get their own, larger limit
/// on their own routes when documents arrive in M5.
const MAX_BODY_BYTES: usize = 1024 * 1024;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
}

/// The full API router with its middleware stack.
pub fn router(state: AppState) -> Router {
    let api_v1 = Router::new();

    Router::new()
        .merge(health::router())
        .nest("/api/v1", api_v1)
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::new(REQUEST_ID_HEADER, MakeRequestUuid))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(|req: &Request<_>| {
                            let request_id = req
                                .headers()
                                .get(REQUEST_ID_HEADER)
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or_default();
                            tracing::info_span!(
                                "request",
                                method = %req.method(),
                                path = %req.uri().path(),
                                request_id,
                            )
                        })
                        .on_response(DefaultOnResponse::new().latency_unit(LatencyUnit::Millis)),
                )
                .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER))
                .layer(TimeoutLayer::with_status_code(
                    axum::http::StatusCode::REQUEST_TIMEOUT,
                    REQUEST_TIMEOUT,
                ))
                .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES)),
        )
}

/// Serves `app` on `addr` until `shutdown` is cancelled, then drains in-flight requests.
pub async fn serve(
    addr: SocketAddr,
    app: Router,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await
}
