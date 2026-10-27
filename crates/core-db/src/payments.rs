//! Money received, refunded and paid out, and which invoices each payment
//! pays.

use chrono::{DateTime, NaiveDate, Utc};
use core_domain::UserId;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Payment {
    pub id: Uuid,
    pub kind: String,
    pub amount_minor: i64,
    pub currency: String,
    pub method: String,
    pub paid_on: NaiveDate,
    pub reference: Option<String>,
    pub organisation_id: Option<Uuid>,
    pub organisation_name: Option<String>,
    pub talent_id: Option<Uuid>,
    pub talent_name: Option<String>,
    pub booking_id: Option<Uuid>,
    pub refund_of: Option<Uuid>,
    pub settlement_id: Option<Uuid>,
    pub allocated_minor: i64,
    pub notes: Option<String>,
    pub voided_at: Option<DateTime<Utc>>,
    pub void_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
}

#[derive(Debug, Default)]
pub struct Filter<'a> {
    pub kind: Option<&'a str>,
    pub booking_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub talent_id: Option<Uuid>,
    pub invoice_id: Option<Uuid>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub offset: i64,
    pub limit: i64,
}

pub async fn list(conn: &mut PgConnection, f: Filter<'_>) -> Result<(Vec<Payment>, i64)> {
    let rows = sqlx::query!(
        r#"SELECT p.id, p.kind, p.amount_minor, p.currency, p.method, p.paid_on, p.reference,
                  p.organisation_id, o.name AS "organisation_name?", p.talent_id, t.name AS "talent_name?",
                  p.booking_id, p.refund_of, p.settlement_id, p.allocated_minor, p.notes, p.voided_at,
                  p.void_reason, p.version, p.created_at, p.created_by AS "created_by: UserId",
                  count(*) OVER () AS "total!"
           FROM payments p
           LEFT JOIN organisations o ON o.id = p.organisation_id
           LEFT JOIN talent t ON t.id = p.talent_id
           WHERE p.workspace_id = app_workspace_id()
             AND ($1::text IS NULL OR p.kind = $1)
             AND ($2::uuid IS NULL OR p.booking_id = $2)
             AND ($3::uuid IS NULL OR p.organisation_id = $3)
             AND ($4::uuid IS NULL OR p.talent_id = $4)
             AND ($5::uuid IS NULL OR EXISTS (SELECT 1 FROM payment_allocations a
                  WHERE a.payment_id = p.id AND a.invoice_id = $5 AND a.removed_at IS NULL))
             AND ($6::date IS NULL OR p.paid_on >= $6)
             AND ($7::date IS NULL OR p.paid_on <= $7)
           ORDER BY p.paid_on DESC, p.created_at DESC
           OFFSET $8 LIMIT $9"#,
        f.kind,
        f.booking_id,
        f.organisation_id,
        f.talent_id,
        f.invoice_id,
        f.from,
        f.to,
        f.offset,
        f.limit
    )
    .fetch_all(conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    let items = rows
        .into_iter()
        .map(|r| Payment {
            id: r.id,
            kind: r.kind,
            amount_minor: r.amount_minor,
            currency: r.currency,
            method: r.method,
            paid_on: r.paid_on,
            reference: r.reference,
            organisation_id: r.organisation_id,
            organisation_name: r.organisation_name,
            talent_id: r.talent_id,
            talent_name: r.talent_name,
            booking_id: r.booking_id,
            refund_of: r.refund_of,
            settlement_id: r.settlement_id,
            allocated_minor: r.allocated_minor,
            notes: r.notes,
            voided_at: r.voided_at,
            void_reason: r.void_reason,
            version: r.version,
            created_at: r.created_at,
            created_by: r.created_by,
        })
        .collect();
    Ok((items, total))
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Payment>> {
    let r = sqlx::query!(
        r#"SELECT p.id, p.kind, p.amount_minor, p.currency, p.method, p.paid_on, p.reference,
                  p.organisation_id, o.name AS "organisation_name?", p.talent_id, t.name AS "talent_name?",
                  p.booking_id, p.refund_of, p.settlement_id, p.allocated_minor, p.notes, p.voided_at,
                  p.void_reason, p.version, p.created_at, p.created_by AS "created_by: UserId",
                  count(*) OVER () AS "total!"
           FROM payments p
           LEFT JOIN organisations o ON o.id = p.organisation_id
           LEFT JOIN talent t ON t.id = p.talent_id
           WHERE p.workspace_id = app_workspace_id() AND p.id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?;
    Ok(r.map(|r| Payment {
        id: r.id,
        kind: r.kind,
        amount_minor: r.amount_minor,
        currency: r.currency,
        method: r.method,
        paid_on: r.paid_on,
        reference: r.reference,
        organisation_id: r.organisation_id,
        organisation_name: r.organisation_name,
        talent_id: r.talent_id,
        talent_name: r.talent_name,
        booking_id: r.booking_id,
        refund_of: r.refund_of,
        settlement_id: r.settlement_id,
        allocated_minor: r.allocated_minor,
        notes: r.notes,
        voided_at: r.voided_at,
        void_reason: r.void_reason,
        version: r.version,
        created_at: r.created_at,
        created_by: r.created_by,
    }))
}

pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM payments WHERE workspace_id = app_workspace_id() AND id = $1 FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct NewPayment<'a> {
    pub id: Uuid,
    pub kind: &'a str,
    pub amount_minor: i64,
    pub currency: &'a str,
    pub method: &'a str,
    pub paid_on: NaiveDate,
    pub reference: Option<&'a str>,
    pub organisation_id: Option<Uuid>,
    pub talent_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
    pub refund_of: Option<Uuid>,
    pub settlement_id: Option<Uuid>,
    pub notes: Option<&'a str>,
}

pub async fn insert(conn: &mut PgConnection, p: NewPayment<'_>, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO payments (id, workspace_id, kind, amount_minor, currency, method, paid_on, reference,
                                 organisation_id, talent_id, booking_id, refund_of, settlement_id, notes,
                                 created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $14)"#,
        p.id,
        p.kind,
        p.amount_minor,
        p.currency,
        p.method,
        p.paid_on,
        p.reference,
        p.organisation_id,
        p.talent_id,
        p.booking_id,
        p.refund_of,
        p.settlement_id,
        p.notes,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Changes how much of a payment is used (allocated or refunded).
pub async fn add_allocated(conn: &mut PgConnection, id: Uuid, delta: i64) -> Result<()> {
    sqlx::query!(
        r#"UPDATE payments SET allocated_minor = allocated_minor + $2
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        delta
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn void(
    conn: &mut PgConnection,
    id: Uuid,
    reason: Option<&str>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE payments SET voided_at = now(), void_reason = $2, allocated_minor = 0, updated_by = $3
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        reason,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Refunds made against a payment, not voided.
pub async fn refunded(conn: &mut PgConnection, id: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT coalesce(sum(amount_minor), 0)::bigint AS "s!" FROM payments
           WHERE workspace_id = app_workspace_id() AND refund_of = $1 AND voided_at IS NULL"#,
        id
    )
    .fetch_one(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct Allocation {
    pub id: Uuid,
    pub payment_id: Uuid,
    pub invoice_id: Uuid,
    pub invoice_reference: Option<String>,
    pub amount_minor: i64,
    pub paid_on: NaiveDate,
    pub method: String,
    pub created_at: DateTime<Utc>,
}

/// Live allocations of a payment, or to an invoice.
pub async fn allocations(
    conn: &mut PgConnection,
    payment_id: Option<Uuid>,
    invoice_id: Option<Uuid>,
) -> Result<Vec<Allocation>> {
    Ok(sqlx::query_as!(
        Allocation,
        r#"SELECT a.id, a.payment_id, a.invoice_id, i.reference AS "invoice_reference?", a.amount_minor,
                  p.paid_on, p.method, a.created_at
           FROM payment_allocations a
           JOIN payments p ON p.id = a.payment_id
           JOIN invoices i ON i.id = a.invoice_id
           WHERE a.workspace_id = app_workspace_id() AND a.removed_at IS NULL
             AND ($1::uuid IS NULL OR a.payment_id = $1) AND ($2::uuid IS NULL OR a.invoice_id = $2)
           ORDER BY a.created_at"#,
        payment_id,
        invoice_id
    )
    .fetch_all(conn)
    .await?)
}

pub async fn allocate(
    conn: &mut PgConnection,
    payment_id: Uuid,
    invoice_id: Uuid,
    amount_minor: i64,
    by: Option<UserId>,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query!(
        r#"INSERT INTO payment_allocations (id, workspace_id, payment_id, invoice_id, amount_minor, created_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5)"#,
        id,
        payment_id,
        invoice_id,
        amount_minor,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(id)
}

/// Takes an allocation back; returns (payment, invoice, amount) when it was live.
pub async fn remove_allocation(
    conn: &mut PgConnection,
    id: Uuid,
    by: Option<UserId>,
) -> Result<Option<(Uuid, Uuid, i64)>> {
    Ok(sqlx::query!(
        r#"UPDATE payment_allocations SET removed_at = now(), removed_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND removed_at IS NULL
           RETURNING payment_id, invoice_id, amount_minor"#,
        id,
        by.map(|u| u.0)
    )
    .fetch_optional(conn)
    .await?
    .map(|r| (r.payment_id, r.invoice_id, r.amount_minor)))
}

/// What live allocations pay towards an invoice.
pub async fn paid_towards(conn: &mut PgConnection, invoice_id: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT coalesce(sum(a.amount_minor), 0)::bigint AS "s!" FROM payment_allocations a
           WHERE a.workspace_id = app_workspace_id() AND a.invoice_id = $1 AND a.removed_at IS NULL"#,
        invoice_id
    )
    .fetch_one(conn)
    .await?)
}
