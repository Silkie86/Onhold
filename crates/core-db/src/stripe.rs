//! Stripe Connect: the workspace's connected account, the events Stripe
//! sent, each invoice's current Checkout session and the PaymentIntent
//! behind a card payment.

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct StripeAccount {
    pub account_id: String,
    pub livemode: bool,
    pub charges_enabled: bool,
    pub details_submitted: bool,
    pub default_currency: Option<String>,
    pub display_name: Option<String>,
    pub connected_at: DateTime<Utc>,
    pub connected_by: Option<UserId>,
    pub disconnected_at: Option<DateTime<Utc>>,
}

/// What Stripe says about an account.
#[derive(Debug, Clone)]
pub struct AccountState<'a> {
    pub charges_enabled: bool,
    pub details_submitted: bool,
    pub default_currency: Option<&'a str>,
    pub display_name: Option<&'a str>,
}

/// The workspace's connected account, else the one it last had.
pub async fn account(conn: &mut PgConnection) -> Result<Option<StripeAccount>> {
    Ok(sqlx::query_as!(
        StripeAccount,
        r#"SELECT account_id, livemode, charges_enabled, details_submitted,
                  default_currency::text AS default_currency, display_name, connected_at,
                  connected_by AS "connected_by: UserId", disconnected_at
           FROM stripe_accounts WHERE workspace_id = app_workspace_id()
           ORDER BY disconnected_at IS NULL DESC, connected_at DESC LIMIT 1"#
    )
    .fetch_optional(conn)
    .await?)
}

/// One account the workspace has or had.
pub async fn account_by_id(
    conn: &mut PgConnection,
    account_id: &str,
) -> Result<Option<StripeAccount>> {
    Ok(sqlx::query_as!(
        StripeAccount,
        r#"SELECT account_id, livemode, charges_enabled, details_submitted,
                  default_currency::text AS default_currency, display_name, connected_at,
                  connected_by AS "connected_by: UserId", disconnected_at
           FROM stripe_accounts WHERE workspace_id = app_workspace_id() AND account_id = $1"#,
        account_id
    )
    .fetch_optional(conn)
    .await?)
}

/// The workspace an account's events belong to, from any workspace: the
/// one it is connected to, else the one it was last connected to.
pub async fn account_workspace(
    conn: &mut PgConnection,
    account_id: &str,
) -> Result<Option<WorkspaceId>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT stripe_account_workspace($1) AS "w: WorkspaceId""#,
        account_id
    )
    .fetch_one(conn)
    .await?)
}

/// The workspace an event belongs to, from any workspace: the one its
/// Checkout session (`session`) names (`workspace`), if that workspace made
/// the session on this account; else the one holding the payment it is
/// about (`intent`), so long as that workspace has had the account;
/// otherwise as [`account_workspace`].
pub async fn event_workspace(
    conn: &mut PgConnection,
    account_id: &str,
    workspace: Option<Uuid>,
    session: Option<&str>,
    intent: Option<&str>,
) -> Result<Option<WorkspaceId>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT stripe_event_workspace($1, $2, $3, $4) AS "w: WorkspaceId""#,
        account_id,
        workspace,
        session,
        intent
    )
    .fetch_one(conn)
    .await?)
}

/// The workspace an account is connected to now, from any workspace.
pub async fn account_live_workspace(
    conn: &mut PgConnection,
    account_id: &str,
) -> Result<Option<WorkspaceId>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT stripe_account_live_workspace($1) AS "w: WorkspaceId""#,
        account_id
    )
    .fetch_one(conn)
    .await?)
}

/// Whether a kept event still waits to be processed.
pub async fn event_pending(conn: &mut PgConnection, id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT processed_at IS NULL AS "p!" FROM stripe_events
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?
    .unwrap_or(false))
}

/// Connects an account (again). The caller disconnects any other first.
pub async fn connect(
    conn: &mut PgConnection,
    account_id: &str,
    livemode: bool,
    state: &AccountState<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO stripe_accounts
               (workspace_id, account_id, livemode, charges_enabled, details_submitted,
                default_currency, display_name, connected_by)
           VALUES (app_workspace_id(), $1, $2, $3, $4, $5, $6, $7)
           ON CONFLICT (workspace_id, account_id) DO UPDATE SET
               livemode = EXCLUDED.livemode, charges_enabled = EXCLUDED.charges_enabled,
               details_submitted = EXCLUDED.details_submitted,
               default_currency = EXCLUDED.default_currency,
               display_name = EXCLUDED.display_name, connected_by = EXCLUDED.connected_by,
               connected_at = now(), state_at = now(), disconnected_at = NULL"#,
        account_id,
        livemode,
        state.charges_enabled,
        state.details_submitted,
        state.default_currency,
        state.display_name,
        by as Option<UserId>,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Stripe's word on the connected account as of `at`; an older word than
/// the one kept changes nothing.
pub async fn update_account(
    conn: &mut PgConnection,
    account_id: &str,
    state: &AccountState<'_>,
    at: DateTime<Utc>,
) -> Result<bool> {
    let n = sqlx::query!(
        r#"UPDATE stripe_accounts SET charges_enabled = $2, details_submitted = $3,
               default_currency = $4, display_name = $5, state_at = $6
           WHERE workspace_id = app_workspace_id() AND account_id = $1
             AND disconnected_at IS NULL AND state_at <= $6"#,
        account_id,
        state.charges_enabled,
        state.details_submitted,
        state.default_currency,
        state.display_name,
        at,
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// Marks the account disconnected; it stays for the record.
pub async fn disconnect(conn: &mut PgConnection, account_id: &str) -> Result<bool> {
    let n = sqlx::query!(
        r#"UPDATE stripe_accounts SET disconnected_at = now(), charges_enabled = false
           WHERE workspace_id = app_workspace_id() AND account_id = $1
             AND disconnected_at IS NULL"#,
        account_id
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// Keeps an event Stripe sent; false if it was already here.
pub async fn insert_event(
    conn: &mut PgConnection,
    id: &str,
    kind: &str,
    payload: &Value,
) -> Result<bool> {
    let n = sqlx::query!(
        r#"INSERT INTO stripe_events (id, workspace_id, type, payload)
           VALUES ($1, app_workspace_id(), $2, $3) ON CONFLICT DO NOTHING"#,
        id,
        kind,
        payload
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n > 0)
}

#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub id: String,
    pub kind: String,
    pub payload: Value,
    pub processed_at: Option<DateTime<Utc>>,
}

/// An event, locked while it is processed.
pub async fn lock_event(conn: &mut PgConnection, id: &str) -> Result<Option<StoredEvent>> {
    Ok(sqlx::query_as!(
        StoredEvent,
        r#"SELECT id, type AS kind, payload, processed_at FROM stripe_events
           WHERE workspace_id = app_workspace_id() AND id = $1 FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn finish_event(conn: &mut PgConnection, id: &str, outcome: &str) -> Result<()> {
    sqlx::query!(
        r#"UPDATE stripe_events SET processed_at = now(), outcome = $2
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        outcome
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The payer's current Checkout session for an invoice.
#[derive(Debug, Clone)]
pub struct InvoiceSession {
    pub id: String,
    pub url: String,
    pub amount_minor: i64,
    pub expires_at: DateTime<Utc>,
    /// The connected account it was made on.
    pub account_id: String,
}

pub async fn invoice_session(
    conn: &mut PgConnection,
    invoice_id: Uuid,
) -> Result<Option<InvoiceSession>> {
    let row = sqlx::query!(
        r#"SELECT stripe_session_id, stripe_session_url, stripe_session_amount,
                  stripe_session_expires_at, stripe_session_account
           FROM invoices WHERE workspace_id = app_workspace_id() AND id = $1"#,
        invoice_id
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.and_then(|r| {
        Some(InvoiceSession {
            id: r.stripe_session_id?,
            url: r.stripe_session_url?,
            amount_minor: r.stripe_session_amount?,
            expires_at: r.stripe_session_expires_at?,
            account_id: r.stripe_session_account?,
        })
    }))
}

/// Sets or (with `None`) forgets an invoice's session.
pub async fn set_invoice_session(
    conn: &mut PgConnection,
    invoice_id: Uuid,
    session: Option<&InvoiceSession>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE invoices SET stripe_session_id = $2, stripe_session_url = $3,
               stripe_session_amount = $4, stripe_session_expires_at = $5,
               stripe_session_account = $6
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        invoice_id,
        session.map(|s| s.id.as_str()),
        session.map(|s| s.url.as_str()),
        session.map(|s| s.amount_minor),
        session.map(|s| s.expires_at),
        session.map(|s| s.account_id.as_str()),
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Notes a Checkout session made here for an invoice, on an account.
pub async fn issue_session(
    conn: &mut PgConnection,
    invoice_id: Uuid,
    session_id: &str,
    account_id: &str,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO stripe_checkout_sessions (workspace_id, id, account_id, invoice_id)
           VALUES (app_workspace_id(), $1, $2, $3) ON CONFLICT DO NOTHING"#,
        session_id,
        account_id,
        invoice_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The invoice a Checkout session was made here for, if this workspace
/// made it on that account.
pub async fn issued_session(
    conn: &mut PgConnection,
    session_id: &str,
    account_id: &str,
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT invoice_id FROM stripe_checkout_sessions
           WHERE workspace_id = app_workspace_id() AND id = $1 AND account_id = $2"#,
        session_id,
        account_id
    )
    .fetch_optional(conn)
    .await?)
}

/// Forgets an invoice's session if it is still the one named.
pub async fn clear_invoice_session(
    conn: &mut PgConnection,
    invoice_id: Uuid,
    session_id: &str,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE invoices SET stripe_session_id = NULL, stripe_session_url = NULL,
               stripe_session_amount = NULL, stripe_session_expires_at = NULL,
               stripe_session_account = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND stripe_session_id = $2"#,
        invoice_id,
        session_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The payment recorded for a PaymentIntent.
pub async fn payment_for_intent(conn: &mut PgConnection, intent: &str) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM payments WHERE workspace_id = app_workspace_id()
             AND stripe_payment_intent = $1 AND kind = 'payment'"#,
        intent
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn set_payment_intent(
    conn: &mut PgConnection,
    payment_id: Uuid,
    intent: &str,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE payments SET stripe_payment_intent = $2
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        payment_id,
        intent
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Every open session made on an account, to close when it is disconnected.
pub async fn sessions_on(conn: &mut PgConnection, account_id: &str) -> Result<Vec<(Uuid, String)>> {
    Ok(sqlx::query!(
        r#"SELECT id, stripe_session_id AS "session!" FROM invoices
           WHERE workspace_id = app_workspace_id() AND stripe_session_account = $1
             AND stripe_session_id IS NOT NULL"#,
        account_id
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| (r.id, r.session))
    .collect())
}

/// What has been recorded as refunded in Stripe of a card payment: its
/// live refunds made through Stripe, not ones recorded by hand.
pub async fn refunded_in_stripe(
    conn: &mut PgConnection,
    payment_id: Uuid,
    intent: &str,
) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT coalesce(sum(amount_minor), 0)::bigint AS "s!" FROM payments
           WHERE workspace_id = app_workspace_id() AND refund_of = $1 AND kind = 'refund'
             AND method = 'stripe' AND reference = $2 AND voided_at IS NULL"#,
        payment_id,
        intent
    )
    .fetch_one(conn)
    .await?)
}

/// Whether a payment event for a PaymentIntent is kept but not yet processed.
pub async fn payment_event_pending(conn: &mut PgConnection, intent: &str) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM stripe_events
             WHERE workspace_id = app_workspace_id() AND processed_at IS NULL
               AND type LIKE 'checkout.session.%'
               AND payload #>> '{data,object,payment_intent}' = $1) AS "p!""#,
        intent
    )
    .fetch_one(conn)
    .await?)
}
