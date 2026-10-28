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
    pub undo_error: Option<String>,
    /// Records it created that an undo has still to deal with.
    pub to_undo: i32,
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
                  undone_count, kept_count, undone_at, undo_error,
                  (SELECT count(*) FROM import_records r
                   WHERE r.workspace_id = imports.workspace_id AND r.import_id = imports.id)::int AS "to_undo!"
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
                  undone_count, kept_count, undone_at, undo_error,
                  (SELECT count(*) FROM import_records r
                   WHERE r.workspace_id = imports.workspace_id AND r.import_id = imports.id)::int AS "to_undo!"
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

/// Notes a record the import created, for undoing it, with its version now.
pub async fn created(conn: &mut PgConnection, id: Uuid, entity: &str, record: Uuid) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO import_records (workspace_id, import_id, entity, record_id, version)
           SELECT app_workspace_id(), $1, $2, $3, v.version
           FROM (SELECT version FROM talent WHERE $2 = 'talent' AND id = $3
                 UNION ALL SELECT version FROM organisations WHERE $2 = 'organisation' AND id = $3
                 UNION ALL SELECT version FROM contacts WHERE $2 = 'contact' AND id = $3) v
           ON CONFLICT (import_id, record_id) DO UPDATE SET version = EXCLUDED.version"#,
        id,
        entity,
        record
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// After the import itself changed a record, that version is where it
/// stands; nothing when the import did not create it.
pub async fn touched(conn: &mut PgConnection, id: Uuid, record: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE import_records r
           SET version = coalesce(
               (SELECT version FROM talent WHERE r.entity = 'talent' AND id = r.record_id),
               (SELECT version FROM organisations WHERE r.entity = 'organisation' AND id = r.record_id),
               (SELECT version FROM contacts WHERE r.entity = 'contact' AND id = r.record_id),
               r.version)
           WHERE r.workspace_id = app_workspace_id() AND r.import_id = $1 AND r.record_id = $2"#,
        id,
        record
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The kinds of record the import created that are still to undo.
pub async fn recorded_entities(conn: &mut PgConnection, id: Uuid) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT DISTINCT entity FROM import_records
           WHERE workspace_id = app_workspace_id() AND import_id = $1"#,
        id
    )
    .fetch_all(conn)
    .await?)
}

/// Starts undoing a finished import. False unless it is `done` or `failed`.
pub async fn start_undo(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE imports SET undo_from = status, status = 'undoing', undo_error = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND status IN ('done', 'failed')"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Whether the import is being undone, holding it so one job at a time
/// works on it.
pub async fn lock_undoing(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT status FROM imports WHERE workspace_id = app_workspace_id() AND id = $1 FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?
    .is_some_and(|s| s == "undoing"))
}

/// The next records of one kind the import created, still to undo, with
/// their versions once the import was done with them.
pub async fn created_records(
    conn: &mut PgConnection,
    id: Uuid,
    entity: &str,
    limit: i64,
) -> Result<Vec<(Uuid, i64)>> {
    Ok(sqlx::query!(
        r#"SELECT record_id, version FROM import_records
           WHERE workspace_id = app_workspace_id() AND import_id = $1 AND entity = $2
           ORDER BY record_id LIMIT $3"#,
        id,
        entity,
        limit
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| (r.record_id, r.version))
    .collect())
}

/// The version an import noted for a record it created, if it did.
pub async fn created_version(
    conn: &mut PgConnection,
    id: Uuid,
    record: Uuid,
) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM import_records
           WHERE workspace_id = app_workspace_id() AND import_id = $1 AND record_id = $2"#,
        id,
        record
    )
    .fetch_optional(conn)
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

/// Ends an undo: `undone`, or when it gave up, back to what it was before
/// with why, so it can be tried again.
pub async fn finish_undo(conn: &mut PgConnection, id: Uuid, error: Option<&str>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE imports
           SET status = CASE WHEN $2::text IS NULL THEN 'undone' ELSE coalesce(undo_from, 'done') END,
               undo_error = $2, undone_at = CASE WHEN $2::text IS NULL THEN now() END
           WHERE workspace_id = app_workspace_id() AND id = $1 AND status = 'undoing'"#,
        id,
        error
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A live record's version, holding it until the transaction ends so it
/// cannot change while an undo decides on it; `None` once it is gone. For
/// a contact, also its organisation.
pub async fn lock_record(
    conn: &mut PgConnection,
    entity: &str,
    id: Uuid,
) -> Result<Option<(i64, Option<Uuid>)>> {
    Ok(match entity {
        "talent" => sqlx::query_scalar!(
            r#"SELECT version FROM talent
               WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL FOR UPDATE"#,
            id
        )
        .fetch_optional(conn)
        .await?
        .map(|v| (v, None)),
        "organisation" => sqlx::query_scalar!(
            r#"SELECT version FROM organisations
               WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL FOR UPDATE"#,
            id
        )
        .fetch_optional(conn)
        .await?
        .map(|v| (v, None)),
        _ => sqlx::query!(
            r#"SELECT version, organisation_id FROM contacts
               WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL FOR UPDATE"#,
            id
        )
        .fetch_optional(conn)
        .await?
        .map(|r| (r.version, r.organisation_id)),
    })
}

/// Whether a record is in use elsewhere, so undoing an import keeps it:
/// on an enquiry, booking, invoice, payment, conversation, task or
/// document; for talent also a calendar, availability, a portal or a
/// pitching list. People at an organisation are asked separately.
pub async fn record_in_use(conn: &mut PgConnection, entity: &str, id: Uuid) -> Result<bool> {
    let used = match entity {
        "talent" => sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM booking_talent WHERE workspace_id = app_workspace_id() AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM enquiry_talent WHERE workspace_id = app_workspace_id() AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM invoices WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND (issuer_talent_id = $1 OR payer_talent_id = $1))
                   OR EXISTS (SELECT 1 FROM payments WHERE workspace_id = app_workspace_id() AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM settlements WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM threads WHERE workspace_id = app_workspace_id() AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM availability WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM talent_members WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM talent_calendars WHERE workspace_id = app_workspace_id()
                              AND disconnected_at IS NULL AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM calendar_pushes WHERE workspace_id = app_workspace_id()
                              AND disconnected_at IS NULL AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM calendar_feeds WHERE workspace_id = app_workspace_id()
                              AND revoked_at IS NULL AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM calendar_connect_links WHERE workspace_id = app_workspace_id()
                              AND revoked_at IS NULL AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM invitations WHERE workspace_id = app_workspace_id()
                              AND revoked_at IS NULL AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM talent WHERE workspace_id = app_workspace_id() AND id = $1
                              AND portal_user_id IS NOT NULL)
                   OR EXISTS (SELECT 1 FROM organisation_items WHERE workspace_id = app_workspace_id() AND talent_id = $1)
                   OR EXISTS (SELECT 1 FROM tasks WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND related_id = $1)
                   OR EXISTS (SELECT 1 FROM documents WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND related_id = $1)
                   AS "used!""#,
            id
        )
        .fetch_one(conn)
        .await?,
        "organisation" => sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM enquiries WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND organisation_id = $1)
                   OR EXISTS (SELECT 1 FROM enquiries WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND venue_id = $1)
                   OR EXISTS (SELECT 1 FROM bookings WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND organisation_id = $1)
                   OR EXISTS (SELECT 1 FROM bookings WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND venue_id = $1)
                   OR EXISTS (SELECT 1 FROM invoices WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND payer_organisation_id = $1)
                   OR EXISTS (SELECT 1 FROM payments WHERE workspace_id = app_workspace_id() AND organisation_id = $1)
                   OR EXISTS (SELECT 1 FROM threads WHERE workspace_id = app_workspace_id() AND organisation_id = $1)
                   OR EXISTS (SELECT 1 FROM tasks WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND related_id = $1)
                   OR EXISTS (SELECT 1 FROM documents WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND related_id = $1)
                   AS "used!""#,
            id
        )
        .fetch_one(conn)
        .await?,
        _ => sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM enquiries WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND contact_id = $1)
                   OR EXISTS (SELECT 1 FROM bookings WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND contact_id = $1)
                   OR EXISTS (SELECT 1 FROM invoices WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND payer_contact_id = $1)
                   OR EXISTS (SELECT 1 FROM threads WHERE workspace_id = app_workspace_id() AND contact_id = $1)
                   OR EXISTS (SELECT 1 FROM tasks WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND related_id = $1)
                   OR EXISTS (SELECT 1 FROM documents WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                              AND related_id = $1)
                   AS "used!""#,
            id
        )
        .fetch_one(conn)
        .await?,
    };
    Ok(used)
}

/// Whether anyone is left at an organisation.
pub async fn has_people(conn: &mut PgConnection, organisation: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM contacts WHERE workspace_id = app_workspace_id()
                          AND organisation_id = $1 AND deleted_at IS NULL) AS "people!""#,
        organisation
    )
    .fetch_one(conn)
    .await?)
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
