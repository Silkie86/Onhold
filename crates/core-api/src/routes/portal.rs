//! The talent portal: the free viewer a talent gets when their agency
//! invites them, and the agency's side of it (who has access, and what
//! talent may see and do).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::documents::file_response;
use super::{IdempotencyHeader, WorkspaceHeader};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::WsCtx;
use crate::services::availability::AvailabilityView;
use crate::services::portal::{
    self, AgencyMessageInput, AnswerInput, DatesQuery, MessagesQuery, PortalAccess,
    PortalAvailability, PortalCalendarLink, PortalConversation, PortalDate, PortalDateDetail,
    PortalDocument, PortalHome, PortalMessage, PortalMessageInput, PortalNewAvailability,
    PortalProfile, PortalProfileInput, PortalSettings, PortalSharing, PortalSharingInput,
    PortalStatement,
};
use crate::services::talent_calendars::CreatedConnectLink;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_portal_settings, update_portal_settings))
        .routes(routes!(
            get_portal_access,
            invite_to_portal,
            remove_portal_access
        ))
        .routes(routes!(portal_home))
        .routes(routes!(portal_dates))
        .routes(routes!(portal_date))
        .routes(routes!(answer_portal_date))
        .routes(routes!(portal_availability, add_portal_availability))
        .routes(routes!(remove_portal_availability))
        .routes(routes!(portal_calendar_link))
        .routes(routes!(portal_documents))
        .routes(routes!(download_portal_document))
        .routes(routes!(portal_money))
        .routes(routes!(portal_messages, send_portal_message))
        .routes(routes!(portal_profiles))
        .routes(routes!(update_portal_profile))
        .routes(routes!(write_to_talent))
        .routes(routes!(portal_sharing, update_portal_sharing))
}

// For the agency --------------------------------------------------------------------------------

/// What the workspace lets its talent see and do in the portal.
#[utoipa::path(get, path = "/workspace/portal", tag = "portal", params(WorkspaceHeader),
    responses((status = 200, body = PortalSettings), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_portal_settings(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<PortalSettings>> {
    Ok(Json(portal::get_settings(&state, &ctx).await?))
}

/// Change the portal's switches; every switch is replaced.
#[utoipa::path(put, path = "/workspace/portal", tag = "portal", params(WorkspaceHeader, IdempotencyHeader),
    request_body = PortalSettings,
    responses((status = 200, body = PortalSettings), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_portal_settings(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<PortalSettings>,
) -> ApiResult<Json<PortalSettings>> {
    Ok(Json(portal::update_settings(&state, &ctx, b).await?))
}

/// Who a talent's portal opens for, and any invitation still waiting.
#[utoipa::path(get, path = "/talent/{id}/portal", tag = "portal", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = PortalAccess), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_portal_access(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PortalAccess>> {
    Ok(Json(portal::access(&state, &ctx, id).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PortalInviteBody {
    pub email: String,
}

/// Invite the talent to their portal by email. A waiting invitation is replaced.
#[utoipa::path(post, path = "/talent/{id}/portal", tag = "portal",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = PortalInviteBody,
    responses((status = 200, body = PortalAccess), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn invite_to_portal(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<PortalInviteBody>,
) -> ApiResult<Json<PortalAccess>> {
    Ok(Json(portal::invite(&state, &ctx, id, &b.email).await?))
}

/// End the talent's portal access and cancel waiting invitations.
#[utoipa::path(delete, path = "/talent/{id}/portal", tag = "portal",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn remove_portal_access(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    portal::remove_access(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// For the talent --------------------------------------------------------------------------------

/// What the portal opens with: whose dates these are and what the agency shares.
#[utoipa::path(get, path = "/portal", tag = "portal", params(WorkspaceHeader),
    responses((status = 200, body = PortalHome), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn portal_home(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<PortalHome>> {
    Ok(Json(portal::home(&state, &ctx).await?))
}

/// The talent's upcoming dates (soonest first) or past ones (latest first).
#[utoipa::path(get, path = "/portal/dates", tag = "portal", params(WorkspaceHeader, DatesQuery),
    responses((status = 200, body = Vec<PortalDate>), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn portal_dates(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<DatesQuery>,
) -> ApiResult<Json<Vec<PortalDate>>> {
    Ok(Json(portal::dates(&state, &ctx, &q).await?))
}

/// A date with its times, travel and documents.
#[utoipa::path(get, path = "/portal/dates/{id}", tag = "portal", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = PortalDateDetail), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = [])))]
async fn portal_date(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PortalDateDetail>> {
    Ok(Json(portal::date(&state, &ctx, id).await?))
}

/// Confirm or decline a date. The agent gets a task saying so; the booking
/// itself stays the agency's to move.
#[utoipa::path(post, path = "/portal/dates/{id}/answer", tag = "portal",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = AnswerInput,
    responses((status = 200, body = PortalDateDetail), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = [])))]
async fn answer_portal_date(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<AnswerInput>,
) -> ApiResult<Json<PortalDateDetail>> {
    Ok(Json(portal::answer(&state, &ctx, id, b).await?))
}

/// The talent's availability, busy times from their calendars, and the calendars themselves.
#[utoipa::path(get, path = "/portal/availability", tag = "portal", params(WorkspaceHeader),
    responses((status = 200, body = PortalAvailability), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn portal_availability(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<PortalAvailability>> {
    Ok(Json(portal::availability(&state, &ctx).await?))
}

/// Mark a time unavailable or tentative.
#[utoipa::path(post, path = "/portal/availability", tag = "portal", params(WorkspaceHeader, IdempotencyHeader),
    request_body = PortalNewAvailability,
    responses((status = 201, body = AvailabilityView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn add_portal_availability(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<PortalNewAvailability>,
) -> ApiResult<(StatusCode, Json<AvailabilityView>)> {
    Ok((
        StatusCode::CREATED,
        Json(portal::add_availability(&state, &ctx, b).await?),
    ))
}

/// Remove a time the talent marked themselves.
#[utoipa::path(delete, path = "/portal/availability/{id}", tag = "portal",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = [])))]
async fn remove_portal_availability(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    portal::remove_availability(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A link to connect the talent's own Google, Microsoft or iCal calendar.
#[utoipa::path(post, path = "/portal/calendar-link", tag = "portal", params(WorkspaceHeader, IdempotencyHeader),
    request_body = PortalCalendarLink,
    responses((status = 201, body = CreatedConnectLink), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn portal_calendar_link(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<PortalCalendarLink>,
) -> ApiResult<(StatusCode, Json<CreatedConnectLink>)> {
    Ok((
        StatusCode::CREATED,
        Json(portal::calendar_link(&state, &ctx, b).await?),
    ))
}

/// Documents on the talent's dates (from three months back) and on their record.
#[utoipa::path(get, path = "/portal/documents", tag = "portal", params(WorkspaceHeader),
    responses((status = 200, body = Vec<PortalDocument>), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn portal_documents(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Vec<PortalDocument>>> {
    Ok(Json(portal::documents(&state, &ctx).await?))
}

/// A document's current file.
#[utoipa::path(get, path = "/portal/documents/{id}/download", tag = "portal",
    params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, description = "The file"), (status = 404, body = ErrorBody)),
    security(("session" = [])))]
async fn download_portal_document(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    Ok(file_response(portal::download(&state, &ctx, id).await?))
}

/// The talent's statements: what each date paid them, and what was paid out.
#[utoipa::path(get, path = "/portal/money", tag = "portal", params(WorkspaceHeader),
    responses((status = 200, body = Vec<PortalStatement>), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn portal_money(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Vec<PortalStatement>>> {
    Ok(Json(portal::money(&state, &ctx).await?))
}

/// The talent's conversation with the agency, latest messages last.
/// Opening it marks it read.
#[utoipa::path(get, path = "/portal/messages", tag = "portal", params(WorkspaceHeader, MessagesQuery),
    responses((status = 200, body = PortalConversation), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn portal_messages(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<MessagesQuery>,
) -> ApiResult<Json<PortalConversation>> {
    Ok(Json(portal::messages(&state, &ctx, &q).await?))
}

/// Write to the agency. The conversation shows in the agency's inbox.
#[utoipa::path(post, path = "/portal/messages", tag = "portal", params(WorkspaceHeader, IdempotencyHeader),
    request_body = PortalMessageInput,
    responses((status = 201, body = PortalMessage), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn send_portal_message(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<PortalMessageInput>,
) -> ApiResult<(StatusCode, Json<PortalMessage>)> {
    Ok((
        StatusCode::CREATED,
        Json(portal::send_message(&state, &ctx, b).await?),
    ))
}

/// The talent's own records: name, bio and where they are based.
#[utoipa::path(get, path = "/portal/profile", tag = "portal", params(WorkspaceHeader),
    responses((status = 200, body = Vec<PortalProfile>), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn portal_profiles(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Vec<PortalProfile>>> {
    Ok(Json(portal::profiles(&state, &ctx).await?))
}

/// Change their bio and where they are based.
#[utoipa::path(put, path = "/portal/profile/{id}", tag = "portal",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = PortalProfileInput,
    responses((status = 200, body = PortalProfile), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)),
    security(("session" = [])))]
async fn update_portal_profile(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<PortalProfileInput>,
) -> ApiResult<Json<PortalProfile>> {
    Ok(Json(portal::update_profile(&state, &ctx, id, b).await?))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WrittenToTalent {
    /// The conversation in the inbox.
    pub thread_id: Uuid,
}

/// Write to a talent through their portal; they get an email saying so.
/// Starts their conversation in the inbox if it has not begun.
#[utoipa::path(post, path = "/talent/{id}/portal/messages", tag = "portal",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = AgencyMessageInput,
    responses((status = 201, body = WrittenToTalent), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn write_to_talent(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<AgencyMessageInput>,
) -> ApiResult<(StatusCode, Json<WrittenToTalent>)> {
    let thread_id = portal::write_to_talent(&state, &ctx, id, b).await?;
    Ok((StatusCode::CREATED, Json(WrittenToTalent { thread_id })))
}

/// Whether the talent shares when they are booked with their other workspaces.
#[utoipa::path(get, path = "/portal/sharing", tag = "portal", params(WorkspaceHeader),
    responses((status = 200, body = PortalSharing), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn portal_sharing(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<PortalSharing>> {
    Ok(Json(portal::sharing(&state, &ctx).await?))
}

/// Share (or stop sharing) when they are booked, times only, with every
/// other workspace they are talent with.
#[utoipa::path(put, path = "/portal/sharing", tag = "portal", params(WorkspaceHeader, IdempotencyHeader),
    request_body = PortalSharingInput,
    responses((status = 200, body = PortalSharing), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn update_portal_sharing(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<PortalSharingInput>,
) -> ApiResult<Json<PortalSharing>> {
    Ok(Json(portal::set_sharing(&state, &ctx, b).await?))
}
