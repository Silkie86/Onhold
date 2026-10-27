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
                  updated_at, created_by AS "created_by: UserId", started_at, finished_at
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
                  updated_at, created_by AS "created_by: UserId", started_at, finished_at
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

/// Ends the run (`done` or `failed`) and drops the uploaded file.
pub async fn finish(
    conn: &mut PgConnection,
    id: Uuid,
    status: &str,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE imports SET status = $2, error = $3, csv = NULL, finished_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        status,
        error
    )
    .execute(conn)
    .await?;
    Ok(())
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
