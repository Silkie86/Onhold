//! Things: the stand-in entity M2's engines are tested with. Real modules
//! (contacts, quotes, jobs) follow the same shape.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use core_domain::UserId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{IdempotencyHeader, IfMatchHeader, WorkspaceHeader, next_before, nullable, with_etag};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::{IfMatch, WsCtx};
use crate::services::things as svc;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_things, create_thing))
        .routes(routes!(get_thing, update_thing, delete_thing))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ThingView {
    pub id: Uuid,
    pub name: String,
    pub pipeline_id: Option<Uuid>,
    /// The current stage's key.
    pub stage: Option<String>,
    pub stage_entered_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    /// Custom field values by key.
    #[schema(value_type = Object)]
    pub custom: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

impl From<svc::View> for ThingView {
    fn from(v: svc::View) -> Self {
        let t = v.thing;
        Self {
            id: t.id,
            name: t.name,
            pipeline_id: t.pipeline_id,
            stage: v.stage,
            stage_entered_at: t.stage_entered_at,
            due_at: t.due_at,
            custom: t.custom,
            version: t.version,
            created_at: t.created_at,
            updated_at: t.updated_at,
            created_by: t.created_by,
            updated_by: t.updated_by,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ThingListQuery {
    pub pipeline_id: Option<Uuid>,
    /// A stage key (in `pipeline_id`, or the default pipeline).
    pub stage: Option<String>,
    /// Names containing this text.
    pub q: Option<String>,
    pub before: Option<Uuid>,
    /// 1 to 200, default 50.
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ThingPage {
    pub things: Vec<ThingView>,
    /// Pass as `before` for the next page; absent on the last page.
    pub next_before: Option<Uuid>,
}

/// Things, newest first.
#[utoipa::path(get, path = "/things", tag = "things", params(WorkspaceHeader, ThingListQuery),
    responses((status = 200, body = ThingPage), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_things(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<ThingListQuery>,
) -> ApiResult<Json<ThingPage>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let rows = svc::list(
        &state,
        &ctx,
        q.pipeline_id,
        q.stage.as_deref(),
        q.q.as_deref(),
        q.before,
        limit,
    )
    .await?;
    let next_before = next_before(&rows, limit, |v| v.thing.id);
    Ok(Json(ThingPage {
        things: rows.into_iter().map(Into::into).collect(),
        next_before,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateThingRequest {
    pub name: String,
    pub due_at: Option<DateTime<Utc>>,
    /// Defaults to the default pipeline.
    pub pipeline_id: Option<Uuid>,
    /// Defaults to the pipeline's first stage.
    pub stage: Option<String>,
    /// Custom field values by key. Missing fields take their defaults.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub custom: Map<String, Value>,
}

/// Create a thing. Custom values are validated against the field definitions.
#[utoipa::path(post, path = "/things", tag = "things", params(WorkspaceHeader, IdempotencyHeader),
    request_body = CreateThingRequest,
    responses((status = 201, body = ThingView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 422, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_thing(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<CreateThingRequest>,
) -> ApiResult<(StatusCode, Json<ThingView>)> {
    let v = svc::create(
        &state,
        &ctx,
        svc::New {
            name: b.name,
            due_at: b.due_at,
            pipeline_id: b.pipeline_id,
            stage: b.stage,
            custom: b.custom,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(v.into())))
}

#[utoipa::path(get, path = "/things/{id}", tag = "things", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = ThingView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_thing(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = svc::get(&state, &ctx, id).await?;
    Ok(with_etag(v.thing.version, ThingView::from(v)))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct ThingPatchRequest {
    pub name: Option<String>,
    /// `null` clears it.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<DateTime<Utc>>)]
    pub due_at: Option<Option<DateTime<Utc>>>,
    /// Move to this stage key. Refused (422) if the pipeline does not allow
    /// the move or the stage's required fields are empty.
    pub stage: Option<String>,
    /// Only the keys sent change; `null` clears a value.
    #[schema(value_type = Option<Object>)]
    pub custom: Option<Map<String, Value>>,
}

#[utoipa::path(patch, path = "/things/{id}", tag = "things",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = ThingPatchRequest,
    responses((status = 200, body = ThingView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 412, body = ErrorBody), (status = 422, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_thing(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<ThingPatchRequest>,
) -> ApiResult<Response> {
    let patch = svc::Patch {
        name: b.name,
        due_at: b.due_at,
        stage: b.stage,
        custom: b.custom,
    };
    let v = svc::update(&state, &ctx, id, patch, if_match).await?;
    Ok(with_etag(v.thing.version, ThingView::from(v)))
}

#[utoipa::path(delete, path = "/things/{id}", tag = "things", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_thing(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    svc::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
