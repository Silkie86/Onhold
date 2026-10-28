//! Pitching: the workspace's own settings for working through a list of
//! people to sell to (regions, statuses, what it pitches, its actions and
//! answers), and the pitch log of each organisation.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{IdempotencyHeader, IfMatchHeader, WorkspaceHeader, with_etag};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::{IfMatch, WsCtx};
use crate::services::pitches::{self, NewPitch, PitchFilter, PitchPage, PitchPatch, PitchView};
use crate::services::pitching::{self, PitchingSettings, PitchingSettingsView};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_settings, update_settings))
        .routes(routes!(list_pitches))
        .routes(routes!(log_pitch))
        .routes(routes!(update_pitch, delete_pitch))
}

/// The workspace's regions, statuses and what it pitches (OnHold's
/// defaults until it saves its own), with every country to place in a
/// region.
#[utoipa::path(get, path = "/pitching/settings", tag = "pitching", params(WorkspaceHeader),
    responses((status = 200, body = PitchingSettingsView), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_settings(State(state): State<AppState>, WsCtx(ctx): WsCtx) -> ApiResult<Response> {
    let v = pitching::get(&state, &ctx).await?;
    Ok(with_etag(v.version, v))
}

/// Replace the settings. Each country may be in one region at most.
#[utoipa::path(put, path = "/pitching/settings", tag = "pitching", params(WorkspaceHeader, IfMatchHeader),
    request_body = PitchingSettings,
    responses((status = 200, body = PitchingSettingsView), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_settings(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    IfMatch(if_match): IfMatch,
    Json(b): Json<PitchingSettings>,
) -> ApiResult<Response> {
    let v = pitching::update(&state, &ctx, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

/// An organisation's pitch log, latest first.
#[utoipa::path(get, path = "/organisations/{id}/pitches", tag = "pitching",
    params(WorkspaceHeader, ("id" = Uuid, Path), PitchFilter),
    responses((status = 200, body = PitchPage), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_pitches(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Query(f): Query<PitchFilter>,
) -> ApiResult<Json<PitchPage>> {
    Ok(Json(pitches::list(&state, &ctx, id, &f).await?))
}

/// Log something done to win an organisation over, or an answer it gave.
/// It can be backdated.
#[utoipa::path(post, path = "/pitches", tag = "pitching", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewPitch,
    responses((status = 201, body = PitchView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn log_pitch(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewPitch>,
) -> ApiResult<(StatusCode, Json<PitchView>)> {
    Ok((
        StatusCode::CREATED,
        Json(pitches::create(&state, &ctx, b).await?),
    ))
}

/// Change an entry: who, what, how, when, or the note. Its organisation and
/// whether it is an action or an answer stay.
#[utoipa::path(patch, path = "/pitches/{id}", tag = "pitching",
    params(WorkspaceHeader, IfMatchHeader, ("id" = Uuid, Path)),
    request_body = PitchPatch,
    responses((status = 200, body = PitchView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_pitch(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<PitchPatch>,
) -> ApiResult<Response> {
    let v = pitches::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(delete, path = "/pitches/{id}", tag = "pitching",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_pitch(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    pitches::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
