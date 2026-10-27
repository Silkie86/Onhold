//! Liveness and readiness probes, served by both the API and the worker.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use sqlx::PgPool;

use crate::AppState;

const READY_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize)]
pub struct HealthBody {
    pub status: &'static str,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
}

/// A standalone probe router for processes with no other HTTP surface (the worker).
pub fn standalone(db: PgPool) -> Router {
    router().with_state(AppState { db })
}

/// Liveness: the process is up and serving. Never touches dependencies, so a
/// database outage does not get every container restarted.
async fn healthz() -> Json<HealthBody> {
    Json(HealthBody { status: "ok" })
}

/// Readiness: the process can do useful work, which today means the database answers.
async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<HealthBody>) {
    match tokio::time::timeout(READY_CHECK_TIMEOUT, core_db::ping(&state.db)).await {
        Ok(Ok(())) => (StatusCode::OK, Json(HealthBody { status: "ready" })),
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthBody {
                    status: "unavailable",
                }),
            )
        }
        Err(_) => {
            tracing::warn!("readiness check timed out");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthBody {
                    status: "unavailable",
                }),
            )
        }
    }
}
