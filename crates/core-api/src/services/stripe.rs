//! Card payments through Stripe Connect. A workspace admin connects the
//! client's own Stripe account (Stripe's OAuth, Standard accounts); the
//! payer's invoice page then offers "Pay by card", which opens a Stripe
//! Checkout session on that account for what the invoice still owes.
//! Stripe's signed webhooks are kept once each and processed by a job:
//! a completed session records the payment against the invoice, a refund
//! made in Stripe records the refund here, and account changes keep the
//! connection's state current.

use chrono::{DateTime, Duration, Utc};
use core_db::Tx;
use core_db::stripe::{AccountState, InvoiceSession, StripeAccount};
use core_domain::{Ctx, Money, Permission, UserId, WorkspaceId};
use core_integrations::stripe::{
    self as api, CheckoutSession, Event, NewCheckout, StripeClient, StripeError,
};
use data_encoding::BASE64URL_NOPAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::payments::{AllocationInput, NewPayment, NewRefund};
use super::{Change, begin, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const ENTITY: &str = "stripe_account";
/// Processes one event Stripe sent (`event_id`).
pub const JOB_EVENT: &str = "stripe.event";

const STATE_AAD: &[u8] = b"stripe_connect_state";
/// How long the admin has to finish connecting on Stripe's side.
const CONNECT_TTL_SECS: i64 = 30 * 60;
/// A session closer to expiry than this is replaced rather than reused.
const SESSION_MARGIN_MINS: i64 = 10;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StripeStatus {
    /// Whether this OnHold server takes card payments at all.
    pub available: bool,
    pub connected: bool,
    pub account_id: Option<String>,
    pub display_name: Option<String>,
    /// A live account rather than Stripe's test mode.
    pub livemode: bool,
    /// Stripe lets the account take payments (it may want more details first).
    pub charges_enabled: bool,
    pub details_submitted: bool,
    pub default_currency: Option<String>,
    pub connected_at: Option<DateTime<Utc>>,
    pub disconnected_at: Option<DateTime<Utc>>,
}

fn status_of(state: &AppState, a: Option<StripeAccount>) -> StripeStatus {
    let available = state.config.stripe.is_some();
    match a {
        Some(a) => StripeStatus {
            available,
            connected: a.disconnected_at.is_none(),
            account_id: Some(a.account_id),
            display_name: a.display_name,
            livemode: a.livemode,
            charges_enabled: a.charges_enabled && a.disconnected_at.is_none(),
            details_submitted: a.details_submitted,
            default_currency: a.default_currency,
            connected_at: Some(a.connected_at),
            disconnected_at: a.disconnected_at,
        },
        None => StripeStatus {
            available,
            connected: false,
            account_id: None,
            display_name: None,
            livemode: false,
            charges_enabled: false,
            details_submitted: false,
            default_currency: None,
            connected_at: None,
            disconnected_at: None,
        },
    }
}

fn to_json(s: &StripeStatus) -> Value {
    serde_json::to_value(s).unwrap_or(Value::Null)
}

fn client(state: &AppState) -> ApiResult<&StripeClient> {
    state.config.stripe.as_ref().ok_or_else(|| {
        ApiError::conflict(
            "stripe_not_available",
            "card payments are not set up on this server",
        )
    })
}

fn stripe_error(err: StripeError) -> ApiError {
    match err {
        StripeError::Unavailable(m) => ApiError::unavailable(
            "stripe_unavailable",
            format!("Stripe could not be reached; try again shortly ({m})"),
        ),
        StripeError::Refused { message, .. } => {
            ApiError::conflict("stripe_refused", format!("Stripe said: {message}"))
        }
        StripeError::Invalid(m) => ApiError::internal(format!("stripe: {m}")),
    }
}

/// The connection's state, for Settings → Payments.
pub async fn status(state: &AppState, ctx: &Ctx) -> ApiResult<StripeStatus> {
    ctx.authorize(Permission::FinanceConfig)?;
    let mut tx = begin(state, ctx).await?;
    let a = core_db::stripe::account(&mut tx).await?;
    tx.commit().await?;
    Ok(status_of(state, a))
}

#[derive(Debug, Serialize, Deserialize)]
struct ConnectState {
    workspace: WorkspaceId,
    user: Option<UserId>,
    nonce: String,
    exp: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConnectStart {
    /// Send the admin here; Stripe sends them back to Settings → Payments
    /// with `code` and `state`, which the page passes to the finish call.
    pub url: String,
}

/// Where the admin connects (or creates) their Stripe account.
pub async fn start_connect(state: &AppState, ctx: &Ctx) -> ApiResult<ConnectStart> {
    ctx.authorize(Permission::FinanceConfig)?;
    let client = client(state)?;
    let flow = ConnectState {
        workspace: ctx.workspace_id,
        user: ctx.user_id(),
        nonce: BASE64URL_NOPAD.encode(&core_domain::crypto::random_bytes::<16>()),
        exp: Utc::now().timestamp() + CONNECT_TTL_SECS,
    };
    let json = serde_json::to_vec(&flow).map_err(ApiError::internal)?;
    let sealed = BASE64URL_NOPAD.encode(&state.config.keyring.encrypt(&json, STATE_AAD));
    let redirect = state.config.app_url("/settings/payments", &[]);
    Ok(ConnectStart {
        url: client.authorize_url(&sealed, &redirect).to_string(),
    })
}

fn open_state(state: &AppState, sealed: &str) -> ApiResult<ConnectState> {
    let bad = || {
        ApiError::bad_request(
            "stripe_state_invalid",
            "the connection attempt expired or was not started here; start again",
        )
    };
    let raw = BASE64URL_NOPAD
        .decode(sealed.as_bytes())
        .map_err(|_| bad())?;
    let json = state
        .config
        .keyring
        .decrypt(&raw, STATE_AAD)
        .map_err(|_| bad())?;
    let flow: ConnectState = serde_json::from_slice(&json).map_err(|_| bad())?;
    if flow.exp < Utc::now().timestamp() {
        return Err(bad());
    }
    Ok(flow)
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct FinishConnect {
    /// From Stripe's redirect.
    pub code: String,
    /// From Stripe's redirect, as the start call made it.
    pub state: String,
}

fn account_state(a: &api::Account) -> (Option<String>, Option<String>) {
    let currency = a
        .default_currency
        .as_deref()
        .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()))
        .map(str::to_ascii_uppercase);
    (currency, a.display_name())
}

/// Finishes connecting with what Stripe sent back. Only the admin who
/// started, in the same workspace, can finish.
pub async fn finish_connect(
    state: &AppState,
    ctx: &Ctx,
    req: FinishConnect,
) -> ApiResult<StripeStatus> {
    ctx.authorize(Permission::FinanceConfig)?;
    let client = client(state)?;
    let flow = open_state(state, &req.state)?;
    if flow.workspace != ctx.workspace_id || flow.user != ctx.user_id() {
        return Err(ApiError::bad_request(
            "stripe_state_invalid",
            "this connection was started by someone else or in another workspace",
        ));
    }
    let code = req.code.trim();
    if code.is_empty() || code.len() > 500 {
        return Err(ApiError::invalid("code", "is not a code from Stripe"));
    }
    let token = client.oauth_token(code).await.map_err(stripe_error)?;
    let account = client
        .account(&token.stripe_user_id)
        .await
        .map_err(stripe_error)?;
    if account.id != token.stripe_user_id || !account.id.starts_with("acct_") {
        return Err(ApiError::internal("stripe: the account does not match"));
    }
    let (currency, name) = account_state(&account);
    let mut tx = begin(state, ctx).await?;
    if let Some(other) = core_db::stripe::account_workspace(&mut tx, &account.id).await?
        && other != ctx.workspace_id
    {
        return Err(ApiError::conflict(
            "stripe_account_in_use",
            "this Stripe account is connected to another workspace; disconnect it there first",
        ));
    }
    let before = status_of(state, core_db::stripe::account(&mut tx).await?);
    core_db::stripe::connect(
        &mut tx,
        &account.id,
        token.livemode,
        &AccountState {
            charges_enabled: account.charges_enabled,
            details_submitted: account.details_submitted,
            default_currency: currency.as_deref(),
            display_name: name.as_deref(),
        },
        ctx.user_id(),
    )
    .await?;
    // Sessions on a previous account are no use now.
    core_db::stripe::clear_all_sessions(&mut tx).await?;
    let after = status_of(state, core_db::stripe::account(&mut tx).await?);
    record(
        &mut tx,
        ctx,
        Change {
            topic: "stripe.connected",
            resource: ENTITY,
            object_id: None,
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// Disconnects the account from OnHold (in Stripe too). Payments already
/// recorded stay; open payment links stop being offered.
pub async fn disconnect(state: &AppState, ctx: &Ctx) -> ApiResult<StripeStatus> {
    ctx.authorize(Permission::FinanceConfig)?;
    let mut tx = begin(state, ctx).await?;
    let Some(a) = core_db::stripe::account(&mut tx).await? else {
        return Ok(status_of(state, None));
    };
    if a.disconnected_at.is_some() {
        return Ok(status_of(state, Some(a)));
    }
    if let Some(client) = state.config.stripe.as_ref() {
        client
            .deauthorize(&a.account_id)
            .await
            .map_err(stripe_error)?;
    }
    let before = status_of(state, Some(a.clone()));
    core_db::stripe::disconnect(&mut tx, &a.account_id).await?;
    core_db::stripe::clear_all_sessions(&mut tx).await?;
    let after = status_of(state, core_db::stripe::account(&mut tx).await?);
    record(
        &mut tx,
        ctx,
        Change {
            topic: "stripe.disconnected",
            resource: ENTITY,
            object_id: None,
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

// The payer's side ------------------------------------------------------------------------------

/// Whether the payer can pay this invoice by card now.
pub(crate) async fn card_payments_open(
    state: &AppState,
    tx: &mut Tx,
    inv: &super::invoices::InvoiceView,
) -> ApiResult<bool> {
    if state.config.stripe.is_none()
        || inv.kind != "invoice"
        || !inv.status.is_issued()
        || inv.outstanding.minor <= 0
        || !api::supports_currency(&inv.currency)
    {
        return Ok(false);
    }
    Ok(core_db::stripe::account(tx)
        .await?
        .is_some_and(|a| a.disconnected_at.is_none() && a.charges_enabled))
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PaymentLink {
    /// Stripe's payment page; send the payer there.
    pub url: String,
}

/// A Stripe Checkout session for what the invoice still owes. The open
/// session is reused while it is for the same amount, so pressing Pay twice
/// gives one session.
pub async fn public_pay(state: &AppState, token: &str) -> ApiResult<PaymentLink> {
    let client = client(state)?;
    let (mut tx, ctx, id) = super::invoices::open_public(state, token, Default::default()).await?;
    core_db::invoices::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let inv = super::invoices::load(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !card_payments_open(state, &mut tx, &inv).await? {
        return Err(ApiError::conflict(
            "card_payments_closed",
            if inv.outstanding.minor <= 0 {
                "this invoice has nothing left to pay"
            } else {
                "this invoice cannot be paid by card; use the bank details on it"
            },
        ));
    }
    let Some(account) = core_db::stripe::account(&mut tx).await? else {
        return Err(ApiError::NotFound);
    };
    let now = Utc::now();
    let current = core_db::stripe::invoice_session(&mut tx, id).await?;
    if let Some(s) = &current {
        if s.amount_minor == inv.outstanding.minor
            && s.expires_at > now + Duration::minutes(SESSION_MARGIN_MINS)
        {
            tx.commit().await?;
            return Ok(PaymentLink { url: s.url.clone() });
        }
        // An older amount: close it so it cannot also be paid.
        if let Err(e) = client.expire_checkout(&account.account_id, &s.id).await {
            tracing::warn!(error = %e, session = %s.id, "could not expire an old Checkout session");
        }
    }
    let w = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let reference = inv.reference.clone().unwrap_or_default();
    let description: String = format!("Invoice {reference} from {}", w.name)
        .chars()
        .take(250)
        .collect();
    let back = format!("/invoice/{token}");
    let success = state.config.app_url(&back, &[("paid", "1")]);
    let cancel = state.config.app_url(&back, &[]);
    let metadata = [
        ("invoice_id", id.to_string()),
        ("workspace_id", ctx.workspace_id.to_string()),
        ("reference", reference),
    ];
    let key = core_domain::ids::new_id().to_string();
    let session = client
        .create_checkout(
            &account.account_id,
            &NewCheckout {
                amount_minor: inv.outstanding.minor,
                currency: &inv.currency,
                description: &description,
                success_url: &success,
                cancel_url: &cancel,
                customer_email: inv.sent_to.as_deref(),
                metadata: &metadata,
                idempotency_key: &key,
            },
        )
        .await
        .map_err(stripe_error)?;
    let url = session
        .url
        .clone()
        .filter(|u| u.starts_with("https://") || cfg!(debug_assertions))
        .ok_or_else(|| ApiError::internal("stripe: the session has no payment page"))?;
    let expires_at = session
        .expires_at
        .and_then(|t| DateTime::from_timestamp(t, 0))
        .unwrap_or(now + Duration::hours(23));
    core_db::stripe::set_invoice_session(
        &mut tx,
        id,
        Some(&InvoiceSession {
            id: session.id.clone(),
            url: url.clone(),
            amount_minor: inv.outstanding.minor,
            expires_at,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(PaymentLink { url })
}

// Webhooks --------------------------------------------------------------------------------------

/// Checks and keeps an event Stripe sent; a job processes it. Events for
/// accounts no workspace has connected are acknowledged and dropped, so
/// Stripe stops retrying them.
pub async fn receive_webhook(
    state: &AppState,
    signature: Option<&str>,
    body: &[u8],
) -> ApiResult<()> {
    let client = client(state)?;
    let signed = signature.is_some_and(|sig| {
        core_engines::webhooks::verify(
            &client.config.webhook_secret,
            sig,
            body,
            Utc::now().timestamp(),
            api::SIGNATURE_TOLERANCE_SECS,
        )
    });
    if !signed {
        return Err(ApiError::bad_request(
            "stripe_signature_invalid",
            "the Stripe-Signature header does not match this body",
        ));
    }
    let payload: Value = serde_json::from_slice(body)
        .map_err(|_| ApiError::bad_request("stripe_event_invalid", "the body is not JSON"))?;
    let event: Event = serde_json::from_value(payload.clone()).map_err(|_| {
        ApiError::bad_request("stripe_event_invalid", "the body is not a Stripe event")
    })?;
    let valid_id = event.id.starts_with("evt_")
        && event.id.len() <= 132
        && event
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !valid_id || event.kind.len() > 100 {
        return Err(ApiError::bad_request(
            "stripe_event_invalid",
            "the event's id or type is not Stripe's",
        ));
    }
    let Some(account) = event.account.as_deref() else {
        return Ok(());
    };
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let Some(workspace) = core_db::stripe::account_workspace(&mut tx, account).await? else {
        tracing::info!(account, event = %event.id, "a Stripe event for an account no workspace has");
        tx.commit().await?;
        return Ok(());
    };
    core_db::set_context(&mut tx, Some(workspace), None).await?;
    if core_db::stripe::insert_event(&mut tx, &event.id, &event.kind, &payload).await? {
        core_db::jobs::enqueue(
            &mut tx,
            core_db::jobs::NewJob::new(
                Some(workspace),
                JOB_EVENT,
                &json!({ "event_id": event.id }),
            )
            .dedupe(&event.id),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Processes one kept event, once; returns what came of it.
pub async fn process_event(
    state: &AppState,
    workspace: WorkspaceId,
    event_id: &str,
) -> ApiResult<String> {
    let ctx = Ctx::system(workspace);
    let mut tx = begin(state, &ctx).await?;
    let Some(stored) = core_db::stripe::lock_event(&mut tx, event_id).await? else {
        return Ok("no such event".into());
    };
    if stored.processed_at.is_some() {
        return Ok("already processed".into());
    }
    let outcome = match serde_json::from_value::<Event>(stored.payload) {
        Err(_) => "not understood".to_owned(),
        Ok(event) => match core_db::stripe::account(&mut tx).await? {
            None => "no account is connected".to_owned(),
            Some(a) if event.account.as_deref() != Some(a.account_id.as_str()) => {
                "for an account no longer connected".to_owned()
            }
            Some(a) if a.disconnected_at.is_some() => "the account is disconnected".to_owned(),
            Some(a) if a.livemode != event.livemode => {
                "a test-mode event for a live account, or the other way round".to_owned()
            }
            Some(a) => handle(state, &mut tx, &ctx, &a, &event).await?,
        },
    };
    core_db::stripe::finish_event(&mut tx, event_id, &outcome).await?;
    tx.commit().await?;
    Ok(outcome)
}

fn invoice_of(session: &CheckoutSession) -> Option<Uuid> {
    session
        .metadata
        .get("invoice_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
}

async fn handle(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    account: &StripeAccount,
    event: &Event,
) -> ApiResult<String> {
    let object = &event.data.object;
    match event.kind.as_str() {
        "checkout.session.completed" | "checkout.session.async_payment_succeeded" => {
            let session: CheckoutSession = serde_json::from_value(object.clone())
                .map_err(|e| ApiError::invalid("data.object", e.to_string()))?;
            if session.payment_status.as_deref() != Some("paid") {
                return Ok("waiting for the payment to clear".into());
            }
            record_payment(tx, ctx, &session).await
        }
        "checkout.session.async_payment_failed" | "checkout.session.expired" => {
            let session: CheckoutSession = serde_json::from_value(object.clone())
                .map_err(|e| ApiError::invalid("data.object", e.to_string()))?;
            if let Some(invoice) = invoice_of(&session) {
                core_db::stripe::clear_invoice_session(tx, invoice, &session.id).await?;
            }
            Ok(if event.kind.ends_with("failed") {
                "the payment failed".into()
            } else {
                "the session expired".into()
            })
        }
        "charge.refunded" => record_refund(tx, ctx, object).await,
        "account.updated" => {
            let a: api::Account = serde_json::from_value(object.clone())
                .map_err(|e| ApiError::invalid("data.object", e.to_string()))?;
            if a.id != account.account_id {
                return Ok("for another account".into());
            }
            let (currency, name) = account_state(&a);
            let before = status_of(state, Some(account.clone()));
            core_db::stripe::update_account(
                tx,
                &a.id,
                &AccountState {
                    charges_enabled: a.charges_enabled,
                    details_submitted: a.details_submitted,
                    default_currency: currency.as_deref(),
                    display_name: name.as_deref(),
                },
            )
            .await?;
            let after = status_of(state, core_db::stripe::account(tx).await?);
            record(
                tx,
                ctx,
                Change {
                    topic: "stripe.updated",
                    resource: ENTITY,
                    object_id: None,
                    before: Some(to_json(&before)),
                    after: Some(to_json(&after)),
                    event_extra: None,
                },
            )
            .await?;
            Ok("account updated".into())
        }
        "account.application.deauthorized" => {
            let before = status_of(state, Some(account.clone()));
            if core_db::stripe::disconnect(tx, &account.account_id).await? {
                core_db::stripe::clear_all_sessions(tx).await?;
                let after = status_of(state, core_db::stripe::account(tx).await?);
                record(
                    tx,
                    ctx,
                    Change {
                        topic: "stripe.disconnected",
                        resource: ENTITY,
                        object_id: None,
                        before: Some(to_json(&before)),
                        after: Some(to_json(&after)),
                        event_extra: Some(json!({ "by": "stripe" })),
                    },
                )
                .await?;
            }
            Ok("disconnected in Stripe".into())
        }
        _ => Ok("ignored".into()),
    }
}

/// A paid session becomes a card payment, allocated to its invoice as far
/// as the invoice still owes. Money beyond that (the invoice was paid
/// another way meanwhile) stays unallocated for the client to refund.
async fn record_payment(tx: &mut Tx, ctx: &Ctx, session: &CheckoutSession) -> ApiResult<String> {
    let Some(intent) = session.payment_intent.as_deref() else {
        return Ok("the session has no payment".into());
    };
    let (Some(amount), Some(currency)) = (session.amount_total, session.currency.as_deref()) else {
        return Ok("the session has no amount".into());
    };
    let Some(invoice_id) = invoice_of(session) else {
        return Ok("not for an OnHold invoice".into());
    };
    if amount <= 0 {
        return Ok("nothing was paid".into());
    }
    let currency = currency.to_ascii_uppercase();
    let inv = match core_db::invoices::lock(tx, invoice_id).await? {
        Some(_) => super::invoices::load(tx, invoice_id).await?,
        None => None,
    };
    if core_db::stripe::payment_for_intent(tx, intent)
        .await?
        .is_some()
    {
        return Ok("already recorded".into());
    }
    let allocate = inv.as_ref().and_then(|inv| {
        (inv.kind == "invoice"
            && inv.status.is_issued()
            && inv.currency == currency
            && inv.outstanding.minor > 0)
            .then(|| AllocationInput {
                invoice_id,
                amount_minor: Some(amount.min(inv.outstanding.minor)),
            })
    });
    let reference = inv.as_ref().and_then(|i| i.reference.clone());
    let payment = super::payments::create_in(
        tx,
        ctx,
        NewPayment {
            amount: Money {
                minor: amount,
                currency,
            },
            method: None,
            paid_on: None,
            reference: Some(intent.to_owned()),
            organisation_id: inv.as_ref().and_then(|i| i.payer_organisation_id),
            booking_id: inv.as_ref().and_then(|i| i.booking_id),
            allocations: allocate.into_iter().collect(),
            notes: Some(format!(
                "Paid by card through Stripe for {}.",
                reference.as_deref().unwrap_or("an invoice")
            )),
        },
        Some("stripe"),
    )
    .await?;
    core_db::stripe::set_payment_intent(tx, payment, intent).await?;
    core_db::stripe::clear_invoice_session(tx, invoice_id, &session.id).await?;
    Ok(format!("payment {payment} recorded"))
}

#[derive(Debug, Deserialize)]
struct Charge {
    payment_intent: Option<String>,
    #[serde(default)]
    amount_refunded: i64,
}

/// A refund made in Stripe: what was refunded beyond the refunds already
/// recorded is taken off the invoice (its allocation shrinks, so it owes
/// again) and recorded as a refund of the payment.
async fn record_refund(tx: &mut Tx, ctx: &Ctx, object: &Value) -> ApiResult<String> {
    let charge: Charge = serde_json::from_value(object.clone())
        .map_err(|e| ApiError::invalid("data.object", e.to_string()))?;
    let Some(intent) = charge.payment_intent.as_deref() else {
        return Ok("the charge has no payment".into());
    };
    let Some(payment_id) = core_db::stripe::payment_for_intent(tx, intent).await? else {
        return Ok("no payment was recorded for it".into());
    };
    core_db::payments::lock(tx, payment_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let payment = super::payments::load(tx, payment_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if payment.voided_at.is_some() {
        return Ok("the payment was voided here".into());
    }
    let already = core_db::payments::refunded(tx, payment_id).await?;
    let delta = charge.amount_refunded - already;
    if delta <= 0 {
        return Ok("already recorded".into());
    }
    let delta = delta.min(payment.amount.minor - already);
    // Free enough of the payment from its invoices, newest allocation first.
    let mut free = payment.unallocated.minor;
    let mut touched = Vec::new();
    for a in payment.allocations.iter().rev() {
        if free >= delta {
            break;
        }
        core_db::invoices::lock(tx, a.invoice_id).await?;
        core_db::payments::remove_allocation(tx, a.id, ctx.user_id()).await?;
        core_db::payments::add_allocated(tx, payment_id, -a.amount_minor).await?;
        free += a.amount_minor;
        touched.push((a.invoice_id, a.amount_minor));
    }
    // Put back what was freed beyond the refund.
    if let Some(&(invoice, _)) = touched.last() {
        let keep = free - delta;
        if keep > 0 {
            core_db::payments::allocate(tx, payment_id, invoice, keep, ctx.user_id()).await?;
            core_db::payments::add_allocated(tx, payment_id, keep).await?;
        }
    }
    if !touched.is_empty() {
        let after = super::payments::load(tx, payment_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        record(
            tx,
            ctx,
            Change {
                topic: "payment.unallocated",
                resource: super::payments::ENTITY,
                object_id: Some(payment_id),
                before: Some(super::payments::to_json(&payment)),
                after: Some(super::payments::to_json(&after)),
                event_extra: None,
            },
        )
        .await?;
        for (invoice, _) in &touched {
            super::invoices::recompute(tx, ctx, *invoice).await?;
        }
    }
    let refund = super::payments::refund_in(
        tx,
        ctx,
        payment_id,
        NewRefund {
            amount_minor: delta,
            method: Some("stripe".into()),
            paid_on: None,
            reference: Some(intent.to_owned()),
            notes: Some("Refunded in Stripe.".into()),
        },
    )
    .await?;
    Ok(format!("refund {refund} recorded"))
}
