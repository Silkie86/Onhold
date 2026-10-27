//! Stored files: uploads, generated PDFs and their bytes, wherever the
//! deployment keeps them. A PDF's text is read by a job (`file.read`) so it
//! can be searched and its fields suggested. A file that nothing uses is
//! removed by the worker after a day.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use core_db::Tx;
use core_db::files::File;
use core_db::jobs::NewJob;
use core_domain::{Ctx, Permission, UserId, WorkspaceId};
use core_integrations::storage::{BlobStore, DirectoryStore, S3Store, StorageError};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use super::begin;
use crate::AppState;
use crate::config::StorageConfig;
use crate::error::{ApiError, ApiResult};

pub const JOB_READ: &str = "file.read";
/// Unattached uploads older than this are removed.
pub const UNATTACHED_TTL: chrono::Duration = chrono::Duration::hours(24);

/// Where new files' bytes go. Files already stored stay where they were
/// put: each records its own storage.
pub struct Storage {
    remote: Option<Arc<dyn BlobStore>>,
}

impl Storage {
    pub fn from_config(config: &StorageConfig) -> Result<Self, ApiError> {
        let remote: Option<Arc<dyn BlobStore>> = match config {
            StorageConfig::Database => None,
            StorageConfig::Directory(dir) => Some(Arc::new(DirectoryStore::new(dir.clone()))),
            StorageConfig::S3(s3) => Some(Arc::new(
                S3Store::new(s3.clone()).map_err(|e| ApiError::internal(e.to_string()))?,
            )),
        };
        Ok(Self { remote })
    }

    /// `database`, `directory` or `s3`.
    pub fn kind(&self) -> &'static str {
        self.remote.as_ref().map_or("database", |r| r.kind())
    }

    fn store_for(&self, kind: &str) -> ApiResult<&Arc<dyn BlobStore>> {
        match &self.remote {
            Some(r) if r.kind() == kind => Ok(r),
            _ => Err(ApiError::internal(format!(
                "a file is kept in `{kind}` storage, which this server is not set up for"
            ))),
        }
    }

    /// Deletes bytes kept outside the database (the database's go with their row).
    pub async fn delete(&self, kind: &str, key: &str) -> ApiResult<()> {
        if kind == "database" {
            return Ok(());
        }
        self.store_for(kind)?
            .delete(key)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))
    }
}

/// What people see of a file.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FileView {
    pub id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    /// Hex SHA-256 of the bytes.
    pub sha256: String,
    /// For PDFs: `pending` until its text is read, then `done` or `failed`.
    pub read_state: String,
    pub read_error: Option<String>,
    pub page_count: Option<i32>,
    /// Pages look scanned: only OCR could read them.
    pub needs_ocr: bool,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
}

pub fn hex(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(bytes)
}

impl From<File> for FileView {
    fn from(f: File) -> Self {
        Self {
            id: f.id,
            filename: f.filename,
            content_type: f.content_type,
            size_bytes: f.size_bytes,
            sha256: hex(&f.sha256),
            read_state: f.read_state,
            read_error: f.read_error,
            page_count: f.page_count,
            needs_ocr: f.needs_ocr,
            created_at: f.created_at,
            created_by: f.created_by,
        }
    }
}

pub const PDF: &str = "application/pdf";

/// What a file really is, from its first bytes, falling back to what the
/// uploader said for kinds that have no signature (text, CSV).
pub fn sniff(bytes: &[u8], claimed: Option<&str>, filename: &str) -> String {
    let head = &bytes[..bytes.len().min(1024)];
    let starts = |sig: &[u8]| bytes.starts_with(sig);
    if head.windows(5).any(|w| w == b"%PDF-") {
        return PDF.into();
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        return "image/png".into();
    }
    if starts(b"\xff\xd8\xff") {
        return "image/jpeg".into();
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return "image/gif".into();
    }
    if bytes.len() > 12 && starts(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return "image/webp".into();
    }
    if starts(b"PK\x03\x04") {
        let lower = filename.to_lowercase();
        return if lower.ends_with(".docx") {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into()
        } else if lower.ends_with(".xlsx") {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into()
        } else {
            "application/zip".into()
        };
    }
    match claimed.map(|c| c.split(';').next().unwrap_or("").trim().to_lowercase()) {
        Some(c) if (c == "text/plain" || c == "text/csv") && std::str::from_utf8(head).is_ok() => c,
        _ => "application/octet-stream".into(),
    }
}

/// A filename safe to store and to send back in a header.
pub fn clean_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '"' | '\\' | '/' | ':' | '*' | '?' | '<' | '>' | '|')
        })
        .take(200)
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_owned();
    if cleaned.is_empty() {
        "file".into()
    } else {
        cleaned
    }
}

/// Keeps bytes and records the file, in the caller's transaction. Bytes
/// kept outside the database are written first: if the transaction then
/// fails they are orphaned, which costs space but never loses a file.
pub(crate) async fn store(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
    attached: bool,
) -> ApiResult<File> {
    let id = core_domain::ids::new_id();
    let sha = Sha256::digest(bytes);
    let key = format!("{}/{}", ctx.workspace_id.0, id.simple());
    let storage = state.storage.kind();
    if let Some(remote) = &state.storage.remote {
        remote
            .put(&key, bytes, content_type)
            .await
            .map_err(|e| match e {
                StorageError::Unavailable(m) => ApiError::internal(m),
                other => ApiError::internal(other.to_string()),
            })?;
    }
    let is_pdf = content_type == PDF;
    core_db::files::insert(
        tx,
        core_db::files::NewFile {
            id,
            filename,
            content_type,
            size_bytes: bytes.len() as i64,
            sha256: sha.as_slice(),
            storage,
            storage_key: &key,
            read_state: if is_pdf { "pending" } else { "skipped" },
            attached,
        },
        ctx.user_id(),
    )
    .await?;
    if storage == "database" {
        core_db::files::insert_blob(tx, id, bytes).await?;
    }
    if is_pdf {
        let payload = json!({ "file_id": id });
        core_db::jobs::enqueue(
            tx,
            NewJob::new(Some(ctx.workspace_id), JOB_READ, &payload).dedupe(&id.to_string()),
        )
        .await?;
    }
    core_db::files::get(tx, id).await?.ok_or(ApiError::NotFound)
}

/// A file's bytes.
pub(crate) async fn bytes(state: &AppState, tx: &mut Tx, file: &File) -> ApiResult<Vec<u8>> {
    if file.storage == "database" {
        return core_db::files::blob(tx, file.id)
            .await?
            .ok_or(ApiError::NotFound);
    }
    state
        .storage
        .store_for(&file.storage)?
        .get(&file.storage_key)
        .await
        .map_err(|e| match e {
            StorageError::NotFound => ApiError::NotFound,
            StorageError::Unavailable(m) => ApiError::internal(m),
            other => ApiError::internal(other.to_string()),
        })
}

/// An upload: kept for a day unless attached to a document or import.
pub async fn upload(
    state: &AppState,
    ctx: &Ctx,
    filename: &str,
    claimed_type: Option<&str>,
    body: &[u8],
) -> ApiResult<FileView> {
    ctx.authorize(Permission::DocumentWrite)?;
    if body.is_empty() {
        return Err(ApiError::invalid("file", "the file is empty"));
    }
    if body.len() > state.config.max_upload_bytes {
        return Err(ApiError::invalid(
            "file",
            format!(
                "files may be at most {} MB",
                state.config.max_upload_bytes / (1024 * 1024)
            ),
        ));
    }
    let filename = clean_filename(filename);
    let content_type = sniff(body, claimed_type, &filename);
    let mut tx = begin(state, ctx).await?;
    let file = store(state, &mut tx, ctx, &filename, &content_type, body, false).await?;
    tx.commit().await?;
    Ok(file.into())
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<FileView> {
    ctx.authorize(Permission::DocumentRead)?;
    let mut tx = begin(state, ctx).await?;
    let file = core_db::files::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(file.into())
}

/// A file to send back: its record and bytes.
pub struct Download {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

impl Download {
    /// PDFs and images may show in the browser; anything else downloads.
    pub fn inline(&self) -> bool {
        self.content_type == PDF
            || matches!(
                self.content_type.as_str(),
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            )
    }
}

pub async fn download(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<Download> {
    ctx.authorize(Permission::DocumentRead)?;
    let mut tx = begin(state, ctx).await?;
    let file = core_db::files::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let bytes = bytes(state, &mut tx, &file).await?;
    tx.commit().await?;
    Ok(Download {
        filename: file.filename,
        content_type: file.content_type,
        bytes,
    })
}

/// Reads a PDF's text (the worker's `file.read`): for search, and for an
/// import waiting on it, the suggested fields and matching records.
pub async fn read(state: &AppState, workspace: WorkspaceId, id: Uuid) -> ApiResult<()> {
    let ctx = Ctx::system(workspace);
    let mut tx = begin(state, &ctx).await?;
    let Some(file) = core_db::files::get(&mut tx, id).await? else {
        return Ok(());
    };
    if file.read_state != "pending" {
        return Ok(());
    }
    let data = bytes(state, &mut tx, &file).await?;
    let extracted =
        tokio::task::spawn_blocking(move || core_pdf::extract(&data, &core_pdf::Limits::default()))
            .await
            .map_err(ApiError::internal)?;
    match &extracted {
        Ok(x) => {
            let text = x.text();
            core_db::files::set_read(
                &mut tx,
                id,
                core_db::files::ReadResult {
                    state: "done",
                    error: None,
                    page_count: i32::try_from(x.pages.len()).ok(),
                    needs_ocr: x.needs_ocr(),
                    // Postgres text cannot hold NUL.
                    text: Some(&text.replace('\0', "")),
                },
            )
            .await?;
        }
        Err(e) => {
            let message = e.to_string();
            core_db::files::set_read(
                &mut tx,
                id,
                core_db::files::ReadResult {
                    state: "failed",
                    error: Some(&message),
                    page_count: None,
                    needs_ocr: false,
                    text: None,
                },
            )
            .await?;
        }
    }
    if let Some(import) = core_db::pdf_imports::for_file(&mut tx, id).await? {
        super::pdf_imports::after_read(&mut tx, &ctx, import, extracted.as_ref().ok()).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Removes uploads nobody attached within a day (the worker's sweep).
pub async fn sweep_unattached(
    state: &AppState,
    worker: &mut sqlx::PgConnection,
) -> ApiResult<usize> {
    let before = Utc::now() - UNATTACHED_TTL;
    let mut removed = 0;
    for f in core_db::files::unattached_before(worker, before, 200).await? {
        state.storage.delete(&f.storage, &f.storage_key).await?;
        if core_db::files::delete_unattached(worker, f.id).await? {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_by_content() {
        assert_eq!(sniff(b"%PDF-1.7\n...", Some("text/plain"), "a.txt"), PDF);
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nxxxx", None, "a"), "image/png");
        assert_eq!(
            sniff(b"<html><script>", Some("text/html"), "a.html"),
            "application/octet-stream"
        );
        assert_eq!(sniff(b"a,b\n1,2", Some("text/csv"), "a.csv"), "text/csv");
        assert!(sniff(b"PK\x03\x04....", None, "Contract.DOCX").len() > 20);
    }

    #[test]
    fn cleans_filenames() {
        assert_eq!(clean_filename("../../etc/passwd"), "passwd");
        assert_eq!(clean_filename("C:\\Users\\x\\inv\"oice.pdf"), "invoice.pdf");
        assert_eq!(clean_filename("..."), "file");
        assert_eq!(clean_filename("a\r\nb.pdf"), "ab.pdf");
    }
}
