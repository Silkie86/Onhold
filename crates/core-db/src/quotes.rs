//! Quotes: priced offers on a hold, sent to the buyer by secret link. An
//! accepted quote is frozen by the database (`quotes_accepted_frozen`).

use std::net::IpAddr;

use chrono::{DateTime, NaiveDate, Utc};
use core_domain::{QuoteStatus, UserId, WorkspaceId};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

/// The trigger that refuses changes to an accepted quote.
pub const ACCEPTED_FROZEN: &str = "quotes_accepted_frozen";

#[derive(Debug, Clone)]
pub struct Quote {
    pub id: Uuid,
    pub booking_id: Uuid,
    pub booking_title: String,
    pub number: i64,
    pub revision: i32,
    pub supersedes_id: Option<Uuid>,
    pub status: QuoteStatus,
    pub currency: String,
    pub line_items: Value,
    pub total_minor: i64,
    pub valid_until: Option<NaiveDate>,
    pub message: Option<String>,
    pub terms: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
    pub sent_to: Option<String>,
    pub viewed_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_name: Option<String>,
    pub decided_email: Option<String>,
    pub decided_ip: Option<IpAddr>,
    pub decided_user_agent: Option<String>,
    pub decline_reason: Option<String>,
    pub snapshot: Option<Value>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

/// Quotes, newest first: one booking's, or all, of any of `statuses` (all
/// when empty).
pub async fn list(
    conn: &mut PgConnection,
    booking_id: Option<Uuid>,
    statuses: &[QuoteStatus],
    offset: i64,
    limit: i64,
) -> Result<(Vec<Quote>, i64)> {
    let rows = sqlx::query!(
        r#"SELECT q.id, q.booking_id, b.title AS booking_title, q.number, q.revision, q.supersedes_id,
                  q.status AS "status: QuoteStatus", q.currency, q.line_items, q.total_minor, q.valid_until,
                  q.message, q.terms, q.sent_at, q.sent_to, q.viewed_at, q.decided_at, q.decided_name,
                  q.decided_email, q.decided_ip AS "decided_ip: IpAddr", q.decided_user_agent,
                  q.decline_reason, q.snapshot, q.version, q.created_at, q.updated_at,
                  q.created_by AS "created_by: UserId", q.updated_by AS "updated_by: UserId",
                  count(*) OVER () AS "total!"
           FROM quotes q JOIN bookings b ON b.id = q.booking_id
           WHERE q.workspace_id = app_workspace_id() AND q.deleted_at IS NULL
             AND ($1::uuid IS NULL OR q.booking_id = $1)
             AND (cardinality($2::quote_status[]) = 0 OR q.status = ANY($2))
           ORDER BY q.number DESC, q.revision DESC
           OFFSET $3 LIMIT $4"#,
        booking_id,
        statuses as &[QuoteStatus],
        offset,
        limit
    )
    .fetch_all(conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    let items = rows
        .into_iter()
        .map(|r| Quote {
            id: r.id,
            booking_id: r.booking_id,
            booking_title: r.booking_title,
            number: r.number,
            revision: r.revision,
            supersedes_id: r.supersedes_id,
            status: r.status,
            currency: r.currency,
            line_items: r.line_items,
            total_minor: r.total_minor,
            valid_until: r.valid_until,
            message: r.message,
            terms: r.terms,
            sent_at: r.sent_at,
            sent_to: r.sent_to,
            viewed_at: r.viewed_at,
            decided_at: r.decided_at,
            decided_name: r.decided_name,
            decided_email: r.decided_email,
            decided_ip: r.decided_ip,
            decided_user_agent: r.decided_user_agent,
            decline_reason: r.decline_reason,
            snapshot: r.snapshot,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
            created_by: r.created_by,
            updated_by: r.updated_by,
        })
        .collect();
    Ok((items, total))
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Quote>> {
    Ok(sqlx::query_as!(
        Quote,
        r#"SELECT q.id, q.booking_id, b.title AS booking_title, q.number, q.revision, q.supersedes_id,
                  q.status AS "status: QuoteStatus", q.currency, q.line_items, q.total_minor, q.valid_until,
                  q.message, q.terms, q.sent_at, q.sent_to, q.viewed_at, q.decided_at, q.decided_name,
                  q.decided_email, q.decided_ip AS "decided_ip: IpAddr", q.decided_user_agent,
                  q.decline_reason, q.snapshot, q.version, q.created_at, q.updated_at,
                  q.created_by AS "created_by: UserId", q.updated_by AS "updated_by: UserId"
           FROM quotes q JOIN bookings b ON b.id = q.booking_id
           WHERE q.workspace_id = app_workspace_id() AND q.id = $1 AND q.deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Locks the row for an update in this transaction; returns its version.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM quotes
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// The quote behind a buyer's link, in any workspace: (quote, workspace).
pub async fn find_public(
    conn: &mut PgConnection,
    token_hash: &[u8],
) -> Result<Option<(Uuid, WorkspaceId)>> {
    Ok(sqlx::query!(
        r#"SELECT id AS "id!", workspace_id AS "workspace_id!: WorkspaceId" FROM quote_public_find($1)"#,
        token_hash
    )
    .fetch_optional(conn)
    .await?
    .map(|r| (r.id, r.workspace_id)))
}

pub struct Values<'a> {
    pub currency: &'a str,
    pub line_items: &'a Value,
    pub total_minor: i64,
    pub valid_until: Option<NaiveDate>,
    pub message: Option<&'a str>,
    pub terms: Option<&'a str>,
}

#[allow(clippy::too_many_arguments)]
pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    booking_id: Uuid,
    number: i64,
    revision: i32,
    supersedes_id: Option<Uuid>,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO quotes (id, workspace_id, booking_id, number, revision, supersedes_id, currency,
                               line_items, total_minor, valid_until, message, terms, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12)"#,
        id,
        booking_id,
        number,
        revision,
        supersedes_id,
        v.currency,
        v.line_items,
        v.total_minor,
        v.valid_until,
        v.message,
        v.terms,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Replaces a draft's values. The caller has locked the row, checked its
/// version and that it is a draft.
pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE quotes SET currency = $2, line_items = $3, total_minor = $4, valid_until = $5, message = $6,
                  terms = $7, updated_by = $8
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.currency,
        v.line_items,
        v.total_minor,
        v.valid_until,
        v.message,
        v.terms,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Marks a draft sent, with its link's hash and who it went to.
pub async fn mark_sent(
    conn: &mut PgConnection,
    id: Uuid,
    token_hash: &[u8],
    sent_to: &str,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE quotes SET status = 'sent', sent_at = now(), sent_to = $3, token_hash = $2, updated_by = $4
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

/// The buyer opened the link for the first time.
pub async fn mark_viewed(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE quotes SET status = 'viewed', viewed_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1 AND status = 'sent'"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// The buyer's answer.
pub struct Decision<'a> {
    pub status: QuoteStatus,
    pub name: &'a str,
    pub email: Option<&'a str>,
    pub ip: Option<IpAddr>,
    pub user_agent: Option<&'a str>,
    pub decline_reason: Option<&'a str>,
    /// Exactly what was accepted (accepting only).
    pub snapshot: Option<&'a Value>,
}

pub async fn decide(conn: &mut PgConnection, id: Uuid, d: Decision<'_>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE quotes SET status = $2, decided_at = now(), decided_name = $3, decided_email = $4,
                  decided_ip = $5, decided_user_agent = $6, decline_reason = $7, snapshot = $8
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        d.status as QuoteStatus,
        d.name,
        d.email,
        d.ip as Option<IpAddr>,
        d.user_agent,
        d.decline_reason,
        d.snapshot
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Withdrawn or expired: closed without an answer.
pub async fn close(
    conn: &mut PgConnection,
    id: Uuid,
    status: QuoteStatus,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE quotes SET status = $2, updated_by = $3
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        status as QuoteStatus,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE quotes SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// A booking's quotes still out with the buyer.
pub async fn open_for_booking(conn: &mut PgConnection, booking_id: Uuid) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM quotes
           WHERE workspace_id = app_workspace_id() AND booking_id = $1 AND deleted_at IS NULL
             AND status IN ('sent', 'viewed')
           ORDER BY number, revision"#,
        booking_id
    )
    .fetch_all(conn)
    .await?)
}

/// A booking's accepted quotes that no accepted revision has replaced.
pub async fn accepted_for_booking(conn: &mut PgConnection, booking_id: Uuid) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT q.id FROM quotes q
           WHERE q.workspace_id = app_workspace_id() AND q.booking_id = $1 AND q.deleted_at IS NULL
             AND q.status = 'accepted'
             AND NOT EXISTS (
                 SELECT 1 FROM quotes r
                 WHERE r.workspace_id = q.workspace_id AND r.supersedes_id = q.id
                   AND r.deleted_at IS NULL AND r.status = 'accepted')
           ORDER BY q.number, q.revision"#,
        booking_id
    )
    .fetch_all(conn)
    .await?)
}

/// The latest revision number of a quote number.
pub async fn latest_revision(conn: &mut PgConnection, number: i64) -> Result<i32> {
    Ok(sqlx::query_scalar!(
        r#"SELECT coalesce(max(revision), 0) AS "n!" FROM quotes
           WHERE workspace_id = app_workspace_id() AND number = $1"#,
        number
    )
    .fetch_one(conn)
    .await?)
}

/// A quote that has passed its validity date, in some workspace.
#[derive(Debug, Clone)]
pub struct DueItem {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub valid_until: NaiveDate,
}

/// Open quotes whose last valid day has ended in their workspace's
/// timezone, across workspaces (worker role).
pub async fn lapsed(
    conn: &mut PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<DueItem>> {
    Ok(sqlx::query_as!(
        DueItem,
        r#"SELECT q.id, q.workspace_id AS "workspace_id: WorkspaceId", q.valid_until AS "valid_until!"
           FROM quotes q JOIN workspaces w ON w.id = q.workspace_id
           WHERE q.status IN ('sent', 'viewed') AND q.deleted_at IS NULL
             AND q.valid_until < ($1 AT TIME ZONE w.default_timezone)::date
             AND w.status IN ('trial', 'active')
           ORDER BY q.valid_until
           LIMIT $2"#,
        now,
        limit
    )
    .fetch_all(conn)
    .await?)
}
