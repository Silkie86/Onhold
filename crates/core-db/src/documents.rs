//! Documents, their versions, and requests to sign them.

use std::net::IpAddr;

use chrono::{DateTime, NaiveDate, Utc};
use core_domain::{UserId, WorkspaceId};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

/// The trigger that refuses changes to a given signature.
pub const SIGNED_FROZEN: &str = "signature_requests_signed_frozen";

#[derive(Debug, Clone)]
pub struct Document {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub related_entity: Option<String>,
    pub related_id: Option<Uuid>,
    pub source: String,
    pub template_key: Option<String>,
    pub current_version: i32,
    pub document_date: Option<NaiveDate>,
    pub expires_on: Option<NaiveDate>,
    pub signature_status: String,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub custom: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
    /// The current version's state and file.
    pub status: Option<String>,
    pub file_id: Option<Uuid>,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
}

pub struct Filter<'a> {
    pub related_id: Option<Uuid>,
    pub kinds: &'a [String],
    /// Words in the title, tags or the file's text.
    pub q: Option<&'a str>,
    /// Only those running out by this day.
    pub expires_before: Option<NaiveDate>,
    pub offset: i64,
    pub limit: i64,
}

/// Documents, newest first; with `q`, the best matches first.
pub async fn list(conn: &mut PgConnection, f: Filter<'_>) -> Result<(Vec<Document>, i64)> {
    let rows = sqlx::query!(
        r#"SELECT d.id, d.kind, d.title, d.related_entity, d.related_id, d.source, d.template_key, d.current_version,
                  d.document_date, d.expires_on, d.signature_status, d.notes, d.tags, d.custom, d.version,
                  d.created_at, d.updated_at, d.created_by AS "created_by: UserId", d.updated_by AS "updated_by: UserId",
                  v.status AS "status?", v.file_id AS "file_id?", fl.filename AS "filename?",
                  fl.content_type AS "content_type?", fl.size_bytes AS "size_bytes?",
                  count(*) OVER () AS "total!"
           FROM documents d
           LEFT JOIN document_versions v ON v.document_id = d.id AND v.number = d.current_version
           LEFT JOIN files fl ON fl.id = v.file_id
           WHERE d.workspace_id = app_workspace_id() AND d.deleted_at IS NULL
             AND ($1::uuid IS NULL OR d.related_id = $1)
             AND (cardinality($2::text[]) = 0 OR d.kind = ANY($2))
             AND ($3::date IS NULL OR d.expires_on <= $3)
             AND ($4::text IS NULL
                  OR d.search_text LIKE '%' || lower($4) || '%'
                  OR fl.text_search @@ plainto_tsquery('simple', $4))
           ORDER BY CASE WHEN $4::text IS NOT NULL
                         THEN ts_rank(fl.text_search, plainto_tsquery('simple', $4))
                              + (d.search_text LIKE '%' || lower($4) || '%')::int END DESC NULLS LAST,
                    CASE WHEN $3::date IS NOT NULL THEN d.expires_on END ASC,
                    d.created_at DESC, d.id DESC
           OFFSET $5 LIMIT $6"#,
        f.related_id,
        f.kinds,
        f.expires_before,
        f.q,
        f.offset,
        f.limit
    )
    .fetch_all(conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    Ok((
        rows.into_iter()
            .map(|r| Document {
                id: r.id,
                kind: r.kind,
                title: r.title,
                related_entity: r.related_entity,
                related_id: r.related_id,
                source: r.source,
                template_key: r.template_key,
                current_version: r.current_version,
                document_date: r.document_date,
                expires_on: r.expires_on,
                signature_status: r.signature_status,
                notes: r.notes,
                tags: r.tags,
                custom: r.custom,
                version: r.version,
                created_at: r.created_at,
                updated_at: r.updated_at,
                created_by: r.created_by,
                updated_by: r.updated_by,
                status: r.status,
                file_id: r.file_id,
                filename: r.filename,
                content_type: r.content_type,
                size_bytes: r.size_bytes,
            })
            .collect(),
        total,
    ))
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Document>> {
    Ok(sqlx::query_as!(
        Document,
        r#"SELECT d.id, d.kind, d.title, d.related_entity, d.related_id, d.source, d.template_key, d.current_version,
                  d.document_date, d.expires_on, d.signature_status, d.notes, d.tags, d.custom, d.version,
                  d.created_at, d.updated_at, d.created_by AS "created_by: UserId", d.updated_by AS "updated_by: UserId",
                  v.status AS "status?", v.file_id AS "file_id?", fl.filename AS "filename?",
                  fl.content_type AS "content_type?", fl.size_bytes AS "size_bytes?"
           FROM documents d
           LEFT JOIN document_versions v ON v.document_id = d.id AND v.number = d.current_version
           LEFT JOIN files fl ON fl.id = v.file_id
           WHERE d.workspace_id = app_workspace_id() AND d.id = $1 AND d.deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Locks the row for an update in this transaction; returns its version.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM documents
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct NewDocument<'a> {
    pub id: Uuid,
    pub kind: &'a str,
    pub title: &'a str,
    pub related_entity: Option<&'a str>,
    pub related_id: Option<Uuid>,
    pub source: &'a str,
    pub template_key: Option<&'a str>,
    pub document_date: Option<NaiveDate>,
    pub expires_on: Option<NaiveDate>,
    pub notes: Option<&'a str>,
    pub tags: &'a [String],
    pub custom: &'a Value,
}

pub async fn insert(conn: &mut PgConnection, d: NewDocument<'_>, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO documents (id, workspace_id, kind, title, related_entity, related_id, source, template_key,
                                  document_date, expires_on, notes, tags, custom, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13)"#,
        d.id,
        d.kind,
        d.title,
        d.related_entity,
        d.related_id,
        d.source,
        d.template_key,
        d.document_date,
        d.expires_on,
        d.notes,
        d.tags,
        d.custom,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub struct Edit<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub related_entity: Option<&'a str>,
    pub related_id: Option<Uuid>,
    pub document_date: Option<NaiveDate>,
    pub expires_on: Option<NaiveDate>,
    pub notes: Option<&'a str>,
    pub tags: &'a [String],
    pub custom: &'a Value,
}

pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    e: Edit<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE documents SET kind = $2, title = $3, related_entity = $4, related_id = $5, document_date = $6,
                  expires_on = $7, notes = $8, tags = $9, custom = $10, updated_by = $11
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        e.kind,
        e.title,
        e.related_entity,
        e.related_id,
        e.document_date,
        e.expires_on,
        e.notes,
        e.tags,
        e.custom,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE documents SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn set_signature_status(
    conn: &mut PgConnection,
    id: Uuid,
    status: &str,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE documents SET signature_status = $2, updated_by = coalesce($3, updated_by)
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        status,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Whether an issued invoice or a settlement keeps this document as its PDF.
pub async fn kept_by_record(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM invoices WHERE workspace_id = app_workspace_id() AND document_id = $1
                            AND status <> 'draft' AND deleted_at IS NULL)
               OR EXISTS (SELECT 1 FROM settlements WHERE workspace_id = app_workspace_id() AND document_id = $1
                            AND deleted_at IS NULL) AS "kept!""#,
        id
    )
    .fetch_one(conn)
    .await?)
}

/// Marks the document signed by one round of signatures.
pub async fn set_signed_round(conn: &mut PgConnection, id: Uuid, round_id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE documents SET signature_status = 'signed', signed_round_id = $2
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        round_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The round the signed copy was sealed from, if it is signed.
pub async fn signed_round(conn: &mut PgConnection, id: Uuid) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT signed_round_id FROM documents
           WHERE workspace_id = app_workspace_id() AND id = $1 AND signature_status = 'signed'"#,
        id
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

// Versions ----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Version {
    pub id: Uuid,
    pub document_id: Uuid,
    pub number: i32,
    pub purpose: String,
    pub status: String,
    pub file_id: Option<Uuid>,
    pub html: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<Vec<u8>>,
}

pub async fn versions(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<Version>> {
    Ok(sqlx::query_as!(
        Version,
        r#"SELECT v.id, v.document_id, v.number, v.purpose, v.status, v.file_id, NULL::text AS html, v.error,
                  v.created_at, v.created_by AS "created_by: UserId", f.filename AS "filename?",
                  f.content_type AS "content_type?", f.size_bytes AS "size_bytes?", f.sha256 AS "sha256?"
           FROM document_versions v LEFT JOIN files f ON f.id = v.file_id
           WHERE v.workspace_id = app_workspace_id() AND v.document_id = $1
           ORDER BY v.number DESC"#,
        document_id
    )
    .fetch_all(conn)
    .await?)
}

pub async fn version(
    conn: &mut PgConnection,
    document_id: Uuid,
    number: i32,
) -> Result<Option<Version>> {
    Ok(sqlx::query_as!(
        Version,
        r#"SELECT v.id, v.document_id, v.number, v.purpose, v.status, v.file_id, v.html, v.error,
                  v.created_at, v.created_by AS "created_by: UserId", f.filename AS "filename?",
                  f.content_type AS "content_type?", f.size_bytes AS "size_bytes?", f.sha256 AS "sha256?"
           FROM document_versions v LEFT JOIN files f ON f.id = v.file_id
           WHERE v.workspace_id = app_workspace_id() AND v.document_id = $1 AND v.number = $2"#,
        document_id,
        number
    )
    .fetch_optional(conn)
    .await?)
}

pub struct NewVersion<'a> {
    pub purpose: &'a str,
    pub status: &'a str,
    pub file_id: Option<Uuid>,
    pub html: Option<&'a str>,
}

/// Adds the next version and makes it current; returns its number. The
/// caller holds the document's lock.
pub async fn add_version(
    conn: &mut PgConnection,
    document_id: Uuid,
    v: NewVersion<'_>,
    by: Option<UserId>,
) -> Result<i32> {
    let number = sqlx::query_scalar!(
        r#"UPDATE documents SET current_version = current_version + 1, updated_by = coalesce($2, updated_by)
           WHERE workspace_id = app_workspace_id() AND id = $1
           RETURNING current_version"#,
        document_id,
        by.map(|u| u.0)
    )
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query!(
        r#"INSERT INTO document_versions (id, workspace_id, document_id, number, purpose, status, file_id, html, created_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8)"#,
        core_domain::ids::new_id(),
        document_id,
        number,
        v.purpose,
        v.status,
        v.file_id,
        v.html,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(number)
}

/// A version finished printing (`file_id`) or failed (`error`).
pub async fn finish_version(
    conn: &mut PgConnection,
    document_id: Uuid,
    number: i32,
    file_id: Option<Uuid>,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE document_versions
           SET status = CASE WHEN $3::uuid IS NOT NULL THEN 'ready' ELSE 'failed' END, file_id = $3, error = $4
           WHERE workspace_id = app_workspace_id() AND document_id = $1 AND number = $2"#,
        document_id,
        number,
        file_id,
        error
    )
    .execute(conn)
    .await?;
    Ok(())
}

// Signatures -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Signature {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version_number: i32,
    pub round_id: Uuid,
    pub position: i32,
    pub signer_name: String,
    pub signer_email: String,
    pub status: String,
    pub sent_at: DateTime<Utc>,
    pub viewed_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub signed_name: Option<String>,
    pub signed_ip: Option<IpAddr>,
    pub signed_user_agent: Option<String>,
    pub pdf_sha256: Option<Vec<u8>>,
    pub decline_reason: Option<String>,
    pub version: i64,
}

pub async fn signatures(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<Signature>> {
    Ok(sqlx::query_as!(
        Signature,
        r#"SELECT id, document_id, version_number, round_id, position, signer_name, signer_email, status, sent_at, viewed_at,
                  decided_at, signed_name, signed_ip AS "signed_ip: IpAddr", signed_user_agent, pdf_sha256,
                  decline_reason, version
           FROM signature_requests WHERE workspace_id = app_workspace_id() AND document_id = $1
           ORDER BY version_number DESC, position, id"#,
        document_id
    )
    .fetch_all(conn)
    .await?)
}

/// The document a signing request is for, without locking anything.
pub async fn signature_document(conn: &mut PgConnection, id: Uuid) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        "SELECT document_id FROM signature_requests WHERE workspace_id = app_workspace_id() AND id = $1",
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn signature(conn: &mut PgConnection, id: Uuid) -> Result<Option<Signature>> {
    Ok(sqlx::query_as!(
        Signature,
        r#"SELECT id, document_id, version_number, round_id, position, signer_name, signer_email, status, sent_at, viewed_at,
                  decided_at, signed_name, signed_ip AS "signed_ip: IpAddr", signed_user_agent, pdf_sha256,
                  decline_reason, version
           FROM signature_requests WHERE workspace_id = app_workspace_id() AND id = $1 FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct NewSignature<'a> {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version_number: i32,
    pub round_id: Uuid,
    pub position: i32,
    pub signer_name: &'a str,
    pub signer_email: &'a str,
    pub token_hash: &'a [u8],
}

pub async fn insert_signature(
    conn: &mut PgConnection,
    s: NewSignature<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO signature_requests (id, workspace_id, document_id, version_number, round_id, position,
                                           signer_name, signer_email, token_hash, created_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9)"#,
        s.id,
        s.document_id,
        s.version_number,
        s.round_id,
        s.position,
        s.signer_name,
        s.signer_email,
        s.token_hash,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Cancels a document's requests still waiting for an answer; returns how many.
pub async fn cancel_pending(conn: &mut PgConnection, document_id: Uuid) -> Result<u64> {
    Ok(sqlx::query!(
        r#"UPDATE signature_requests SET status = 'cancelled'
           WHERE workspace_id = app_workspace_id() AND document_id = $1 AND status = 'pending'"#,
        document_id
    )
    .execute(conn)
    .await?
    .rows_affected())
}

pub async fn mark_signature_viewed(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE signature_requests SET viewed_at = coalesce(viewed_at, now())
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub struct Decision<'a> {
    pub signed: bool,
    pub name: &'a str,
    pub ip: Option<IpAddr>,
    pub user_agent: Option<&'a str>,
    pub pdf_sha256: Option<&'a [u8]>,
    pub decline_reason: Option<&'a str>,
}

pub async fn decide_signature(conn: &mut PgConnection, id: Uuid, d: Decision<'_>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE signature_requests
           SET status = CASE WHEN $2 THEN 'signed' ELSE 'declined' END, decided_at = now(), signed_name = $3,
               signed_ip = $4, signed_user_agent = $5, pdf_sha256 = $6, decline_reason = $7
           WHERE workspace_id = app_workspace_id() AND id = $1 AND status = 'pending'"#,
        id,
        d.signed,
        d.name,
        d.ip as Option<IpAddr>,
        d.user_agent,
        d.pdf_sha256,
        d.decline_reason
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The signing request behind a signer's link, in any workspace.
pub async fn find_public_signature(
    conn: &mut PgConnection,
    token_hash: &[u8],
) -> Result<Option<(Uuid, WorkspaceId)>> {
    Ok(sqlx::query!(
        r#"SELECT id AS "id!", workspace_id AS "workspace_id!: WorkspaceId" FROM signature_public_find($1)"#,
        token_hash
    )
    .fetch_optional(conn)
    .await?
    .map(|r| (r.id, r.workspace_id)))
}

/// Documents whose `expires_on` (at midnight in the workspace's time zone,
/// plus a rule's offset) falls in `(from, to]`, for date-offset rules.
pub async fn expiring_between(
    conn: &mut PgConnection,
    timezone: &str,
    offset_minutes: i32,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<(Uuid, DateTime<Utc>)>> {
    Ok(sqlx::query!(
        r#"SELECT id AS "id!", at AS "at!" FROM (
               SELECT id, expires_on::timestamp AT TIME ZONE $1 + make_interval(mins => $2::int) AS at
               FROM documents
               WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL AND expires_on IS NOT NULL
           ) t
           WHERE at > $3 AND at <= $4
           ORDER BY at, id
           LIMIT $5"#,
        timezone,
        offset_minutes,
        from,
        to,
        limit
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| (r.id, r.at))
    .collect())
}
