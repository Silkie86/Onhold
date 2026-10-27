//! Files and documents: uploads, generated and uploaded documents with
//! their versions, e-signature (and the signer's page), booking packs,
//! document templates, and bringing in past PDFs.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{IdempotencyHeader, IfMatchHeader, WorkspaceHeader, with_etag};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::{ClientMeta, IfMatch, WsCtx};
use crate::services::documents::{
    self, DocumentDetail, DocumentFilter, DocumentPage, DocumentPatch, DocumentTemplate,
    GenerateDocument, NewDocument, NewVersionFile, PublicSigning, RequestSignatures, SignDecision,
    TemplateSource,
};
use crate::services::files::{self, Download, FileView};
use crate::services::pdf_imports::{
    self, ConfirmPdfImport, NewPdfImports, PdfImportView, PdfTemplateView, Reading,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(upload_file))
        .routes(routes!(get_file))
        .routes(routes!(download_file))
        .routes(routes!(read_file))
        .routes(routes!(list_documents, create_document))
        .routes(routes!(generate_document))
        .routes(routes!(document_templates))
        .routes(routes!(document_template_source))
        .routes(routes!(get_document, update_document, delete_document))
        .routes(routes!(download_document))
        .routes(routes!(regenerate_document))
        .routes(routes!(add_document_version))
        .routes(routes!(request_signatures, cancel_signatures))
        .routes(routes!(booking_pack))
        .routes(routes!(list_pdf_imports, create_pdf_imports))
        .routes(routes!(get_pdf_import))
        .routes(routes!(confirm_pdf_import))
        .routes(routes!(discard_pdf_import))
        .routes(routes!(rematch_pdf_import))
        .routes(routes!(list_pdf_templates, create_pdf_template))
        .routes(routes!(archive_pdf_template))
        .routes(routes!(public_signing, public_sign))
        .routes(routes!(public_signing_pdf))
}

/// Sends a stored file back. PDFs and images may open in the browser;
/// everything else downloads. Nothing is sniffed, and the file is
/// sandboxed so it cannot run script against the app's origin.
pub(crate) fn file_response(d: Download) -> Response {
    let mut headers = HeaderMap::new();
    let ct = HeaderValue::from_str(&d.content_type)
        .unwrap_or(HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::CONTENT_TYPE, ct);
    let ascii: String = d
        .filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_. ".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = url::form_urlencoded::byte_serialize(d.filename.as_bytes()).collect();
    let disposition = format!(
        "{}; filename=\"{ascii}\"; filename*=UTF-8''{}",
        if d.inline() { "inline" } else { "attachment" },
        encoded.replace('+', "%20")
    );
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    (headers, d.bytes).into_response()
}

// Files ------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct UploadQuery {
    /// The file's name, as it should be kept.
    pub filename: String,
}

/// Upload a file as the raw request body (up to 25 MB). It is kept for a
/// day unless a document or import uses it. A PDF's text is read in the
/// background so it can be searched.
#[utoipa::path(post, path = "/files", tag = "documents", params(WorkspaceHeader, UploadQuery),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 201, body = FileView), (status = 400, body = ErrorBody), (status = 413, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn upload_file(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<FileView>)> {
    let claimed = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    Ok((
        StatusCode::CREATED,
        Json(files::upload(&state, &ctx, &q.filename, claimed, &body).await?),
    ))
}

#[utoipa::path(get, path = "/files/{id}", tag = "documents", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = FileView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_file(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<FileView>> {
    Ok(Json(files::get(&state, &ctx, id).await?))
}

#[utoipa::path(get, path = "/files/{id}/content", tag = "documents", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, content_type = "application/octet-stream", body = Vec<u8>), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn download_file(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    Ok(file_response(files::download(&state, &ctx, id).await?))
}

/// Fields suggested from a stored PDF's text, without importing it.
#[utoipa::path(get, path = "/files/{id}/reading", tag = "documents", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = Reading), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn read_file(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Reading>> {
    Ok(Json(pdf_imports::read_file(&state, &ctx, id).await?))
}

// Documents ---------------------------------------------------------------------------------

/// Documents, newest first. `q` searches titles, tags and the text inside PDFs.
#[utoipa::path(get, path = "/documents", tag = "documents", params(WorkspaceHeader, DocumentFilter),
    responses((status = 200, body = DocumentPage)), security(("session" = []), ("api_key" = [])))]
async fn list_documents(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(f): Query<DocumentFilter>,
) -> ApiResult<Json<DocumentPage>> {
    Ok(Json(documents::list(&state, &ctx, &f).await?))
}

/// A document from an uploaded file.
#[utoipa::path(post, path = "/documents", tag = "documents", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewDocument,
    responses((status = 201, body = DocumentDetail), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_document(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewDocument>,
) -> ApiResult<(StatusCode, Json<DocumentDetail>)> {
    Ok((
        StatusCode::CREATED,
        Json(documents::create(&state, &ctx, b).await?),
    ))
}

/// Make a document from a template (a contract, confirmation or advance
/// sheet from a booking) and print it.
#[utoipa::path(post, path = "/documents/generate", tag = "documents", params(WorkspaceHeader, IdempotencyHeader),
    request_body = GenerateDocument,
    responses((status = 201, body = DocumentDetail), (status = 400, body = ErrorBody), (status = 422, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn generate_document(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<GenerateDocument>,
) -> ApiResult<(StatusCode, Json<DocumentDetail>)> {
    Ok((
        StatusCode::CREATED,
        Json(documents::generate(&state, &ctx, b).await?),
    ))
}

/// The templates documents can be made from.
#[utoipa::path(get, path = "/document-templates", tag = "documents", params(WorkspaceHeader),
    responses((status = 200, body = Vec<DocumentTemplate>)), security(("session" = []), ("api_key" = [])))]
async fn document_templates(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Vec<DocumentTemplate>>> {
    Ok(Json(documents::templates(&state, &ctx).await?))
}

/// A built-in template's source, to start the workspace's own version from
/// (save it as a `document` template with the same key).
#[utoipa::path(get, path = "/document-templates/{key}/source", tag = "documents",
    params(WorkspaceHeader, ("key" = String, Path)),
    responses((status = 200, body = TemplateSource), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn document_template_source(
    WsCtx(_ctx): WsCtx,
    Path(key): Path<String>,
) -> ApiResult<Json<TemplateSource>> {
    Ok(Json(documents::built_in_source(&key)?))
}

/// A document with its versions and signature requests.
#[utoipa::path(get, path = "/documents/{id}", tag = "documents", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = DocumentDetail), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_document(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let d = documents::get(&state, &ctx, id).await?;
    Ok(with_etag(d.document.version, d))
}

/// Change what is recorded about a document.
#[utoipa::path(patch, path = "/documents/{id}", tag = "documents",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = DocumentPatch,
    responses((status = 200, body = DocumentDetail), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_document(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<DocumentPatch>,
) -> ApiResult<Response> {
    let d = documents::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(d.document.version, d))
}

#[utoipa::path(delete, path = "/documents/{id}", tag = "documents", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn delete_document(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    documents::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct VersionQuery {
    /// Default: the current version.
    pub version: Option<i32>,
}

/// A version's PDF (or uploaded file).
#[utoipa::path(get, path = "/documents/{id}/content", tag = "documents",
    params(WorkspaceHeader, ("id" = Uuid, Path), VersionQuery),
    responses((status = 200, content_type = "application/pdf", body = Vec<u8>), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn download_document(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<VersionQuery>,
) -> ApiResult<Response> {
    Ok(file_response(
        documents::download(&state, &ctx, id, q.version).await?,
    ))
}

/// Make a generated document again from its template, as a new version.
#[utoipa::path(post, path = "/documents/{id}/regenerate", tag = "documents",
    params(WorkspaceHeader, IfMatchHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = DocumentDetail), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn regenerate_document(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
) -> ApiResult<Json<DocumentDetail>> {
    Ok(Json(
        documents::regenerate(&state, &ctx, id, if_match).await?,
    ))
}

/// A new version of an uploaded document from another uploaded file.
#[utoipa::path(post, path = "/documents/{id}/versions", tag = "documents",
    params(WorkspaceHeader, IfMatchHeader, ("id" = Uuid, Path)), request_body = NewVersionFile,
    responses((status = 200, body = DocumentDetail), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn add_document_version(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<NewVersionFile>,
) -> ApiResult<Json<DocumentDetail>> {
    Ok(Json(
        documents::add_file_version(&state, &ctx, id, b, if_match).await?,
    ))
}

/// Send the current version for e-signature: each signer gets their own link.
#[utoipa::path(post, path = "/documents/{id}/signatures", tag = "documents",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = RequestSignatures,
    responses((status = 200, body = DocumentDetail), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn request_signatures(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<RequestSignatures>,
) -> ApiResult<Json<DocumentDetail>> {
    Ok(Json(
        documents::request_signatures(&state, &ctx, id, b, if_match).await?,
    ))
}

/// Stop waiting for signatures.
#[utoipa::path(delete, path = "/documents/{id}/signatures", tag = "documents",
    params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = DocumentDetail), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn cancel_signatures(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<DocumentDetail>> {
    Ok(Json(documents::cancel_signatures(&state, &ctx, id).await?))
}

/// Every document of a booking and its invoices, as one zip.
#[utoipa::path(get, path = "/bookings/{id}/pack", tag = "documents", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, content_type = "application/zip", body = Vec<u8>), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn booking_pack(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    Ok(file_response(
        documents::booking_pack(&state, &ctx, id).await?,
    ))
}

// Past PDFs ------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct ImportStatusQuery {
    /// Comma-separated: reading, ready, confirmed, discarded, failed.
    pub status: Option<String>,
}

#[utoipa::path(get, path = "/pdf-imports", tag = "documents", params(WorkspaceHeader, ImportStatusQuery),
    responses((status = 200, body = Vec<PdfImportView>)), security(("session" = []), ("api_key" = [])))]
async fn list_pdf_imports(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<ImportStatusQuery>,
) -> ApiResult<Json<Vec<PdfImportView>>> {
    Ok(Json(
        pdf_imports::list(&state, &ctx, q.status.as_deref()).await?,
    ))
}

/// Start reading a batch of uploaded PDFs.
#[utoipa::path(post, path = "/pdf-imports", tag = "documents", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewPdfImports,
    responses((status = 201, body = Vec<PdfImportView>), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_pdf_imports(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewPdfImports>,
) -> ApiResult<(StatusCode, Json<Vec<PdfImportView>>)> {
    Ok((
        StatusCode::CREATED,
        Json(pdf_imports::create(&state, &ctx, b).await?),
    ))
}

#[utoipa::path(get, path = "/pdf-imports/{id}", tag = "documents", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = PdfImportView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_pdf_import(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = pdf_imports::get(&state, &ctx, id).await?;
    Ok(with_etag(v.version, v))
}

/// Keep the PDF as a document on a record, and for an invoice, as a past invoice.
#[utoipa::path(post, path = "/pdf-imports/{id}/confirm", tag = "documents",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = ConfirmPdfImport,
    responses((status = 200, body = PdfImportView), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn confirm_pdf_import(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<ConfirmPdfImport>,
) -> ApiResult<Json<PdfImportView>> {
    Ok(Json(
        pdf_imports::confirm(&state, &ctx, id, b, if_match).await?,
    ))
}

#[utoipa::path(post, path = "/pdf-imports/{id}/discard", tag = "documents", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = PdfImportView), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn discard_pdf_import(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PdfImportView>> {
    Ok(Json(pdf_imports::discard(&state, &ctx, id).await?))
}

/// Read a ready import's text again, with the current issuer templates.
#[utoipa::path(post, path = "/pdf-imports/{id}/rematch", tag = "documents", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = PdfImportView), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn rematch_pdf_import(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PdfImportView>> {
    Ok(Json(pdf_imports::rematch(&state, &ctx, id).await?))
}

/// Field patterns for issuers whose PDFs arrive often.
#[utoipa::path(get, path = "/pdf-templates", tag = "documents", params(WorkspaceHeader),
    responses((status = 200, body = Vec<PdfTemplateView>)), security(("session" = []), ("api_key" = [])))]
async fn list_pdf_templates(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Vec<PdfTemplateView>>> {
    Ok(Json(pdf_imports::templates(&state, &ctx).await?))
}

#[utoipa::path(post, path = "/pdf-templates", tag = "documents", params(WorkspaceHeader, IdempotencyHeader),
    request_body = core_pdf::templates::Template,
    responses((status = 201, body = PdfTemplateView), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_pdf_template(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<core_pdf::templates::Template>,
) -> ApiResult<(StatusCode, Json<PdfTemplateView>)> {
    Ok((
        StatusCode::CREATED,
        Json(pdf_imports::create_template(&state, &ctx, b).await?),
    ))
}

#[utoipa::path(delete, path = "/pdf-templates/{id}", tag = "documents", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn archive_pdf_template(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    pdf_imports::archive_template(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// The signer's page ------------------------------------------------------------------------------

/// What a signer's link shows.
#[utoipa::path(get, path = "/public/sign/{token}", tag = "public", params(("token" = String, Path)),
    responses((status = 200, body = PublicSigning), (status = 404, body = ErrorBody)))]
async fn public_signing(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    Path(token): Path<String>,
) -> ApiResult<Json<PublicSigning>> {
    Ok(Json(documents::public_get(&state, &token, meta).await?))
}

/// Sign (typing one's name) or decline.
#[utoipa::path(post, path = "/public/sign/{token}", tag = "public", params(("token" = String, Path)),
    request_body = SignDecision,
    responses((status = 200, body = PublicSigning), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)))]
async fn public_sign(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    Path(token): Path<String>,
    Json(b): Json<SignDecision>,
) -> ApiResult<Json<PublicSigning>> {
    Ok(Json(
        documents::public_decide(&state, &token, meta, b).await?,
    ))
}

/// The PDF to sign (the signed copy once everyone has signed).
#[utoipa::path(get, path = "/public/sign/{token}/pdf", tag = "public", params(("token" = String, Path)),
    responses((status = 200, content_type = "application/pdf", body = Vec<u8>), (status = 404, body = ErrorBody)))]
async fn public_signing_pdf(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Response> {
    Ok(file_response(documents::public_pdf(&state, &token).await?))
}
