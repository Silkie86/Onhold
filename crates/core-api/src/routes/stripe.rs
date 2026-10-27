//! Card payments through Stripe Connect: connecting the workspace's Stripe
//! account, the payer's "Pay by card" button, and Stripe's webhooks.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::{IdempotencyHeader, WorkspaceHeader};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::WsCtx;
use crate::services::stripe::{self, ConnectStart, FinishConnect, PaymentLink, StripeStatus};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(stripe_status, stripe_disconnect))
        .routes(routes!(stripe_connect))
        .routes(routes!(stripe_connect_finish))
        .routes(routes!(public_invoice_pay))
        .routes(routes!(stripe_webhook))
}

/// The workspace's Stripe connection (needs `finance:config`).
#[utoipa::path(get, path = "/finance/stripe", tag = "finance", params(WorkspaceHeader),
    responses((status = 200, body = StripeStatus)), security(("session" = []), ("api_key" = [])))]
async fn stripe_status(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<StripeStatus>> {
    Ok(Json(stripe::status(&state, &ctx).await?))
}

/// Disconnect the Stripe account, here and in Stripe.
#[utoipa::path(delete, path = "/finance/stripe", tag = "finance", params(WorkspaceHeader, IdempotencyHeader),
    responses((status = 200, body = StripeStatus), (status = 503, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn stripe_disconnect(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<StripeStatus>> {
    Ok(Json(stripe::disconnect(&state, &ctx).await?))
}

/// Start connecting a Stripe account: send the admin to the returned URL.
#[utoipa::path(post, path = "/finance/stripe/connect", tag = "finance", params(WorkspaceHeader, IdempotencyHeader),
    responses((status = 200, body = ConnectStart), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn stripe_connect(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<ConnectStart>> {
    Ok(Json(stripe::start_connect(&state, &ctx).await?))
}

/// Finish connecting with the `code` and `state` Stripe sent back.
#[utoipa::path(post, path = "/finance/stripe/connect/finish", tag = "finance", params(WorkspaceHeader, IdempotencyHeader),
    request_body = FinishConnect,
    responses((status = 200, body = StripeStatus), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 503, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn stripe_connect_finish(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<FinishConnect>,
) -> ApiResult<Json<StripeStatus>> {
    Ok(Json(stripe::finish_connect(&state, &ctx, b).await?))
}

/// The payer pays by card: a Stripe payment page for what the invoice owes.
#[utoipa::path(post, path = "/public/invoices/{token}/pay", tag = "public", params(("token" = String, Path)),
    responses((status = 200, body = PaymentLink), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 503, body = ErrorBody)))]
async fn public_invoice_pay(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Json<PaymentLink>> {
    Ok(Json(stripe::public_pay(&state, &token).await?))
}

/// Stripe's events for connected accounts, signed with `Stripe-Signature`.
#[utoipa::path(post, path = "/webhooks/stripe", tag = "public",
    request_body(content = Object, content_type = "application/json"),
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn stripe_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let signature = headers
        .get(core_integrations::stripe::SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok());
    stripe::receive_webhook(&state, signature, &body).await?;
    Ok(StatusCode::NO_CONTENT)
}
