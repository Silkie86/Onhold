//! Text messages: the workspace's SMS and WhatsApp numbers, texting
//! someone, and the addresses Twilio and Meta send what comes in to.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{IdempotencyHeader, WorkspaceHeader};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::WsCtx;
use crate::services::inbox::ThreadDetail;
use crate::services::messaging::{
    self, ConnectSms, ConnectWhatsapp, Messaging, MessagingChannel, StartText,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_numbers))
        .routes(routes!(connect_sms))
        .routes(routes!(connect_whatsapp))
        .routes(routes!(disconnect_number))
        .routes(routes!(start_text))
        .routes(routes!(messaging_verify, messaging_hook))
}

/// The workspace's SMS and WhatsApp numbers.
#[utoipa::path(get, path = "/inbox/numbers", tag = "inbox", params(WorkspaceHeader),
    responses((status = 200, body = Messaging)), security(("session" = []), ("api_key" = [])))]
async fn list_numbers(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Messaging>> {
    Ok(Json(messaging::list(&state, &ctx).await?))
}

/// Connect the workspace's Twilio number for SMS (checked with Twilio).
#[utoipa::path(post, path = "/inbox/numbers/sms", tag = "inbox", params(WorkspaceHeader, IdempotencyHeader),
    request_body = ConnectSms,
    responses((status = 201, body = MessagingChannel), (status = 400, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 503, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn connect_sms(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<ConnectSms>,
) -> ApiResult<(StatusCode, Json<MessagingChannel>)> {
    Ok((
        StatusCode::CREATED,
        Json(messaging::connect_sms(&state, &ctx, b).await?),
    ))
}

/// Connect the workspace's WhatsApp number in Meta's Cloud API (checked with Meta).
#[utoipa::path(post, path = "/inbox/numbers/whatsapp", tag = "inbox", params(WorkspaceHeader, IdempotencyHeader),
    request_body = ConnectWhatsapp,
    responses((status = 201, body = MessagingChannel), (status = 400, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 503, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn connect_whatsapp(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<ConnectWhatsapp>,
) -> ApiResult<(StatusCode, Json<MessagingChannel>)> {
    Ok((
        StatusCode::CREATED,
        Json(messaging::connect_whatsapp(&state, &ctx, b).await?),
    ))
}

/// Stop using a number and forget its credentials; its conversations stay.
#[utoipa::path(delete, path = "/inbox/numbers/{id}", tag = "inbox", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn disconnect_number(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    messaging::disconnect(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Text a contact or a number from the workspace's SMS or WhatsApp number.
#[utoipa::path(post, path = "/inbox/texts", tag = "inbox", params(WorkspaceHeader, IdempotencyHeader),
    request_body = StartText,
    responses((status = 201, body = ThreadDetail), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn start_text(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<StartText>,
) -> ApiResult<(StatusCode, Json<ThreadDetail>)> {
    Ok((
        StatusCode::CREATED,
        Json(messaging::start(&state, &ctx, b).await?),
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
struct Verify {
    #[serde(rename = "hub.mode")]
    #[param(rename = "hub.mode")]
    mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    #[param(rename = "hub.verify_token")]
    verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    #[param(rename = "hub.challenge")]
    challenge: Option<String>,
}

/// Meta's check of a WhatsApp number's callback URL.
#[utoipa::path(get, path = "/hooks/messaging/{token}", tag = "public", params(("token" = String, Path), Verify),
    responses((status = 200, body = String), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
async fn messaging_verify(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(q): Query<Verify>,
) -> ApiResult<String> {
    messaging::meta_verify(
        &state,
        &token,
        q.mode.as_deref(),
        q.verify_token.as_deref(),
        q.challenge.as_deref(),
    )
    .await
}

/// Texts in, and what became of texts sent, from Twilio or Meta.
#[utoipa::path(post, path = "/hooks/messaging/{token}", tag = "public", params(("token" = String, Path)),
    request_body(content = String, content_type = "application/x-www-form-urlencoded"),
    responses((status = 200), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
async fn messaging_hook(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let twiml = messaging::hook(
        &state,
        &token,
        get(core_integrations::messaging::TWILIO_SIGNATURE),
        get(core_integrations::messaging::META_SIGNATURE),
        &body,
    )
    .await?;
    Ok(match twiml {
        Some(xml) => ([(header::CONTENT_TYPE, "text/xml")], xml).into_response(),
        None => StatusCode::OK.into_response(),
    })
}
