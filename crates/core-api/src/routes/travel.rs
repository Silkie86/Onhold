//! Travel on a booking (flights, trains, ground transport, hotels), its
//! income and cost lines, and the money check that says whether it pays.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{IdempotencyHeader, IfMatchHeader, WorkspaceHeader, with_etag};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::{IfMatch, WsCtx};
use crate::services::travel::{
    self, MoneyCheck, MoneyLineInput, MoneyLineView, TravelInput, TravelView,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_travel, create_travel))
        .routes(routes!(update_travel, delete_travel))
        .routes(routes!(money_check, create_line))
        .routes(routes!(update_line, delete_line))
}

/// A booking's travel, in time order.
#[utoipa::path(get, path = "/bookings/{id}/travel", tag = "travel", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = Vec<TravelView>), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_travel(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<TravelView>>> {
    Ok(Json(travel::list_travel(&state, &ctx, id).await?))
}

/// Add a flight, train, ride or hotel to a booking.
#[utoipa::path(post, path = "/bookings/{id}/travel", tag = "travel",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = TravelInput,
    responses((status = 201, body = TravelView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_travel(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<TravelInput>,
) -> ApiResult<(StatusCode, Json<TravelView>)> {
    Ok((
        StatusCode::CREATED,
        Json(travel::create_travel(&state, &ctx, id, b).await?),
    ))
}

/// Change a travel item; every field is replaced.
#[utoipa::path(put, path = "/travel/{id}", tag = "travel",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = TravelInput,
    responses((status = 200, body = TravelView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_travel(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<TravelInput>,
) -> ApiResult<Response> {
    let v = travel::update_travel(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(delete, path = "/travel/{id}", tag = "travel", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_travel(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    travel::delete_travel(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Whether a booking pays: the fee and other income, less commission,
/// travel and other costs, in the fee's currency, with warnings.
#[utoipa::path(get, path = "/bookings/{id}/money", tag = "travel", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = MoneyCheck), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn money_check(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MoneyCheck>> {
    Ok(Json(travel::money_check(&state, &ctx, id).await?))
}

/// Add income (a buyout) or a cost to a booking.
#[utoipa::path(post, path = "/bookings/{id}/money", tag = "travel",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = MoneyLineInput,
    responses((status = 201, body = MoneyLineView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_line(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<MoneyLineInput>,
) -> ApiResult<(StatusCode, Json<MoneyLineView>)> {
    Ok((
        StatusCode::CREATED,
        Json(travel::create_line(&state, &ctx, id, b).await?),
    ))
}

/// Change an income or cost line; every field is replaced.
#[utoipa::path(put, path = "/money-lines/{id}", tag = "travel",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = MoneyLineInput,
    responses((status = 200, body = MoneyLineView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_line(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<MoneyLineInput>,
) -> ApiResult<Response> {
    let v = travel::update_line(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(delete, path = "/money-lines/{id}", tag = "travel", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_line(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    travel::delete_line(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
