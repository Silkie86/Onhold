//! Money: finance settings, tax codes and exchange rates; invoices and
//! credit notes (and the payer's page); payments, refunds and payouts;
//! settlements; reports and CSV exports.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDate;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::documents::file_response;
use super::{IdempotencyHeader, IfMatchHeader, WorkspaceHeader, with_etag};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::{ClientMeta, IfMatch, WsCtx};
use crate::services::finance::{
    self, FinanceSettings, FinanceSettingsInput, FxQuote, TaxCodeInput, TaxCodeView,
};
use crate::services::invoices::{
    self, HistoricalInvoice, InvoiceBooking, InvoiceFilter, InvoicePage, InvoicePatch, InvoiceView,
    IssueInvoice, NewCreditNote, NewInvoice, PublicInvoice, SendInvoice, SentInvoice, VoidInvoice,
};
use crate::services::payments::{
    self, AllocationInput, AllocationView, NewPayment, NewRefund, PaymentFilter, PaymentPage,
    PaymentView, VoidPayment,
};
use crate::services::reports::{self, FinanceReport, Period};
use crate::services::settlements::{
    self, NewPayout, SettlementFilter, SettlementPatch, SettlementView,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_finance_settings, update_finance_settings))
        .routes(routes!(list_tax_codes, create_tax_code))
        .routes(routes!(update_tax_code))
        .routes(routes!(fx_rate))
        .routes(routes!(list_invoices, create_invoice))
        .routes(routes!(create_historical_invoice))
        .routes(routes!(get_invoice, update_invoice, delete_invoice))
        .routes(routes!(issue_invoice))
        .routes(routes!(send_invoice))
        .routes(routes!(void_invoice))
        .routes(routes!(credit_invoice))
        .routes(routes!(invoice_pdf))
        .routes(routes!(invoice_allocations))
        .routes(routes!(invoice_booking))
        .routes(routes!(list_payments, create_payment))
        .routes(routes!(get_payment))
        .routes(routes!(allocate_payment))
        .routes(routes!(remove_allocation))
        .routes(routes!(refund_payment))
        .routes(routes!(void_payment))
        .routes(routes!(list_settlements))
        .routes(routes!(settle_booking))
        .routes(routes!(
            get_settlement,
            update_settlement,
            delete_settlement
        ))
        .routes(routes!(finalise_settlement))
        .routes(routes!(reopen_settlement))
        .routes(routes!(settlement_payout))
        .routes(routes!(finance_report))
        .routes(routes!(export_csv))
        .routes(routes!(public_invoice))
        .routes(routes!(public_invoice_pdf))
}

// Settings ----------------------------------------------------------------------------------------

#[utoipa::path(get, path = "/finance/settings", tag = "finance", params(WorkspaceHeader),
    responses((status = 200, body = FinanceSettings)), security(("session" = []), ("api_key" = [])))]
async fn get_finance_settings(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Response> {
    let s = finance::get_settings(&state, &ctx).await?;
    Ok(with_etag(s.version, s))
}

/// Replace the finance settings (needs `finance:config`).
#[utoipa::path(put, path = "/finance/settings", tag = "finance", params(WorkspaceHeader, IfMatchHeader),
    request_body = FinanceSettingsInput,
    responses((status = 200, body = FinanceSettings), (status = 400, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_finance_settings(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    IfMatch(if_match): IfMatch,
    Json(b): Json<FinanceSettingsInput>,
) -> ApiResult<Response> {
    let s = finance::update_settings(&state, &ctx, b, if_match).await?;
    Ok(with_etag(s.version, s))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ArchivedQuery {
    #[serde(default)]
    pub include_archived: bool,
}

/// Tax codes; a workspace starts with its country's usual rates.
#[utoipa::path(get, path = "/tax-codes", tag = "finance", params(WorkspaceHeader, ArchivedQuery),
    responses((status = 200, body = Vec<TaxCodeView>)), security(("session" = []), ("api_key" = [])))]
async fn list_tax_codes(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<ArchivedQuery>,
) -> ApiResult<Json<Vec<TaxCodeView>>> {
    Ok(Json(
        finance::tax_codes(&state, &ctx, q.include_archived).await?,
    ))
}

#[utoipa::path(post, path = "/tax-codes", tag = "finance", params(WorkspaceHeader, IdempotencyHeader),
    request_body = TaxCodeInput,
    responses((status = 201, body = TaxCodeView), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_tax_code(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<TaxCodeInput>,
) -> ApiResult<(StatusCode, Json<TaxCodeView>)> {
    Ok((
        StatusCode::CREATED,
        Json(finance::create_tax_code(&state, &ctx, b).await?),
    ))
}

#[utoipa::path(put, path = "/tax-codes/{id}", tag = "finance", params(WorkspaceHeader, IfMatchHeader, ("id" = Uuid, Path)),
    request_body = TaxCodeInput,
    responses((status = 200, body = TaxCodeView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_tax_code(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<TaxCodeInput>,
) -> ApiResult<Json<TaxCodeView>> {
    Ok(Json(
        finance::update_tax_code(&state, &ctx, id, b, if_match).await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct FxQuery {
    pub from: String,
    /// Default: the base currency.
    pub to: Option<String>,
    /// Default today.
    pub on: Option<NaiveDate>,
}

/// The reference exchange rate (ECB, crossed through the euro) on a day.
#[utoipa::path(get, path = "/fx-rates", tag = "finance", params(WorkspaceHeader, FxQuery),
    responses((status = 200, body = FxQuote), (status = 422, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn fx_rate(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<FxQuery>,
) -> ApiResult<Json<FxQuote>> {
    Ok(Json(
        finance::rate(&state, &ctx, &q.from, q.to.as_deref(), q.on).await?,
    ))
}

// Invoices ----------------------------------------------------------------------------------------

#[utoipa::path(get, path = "/invoices", tag = "invoices", params(WorkspaceHeader, InvoiceFilter),
    responses((status = 200, body = InvoicePage), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_invoices(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(f): Query<InvoiceFilter>,
) -> ApiResult<Json<InvoicePage>> {
    Ok(Json(invoices::list(&state, &ctx, &f).await?))
}

/// A new draft invoice.
#[utoipa::path(post, path = "/invoices", tag = "invoices", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewInvoice,
    responses((status = 201, body = InvoiceView), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_invoice(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewInvoice>,
) -> ApiResult<(StatusCode, Json<InvoiceView>)> {
    Ok((
        StatusCode::CREATED,
        Json(invoices::create(&state, &ctx, b).await?),
    ))
}

/// Record an invoice from before OnHold, as it was.
#[utoipa::path(post, path = "/invoices/historical", tag = "invoices", params(WorkspaceHeader, IdempotencyHeader),
    request_body = HistoricalInvoice,
    responses((status = 201, body = InvoiceView), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_historical_invoice(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<HistoricalInvoice>,
) -> ApiResult<(StatusCode, Json<InvoiceView>)> {
    Ok((
        StatusCode::CREATED,
        Json(invoices::create_historical(&state, &ctx, b).await?),
    ))
}

#[utoipa::path(get, path = "/invoices/{id}", tag = "invoices", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = InvoiceView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_invoice(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = invoices::get(&state, &ctx, id).await?;
    Ok(with_etag(v.version, v))
}

/// Change a draft (or an issued invoice's tags, custom fields and notes).
#[utoipa::path(patch, path = "/invoices/{id}", tag = "invoices",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = InvoicePatch,
    responses((status = 200, body = InvoiceView), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_invoice(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<InvoicePatch>,
) -> ApiResult<Response> {
    let v = invoices::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

/// Delete a draft.
#[utoipa::path(delete, path = "/invoices/{id}", tag = "invoices", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn delete_invoice(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    invoices::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Issue a draft: number it, fix its parties and exchange rate, print its PDF.
#[utoipa::path(post, path = "/invoices/{id}/issue", tag = "invoices",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = IssueInvoice,
    responses((status = 200, body = InvoiceView), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 422, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn issue_invoice(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<IssueInvoice>,
) -> ApiResult<Response> {
    let v = invoices::issue(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

/// Email an issued invoice with its PDF and a link to view it.
#[utoipa::path(post, path = "/invoices/{id}/send", tag = "invoices",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = SendInvoice,
    responses((status = 200, body = SentInvoice), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn send_invoice(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<SendInvoice>,
) -> ApiResult<Json<SentInvoice>> {
    Ok(Json(invoices::send(&state, &ctx, id, b, if_match).await?))
}

/// Void an issued invoice nothing has been paid or credited on.
#[utoipa::path(post, path = "/invoices/{id}/void", tag = "invoices",
    params(WorkspaceHeader, IfMatchHeader, ("id" = Uuid, Path)), request_body = VoidInvoice,
    responses((status = 200, body = InvoiceView), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn void_invoice(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<VoidInvoice>,
) -> ApiResult<Json<InvoiceView>> {
    Ok(Json(invoices::void(&state, &ctx, id, b, if_match).await?))
}

/// A credit note against an issued invoice (all of it by default).
#[utoipa::path(post, path = "/invoices/{id}/credit-notes", tag = "invoices",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = NewCreditNote,
    responses((status = 201, body = InvoiceView), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn credit_invoice(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<NewCreditNote>,
) -> ApiResult<(StatusCode, Json<InvoiceView>)> {
    Ok((
        StatusCode::CREATED,
        Json(invoices::credit(&state, &ctx, id, b).await?),
    ))
}

#[utoipa::path(get, path = "/invoices/{id}/pdf", tag = "invoices", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, content_type = "application/pdf", body = Vec<u8>), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn invoice_pdf(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    Ok(file_response(invoices::pdf(&state, &ctx, id).await?))
}

/// The payments allocated to an invoice.
#[utoipa::path(get, path = "/invoices/{id}/allocations", tag = "invoices", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = Vec<AllocationView>), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn invoice_allocations(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<AllocationView>>> {
    Ok(Json(payments::for_invoice(&state, &ctx, id).await?))
}

/// A deposit, balance or full invoice for a booking, from its accepted quote or fee.
#[utoipa::path(post, path = "/bookings/{id}/invoices", tag = "invoices",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = InvoiceBooking,
    responses((status = 201, body = InvoiceView), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn invoice_booking(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<InvoiceBooking>,
) -> ApiResult<(StatusCode, Json<InvoiceView>)> {
    Ok((
        StatusCode::CREATED,
        Json(invoices::from_booking(&state, &ctx, id, b).await?),
    ))
}

// Payments ----------------------------------------------------------------------------------------

#[utoipa::path(get, path = "/payments", tag = "payments", params(WorkspaceHeader, PaymentFilter),
    responses((status = 200, body = PaymentPage)), security(("session" = []), ("api_key" = [])))]
async fn list_payments(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(f): Query<PaymentFilter>,
) -> ApiResult<Json<PaymentPage>> {
    Ok(Json(payments::list(&state, &ctx, &f).await?))
}

/// Record money received, allocated to the invoices it pays.
#[utoipa::path(post, path = "/payments", tag = "payments", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewPayment,
    responses((status = 201, body = PaymentView), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_payment(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewPayment>,
) -> ApiResult<(StatusCode, Json<PaymentView>)> {
    Ok((
        StatusCode::CREATED,
        Json(payments::create(&state, &ctx, b).await?),
    ))
}

#[utoipa::path(get, path = "/payments/{id}", tag = "payments", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = PaymentView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_payment(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = payments::get(&state, &ctx, id).await?;
    Ok(with_etag(v.version, v))
}

/// Allocate what is left of a payment to an invoice.
#[utoipa::path(post, path = "/payments/{id}/allocations", tag = "payments",
    params(WorkspaceHeader, IfMatchHeader, ("id" = Uuid, Path)), request_body = AllocationInput,
    responses((status = 200, body = PaymentView), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn allocate_payment(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<AllocationInput>,
) -> ApiResult<Json<PaymentView>> {
    Ok(Json(
        payments::allocate(&state, &ctx, id, b, if_match).await?,
    ))
}

#[utoipa::path(delete, path = "/payments/{id}/allocations/{allocation_id}", tag = "payments",
    params(WorkspaceHeader, ("id" = Uuid, Path), ("allocation_id" = Uuid, Path)),
    responses((status = 200, body = PaymentView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn remove_allocation(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path((id, allocation_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<PaymentView>> {
    Ok(Json(
        payments::remove_allocation(&state, &ctx, id, allocation_id).await?,
    ))
}

/// Return money from what is unallocated of a payment.
#[utoipa::path(post, path = "/payments/{id}/refunds", tag = "payments",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = NewRefund,
    responses((status = 201, body = PaymentView), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn refund_payment(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<NewRefund>,
) -> ApiResult<(StatusCode, Json<PaymentView>)> {
    Ok((
        StatusCode::CREATED,
        Json(payments::refund(&state, &ctx, id, b).await?),
    ))
}

/// Void a payment recorded in error; its invoices owe again.
#[utoipa::path(post, path = "/payments/{id}/void", tag = "payments",
    params(WorkspaceHeader, IfMatchHeader, ("id" = Uuid, Path)), request_body = VoidPayment,
    responses((status = 200, body = PaymentView), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn void_payment(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<VoidPayment>,
) -> ApiResult<Json<PaymentView>> {
    Ok(Json(payments::void(&state, &ctx, id, b, if_match).await?))
}

// Settlements -------------------------------------------------------------------------------------

#[utoipa::path(get, path = "/settlements", tag = "settlements", params(WorkspaceHeader, SettlementFilter),
    responses((status = 200, body = Vec<SettlementView>)), security(("session" = []), ("api_key" = [])))]
async fn list_settlements(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(f): Query<SettlementFilter>,
) -> ApiResult<Json<Vec<SettlementView>>> {
    Ok(Json(settlements::list(&state, &ctx, &f).await?))
}

/// Draft a settlement for each talent on a booking that has none.
#[utoipa::path(post, path = "/bookings/{id}/settlements", tag = "settlements",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = Vec<SettlementView>), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn settle_booking(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<SettlementView>>> {
    Ok(Json(
        settlements::create_for_booking(&state, &ctx, id).await?,
    ))
}

#[utoipa::path(get, path = "/settlements/{id}", tag = "settlements", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = SettlementView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_settlement(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = settlements::get(&state, &ctx, id).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(patch, path = "/settlements/{id}", tag = "settlements",
    params(WorkspaceHeader, IfMatchHeader, ("id" = Uuid, Path)), request_body = SettlementPatch,
    responses((status = 200, body = SettlementView), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_settlement(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<SettlementPatch>,
) -> ApiResult<Response> {
    let v = settlements::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(delete, path = "/settlements/{id}", tag = "settlements", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 409, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_settlement(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    settlements::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Fix a settlement and print its statement.
#[utoipa::path(post, path = "/settlements/{id}/finalise", tag = "settlements",
    params(WorkspaceHeader, IfMatchHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = SettlementView), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn finalise_settlement(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
) -> ApiResult<Json<SettlementView>> {
    Ok(Json(
        settlements::finalise(&state, &ctx, id, if_match).await?,
    ))
}

#[utoipa::path(post, path = "/settlements/{id}/reopen", tag = "settlements",
    params(WorkspaceHeader, IfMatchHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = SettlementView), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn reopen_settlement(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
) -> ApiResult<Json<SettlementView>> {
    Ok(Json(settlements::reopen(&state, &ctx, id, if_match).await?))
}

/// Record paying the talent.
#[utoipa::path(post, path = "/settlements/{id}/payouts", tag = "settlements",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = NewPayout,
    responses((status = 200, body = SettlementView), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn settlement_payout(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<NewPayout>,
) -> ApiResult<Json<SettlementView>> {
    Ok(Json(settlements::payout(&state, &ctx, id, b).await?))
}

// Reports -----------------------------------------------------------------------------------------

/// Revenue, what is owed, cash and the enquiry funnel, in the base currency.
#[utoipa::path(get, path = "/reports/finance", tag = "reports", params(WorkspaceHeader, Period),
    responses((status = 200, body = FinanceReport), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn finance_report(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(p): Query<Period>,
) -> ApiResult<Json<FinanceReport>> {
    Ok(Json(reports::finance(&state, &ctx, &p).await?))
}

/// A report table, or the invoice or payment list, as CSV.
#[utoipa::path(get, path = "/reports/{table}/csv", tag = "reports",
    params(WorkspaceHeader, ("table" = String, Path, description = "invoices, payments, revenue_by_month, revenue_by_payer, revenue_by_talent or outstanding"), Period),
    responses((status = 200, content_type = "text/csv", body = String), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn export_csv(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(table): Path<String>,
    Query(p): Query<Period>,
) -> ApiResult<Response> {
    let body = reports::csv(&state, &ctx, &table, &p).await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{table}.csv\"")) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok((headers, body).into_response())
}

// The payer's page --------------------------------------------------------------------------------

#[utoipa::path(get, path = "/public/invoices/{token}", tag = "public", params(("token" = String, Path)),
    responses((status = 200, body = PublicInvoice), (status = 404, body = ErrorBody)))]
async fn public_invoice(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    Path(token): Path<String>,
) -> ApiResult<Json<PublicInvoice>> {
    Ok(Json(invoices::public_get(&state, &token, meta).await?))
}

#[utoipa::path(get, path = "/public/invoices/{token}/pdf", tag = "public", params(("token" = String, Path)),
    responses((status = 200, content_type = "application/pdf", body = Vec<u8>), (status = 404, body = ErrorBody)))]
async fn public_invoice_pdf(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Response> {
    Ok(file_response(invoices::public_pdf(&state, &token).await?))
}
