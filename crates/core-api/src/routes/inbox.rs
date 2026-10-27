//! The unified inbox: connected mailboxes, the threads read from them and
//! replies sent from them.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::documents::file_response;
use super::{IdempotencyHeader, IfMatchHeader, WorkspaceHeader, with_etag};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::{IfMatch, WsCtx};
use crate::services::inbox::{
    self, Compose, MessageView, Reply, ThreadDetail, ThreadEnquiry, ThreadEnquiryMade, ThreadPage,
    ThreadPatch, ThreadQuery,
};
use crate::services::integrations::{
    self, ConnectStart, FinishConnect, ImapConnect, IntegrationPatch, IntegrationView, MailSetup,
    StartConnect,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(mail_setup))
        .routes(routes!(mail_connect))
        .routes(routes!(mail_connect_finish))
        .routes(routes!(mail_connect_imap))
        .routes(routes!(update_mailbox, disconnect_mailbox))
        .routes(routes!(sync_mailbox))
        .routes(routes!(list_threads, compose_thread))
        .routes(routes!(get_thread, update_thread))
        .routes(routes!(reply_thread))
        .routes(routes!(thread_enquiry))
        .routes(routes!(thread_file))
}

// Mailboxes ----------------------------------------------------------------------------

/// Which providers this server can connect, and the mailboxes the caller sees.
#[utoipa::path(get, path = "/inbox/mailboxes", tag = "inbox", params(WorkspaceHeader),
    responses((status = 200, body = MailSetup)), security(("session" = []), ("api_key" = [])))]
async fn mail_setup(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<MailSetup>> {
    Ok(Json(integrations::setup(&state, &ctx).await?))
}

/// Start connecting a Gmail or Microsoft mailbox: send the member to the returned URL.
#[utoipa::path(post, path = "/inbox/mailboxes/connect", tag = "inbox", params(WorkspaceHeader, IdempotencyHeader),
    request_body = StartConnect,
    responses((status = 200, body = ConnectStart), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn mail_connect(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<StartConnect>,
) -> ApiResult<Json<ConnectStart>> {
    Ok(Json(integrations::start_connect(&state, &ctx, b).await?))
}

/// Finish connecting with the `code` and `state` the provider sent back.
#[utoipa::path(post, path = "/inbox/mailboxes/connect/finish", tag = "inbox", params(WorkspaceHeader, IdempotencyHeader),
    request_body = FinishConnect,
    responses((status = 200, body = IntegrationView), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 503, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn mail_connect_finish(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<FinishConnect>,
) -> ApiResult<Json<IntegrationView>> {
    Ok(Json(integrations::finish_connect(&state, &ctx, b).await?))
}

/// Connect any other mailbox by IMAP and SMTP. The servers are tried before it is saved.
#[utoipa::path(post, path = "/inbox/mailboxes/imap", tag = "inbox", params(WorkspaceHeader, IdempotencyHeader),
    request_body = ImapConnect,
    responses((status = 200, body = IntegrationView), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn mail_connect_imap(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<ImapConnect>,
) -> ApiResult<Json<IntegrationView>> {
    Ok(Json(integrations::connect_imap(&state, &ctx, b).await?))
}

/// Share a mailbox with the team, or rename it (its owner only).
#[utoipa::path(patch, path = "/inbox/mailboxes/{id}", tag = "inbox",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = IntegrationPatch,
    responses((status = 200, body = IntegrationView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_mailbox(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<IntegrationPatch>,
) -> ApiResult<Response> {
    let v = integrations::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

/// Stop reading a mailbox and forget its sign-in. Its threads stay.
#[utoipa::path(delete, path = "/inbox/mailboxes/{id}", tag = "inbox",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn disconnect_mailbox(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    integrations::disconnect(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Read a mailbox now rather than at its next turn.
#[utoipa::path(post, path = "/inbox/mailboxes/{id}/sync", tag = "inbox",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = IntegrationView), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn sync_mailbox(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<IntegrationView>> {
    Ok(Json(integrations::sync_now(&state, &ctx, id).await?))
}

// Threads ------------------------------------------------------------------------------

/// Conversations, latest first.
#[utoipa::path(get, path = "/inbox/threads", tag = "inbox", params(WorkspaceHeader, ThreadQuery),
    responses((status = 200, body = ThreadPage), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_threads(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<ThreadQuery>,
) -> ApiResult<Json<ThreadPage>> {
    Ok(Json(inbox::list(&state, &ctx, q).await?))
}

/// Write a new message, from a connected mailbox or OnHold's own sender.
#[utoipa::path(post, path = "/inbox/threads", tag = "inbox", params(WorkspaceHeader, IdempotencyHeader),
    request_body = Compose,
    responses((status = 201, body = ThreadDetail), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn compose_thread(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<Compose>,
) -> ApiResult<(StatusCode, Json<ThreadDetail>)> {
    Ok((
        StatusCode::CREATED,
        Json(inbox::compose(&state, &ctx, b).await?),
    ))
}

/// One conversation with its messages, oldest first.
#[utoipa::path(get, path = "/inbox/threads/{id}", tag = "inbox", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = ThreadDetail), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_thread(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = inbox::get(&state, &ctx, id).await?;
    Ok(with_etag(v.thread.version, v))
}

/// Resolve, snooze, assign, mark read or link a conversation.
#[utoipa::path(patch, path = "/inbox/threads/{id}", tag = "inbox",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = ThreadPatch,
    responses((status = 200, body = ThreadDetail), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_thread(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<ThreadPatch>,
) -> ApiResult<Response> {
    let v = inbox::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.thread.version, v))
}

/// Reply in a conversation. The message is queued and sent in the background.
#[utoipa::path(post, path = "/inbox/threads/{id}/reply", tag = "inbox",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = Reply,
    responses((status = 201, body = MessageView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn reply_thread(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<Reply>,
) -> ApiResult<(StatusCode, Json<MessageView>)> {
    Ok((
        StatusCode::CREATED,
        Json(inbox::reply(&state, &ctx, id, b).await?),
    ))
}

/// Turn a conversation into an enquiry, adding the sender as a contact when needed.
#[utoipa::path(post, path = "/inbox/threads/{id}/enquiry", tag = "inbox",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = ThreadEnquiry,
    responses((status = 201, body = ThreadEnquiryMade), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn thread_enquiry(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<ThreadEnquiry>,
) -> ApiResult<(StatusCode, Json<ThreadEnquiryMade>)> {
    Ok((
        StatusCode::CREATED,
        Json(inbox::make_enquiry(&state, &ctx, id, b).await?),
    ))
}

/// An attachment in a conversation.
#[utoipa::path(get, path = "/inbox/threads/{id}/files/{file_id}", tag = "inbox",
    params(WorkspaceHeader, ("id" = Uuid, Path), ("file_id" = Uuid, Path)),
    responses((status = 200, content_type = "application/octet-stream", body = Vec<u8>), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn thread_file(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path((id, file_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    Ok(file_response(
        inbox::download(&state, &ctx, id, file_id).await?,
    ))
}
