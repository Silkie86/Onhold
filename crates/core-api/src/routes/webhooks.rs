//! Outbound webhooks: endpoints, secrets, the delivery log, replays and pings.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{
    IdempotencyHeader, IfMatchHeader, PageQuery, WorkspaceHeader, next_before, nullable, with_etag,
};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::{IfMatch, WsCtx};
use crate::services::webhooks as svc;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_webhooks, create_webhook))
        .routes(routes!(get_webhook, update_webhook, delete_webhook))
        .routes(routes!(rotate_secret))
        .routes(routes!(deliveries))
        .routes(routes!(replay))
        .routes(routes!(ping))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookView {
    pub id: Uuid,
    pub url: String,
    pub description: Option<String>,
    /// Topics or patterns such as `thing.*`; `*` for everything.
    pub events: Vec<String>,
    pub enabled: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<core_db::webhooks::Endpoint> for WebhookView {
    fn from(e: core_db::webhooks::Endpoint) -> Self {
        Self {
            id: e.id,
            url: e.url,
            description: e.description,
            events: e.events,
            enabled: e.enabled,
            version: e.version,
            created_at: e.created_at,
            updated_at: e.updated_at,
        }
    }
}

#[utoipa::path(get, path = "/webhooks", tag = "webhooks", params(WorkspaceHeader),
    responses((status = 200, body = Vec<WebhookView>), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_webhooks(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Vec<WebhookView>>> {
    Ok(Json(
        svc::list(&state, &ctx)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateWebhookRequest {
    /// `https://` (plain `http://` only for local development). Private and
    /// internal addresses are refused.
    pub url: String,
    pub description: Option<String>,
    pub events: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookWithSecret {
    pub webhook: WebhookView,
    /// Verifies the `OnHold-Signature` header. Shown only now; store it safely.
    pub secret: String,
}

/// Add an endpoint. Each delivery is a POST signed with
/// `OnHold-Signature: t=<unix time>,v1=<hex HMAC-SHA256 of "<t>.<body>">`.
#[utoipa::path(post, path = "/webhooks", tag = "webhooks", params(WorkspaceHeader, IdempotencyHeader),
    request_body = CreateWebhookRequest,
    responses((status = 201, body = WebhookWithSecret), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_webhook(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<CreateWebhookRequest>,
) -> ApiResult<(StatusCode, Json<WebhookWithSecret>)> {
    let (endpoint, secret) = svc::create(&state, &ctx, &b.url, b.description, &b.events).await?;
    Ok((
        StatusCode::CREATED,
        Json(WebhookWithSecret {
            webhook: endpoint.into(),
            secret,
        }),
    ))
}

#[utoipa::path(get, path = "/webhooks/{id}", tag = "webhooks", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = WebhookView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_webhook(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let e = svc::get(&state, &ctx, id).await?;
    Ok(with_etag(e.version, WebhookView::from(e)))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct WebhookPatchRequest {
    pub url: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub description: Option<Option<String>>,
    pub events: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

#[utoipa::path(patch, path = "/webhooks/{id}", tag = "webhooks",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = WebhookPatchRequest,
    responses((status = 200, body = WebhookView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_webhook(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<WebhookPatchRequest>,
) -> ApiResult<Response> {
    let patch = svc::Patch {
        url: b.url,
        description: b.description,
        events: b.events,
        enabled: b.enabled,
    };
    let e = svc::update(&state, &ctx, id, patch, if_match).await?;
    Ok(with_etag(e.version, WebhookView::from(e)))
}

#[utoipa::path(delete, path = "/webhooks/{id}", tag = "webhooks", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_webhook(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    svc::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SecretResponse {
    /// Shown only now. The old secret stops working immediately.
    pub secret: String,
}

/// Replace the signing secret.
#[utoipa::path(post, path = "/webhooks/{id}/rotate-secret", tag = "webhooks", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = SecretResponse), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn rotate_secret(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SecretResponse>> {
    Ok(Json(SecretResponse {
        secret: svc::rotate_secret(&state, &ctx, id).await?,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeliveryView {
    pub id: Uuid,
    pub event_id: Uuid,
    pub topic: Option<String>,
    pub job_id: Option<Uuid>,
    pub attempt: i32,
    pub succeeded: bool,
    /// The endpoint's HTTP status, when it answered.
    pub status_code: Option<i32>,
    pub error: Option<String>,
    /// The start of the endpoint's response body.
    pub response_excerpt: Option<String>,
    pub duration_ms: i32,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeliveryPage {
    pub deliveries: Vec<DeliveryView>,
    pub next_before: Option<Uuid>,
}

/// Every delivery attempt to this endpoint, newest first.
#[utoipa::path(get, path = "/webhooks/{id}/deliveries", tag = "webhooks", params(WorkspaceHeader, PageQuery, ("id" = Uuid, Path)),
    responses((status = 200, body = DeliveryPage), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn deliveries(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<DeliveryPage>> {
    let limit = q.limit();
    let rows = svc::deliveries(&state, &ctx, id, q.before, limit).await?;
    let next_before = next_before(&rows, limit, |d| d.id);
    Ok(Json(DeliveryPage {
        deliveries: rows
            .into_iter()
            .map(|d| DeliveryView {
                id: d.id,
                event_id: d.event_id,
                topic: d.topic,
                job_id: d.job_id,
                attempt: d.attempt,
                succeeded: d.succeeded,
                status_code: d.status_code,
                error: d.error,
                response_excerpt: d.response_excerpt,
                duration_ms: d.duration_ms,
                at: d.created_at,
            })
            .collect(),
        next_before,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ReplayRequest {
    pub event_id: Uuid,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct QueuedResponse {
    /// Follow it at `GET /jobs/{id}`.
    pub job_id: Uuid,
}

/// Send one event to this endpoint again.
#[utoipa::path(post, path = "/webhooks/{id}/replay", tag = "webhooks", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    request_body = ReplayRequest,
    responses((status = 202, body = QueuedResponse), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn replay(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<ReplayRequest>,
) -> ApiResult<(StatusCode, Json<QueuedResponse>)> {
    let job_id = svc::replay(&state, &ctx, id, b.event_id).await?;
    Ok((StatusCode::ACCEPTED, Json(QueuedResponse { job_id })))
}

/// Send a `webhook.ping` event to this endpoint only, to test it.
#[utoipa::path(post, path = "/webhooks/{id}/ping", tag = "webhooks", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 202), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn ping(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    svc::ping(&state, &ctx, id).await?;
    Ok(StatusCode::ACCEPTED)
}
