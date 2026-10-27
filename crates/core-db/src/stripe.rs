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

/// The workspace's account, connected or not.
pub async fn account(conn: &mut PgConnection) -> Result<Option<StripeAccount>> {
    Ok(sqlx::query_as!(
        StripeAccount,
        r#"SELECT account_id, livemode, charges_enabled, details_submitted,
                  default_currency::text AS default_currency, display_name, connected_at,
                  connected_by AS "connected_by: UserId", disconnected_at
           FROM stripe_accounts WHERE workspace_id = app_workspace_id()"#
    )
    .fetch_optional(conn)
    .await?)
}

/// The workspace an account is connected to, from any workspace.
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

/// Connects an account, replacing whichever was connected before.
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
           ON CONFLICT (workspace_id) DO UPDATE SET
               account_id = EXCLUDED.account_id, livemode = EXCLUDED.livemode,
               charges_enabled = EXCLUDED.charges_enabled,
               details_submitted = EXCLUDED.details_submitted,
               default_currency = EXCLUDED.default_currency,
               display_name = EXCLUDED.display_name, connected_by = EXCLUDED.connected_by,
               connected_at = now(), disconnected_at = NULL"#,
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

/// Stripe's latest word on the connected account.
pub async fn update_account(
    conn: &mut PgConnection,
    account_id: &str,
    state: &AccountState<'_>,
) -> Result<bool> {
    let n = sqlx::query!(
        r#"UPDATE stripe_accounts SET charges_enabled = $2, details_submitted = $3,
               default_currency = $4, display_name = $5
           WHERE workspace_id = app_workspace_id() AND account_id = $1"#,
        account_id,
        state.charges_enabled,
        state.details_submitted,
        state.default_currency,
        state.display_name,
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
}

pub async fn invoice_session(
    conn: &mut PgConnection,
    invoice_id: Uuid,
) -> Result<Option<InvoiceSession>> {
    let row = sqlx::query!(
        r#"SELECT stripe_session_id, stripe_session_url, stripe_session_amount,
                  stripe_session_expires_at
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
               stripe_session_amount = $4, stripe_session_expires_at = $5
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        invoice_id,
        session.map(|s| s.id.as_str()),
        session.map(|s| s.url.as_str()),
        session.map(|s| s.amount_minor),
        session.map(|s| s.expires_at),
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Forgets an invoice's session if it is still the one named.
pub async fn clear_invoice_session(
    conn: &mut PgConnection,
    invoice_id: Uuid,
    session_id: &str,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE invoices SET stripe_session_id = NULL, stripe_session_url = NULL,
               stripe_session_amount = NULL, stripe_session_expires_at = NULL
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

/// Forgets every invoice's session, when the account changes.
pub async fn clear_all_sessions(conn: &mut PgConnection) -> Result<()> {
    sqlx::query!(
        r#"UPDATE invoices SET stripe_session_id = NULL, stripe_session_url = NULL,
               stripe_session_amount = NULL, stripe_session_expires_at = NULL
           WHERE workspace_id = app_workspace_id() AND stripe_session_id IS NOT NULL"#
    )
    .execute(conn)
    .await?;
    Ok(())
}
