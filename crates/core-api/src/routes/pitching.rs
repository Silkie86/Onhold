//! Pitching: the workspace's own settings for working through a list of
//! people to sell to (regions, statuses, what it pitches).

use axum::Json;
use axum::extract::State;
use axum::response::Response;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::{IfMatchHeader, WorkspaceHeader, with_etag};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::{IfMatch, WsCtx};
use crate::services::pitching::{self, PitchingSettings, PitchingSettingsView};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_settings, update_settings))
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
