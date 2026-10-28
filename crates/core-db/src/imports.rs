//! Spreadsheet imports: the uploaded file, its mapping, and the run's
//! progress and results.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Import {
    pub id: Uuid,
    pub entity: String,
    pub filename: Option<String>,
    pub status: String,
    pub headers: Vec<String>,
    pub row_count: i32,
    pub mapping: Option<Value>,
    pub options: Option<Value>,
    pub created_count: i32,
    pub updated_count: i32,
    pub skipped_count: i32,
    pub failed_count: i32,
    pub processed: i32,
    pub issues: Value,
    pub error: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub undone_count: i32,
    pub kept_count: i32,
    pub undone_at: Option<DateTime<Utc>>,
}

pub struct New<'a> {
    pub entity: &'a str,
    pub filename: Option<&'a str>,
    pub csv: &'a str,
    pub headers: &'a [String],
    pub row_count: i32,
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    n: New<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO imports (id, workspace_id, entity, filename, csv, headers, row_count, created_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7)"#,
        id,
        n.entity,
        n.filename,
        n.csv,
        n.headers,
        n.row_count,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Import>> {
    Ok(sqlx::query_as!(
        Import,
        r#"SELECT id, entity, filename, status, headers, row_count, mapping, options, created_count,
                  updated_count, skipped_count, failed_count, processed, issues, error, version, created_at,
                  updated_at, created_by AS "created_by: UserId", started_at, finished_at,
                  undone_count, kept_count, undone_at
           FROM imports WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// The uploaded file, while it is still kept.
pub async fn csv(conn: &mut PgConnection, id: Uuid) -> Result<Option<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT csv FROM imports WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

/// Newest first.
pub async fn list(
    conn: &mut PgConnection,
    before: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Import>> {
    Ok(sqlx::query_as!(
        Import,
        r#"SELECT id, entity, filename, status, headers, row_count, mapping, options, created_count,
                  updated_count, skipped_count, failed_count, processed, issues, error, version, created_at,
                  updated_at, created_by AS "created_by: UserId", started_at, finished_at,
                  undone_count, kept_count, undone_at
           FROM imports
           WHERE workspace_id = app_workspace_id() AND ($1::uuid IS NULL OR id < $1)
           ORDER BY id DESC
           LIMIT $2"#,
        before,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Queues an uploaded import with its mapping. False if it was already
/// queued or run.
pub async fn queue(
    conn: &mut PgConnection,
    id: Uuid,
    mapping: &Value,
    options: &Value,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE imports SET status = 'queued', mapping = $2, options = $3
           WHERE workspace_id = app_workspace_id() AND id = $1 AND status = 'uploaded'"#,
        id,
        mapping,
        options
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// The worker picks the import up (again, after a crash).
pub async fn start(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE imports SET status = 'running', started_at = coalesce(started_at, now())
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub created: i32,
    pub updated: i32,
    pub skipped: i32,
    pub failed: i32,
}

/// Adds one batch's results, in the same transaction as the batch's writes.
pub async fn progress(
    conn: &mut PgConnection,
    id: Uuid,
    processed: i32,
    counts: Counts,
    issues: &Value,
    max_issues: i32,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE imports SET processed = $2, created_count = created_count + $3,
                  updated_count = updated_count + $4, skipped_count = skipped_count + $5,
                  failed_count = failed_count + $6,
                  issues = (SELECT coalesce(jsonb_agg(i), '[]') FROM (
                      SELECT i FROM jsonb_array_elements(issues || $7::jsonb) i LIMIT $8) s)
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        processed,
        counts.created,
        counts.updated,
        counts.skipped,
        counts.failed,
        issues,
        i64::from(max_issues)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Ends the run (`done` or `failed`) and drops the uploaded file, and the
/// corrections made to it, which hold its cells.
pub async fn finish(
    conn: &mut PgConnection,
    id: Uuid,
    status: &str,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE imports SET status = $2, error = $3, csv = NULL,
                  options = options - 'edits', finished_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        status,
        error
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Notes a record the import created, for undoing it.
pub async fn created(conn: &mut PgConnection, id: Uuid, entity: &str, record: Uuid) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO import_records (workspace_id, import_id, entity, record_id)
           VALUES (app_workspace_id(), $1, $2, $3) ON CONFLICT DO NOTHING"#,
        id,
        entity,
        record
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Starts undoing a finished import. False unless it is `done` or `failed`.
pub async fn start_undo(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE imports SET status = 'undoing', error = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND status IN ('done', 'failed')"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// The next records of one kind the import created, still to undo.
pub async fn created_records(
    conn: &mut PgConnection,
    id: Uuid,
    entity: &str,
    limit: i64,
) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT record_id FROM import_records
           WHERE workspace_id = app_workspace_id() AND import_id = $1 AND entity = $2
           ORDER BY record_id LIMIT $3"#,
        id,
        entity,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// One batch of an undo, in the same transaction as its deletes.
pub async fn undo_progress(
    conn: &mut PgConnection,
    id: Uuid,
    done: &[Uuid],
    undone: i32,
    kept: i32,
) -> Result<()> {
    sqlx::query!(
        r#"DELETE FROM import_records
           WHERE workspace_id = app_workspace_id() AND import_id = $1 AND record_id = ANY($2)"#,
        id,
        done
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        r#"UPDATE imports SET undone_count = undone_count + $2, kept_count = kept_count + $3
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        undone,
        kept
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Ends an undo: `undone`, or back to `done` with the error when it gave up.
pub async fn finish_undo(conn: &mut PgConnection, id: Uuid, error: Option<&str>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE imports
           SET status = CASE WHEN $2::text IS NULL THEN 'undone' ELSE 'done' END,
               error = $2, undone_at = CASE WHEN $2::text IS NULL THEN now() END
           WHERE workspace_id = app_workspace_id() AND id = $1 AND status = 'undoing'"#,
        id,
        error
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// When a live record was last changed, `None` once it is gone.
pub async fn record_changed_at(
    conn: &mut PgConnection,
    entity: &str,
    id: Uuid,
) -> Result<Option<DateTime<Utc>>> {
    let at = match entity {
        "talent" => {
            sqlx::query_scalar!(
                r#"SELECT updated_at FROM talent
                   WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
                id
            )
            .fetch_optional(conn)
            .await?
        }
        "organisation" => {
            sqlx::query_scalar!(
                r#"SELECT updated_at FROM organisations
                   WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
                id
            )
            .fetch_optional(conn)
            .await?
        }
        _ => {
            sqlx::query_scalar!(
                r#"SELECT updated_at FROM contacts
                   WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
                id
            )
            .fetch_optional(conn)
            .await?
        }
    };
    Ok(at)
}

/// Whether a record is in use elsewhere, so undoing an import keeps it:
/// on an enquiry, booking, invoice or conversation, or an organisation
/// with people left at it.
pub async fn record_in_use(conn: &mut PgConnection, entity: &str, id: Uuid) -> Result<bool> {
    let used = match entity {
        "talent" => sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM booking_talent WHERE workspace_id = app_workspace_id() AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM enquiry_talent WHERE workspace_id = app_workspace_id() AND talent_id = $1)
                   AS "used!""#,
            id
        )
        .fetch_one(conn)
        .await?,
        "organisation" => sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM contacts WHERE workspace_id = app_workspace_id()
                              AND organisation_id = $1 AND deleted_at IS NULL)
                   OR EXISTS (SELECT 1 FROM enquiries WHERE workspace_id = app_workspace_id() AND organisation_id = $1)
                   OR EXISTS (SELECT 1 FROM bookings WHERE workspace_id = app_workspace_id() AND organisation_id = $1)
                   OR EXISTS (SELECT 1 FROM invoices WHERE workspace_id = app_workspace_id() AND payer_organisation_id = $1)
                   OR EXISTS (SELECT 1 FROM threads WHERE workspace_id = app_workspace_id() AND organisation_id = $1)
                   AS "used!""#,
            id
        )
        .fetch_one(conn)
        .await?,
        _ => sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM enquiries WHERE workspace_id = app_workspace_id() AND contact_id = $1)
                   OR EXISTS (SELECT 1 FROM bookings WHERE workspace_id = app_workspace_id() AND contact_id = $1)
                   OR EXISTS (SELECT 1 FROM invoices WHERE workspace_id = app_workspace_id() AND payer_contact_id = $1)
                   OR EXISTS (SELECT 1 FROM threads WHERE workspace_id = app_workspace_id() AND contact_id = $1)
                   AS "used!""#,
            id
        )
        .fetch_one(conn)
        .await?,
    };
    Ok(used)
}

/// Worker cleanup: uploads that were never run, older than `before`.
pub async fn delete_stale_uploads(conn: &mut PgConnection, before: DateTime<Utc>) -> Result<u64> {
    Ok(sqlx::query!(
        r#"DELETE FROM imports WHERE status = 'uploaded' AND created_at < $1"#,
        before
    )
    .execute(conn)
    .await?
    .rows_affected())
}
