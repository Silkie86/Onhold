//! Settlements: what each talent is owed for a booking once the agency's
//! commission and expenses come off.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Settlement {
    pub id: Uuid,
    pub booking_id: Uuid,
    pub booking_title: String,
    pub talent_id: Uuid,
    pub talent_name: String,
    pub status: String,
    pub currency: String,
    pub gross_minor: i64,
    pub commission_bps: i32,
    pub commission_minor: i64,
    pub expenses: Value,
    pub expenses_minor: i64,
    pub net_minor: i64,
    /// Payouts recorded against it, not voided.
    pub paid_out_minor: i64,
    pub notes: Option<String>,
    pub finalised_at: Option<DateTime<Utc>>,
    pub document_id: Option<Uuid>,
    pub commission_invoice_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn list(
    conn: &mut PgConnection,
    booking_id: Option<Uuid>,
    talent_id: Option<Uuid>,
    status: Option<&str>,
) -> Result<Vec<Settlement>> {
    Ok(sqlx::query_as!(
        Settlement,
        r#"SELECT s.id, s.booking_id, b.title AS booking_title, s.talent_id, t.name AS talent_name, s.status,
                  s.currency, s.gross_minor, s.commission_bps, s.commission_minor, s.expenses, s.expenses_minor,
                  s.net_minor,
                  (SELECT coalesce(sum(p.amount_minor), 0) FROM payments p
                   WHERE p.settlement_id = s.id AND p.kind = 'payout' AND p.voided_at IS NULL)::bigint
                      AS "paid_out_minor!",
                  s.notes, s.finalised_at, s.document_id, s.commission_invoice_id, s.version, s.created_at,
                  s.updated_at
           FROM settlements s JOIN bookings b ON b.id = s.booking_id JOIN talent t ON t.id = s.talent_id
           WHERE s.workspace_id = app_workspace_id() AND s.deleted_at IS NULL
               AND ($1::uuid IS NULL OR s.booking_id = $1) AND ($2::uuid IS NULL OR s.talent_id = $2)
               AND ($3::text IS NULL OR s.status = $3)
             ORDER BY b.starts_at DESC, t.name
             LIMIT 500"#,
        booking_id,
        talent_id,
        status
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Settlement>> {
    Ok(sqlx::query_as!(
        Settlement,
        r#"SELECT s.id, s.booking_id, b.title AS booking_title, s.talent_id, t.name AS talent_name, s.status,
                  s.currency, s.gross_minor, s.commission_bps, s.commission_minor, s.expenses, s.expenses_minor,
                  s.net_minor,
                  (SELECT coalesce(sum(p.amount_minor), 0) FROM payments p
                   WHERE p.settlement_id = s.id AND p.kind = 'payout' AND p.voided_at IS NULL)::bigint
                      AS "paid_out_minor!",
                  s.notes, s.finalised_at, s.document_id, s.commission_invoice_id, s.version, s.created_at,
                  s.updated_at
           FROM settlements s JOIN bookings b ON b.id = s.booking_id JOIN talent t ON t.id = s.talent_id
           WHERE s.workspace_id = app_workspace_id() AND s.id = $1 AND s.deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM settlements
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn find(
    conn: &mut PgConnection,
    booking_id: Uuid,
    talent_id: Uuid,
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM settlements
           WHERE workspace_id = app_workspace_id() AND booking_id = $1 AND talent_id = $2 AND deleted_at IS NULL"#,
        booking_id,
        talent_id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct Values<'a> {
    pub currency: &'a str,
    pub gross_minor: i64,
    pub commission_bps: i32,
    pub commission_minor: i64,
    pub expenses: &'a Value,
    pub expenses_minor: i64,
    pub net_minor: i64,
    pub notes: Option<&'a str>,
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    booking_id: Uuid,
    talent_id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO settlements (id, workspace_id, booking_id, talent_id, currency, gross_minor, commission_bps,
                                    commission_minor, expenses, expenses_minor, net_minor, notes, created_by,
                                    updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12)"#,
        id,
        booking_id,
        talent_id,
        v.currency,
        v.gross_minor,
        v.commission_bps,
        v.commission_minor,
        v.expenses,
        v.expenses_minor,
        v.net_minor,
        v.notes,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE settlements SET currency = $2, gross_minor = $3, commission_bps = $4, commission_minor = $5,
                  expenses = $6, expenses_minor = $7, net_minor = $8, notes = $9, updated_by = $10
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.currency,
        v.gross_minor,
        v.commission_bps,
        v.commission_minor,
        v.expenses,
        v.expenses_minor,
        v.net_minor,
        v.notes,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn finalise(
    conn: &mut PgConnection,
    id: Uuid,
    document_id: Option<Uuid>,
    commission_invoice_id: Option<Uuid>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE settlements SET status = 'final', finalised_at = now(), finalised_by = $4, document_id = $2,
                  commission_invoice_id = $3, updated_by = $4
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        document_id,
        commission_invoice_id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Back to draft, to correct it (its statement stays as a past version).
pub async fn reopen(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE settlements SET status = 'draft', finalised_at = NULL, finalised_by = NULL, commission_invoice_id = NULL,
                  updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE settlements SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL AND status = 'draft'"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}
