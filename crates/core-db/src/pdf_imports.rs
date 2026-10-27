//! Past PDFs on their way in: read, matched to a record, then confirmed.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct PdfImport {
    pub id: Uuid,
    pub file_id: Uuid,
    pub filename: String,
    pub size_bytes: i64,
    pub page_count: Option<i32>,
    pub needs_ocr: bool,
    pub status: String,
    pub found: Value,
    pub matches: Value,
    pub error: Option<String>,
    pub document_id: Option<Uuid>,
    pub invoice_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

pub async fn list(conn: &mut PgConnection, statuses: &[String], limit: i64) -> Result<Vec<PdfImport>> {
    Ok(sqlx::query_as!(
        PdfImport,
        r#"SELECT p.id, p.file_id, f.filename, f.size_bytes, f.page_count, f.needs_ocr, p.status, p.found,
                  p.matches, p.error, p.document_id, p.invoice_id, p.version, p.created_at
           FROM pdf_imports p JOIN files f ON f.id = p.file_id
           WHERE p.workspace_id = app_workspace_id() AND (cardinality($1::text[]) = 0 OR p.status = ANY($1))
             ORDER BY p.created_at DESC LIMIT $2"#,
        statuses,
        limit
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<PdfImport>> {
    Ok(sqlx::query_as!(
        PdfImport,
        r#"SELECT p.id, p.file_id, f.filename, f.size_bytes, f.page_count, f.needs_ocr, p.status, p.found,
                  p.matches, p.error, p.document_id, p.invoice_id, p.version, p.created_at
           FROM pdf_imports p JOIN files f ON f.id = p.file_id
           WHERE p.workspace_id = app_workspace_id() AND p.id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<(i64, String)>> {
    Ok(sqlx::query!(
        r#"SELECT version, status FROM pdf_imports WHERE workspace_id = app_workspace_id() AND id = $1 FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?
    .map(|r| (r.version, r.status)))
}

pub async fn insert(conn: &mut PgConnection, id: Uuid, file_id: Uuid, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO pdf_imports (id, workspace_id, file_id, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $3)"#,
        id,
        file_id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The reading's result.
pub async fn set_read(
    conn: &mut PgConnection,
    id: Uuid,
    status: &str,
    found: &Value,
    matches: &Value,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE pdf_imports SET status = $2, found = $3, matches = $4, error = $5
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        status,
        found,
        matches,
        error
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn confirm(
    conn: &mut PgConnection,
    id: Uuid,
    document_id: Uuid,
    invoice_id: Option<Uuid>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE pdf_imports SET status = 'confirmed', document_id = $2, invoice_id = $3, updated_by = $4
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        document_id,
        invoice_id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn discard(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE pdf_imports SET status = 'discarded', updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The import waiting on a file's reading, if any.
pub async fn for_file(conn: &mut PgConnection, file_id: Uuid) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM pdf_imports WHERE workspace_id = app_workspace_id() AND file_id = $1 AND status = 'reading'"#,
        file_id
    )
    .fetch_optional(conn)
    .await?)
}

/// Saved field patterns for frequent issuers.
#[derive(Debug, Clone)]
pub struct PdfTemplate {
    pub id: Uuid,
    pub name: String,
    pub definition: Value,
    pub version: i64,
}

pub async fn templates(conn: &mut PgConnection) -> Result<Vec<PdfTemplate>> {
    Ok(sqlx::query_as!(
        PdfTemplate,
        r#"SELECT id, name, definition, version FROM pdf_templates
           WHERE workspace_id = app_workspace_id() AND archived_at IS NULL ORDER BY name"#
    )
    .fetch_all(conn)
    .await?)
}

pub async fn insert_template(
    conn: &mut PgConnection,
    id: Uuid,
    name: &str,
    definition: &Value,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO pdf_templates (id, workspace_id, name, definition, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $4)"#,
        id,
        name,
        definition,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn archive_template(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE pdf_templates SET archived_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND archived_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}
