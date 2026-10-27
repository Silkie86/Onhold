//! Stored files: what they are and where their bytes are, plus the bytes
//! themselves when they are kept in the database. A file nothing refers
//! to yet (`attached_at` empty) is removed after a while.

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct File {
    pub id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub sha256: Vec<u8>,
    pub storage: String,
    pub storage_key: String,
    pub read_state: String,
    pub read_error: Option<String>,
    pub page_count: Option<i32>,
    pub needs_ocr: bool,
    pub attached_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
}

pub struct NewFile<'a> {
    pub id: Uuid,
    pub filename: &'a str,
    pub content_type: &'a str,
    pub size_bytes: i64,
    pub sha256: &'a [u8],
    pub storage: &'a str,
    pub storage_key: &'a str,
    /// `pending` for PDFs (their text is read by a job), `skipped` otherwise.
    pub read_state: &'a str,
    /// Made by OnHold and in use at once (a generated PDF).
    pub attached: bool,
    /// `upload`, or `mail` for a file that came with a message: those are
    /// reached only through their thread, never through [`get`].
    pub origin: &'a str,
}

pub async fn insert(conn: &mut PgConnection, f: NewFile<'_>, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO files (id, workspace_id, filename, content_type, size_bytes, sha256, storage, storage_key,
                              read_state, attached_at, created_by, origin)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, CASE WHEN $9 THEN now() END, $10, $11)"#,
        f.id,
        f.filename,
        f.content_type,
        f.size_bytes,
        f.sha256,
        f.storage,
        f.storage_key,
        f.read_state,
        f.attached,
        by.map(|u| u.0),
        f.origin
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn insert_blob(conn: &mut PgConnection, file_id: Uuid, data: &[u8]) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO file_blobs (file_id, workspace_id, data) VALUES ($1, app_workspace_id(), $2)"#,
        file_id,
        data
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn blob(conn: &mut PgConnection, file_id: Uuid) -> Result<Option<Vec<u8>>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT data FROM file_blobs WHERE workspace_id = app_workspace_id() AND file_id = $1"#,
        file_id
    )
    .fetch_optional(conn)
    .await?)
}

/// A file uploaded or made in OnHold. Files that came by mail are left
/// out: they are opened through their thread ([`mail_file`]).
pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<File>> {
    Ok(sqlx::query_as!(
        File,
        r#"SELECT id, filename, content_type, size_bytes, sha256, storage, storage_key, read_state, read_error,
                  page_count, needs_ocr, attached_at, created_at, created_by AS "created_by: UserId"
           FROM files
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL AND origin = 'upload'"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// A file that came with (or was sent with) a message. The caller checks
/// that the reader may see its thread.
pub async fn mail_file(conn: &mut PgConnection, id: Uuid) -> Result<Option<File>> {
    Ok(sqlx::query_as!(
        File,
        r#"SELECT id, filename, content_type, size_bytes, sha256, storage, storage_key, read_state, read_error,
                  page_count, needs_ocr, attached_at, created_at, created_by AS "created_by: UserId"
           FROM files
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL AND origin = 'mail'"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Marks a file as in use, so it is kept.
pub async fn attach(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE files SET attached_at = coalesce(attached_at, now())
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// What reading a PDF's text found.
pub struct ReadResult<'a> {
    pub state: &'a str,
    pub error: Option<&'a str>,
    pub page_count: Option<i32>,
    pub needs_ocr: bool,
    pub text: Option<&'a str>,
}

pub async fn set_read(conn: &mut PgConnection, id: Uuid, r: ReadResult<'_>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE files SET read_state = $2, read_error = $3, page_count = $4, needs_ocr = $5, text_content = $6
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        r.state,
        r.error,
        r.page_count,
        r.needs_ocr,
        r.text
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn text(conn: &mut PgConnection, id: Uuid) -> Result<Option<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT text_content FROM files WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

/// A file nothing uses, with where its bytes are (for the worker's cleanup).
#[derive(Debug, Clone)]
pub struct Unattached {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub storage: String,
    pub storage_key: String,
}

/// Files nobody attached to anything, uploaded before `before`.
pub async fn unattached_before(
    conn: &mut PgConnection,
    before: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Unattached>> {
    Ok(sqlx::query_as!(
        Unattached,
        r#"SELECT id, workspace_id AS "workspace_id: WorkspaceId", storage, storage_key FROM files
           WHERE attached_at IS NULL AND deleted_at IS NULL AND created_at < $1
           ORDER BY created_at LIMIT $2"#,
        before,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Deletes an unattached file; bytes kept in the database go with it.
pub async fn delete_unattached(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"DELETE FROM files WHERE id = $1 AND attached_at IS NULL AND deleted_at IS NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}
