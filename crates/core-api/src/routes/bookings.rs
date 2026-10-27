//! Bookings and calendar: enquiries, holds and bookings with clash checks,
//! availability, quotes (and the buyer's page), tasks, the calendar and its
//! iCal feeds.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{IdempotencyHeader, IfMatchHeader, PageQuery, WorkspaceHeader, with_etag};
use crate::AppState;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::extract::{ClientMeta, IfMatch, WsCtx};
use crate::services::availability::{
    self, AvailabilityPatch, AvailabilityQuery, AvailabilityView, NewAvailability,
};
use crate::services::bookings::{
    self, BookingFilter, BookingPage, BookingPatch, BookingView, CalendarQuery, CalendarView,
    ClashQuery, ClashReport, NewBooking, StatusRequest,
};
use crate::services::calendar_feeds::{self, CreatedFeed, FeedView, NewFeed};
use crate::services::crm::ListQuery;
use crate::services::enquiries::{
    self, EnquiryFilter, EnquiryPage, EnquiryPatch, EnquiryView, NewEnquiry,
};
use crate::services::quotes::{
    self, AcceptQuote, DeclineQuote, NewQuote, PublicQuote, QuoteFilter, QuotePage, QuotePatch,
    QuoteView, SendQuote, SentQuote,
};
use crate::services::search::{self as search_svc, TimelinePage};
use crate::services::tasks::{self, NewTask, TaskFilter, TaskPage, TaskPatch, TaskView};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_enquiries, create_enquiry))
        .routes(routes!(get_enquiry, update_enquiry, delete_enquiry))
        .routes(routes!(enquiry_timeline))
        .routes(routes!(list_bookings, create_booking))
        .routes(routes!(clash_check))
        .routes(routes!(get_booking, update_booking, delete_booking))
        .routes(routes!(change_booking_status))
        .routes(routes!(booking_timeline))
        .routes(routes!(list_booking_quotes, create_quote))
        .routes(routes!(calendar))
        .routes(routes!(list_feeds, create_feed))
        .routes(routes!(revoke_feed))
        .routes(routes!(list_availability, create_availability))
        .routes(routes!(update_availability, delete_availability))
        .routes(routes!(list_quotes))
        .routes(routes!(get_quote, update_quote, delete_quote))
        .routes(routes!(send_quote))
        .routes(routes!(revise_quote))
        .routes(routes!(withdraw_quote))
        .routes(routes!(list_tasks, create_task))
        .routes(routes!(get_task, update_task, delete_task))
        .routes(routes!(public_quote))
        .routes(routes!(accept_quote))
        .routes(routes!(decline_quote))
        .routes(routes!(public_calendar))
}

// Enquiries ------------------------------------------------------------------------------

/// Enquiries, newest first unless sorted otherwise.
#[utoipa::path(get, path = "/enquiries", tag = "enquiries", params(WorkspaceHeader, ListQuery, EnquiryFilter),
    responses((status = 200, body = EnquiryPage), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_enquiries(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<ListQuery>,
    Query(f): Query<EnquiryFilter>,
) -> ApiResult<Json<EnquiryPage>> {
    Ok(Json(enquiries::list(&state, &ctx, &q, &f).await?))
}

/// Log an enquiry. It starts in the first stage of the enquiry pipeline.
#[utoipa::path(post, path = "/enquiries", tag = "enquiries", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewEnquiry,
    responses((status = 201, body = EnquiryView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_enquiry(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewEnquiry>,
) -> ApiResult<(StatusCode, Json<EnquiryView>)> {
    Ok((
        StatusCode::CREATED,
        Json(enquiries::create(&state, &ctx, b).await?),
    ))
}

#[utoipa::path(get, path = "/enquiries/{id}", tag = "enquiries", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = EnquiryView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_enquiry(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = enquiries::get(&state, &ctx, id).await?;
    Ok(with_etag(v.version, v))
}

/// Change an enquiry, or move it to another stage.
#[utoipa::path(patch, path = "/enquiries/{id}", tag = "enquiries",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = EnquiryPatch,
    responses((status = 200, body = EnquiryView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 412, body = ErrorBody), (status = 422, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_enquiry(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<EnquiryPatch>,
) -> ApiResult<Response> {
    let v = enquiries::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(delete, path = "/enquiries/{id}", tag = "enquiries", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_enquiry(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    enquiries::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// What happened to this enquiry and its bookings, newest first.
#[utoipa::path(get, path = "/enquiries/{id}/timeline", tag = "enquiries", params(WorkspaceHeader, PageQuery, ("id" = Uuid, Path)),
    responses((status = 200, body = TimelinePage), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn enquiry_timeline(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<TimelinePage>> {
    Ok(Json(
        search_svc::timeline(&state, &ctx, enquiries::ENTITY, id, q.before, q.limit()).await?,
    ))
}

// Bookings -------------------------------------------------------------------------------

/// Bookings, soonest first unless sorted otherwise.
#[utoipa::path(get, path = "/bookings", tag = "bookings", params(WorkspaceHeader, ListQuery, BookingFilter),
    responses((status = 200, body = BookingPage), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_bookings(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<ListQuery>,
    Query(f): Query<BookingFilter>,
) -> ApiResult<Json<BookingPage>> {
    Ok(Json(bookings::list(&state, &ctx, &q, &f).await?))
}

/// Place a hold (the default), or add a pending or confirmed booking.
/// Confirming checks for clashes; see `POST /bookings/clash-check`.
#[utoipa::path(post, path = "/bookings", tag = "bookings", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewBooking,
    responses((status = 201, body = BookingView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_booking(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewBooking>,
) -> ApiResult<(StatusCode, Json<BookingView>)> {
    Ok((
        StatusCode::CREATED,
        Json(bookings::create(&state, &ctx, b).await?),
    ))
}

/// What a booking of these talent at this time would clash with: other
/// bookings and marked availability. Confirmed bookings and unavailable
/// time block confirming; holds, pending bookings and tentative time warn.
#[utoipa::path(post, path = "/bookings/clash-check", tag = "bookings", params(WorkspaceHeader),
    request_body = ClashQuery,
    responses((status = 200, body = ClashReport), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn clash_check(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<ClashQuery>,
) -> ApiResult<Json<ClashReport>> {
    Ok(Json(bookings::clash_check(&state, &ctx, &b).await?))
}

#[utoipa::path(get, path = "/bookings/{id}", tag = "bookings", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = BookingView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_booking(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = bookings::get(&state, &ctx, id).await?;
    Ok(with_etag(v.version, v))
}

/// Change a booking. Moving a confirmed booking, or changing its talent,
/// checks for clashes again.
#[utoipa::path(patch, path = "/bookings/{id}", tag = "bookings",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = BookingPatch,
    responses((status = 200, body = BookingView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_booking(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<BookingPatch>,
) -> ApiResult<Response> {
    let v = bookings::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

/// Delete a booking. Its open quotes are withdrawn.
#[utoipa::path(delete, path = "/bookings/{id}", tag = "bookings", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_booking(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    bookings::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Move a booking to another status: hold → pending → confirmed →
/// completed, or cancelled; an expired hold can be held again. Confirming
/// refuses a clash (409 `booking_clash`) unless `override_reason` is given
/// by someone with `booking:override`.
#[utoipa::path(post, path = "/bookings/{id}/status", tag = "bookings",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = StatusRequest,
    responses((status = 200, body = BookingView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn change_booking_status(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<StatusRequest>,
) -> ApiResult<Response> {
    let v = bookings::change_status(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

/// What happened to this booking and its quotes, newest first.
#[utoipa::path(get, path = "/bookings/{id}/timeline", tag = "bookings", params(WorkspaceHeader, PageQuery, ("id" = Uuid, Path)),
    responses((status = 200, body = TimelinePage), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn booking_timeline(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<TimelinePage>> {
    Ok(Json(
        search_svc::timeline(&state, &ctx, bookings::ENTITY, id, q.before, q.limit()).await?,
    ))
}

/// A booking's quotes, newest first.
#[utoipa::path(get, path = "/bookings/{id}/quotes", tag = "quotes", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = QuotePage), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_booking_quotes(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<QuotePage>> {
    let f = QuoteFilter {
        booking_id: Some(id),
        limit: Some(200),
        ..Default::default()
    };
    Ok(Json(quotes::list(&state, &ctx, &f).await?))
}

/// Start a draft quote for a booking, priced from the booking's fee or its
/// talent's default fees unless lines are given.
#[utoipa::path(post, path = "/bookings/{id}/quotes", tag = "quotes", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    request_body = NewQuote,
    responses((status = 201, body = QuoteView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_quote(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<NewQuote>,
) -> ApiResult<(StatusCode, Json<QuoteView>)> {
    Ok((
        StatusCode::CREATED,
        Json(quotes::create(&state, &ctx, id, b).await?),
    ))
}

// Calendar -------------------------------------------------------------------------------

/// Bookings and availability in a window (at most 400 days), for the calendar.
#[utoipa::path(get, path = "/calendar", tag = "calendar", params(WorkspaceHeader, CalendarQuery),
    responses((status = 200, body = CalendarView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn calendar(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<CalendarQuery>,
) -> ApiResult<Json<CalendarView>> {
    Ok(Json(bookings::calendar(&state, &ctx, &q).await?))
}

/// Your calendar links (an admin sees everyone's).
#[utoipa::path(get, path = "/calendar/feeds", tag = "calendar", params(WorkspaceHeader),
    responses((status = 200, body = Vec<FeedView>), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_feeds(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Vec<FeedView>>> {
    Ok(Json(calendar_feeds::list(&state, &ctx).await?))
}

/// Make a secret iCal link to the workspace's bookings, or one talent's.
/// The link is in the response only.
#[utoipa::path(post, path = "/calendar/feeds", tag = "calendar", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewFeed,
    responses((status = 201, body = CreatedFeed), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_feed(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewFeed>,
) -> ApiResult<(StatusCode, Json<CreatedFeed>)> {
    Ok((
        StatusCode::CREATED,
        Json(calendar_feeds::create(&state, &ctx, b).await?),
    ))
}

/// Stop a calendar link working.
#[utoipa::path(delete, path = "/calendar/feeds/{id}", tag = "calendar", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn revoke_feed(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    calendar_feeds::revoke(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Availability ---------------------------------------------------------------------------

/// A talent's availability in a window (default: last month to a year ahead).
#[utoipa::path(get, path = "/talent/{id}/availability", tag = "calendar", params(WorkspaceHeader, AvailabilityQuery, ("id" = Uuid, Path)),
    responses((status = 200, body = Vec<AvailabilityView>), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_availability(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<AvailabilityQuery>,
) -> ApiResult<Json<Vec<AvailabilityView>>> {
    Ok(Json(availability::list(&state, &ctx, id, &q).await?))
}

/// Mark time a talent is unavailable (blocks confirming), tentative
/// (warns) or available only.
#[utoipa::path(post, path = "/talent/{id}/availability", tag = "calendar", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    request_body = NewAvailability,
    responses((status = 201, body = AvailabilityView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_availability(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<NewAvailability>,
) -> ApiResult<(StatusCode, Json<AvailabilityView>)> {
    Ok((
        StatusCode::CREATED,
        Json(availability::create(&state, &ctx, id, b).await?),
    ))
}

#[utoipa::path(patch, path = "/availability/{id}", tag = "calendar",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = AvailabilityPatch,
    responses((status = 200, body = AvailabilityView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_availability(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<AvailabilityPatch>,
) -> ApiResult<Response> {
    let v = availability::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(delete, path = "/availability/{id}", tag = "calendar", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_availability(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    availability::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Quotes ----------------------------------------------------------------------------------

/// Quotes across bookings, newest first.
#[utoipa::path(get, path = "/quotes", tag = "quotes", params(WorkspaceHeader, QuoteFilter),
    responses((status = 200, body = QuotePage), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_quotes(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(f): Query<QuoteFilter>,
) -> ApiResult<Json<QuotePage>> {
    Ok(Json(quotes::list(&state, &ctx, &f).await?))
}

#[utoipa::path(get, path = "/quotes/{id}", tag = "quotes", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = QuoteView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_quote(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = quotes::get(&state, &ctx, id).await?;
    Ok(with_etag(v.version, v))
}

/// Change a draft quote. A sent quote is revised instead.
#[utoipa::path(patch, path = "/quotes/{id}", tag = "quotes",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = QuotePatch,
    responses((status = 200, body = QuoteView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_quote(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<QuotePatch>,
) -> ApiResult<Response> {
    let v = quotes::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

/// Delete a draft quote.
#[utoipa::path(delete, path = "/quotes/{id}", tag = "quotes", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn delete_quote(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    quotes::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Send a draft quote: the buyer gets an email with a link to view and
/// accept it. Other open quotes on the booking are withdrawn.
#[utoipa::path(post, path = "/quotes/{id}/send", tag = "quotes",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = SendQuote,
    responses((status = 200, body = SentQuote), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn send_quote(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<SendQuote>,
) -> ApiResult<Json<SentQuote>> {
    Ok(Json(quotes::send(&state, &ctx, id, b, if_match).await?))
}

/// Start a revision of a sent quote: a new draft with the same number.
#[utoipa::path(post, path = "/quotes/{id}/revise", tag = "quotes",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 201, body = QuoteView), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn revise_quote(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
) -> ApiResult<(StatusCode, Json<QuoteView>)> {
    Ok((
        StatusCode::CREATED,
        Json(quotes::revise(&state, &ctx, id, if_match).await?),
    ))
}

/// Take back a sent quote before the buyer answers.
#[utoipa::path(post, path = "/quotes/{id}/withdraw", tag = "quotes",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = QuoteView), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn withdraw_quote(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
) -> ApiResult<Response> {
    let v = quotes::withdraw(&state, &ctx, id, if_match).await?;
    Ok(with_etag(v.version, v))
}

// Tasks -----------------------------------------------------------------------------------

/// Tasks: open ones soonest due first, then done ones.
#[utoipa::path(get, path = "/tasks", tag = "tasks", params(WorkspaceHeader, TaskFilter),
    responses((status = 200, body = TaskPage), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_tasks(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(f): Query<TaskFilter>,
) -> ApiResult<Json<TaskPage>> {
    Ok(Json(tasks::list(&state, &ctx, &f).await?))
}

#[utoipa::path(post, path = "/tasks", tag = "tasks", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewTask,
    responses((status = 201, body = TaskView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_task(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewTask>,
) -> ApiResult<(StatusCode, Json<TaskView>)> {
    Ok((
        StatusCode::CREATED,
        Json(tasks::create(&state, &ctx, b).await?),
    ))
}

#[utoipa::path(get, path = "/tasks/{id}", tag = "tasks", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = TaskView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_task(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = tasks::get(&state, &ctx, id).await?;
    Ok(with_etag(v.version, v))
}

/// Change a task, or mark it done (`status: "done"`) or open again.
#[utoipa::path(patch, path = "/tasks/{id}", tag = "tasks",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = TaskPatch,
    responses((status = 200, body = TaskView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_task(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<TaskPatch>,
) -> ApiResult<Response> {
    let v = tasks::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(delete, path = "/tasks/{id}", tag = "tasks", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_task(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    tasks::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Public: the buyer's quote page and calendar feeds -------------------------------------

/// The quote behind a buyer's link. Needs no account; the link is the key.
/// The first view is recorded.
#[utoipa::path(get, path = "/public/quotes/{token}", tag = "public", params(("token" = String, Path)),
    responses((status = 200, body = PublicQuote), (status = 404, body = ErrorBody)))]
async fn public_quote(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    Path(token): Path<String>,
) -> ApiResult<Json<PublicQuote>> {
    Ok(Json(quotes::public_get(&state, &token, meta).await?))
}

/// Accept the quote behind a buyer's link, by name.
#[utoipa::path(post, path = "/public/quotes/{token}/accept", tag = "public", params(("token" = String, Path)),
    request_body = AcceptQuote,
    responses((status = 200, body = PublicQuote), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)))]
async fn accept_quote(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    Path(token): Path<String>,
    Json(b): Json<AcceptQuote>,
) -> ApiResult<Json<PublicQuote>> {
    Ok(Json(quotes::accept(&state, &token, meta, b).await?))
}

/// Decline the quote behind a buyer's link.
#[utoipa::path(post, path = "/public/quotes/{token}/decline", tag = "public", params(("token" = String, Path)),
    request_body = DeclineQuote,
    responses((status = 200, body = PublicQuote), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)))]
async fn decline_quote(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    Path(token): Path<String>,
    Json(b): Json<DeclineQuote>,
) -> ApiResult<Json<PublicQuote>> {
    Ok(Json(quotes::decline(&state, &token, meta, b).await?))
}

/// An iCal feed (`<token>.ics`) for calendar apps to subscribe to.
#[utoipa::path(get, path = "/public/calendar/{file}", tag = "public", params(("file" = String, Path, description = "`<token>.ics`")),
    responses((status = 200, content_type = "text/calendar", body = String), (status = 404, body = ErrorBody)))]
async fn public_calendar(
    State(state): State<AppState>,
    Path(file): Path<String>,
) -> ApiResult<Response> {
    let token = file.strip_suffix(".ics").unwrap_or(&file);
    let body = calendar_feeds::render_public(&state, token)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut res = body.into_response();
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/calendar; charset=utf-8"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=300"),
    );
    Ok(res)
}
