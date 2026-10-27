//! Liveness and readiness probes, served by both the API and the worker.

use std::time::Duration;

use axum::extract::{FromRef, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;

const READY_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthBody {
    pub status: &'static str,
}

impl FromRef<AppState> for PgPool {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}

/// The probes on any router whose state can provide the pool.
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    PgPool: FromRef<S>,
{
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
}

/// The probes as documented routes, for the OpenAPI document.
pub fn openapi_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(healthz))
        .routes(routes!(readyz))
}

/// A standalone probe router for processes with no other HTTP surface (the worker).
pub fn standalone(db: PgPool) -> Router {
    router().with_state(db)
}

/// Liveness: the process is up and serving. Never touches dependencies, so a
/// database outage does not get every container restarted.
#[utoipa::path(get, path = "/healthz", tag = "health", responses((status = 200, body = HealthBody)))]
async fn healthz() -> Json<HealthBody> {
    Json(HealthBody { status: "ok" })
}

/// Readiness: the process can do useful work, which today means the database answers.
#[utoipa::path(get, path = "/readyz", tag = "health",
    responses((status = 200, body = HealthBody), (status = 503, body = HealthBody)))]
async fn readyz(State(db): State<PgPool>) -> (StatusCode, Json<HealthBody>) {
    match tokio::time::timeout(READY_CHECK_TIMEOUT, core_db::ping(&db)).await {
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
