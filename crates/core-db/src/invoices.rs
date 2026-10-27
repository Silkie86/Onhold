//! Invoices and credit notes. Once issued, the database keeps an invoice
//! from changing what it says (`invoices_issued_frozen`); only its payment
//! state, link and PDF move on.

use chrono::{DateTime, NaiveDate, Utc};
use core_domain::{InvoiceStatus, UserId, WorkspaceId};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

/// The trigger that refuses changes to an issued invoice.
pub const ISSUED_FROZEN: &str = "invoices_issued_frozen";
/// The index that keeps numbers unique per issuer and kind.
pub const NUMBER_TAKEN: &str = "invoices_number";

#[derive(Debug, Clone)]
pub struct Invoice {
    pub id: Uuid,
    pub kind: String,
    pub purpose: String,
    pub status: InvoiceStatus,
    pub number: Option<i64>,
    pub reference: Option<String>,
    pub historical: bool,
    pub issuer_kind: String,
    pub issuer_talent_id: Option<Uuid>,
    pub payer_kind: String,
    pub payer_organisation_id: Option<Uuid>,
    pub payer_talent_id: Option<Uuid>,
    pub payer_contact_id: Option<Uuid>,
    /// The payer's name: as given, or the organisation's or talent's.
    pub payer_display: String,
    pub payer_name: Option<String>,
    pub issuer: Option<Value>,
    pub payer: Option<Value>,
    pub booking_id: Option<Uuid>,
    pub booking_title: Option<String>,
    pub credits_invoice_id: Option<Uuid>,
    pub currency: String,
    pub line_items: Value,
    pub tax_lines: Value,
    pub subtotal_minor: i64,
    pub tax_minor: i64,
    pub total_minor: i64,
    pub paid_minor: i64,
    pub credited_minor: i64,
    pub issue_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub base_currency: Option<String>,
    pub fx_rate: Option<String>,
    pub fx_date: Option<NaiveDate>,
    pub fx_source: Option<String>,
    pub base_total_minor: Option<i64>,
    pub notes: Option<String>,
    pub terms: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
    pub sent_to: Option<String>,
    pub viewed_at: Option<DateTime<Utc>>,
    pub document_id: Option<Uuid>,
    pub voided_at: Option<DateTime<Utc>>,
    pub void_reason: Option<String>,
    pub tags: Vec<String>,
    pub custom: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

#[derive(Debug, Default)]
pub struct Filter<'a> {
    pub statuses: &'a [InvoiceStatus],
    pub kind: Option<&'a str>,
    pub booking_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub talent_id: Option<Uuid>,
    pub q: Option<&'a str>,
    pub issued_from: Option<NaiveDate>,
    pub issued_to: Option<NaiveDate>,
    pub offset: i64,
    pub limit: i64,
}

pub async fn list(conn: &mut PgConnection, f: Filter<'_>) -> Result<(Vec<Invoice>, i64)> {
    let q = f.q.map(|q| format!("%{}%", q.to_lowercase().replace(['%', '_'], "")));
    let rows = sqlx::query!(
        r#"SELECT i.id, count(*) OVER () AS "total!" FROM invoices i
           WHERE i.workspace_id = app_workspace_id() AND i.deleted_at IS NULL
             AND (cardinality($1::invoice_status[]) = 0 OR i.status = ANY($1))
             AND ($2::text IS NULL OR i.kind = $2)
             AND ($3::uuid IS NULL OR i.booking_id = $3)
             AND ($4::uuid IS NULL OR i.payer_organisation_id = $4)
             AND ($5::uuid IS NULL OR i.issuer_talent_id = $5 OR i.payer_talent_id = $5
                  OR EXISTS (SELECT 1 FROM booking_talent bt WHERE bt.booking_id = i.booking_id AND bt.talent_id = $5))
             AND ($6::text IS NULL OR i.search_text LIKE $6)
             AND ($7::date IS NULL OR i.issue_date >= $7)
             AND ($8::date IS NULL OR i.issue_date <= $8)
           ORDER BY coalesce(i.issue_date, i.created_at::date) DESC, i.number DESC NULLS FIRST, i.created_at DESC
           OFFSET $9 LIMIT $10"#,
        f.statuses as &[InvoiceStatus],
        f.kind,
        f.booking_id,
        f.organisation_id,
        f.talent_id,
        q,
        f.issued_from,
        f.issued_to,
        f.offset,
        f.limit
    )
    .fetch_all(&mut *conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut items = by_ids(conn, &ids).await?;
    items.sort_by_key(|i| ids.iter().position(|id| *id == i.id));
    Ok((items, total))
}

pub async fn by_ids(conn: &mut PgConnection, ids: &[Uuid]) -> Result<Vec<Invoice>> {
    Ok(sqlx::query_as!(
        Invoice,
        r#"SELECT i.id, i.kind, i.purpose, i.status AS "status: InvoiceStatus", i.number, i.reference,
                  i.historical, i.issuer_kind, i.issuer_talent_id, i.payer_kind, i.payer_organisation_id,
                  i.payer_talent_id, i.payer_contact_id,
                  coalesce(i.payer_name, o.name, pt.name, '') AS "payer_display!", i.payer_name,
                  i.issuer, i.payer, i.booking_id, b.title AS "booking_title?", i.credits_invoice_id,
                  i.currency, i.line_items, i.tax_lines, i.subtotal_minor, i.tax_minor, i.total_minor,
                  i.paid_minor, i.credited_minor, i.issue_date, i.due_date, i.base_currency,
                  i.fx_rate::text AS fx_rate, i.fx_date, i.fx_source, i.base_total_minor, i.notes, i.terms,
                  i.sent_at, i.sent_to, i.viewed_at, i.document_id, i.voided_at, i.void_reason, i.tags,
                  i.custom, i.version, i.created_at, i.updated_at,
                  i.created_by AS "created_by: UserId", i.updated_by AS "updated_by: UserId"
           FROM invoices i
           LEFT JOIN organisations o ON o.id = i.payer_organisation_id
           LEFT JOIN talent pt ON pt.id = i.payer_talent_id
           LEFT JOIN bookings b ON b.id = i.booking_id
           WHERE i.workspace_id = app_workspace_id() AND i.id = ANY($1) AND i.deleted_at IS NULL"#,
        ids
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Invoice>> {
    Ok(sqlx::query_as!(
        Invoice,
        r#"SELECT i.id, i.kind, i.purpose, i.status AS "status: InvoiceStatus", i.number, i.reference,
                  i.historical, i.issuer_kind, i.issuer_talent_id, i.payer_kind, i.payer_organisation_id,
                  i.payer_talent_id, i.payer_contact_id,
                  coalesce(i.payer_name, o.name, pt.name, '') AS "payer_display!", i.payer_name,
                  i.issuer, i.payer, i.booking_id, b.title AS "booking_title?", i.credits_invoice_id,
                  i.currency, i.line_items, i.tax_lines, i.subtotal_minor, i.tax_minor, i.total_minor,
                  i.paid_minor, i.credited_minor, i.issue_date, i.due_date, i.base_currency,
                  i.fx_rate::text AS fx_rate, i.fx_date, i.fx_source, i.base_total_minor, i.notes, i.terms,
                  i.sent_at, i.sent_to, i.viewed_at, i.document_id, i.voided_at, i.void_reason, i.tags,
                  i.custom, i.version, i.created_at, i.updated_at,
                  i.created_by AS "created_by: UserId", i.updated_by AS "updated_by: UserId"
           FROM invoices i
           LEFT JOIN organisations o ON o.id = i.payer_organisation_id
           LEFT JOIN talent pt ON pt.id = i.payer_talent_id
           LEFT JOIN bookings b ON b.id = i.booking_id
           WHERE i.workspace_id = app_workspace_id() AND i.id = $1 AND i.deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Locks the row for this transaction; returns its version.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM invoices
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// The invoice behind a buyer's link, in any workspace.
pub async fn find_public(
    conn: &mut PgConnection,
    token_hash: &[u8],
) -> Result<Option<(Uuid, WorkspaceId)>> {
    Ok(sqlx::query!(
        r#"SELECT id AS "id!", workspace_id AS "workspace_id!: WorkspaceId" FROM invoice_public_find($1)"#,
        token_hash
    )
    .fetch_optional(conn)
    .await?
    .map(|r| (r.id, r.workspace_id)))
}

/// What an invoice says. Who issues it and its kind are set at creation.
pub struct Values<'a> {
    pub purpose: &'a str,
    pub payer_kind: &'a str,
    pub payer_organisation_id: Option<Uuid>,
    pub payer_talent_id: Option<Uuid>,
    pub payer_contact_id: Option<Uuid>,
    pub payer_name: Option<&'a str>,
    pub booking_id: Option<Uuid>,
    pub currency: &'a str,
    pub line_items: &'a Value,
    pub tax_lines: &'a Value,
    pub subtotal_minor: i64,
    pub tax_minor: i64,
    pub total_minor: i64,
    pub issue_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub notes: Option<&'a str>,
    pub terms: Option<&'a str>,
    pub tags: &'a [String],
    pub custom: &'a Value,
}

pub struct Creation<'a> {
    pub id: Uuid,
    pub kind: &'a str,
    pub issuer_talent_id: Option<Uuid>,
    pub credits_invoice_id: Option<Uuid>,
    /// Past invoices arrive already issued, with their own reference.
    pub historical: Option<Historical<'a>>,
}

/// A past invoice brought in as it was.
pub struct Historical<'a> {
    pub reference: &'a str,
    pub paid_minor: i64,
    pub document_id: Option<Uuid>,
}

pub async fn insert(
    conn: &mut PgConnection,
    c: Creation<'_>,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    let issuer_kind = if c.issuer_talent_id.is_some() { "talent" } else { "workspace" };
    // Past invoices go in as drafts and take their status in a second
    // step, so the caller can finish them before the freeze applies.
    let status = InvoiceStatus::Draft;
    let (reference, paid, doc) = match &c.historical {
        Some(h) => (Some(h.reference), h.paid_minor, h.document_id),
        None => (None, 0, None),
    };
    sqlx::query!(
        r#"INSERT INTO invoices (id, workspace_id, kind, purpose, status, reference, historical, issuer_kind,
                                 issuer_talent_id, payer_kind, payer_organisation_id, payer_talent_id,
                                 payer_contact_id, payer_name, booking_id, credits_invoice_id, currency, line_items,
                                 tax_lines, subtotal_minor, tax_minor, total_minor, paid_minor, issue_date,
                                 due_date, notes, terms, document_id, tags, custom, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                   $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $30)"#,
        c.id,
        c.kind,
        v.purpose,
        status as InvoiceStatus,
        reference,
        c.historical.is_some(),
        issuer_kind,
        c.issuer_talent_id,
        v.payer_kind,
        v.payer_organisation_id,
        v.payer_talent_id,
        v.payer_contact_id,
        v.payer_name,
        v.booking_id,
        c.credits_invoice_id,
        v.currency,
        v.line_items,
        v.tax_lines,
        v.subtotal_minor,
        v.tax_minor,
        v.total_minor,
        paid,
        v.issue_date,
        v.due_date,
        v.notes,
        v.terms,
        doc,
        v.tags,
        v.custom,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Replaces a draft's values. The caller has locked it and checked it is a draft.
pub async fn update(conn: &mut PgConnection, id: Uuid, v: Values<'_>, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE invoices SET purpose = $2, payer_kind = $3, payer_organisation_id = $4, payer_talent_id = $5,
                  payer_contact_id = $6, payer_name = $7, booking_id = $8, currency = $9, line_items = $10,
                  tax_lines = $11, subtotal_minor = $12, tax_minor = $13, total_minor = $14, issue_date = $15,
                  due_date = $16, notes = $17, terms = $18, tags = $19, custom = $20, updated_by = $21
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.purpose,
        v.payer_kind,
        v.payer_organisation_id,
        v.payer_talent_id,
        v.payer_contact_id,
        v.payer_name,
        v.booking_id,
        v.currency,
        v.line_items,
        v.tax_lines,
        v.subtotal_minor,
        v.tax_minor,
        v.total_minor,
        v.issue_date,
        v.due_date,
        v.notes,
        v.terms,
        v.tags,
        v.custom,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Tags and custom fields stay editable after issue (they are not printed).
pub async fn update_labels(
    conn: &mut PgConnection,
    id: Uuid,
    tags: &[String],
    custom: &Value,
    notes: Option<&str>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE invoices SET tags = $2, custom = $3, notes = $4, updated_by = $5
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        tags,
        custom,
        notes,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Everything fixed at issue.
pub struct Issue<'a> {
    pub status: InvoiceStatus,
    pub number: i64,
    pub reference: &'a str,
    pub issue_date: NaiveDate,
    pub due_date: NaiveDate,
    pub issuer: &'a Value,
    pub payer: &'a Value,
    pub base_currency: &'a str,
    pub fx_rate: &'a str,
    pub fx_date: NaiveDate,
    pub fx_source: &'a str,
    pub base_total_minor: i64,
}

pub async fn issue(conn: &mut PgConnection, id: Uuid, i: Issue<'_>, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE invoices SET status = $2, number = $3, reference = $4, issue_date = $5, due_date = $6,
                  issuer = $7, payer = $8, base_currency = $9, fx_rate = $10::text::numeric, fx_date = $11,
                  fx_source = $12, base_total_minor = $13, updated_by = $14
           WHERE workspace_id = app_workspace_id() AND id = $1 AND status = 'draft'"#,
        id,
        i.status as InvoiceStatus,
        i.number,
        i.reference,
        i.issue_date,
        i.due_date,
        i.issuer,
        i.payer,
        i.base_currency,
        i.fx_rate,
        i.fx_date,
        i.fx_source,
        i.base_total_minor,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn set_document(conn: &mut PgConnection, id: Uuid, document_id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE invoices SET document_id = $2 WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        document_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Moves the payment state on: amounts and the status they give.
pub async fn set_amounts(
    conn: &mut PgConnection,
    id: Uuid,
    paid_minor: i64,
    credited_minor: i64,
    status: InvoiceStatus,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE invoices SET paid_minor = $2, credited_minor = $3, status = $4
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        paid_minor,
        credited_minor,
        status as InvoiceStatus
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn mark_sent(
    conn: &mut PgConnection,
    id: Uuid,
    token_hash: &[u8],
    sent_to: &str,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE invoices SET sent_at = now(), sent_to = $3, token_hash = $2, updated_by = $4
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        token_hash,
        sent_to,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn has_link(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT token_hash IS NOT NULL AS "e!" FROM invoices WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?
    .unwrap_or(false))
}

pub async fn mark_viewed(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE invoices SET viewed_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1 AND viewed_at IS NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn void(conn: &mut PgConnection, id: Uuid, reason: Option<&str>, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE invoices SET status = 'void', voided_at = now(), void_reason = $2, updated_by = $3
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        reason,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Deletes a draft (an issued invoice is voided or credited instead).
pub async fn delete_draft(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE invoices SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL AND status = 'draft'"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Whether this issuer already has a live invoice (or credit note) with
/// this reference: importing the same past invoice twice is refused.
pub async fn reference_taken(
    conn: &mut PgConnection,
    kind: &str,
    issuer_talent_id: Option<Uuid>,
    reference: &str,
) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM invoices
               WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL AND status <> 'void'
                 AND kind = $1 AND issuer_talent_id IS NOT DISTINCT FROM $2
                 AND lower(reference) = lower($3)
           ) AS "taken!""#,
        kind,
        issuer_talent_id,
        reference
    )
    .fetch_one(conn)
    .await?)
}

/// Issued credit notes against an invoice, totalled.
pub async fn credited_total(conn: &mut PgConnection, invoice_id: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT coalesce(sum(total_minor), 0)::bigint AS "s!" FROM invoices
           WHERE workspace_id = app_workspace_id() AND credits_invoice_id = $1 AND kind = 'credit_note'
             AND status NOT IN ('draft', 'void') AND deleted_at IS NULL"#,
        invoice_id
    )
    .fetch_one(conn)
    .await?)
}

/// A booking's invoices that are not void, with their purposes (to tell what
/// has been invoiced already).
pub async fn for_booking(conn: &mut PgConnection, booking_id: Uuid) -> Result<Vec<Invoice>> {
    Ok(sqlx::query_as!(
        Invoice,
        r#"SELECT i.id, i.kind, i.purpose, i.status AS "status: InvoiceStatus", i.number, i.reference,
                  i.historical, i.issuer_kind, i.issuer_talent_id, i.payer_kind, i.payer_organisation_id,
                  i.payer_talent_id, i.payer_contact_id,
                  coalesce(i.payer_name, o.name, pt.name, '') AS "payer_display!", i.payer_name,
                  i.issuer, i.payer, i.booking_id, b.title AS "booking_title?", i.credits_invoice_id,
                  i.currency, i.line_items, i.tax_lines, i.subtotal_minor, i.tax_minor, i.total_minor,
                  i.paid_minor, i.credited_minor, i.issue_date, i.due_date, i.base_currency,
                  i.fx_rate::text AS fx_rate, i.fx_date, i.fx_source, i.base_total_minor, i.notes, i.terms,
                  i.sent_at, i.sent_to, i.viewed_at, i.document_id, i.voided_at, i.void_reason, i.tags,
                  i.custom, i.version, i.created_at, i.updated_at,
                  i.created_by AS "created_by: UserId", i.updated_by AS "updated_by: UserId"
           FROM invoices i
           LEFT JOIN organisations o ON o.id = i.payer_organisation_id
           LEFT JOIN talent pt ON pt.id = i.payer_talent_id
           LEFT JOIN bookings b ON b.id = i.booking_id
           WHERE i.workspace_id = app_workspace_id() AND i.booking_id = $1 AND i.deleted_at IS NULL
             ORDER BY i.created_at"#,
        booking_id
    )
    .fetch_all(conn)
    .await?)
}

/// An issued invoice whose due date has passed in its workspace's timezone.
#[derive(Debug, Clone)]
pub struct DueItem {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub due_date: NaiveDate,
}

/// Issued or part-paid invoices now overdue, across workspaces (worker role).
pub async fn overdue(conn: &mut PgConnection, now: DateTime<Utc>, limit: i64) -> Result<Vec<DueItem>> {
    Ok(sqlx::query_as!(
        DueItem,
        r#"SELECT i.id, i.workspace_id AS "workspace_id: WorkspaceId", i.due_date AS "due_date!"
           FROM invoices i JOIN workspaces w ON w.id = i.workspace_id
           WHERE i.status IN ('issued', 'part_paid') AND i.deleted_at IS NULL
             AND i.due_date < ($1 AT TIME ZONE w.default_timezone)::date
             AND w.status IN ('trial', 'active')
           ORDER BY i.due_date
           LIMIT $2"#,
        now,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// An invoice whose date (plus a rule's offset) falls in a scan window.
#[derive(Debug, Clone)]
pub struct Reached {
    pub id: Uuid,
    pub at: DateTime<Utc>,
}

/// Open invoices whose `due_date` or `issue_date` (at midnight in the
/// workspace's time zone, plus the offset) falls in `(from, to]`, for
/// date-offset rules such as chasing a balance.
pub async fn date_between(
    conn: &mut PgConnection,
    field: &str,
    timezone: &str,
    offset_minutes: i64,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Reached>> {
    Ok(sqlx::query_as!(
        Reached,
        r#"SELECT id AS "id!", at AS "at!" FROM (
               SELECT id,
                      (CASE WHEN $1 = 'issue_date' THEN issue_date ELSE due_date END)::timestamp AT TIME ZONE $2
                          + make_interval(mins => $3::int) AS at
               FROM invoices
               WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                 AND kind = 'invoice' AND status IN ('issued', 'part_paid', 'overdue')
           ) t
           WHERE at > $4 AND at <= $5
           ORDER BY at, id
           LIMIT $6"#,
        field,
        timezone,
        offset_minutes as i32,
        from,
        to,
        limit
    )
    .fetch_all(conn)
    .await?)
}
