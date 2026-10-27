//! Finance set-up: tax codes, the workspace's invoicing settings, invoice
//! number sequences and reference exchange rates.

use chrono::{DateTime, NaiveDate, Utc};
use core_domain::UserId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

// Tax codes ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TaxCode {
    pub id: Uuid,
    pub name: String,
    pub rate_bps: i32,
    pub kind: String,
    pub note: Option<String>,
    pub is_default: bool,
    pub position: i32,
    pub archived_at: Option<DateTime<Utc>>,
    pub version: i64,
}

/// Tax codes in order; archived ones too when asked.
pub async fn tax_codes(conn: &mut PgConnection, include_archived: bool) -> Result<Vec<TaxCode>> {
    Ok(sqlx::query_as!(
        TaxCode,
        r#"SELECT id, name, rate_bps, kind, note, is_default, position, archived_at, version FROM tax_codes
           WHERE workspace_id = app_workspace_id() AND ($1 OR archived_at IS NULL)
           ORDER BY archived_at IS NOT NULL, position, name"#,
        include_archived
    )
    .fetch_all(conn)
    .await?)
}

pub async fn tax_code(conn: &mut PgConnection, id: Uuid) -> Result<Option<TaxCode>> {
    Ok(sqlx::query_as!(
        TaxCode,
        r#"SELECT id, name, rate_bps, kind, note, is_default, position, archived_at, version FROM tax_codes
           WHERE workspace_id = app_workspace_id() AND id = $1 FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct TaxCodeValues<'a> {
    pub name: &'a str,
    pub rate_bps: i32,
    pub kind: &'a str,
    pub note: Option<&'a str>,
    pub is_default: bool,
    pub position: i32,
}

pub async fn insert_tax_code(
    conn: &mut PgConnection,
    id: Uuid,
    v: TaxCodeValues<'_>,
    by: Option<UserId>,
) -> Result<()> {
    if v.is_default {
        clear_default(conn).await?;
    }
    sqlx::query!(
        r#"INSERT INTO tax_codes (id, workspace_id, name, rate_bps, kind, note, is_default, position, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $8)"#,
        id,
        v.name,
        v.rate_bps,
        v.kind,
        v.note,
        v.is_default,
        v.position,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn update_tax_code(
    conn: &mut PgConnection,
    id: Uuid,
    v: TaxCodeValues<'_>,
    archived: bool,
    by: Option<UserId>,
) -> Result<()> {
    if v.is_default && !archived {
        clear_default(conn).await?;
    }
    sqlx::query!(
        r#"UPDATE tax_codes SET name = $2, rate_bps = $3, kind = $4, note = $5, is_default = $6 AND NOT $8,
                  position = $7, archived_at = CASE WHEN $8 THEN coalesce(archived_at, now()) END, updated_by = $9
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.name,
        v.rate_bps,
        v.kind,
        v.note,
        v.is_default,
        v.position,
        archived,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

async fn clear_default(conn: &mut PgConnection) -> Result<()> {
    sqlx::query!(
        r#"UPDATE tax_codes SET is_default = false WHERE workspace_id = app_workspace_id() AND is_default"#
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn has_tax_codes(conn: &mut PgConnection) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM tax_codes WHERE workspace_id = app_workspace_id()) AS "e!""#
    )
    .fetch_one(conn)
    .await?)
}

// Settings ------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Settings {
    pub legal_name: Option<String>,
    pub address: Value,
    pub tax_number: Option<String>,
    pub company_number: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub invoice_format: String,
    pub credit_note_format: String,
    pub payment_terms_days: i32,
    pub invoicing_model: String,
    pub bank_accounts: Value,
    pub late_fee_text: Option<String>,
    pub invoice_footer: Option<String>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
}

/// The workspace's settings, made with the defaults on first use.
pub async fn settings(conn: &mut PgConnection) -> Result<Settings> {
    sqlx::query!(
        r#"INSERT INTO finance_settings (workspace_id) VALUES (app_workspace_id()) ON CONFLICT DO NOTHING"#
    )
    .execute(&mut *conn)
    .await?;
    Ok(sqlx::query_as!(
        Settings,
        r#"SELECT legal_name, address, tax_number, company_number, email, phone, invoice_format,
                  credit_note_format, payment_terms_days, invoicing_model, bank_accounts, late_fee_text,
                  invoice_footer, version, updated_at
           FROM finance_settings WHERE workspace_id = app_workspace_id()"#
    )
    .fetch_one(conn)
    .await?)
}

/// Locks the settings row (making it first); returns its version.
pub async fn lock_settings(conn: &mut PgConnection) -> Result<i64> {
    settings(&mut *conn).await?;
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM finance_settings WHERE workspace_id = app_workspace_id() FOR UPDATE"#
    )
    .fetch_one(conn)
    .await?)
}

pub struct SettingsValues<'a> {
    pub legal_name: Option<&'a str>,
    pub address: &'a Value,
    pub tax_number: Option<&'a str>,
    pub company_number: Option<&'a str>,
    pub email: Option<&'a str>,
    pub phone: Option<&'a str>,
    pub invoice_format: &'a str,
    pub credit_note_format: &'a str,
    pub payment_terms_days: i32,
    pub invoicing_model: &'a str,
    pub bank_accounts: &'a Value,
    pub late_fee_text: Option<&'a str>,
    pub invoice_footer: Option<&'a str>,
}

pub async fn update_settings(
    conn: &mut PgConnection,
    v: SettingsValues<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE finance_settings SET legal_name = $1, address = $2, tax_number = $3, company_number = $4,
                  email = $5, phone = $6, invoice_format = $7, credit_note_format = $8, payment_terms_days = $9,
                  invoicing_model = $10, bank_accounts = $11, late_fee_text = $12, invoice_footer = $13,
                  updated_by = $14
           WHERE workspace_id = app_workspace_id()"#,
        v.legal_name,
        v.address,
        v.tax_number,
        v.company_number,
        v.email,
        v.phone,
        v.invoice_format,
        v.credit_note_format,
        v.payment_terms_days,
        v.invoicing_model,
        v.bank_accounts,
        v.late_fee_text,
        v.invoice_footer,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

// Sequences -----------------------------------------------------------------------------

/// Takes the next number for an issuer and kind. The row stays locked until
/// the transaction ends, so two issues never share a number and a rolled-back
/// issue gives its number back.
pub async fn next_number(conn: &mut PgConnection, issuer_key: &str, kind: &str) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"INSERT INTO invoice_sequences (workspace_id, issuer_key, kind, value)
           VALUES (app_workspace_id(), $1, $2, 1)
           ON CONFLICT (workspace_id, issuer_key, kind) DO UPDATE SET value = invoice_sequences.value + 1
           RETURNING value"#,
        issuer_key,
        kind
    )
    .fetch_one(conn)
    .await?)
}

// Exchange rates --------------------------------------------------------------------------

/// One currency's rate per euro on the latest day at or before `on`:
/// (that day, the rate as decimal text).
pub async fn rate_per_eur(
    conn: &mut PgConnection,
    currency: &str,
    on: NaiveDate,
) -> Result<Option<(NaiveDate, String)>> {
    Ok(sqlx::query!(
        r#"SELECT rate_date, per_eur::text AS "per_eur!" FROM fx_rates
           WHERE currency = $1 AND rate_date <= $2 AND rate_date > $2 - 14
           ORDER BY rate_date DESC LIMIT 1"#,
        currency,
        on
    )
    .fetch_optional(conn)
    .await?
    .map(|r| (r.rate_date, r.per_eur)))
}

/// Stores a day's rates (worker role). Returns how many were new or changed.
pub async fn store_rates(
    conn: &mut PgConnection,
    day: NaiveDate,
    rates: &[(String, String)],
    source: &str,
) -> Result<u64> {
    let (currencies, values): (Vec<String>, Vec<String>) = rates.iter().cloned().unzip();
    let done = sqlx::query!(
        r#"INSERT INTO fx_rates (rate_date, currency, per_eur, source)
           SELECT $1, c, v::numeric, $4 FROM unnest($2::text[], $3::text[]) AS t (c, v)
           ON CONFLICT (currency, rate_date) DO UPDATE SET per_eur = excluded.per_eur, fetched_at = now()
           WHERE fx_rates.per_eur <> excluded.per_eur"#,
        day,
        &currencies,
        &values,
        source
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}

/// The newest day with stored rates.
pub async fn latest_rate_day(conn: &mut PgConnection) -> Result<Option<NaiveDate>> {
    Ok(sqlx::query_scalar!(r#"SELECT max(rate_date) FROM fx_rates"#)
        .fetch_one(conn)
        .await?)
}
