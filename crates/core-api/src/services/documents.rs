//! Documents: contracts, confirmations, advance sheets, invoices,
//! statements and anything uploaded. Each has numbered versions; a version
//! is a PDF kept as a file. Generated documents are rendered from a
//! template (the workspace's own, or a built-in one) into HTML in the
//! workspace's branding, then printed to PDF. A document can be sent for
//! e-signature: each signer gets a link, reads the PDF and types their
//! name. When all have signed, a signed copy is made: the same PDF with a
//! signing certificate appended, recording each signature and the PDF's
//! fingerprint.

use std::io::Write as _;
use std::net::IpAddr;

use chrono::{DateTime, NaiveDate, Utc};
use core_db::Tx;
use core_db::documents::{Document, Signature, Version};
use core_db::jobs::NewJob;
use core_domain::crypto::{hash_token, new_token};
use core_domain::{Ctx, Permission, UserId, WorkspaceId};
use core_engines::documents::{self as engine, Branding, SignatureRecord};
use core_engines::templates::Kind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::crm::Check;
use super::files::{self, PDF};
use super::{Change, begin, check_version, record, record_with_secret};
use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::routes::nullable;

pub const ENTITY: &str = "document";
pub const JOB_RENDER: &str = "document.render";
/// Seals a document whose signers have all signed (`document_id`, `round_id`).
pub const JOB_SEAL: &str = "document.seal";
/// Records a document can belong to.
pub const RELATED: &[&str] = &[
    "talent",
    "organisation",
    "contact",
    "enquiry",
    "booking",
    "invoice",
];
pub const MAX_SIGNERS: usize = 10;

// Views ----------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DocumentView {
    pub id: Uuid,
    /// contract, rider, invoice, credit_note, receipt, statement, confirmation,
    /// advance_sheet, quote, purchase_order, insurance, other…
    pub kind: String,
    pub title: String,
    pub related_entity: Option<String>,
    pub related_id: Option<Uuid>,
    /// generated, uploaded or imported.
    pub source: String,
    pub template_key: Option<String>,
    pub current_version: i32,
    /// The current version: `rendering`, `ready` or `failed`.
    pub status: Option<String>,
    pub file: Option<DocumentFile>,
    pub document_date: Option<NaiveDate>,
    /// For insurance, visas and the like: when it runs out.
    pub expires_on: Option<NaiveDate>,
    /// none, awaiting, signed or declined.
    pub signature_status: String,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    #[schema(value_type = Object)]
    pub custom: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DocumentFile {
    pub id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
}

impl From<Document> for DocumentView {
    fn from(d: Document) -> Self {
        let file = match (d.file_id, d.filename, d.content_type, d.size_bytes) {
            (Some(id), Some(filename), Some(content_type), Some(size_bytes)) => {
                Some(DocumentFile {
                    id,
                    filename,
                    content_type,
                    size_bytes,
                })
            }
            _ => None,
        };
        Self {
            id: d.id,
            kind: d.kind,
            title: d.title,
            related_entity: d.related_entity,
            related_id: d.related_id,
            source: d.source,
            template_key: d.template_key,
            current_version: d.current_version,
            status: d.status,
            file,
            document_date: d.document_date,
            expires_on: d.expires_on,
            signature_status: d.signature_status,
            notes: d.notes,
            tags: d.tags,
            custom: d.custom,
            version: d.version,
            created_at: d.created_at,
            updated_at: d.updated_at,
            created_by: d.created_by,
            updated_by: d.updated_by,
        }
    }
}

pub fn to_json(v: &DocumentView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

pub(crate) async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<DocumentView>> {
    Ok(core_db::documents::get(tx, id).await?.map(Into::into))
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VersionView {
    pub number: i32,
    /// original, or signed (the original with its signing certificate).
    pub purpose: String,
    pub status: String,
    pub file: Option<DocumentFile>,
    /// SHA-256 of the PDF, in hex.
    pub sha256: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
}

impl From<Version> for VersionView {
    fn from(v: Version) -> Self {
        let file = match (v.file_id, v.filename, v.content_type, v.size_bytes) {
            (Some(id), Some(filename), Some(content_type), Some(size_bytes)) => {
                Some(DocumentFile {
                    id,
                    filename,
                    content_type,
                    size_bytes,
                })
            }
            _ => None,
        };
        Self {
            number: v.number,
            purpose: v.purpose,
            status: v.status,
            file,
            sha256: v.sha256.as_deref().map(files::hex),
            error: v.error,
            created_at: v.created_at,
            created_by: v.created_by,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SignatureView {
    pub id: Uuid,
    pub version_number: i32,
    pub signer_name: String,
    pub signer_email: String,
    /// pending, signed, declined or cancelled.
    pub status: String,
    pub sent_at: DateTime<Utc>,
    pub viewed_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub signed_name: Option<String>,
    #[schema(value_type = Option<String>)]
    pub signed_ip: Option<IpAddr>,
    pub signed_user_agent: Option<String>,
    pub decline_reason: Option<String>,
}

impl From<Signature> for SignatureView {
    fn from(s: Signature) -> Self {
        Self {
            id: s.id,
            version_number: s.version_number,
            signer_name: s.signer_name,
            signer_email: s.signer_email,
            status: s.status,
            sent_at: s.sent_at,
            viewed_at: s.viewed_at,
            decided_at: s.decided_at,
            signed_name: s.signed_name,
            signed_ip: s.signed_ip,
            signed_user_agent: s.signed_user_agent,
            decline_reason: s.decline_reason,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DocumentDetail {
    #[serde(flatten)]
    pub document: DocumentView,
    pub versions: Vec<VersionView>,
    pub signatures: Vec<SignatureView>,
}

// Listing --------------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct DocumentPage {
    pub items: Vec<DocumentView>,
    pub total: i64,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct DocumentFilter {
    /// Documents of one record.
    pub related_id: Option<Uuid>,
    /// Comma-separated kinds.
    pub kind: Option<String>,
    /// Words in the title, tags or the PDF's text.
    pub q: Option<String>,
    /// Documents that run out on or before this day.
    pub expires_before: Option<NaiveDate>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn list(state: &AppState, ctx: &Ctx, f: &DocumentFilter) -> ApiResult<DocumentPage> {
    ctx.authorize(Permission::DocumentRead)?;
    let kinds: Vec<String> = f
        .kind
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
        .collect();
    let q = f.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let mut tx = begin(state, ctx).await?;
    let (rows, total) = core_db::documents::list(
        &mut tx,
        core_db::documents::Filter {
            related_id: f.related_id,
            kinds: &kinds,
            q,
            expires_before: f.expires_before,
            offset: f.offset.unwrap_or(0).max(0),
            limit: f.limit.unwrap_or(50).clamp(1, 200),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(DocumentPage {
        items: rows.into_iter().map(Into::into).collect(),
        total,
    })
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<DocumentDetail> {
    ctx.authorize(Permission::DocumentRead)?;
    let mut tx = begin(state, ctx).await?;
    let detail = detail(&mut tx, id).await?;
    tx.commit().await?;
    Ok(detail)
}

async fn detail(tx: &mut Tx, id: Uuid) -> ApiResult<DocumentDetail> {
    let document = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    let versions = core_db::documents::versions(tx, id).await?;
    let signatures = core_db::documents::signatures(tx, id).await?;
    Ok(DocumentDetail {
        document,
        versions: versions.into_iter().map(Into::into).collect(),
        signatures: signatures.into_iter().map(Into::into).collect(),
    })
}

/// The PDF (or uploaded file) of a version; the current one by default.
pub async fn download(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    number: Option<i32>,
) -> ApiResult<files::Download> {
    ctx.authorize(Permission::DocumentRead)?;
    let mut tx = begin(state, ctx).await?;
    let doc = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let v = core_db::documents::version(&mut tx, id, number.unwrap_or(doc.current_version))
        .await?
        .ok_or(ApiError::NotFound)?;
    let file_id = v
        .file_id
        .ok_or_else(|| ApiError::conflict("document_not_ready", "this version is not ready yet"))?;
    let file = core_db::files::get(&mut tx, file_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let bytes = files::bytes(state, &mut tx, &file).await?;
    tx.commit().await?;
    Ok(files::Download {
        filename: file.filename,
        content_type: file.content_type,
        bytes,
    })
}

// Checks ---------------------------------------------------------------------------------

fn check_kind(check: &mut Check, kind: &str) -> String {
    let k = kind.trim();
    if !core_domain::finance::DOCUMENT_KINDS.contains(&k) {
        check.issue(
            "kind",
            format!(
                "must be one of: {}",
                core_domain::finance::DOCUMENT_KINDS.join(", ")
            ),
        );
    }
    k.to_owned()
}

/// Checks the record a document belongs to exists.
async fn check_related(
    tx: &mut Tx,
    state: &AppState,
    entity: Option<&str>,
    id: Option<Uuid>,
) -> ApiResult<()> {
    match (entity, id) {
        (None, None) => Ok(()),
        (Some(e), Some(id)) if RELATED.contains(&e) => {
            let found = if e == super::invoices::ENTITY {
                core_db::invoices::get(tx, id).await?.is_some()
            } else {
                super::context::load_object(tx, state, e, id)
                    .await?
                    .is_some()
            };
            if found {
                Ok(())
            } else {
                Err(ApiError::invalid(
                    "related_id",
                    format!("there is no such {e}"),
                ))
            }
        }
        (Some(e), Some(_)) => Err(ApiError::invalid(
            "related_entity",
            format!("documents cannot belong to `{e}` records"),
        )),
        _ => Err(ApiError::invalid(
            "related_id",
            "give both related_entity and related_id, or neither",
        )),
    }
}

// Rendering ------------------------------------------------------------------------------

/// The workspace's look for documents: `branding.logo` (a data URI) or
/// `branding.logo_file_id` (an uploaded image), the colour (a full theme's
/// light primary, else `branding.primary_color`) and the theme's typeface.
pub(crate) async fn branding(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    issuer: &Value,
) -> ApiResult<Branding> {
    let w = core_db::workspaces::get(tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let b = &w.branding;
    let mut logo = b.get("logo").and_then(Value::as_str).and_then(engine::logo);
    if logo.is_none()
        && let Some(file_id) = b
            .get("logo_file_id")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
        && let Some(file) = core_db::files::get(tx, file_id).await?
        && file.content_type.starts_with("image/")
        && file.size_bytes <= 1_000_000
    {
        let bytes = files::bytes(state, tx, &file).await?;
        logo = engine::logo(&format!(
            "data:{};base64,{}",
            file.content_type,
            data_encoding::BASE64.encode(&bytes)
        ));
    }
    let name = issuer
        .get("name")
        .and_then(Value::as_str)
        .map_or_else(|| w.name.clone(), str::to_owned);
    let header_line = [
        issuer.get("email").and_then(Value::as_str),
        issuer.get("phone").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    Ok(Branding {
        name,
        logo,
        primary: b
            .pointer("/theme/light/primary")
            .or_else(|| b.get("primary_color"))
            .or_else(|| b.get("primary"))
            .and_then(Value::as_str)
            .and_then(engine::colour),
        header_line: (!header_line.is_empty()).then_some(header_line),
        font: b
            .pointer("/theme/font")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// A template by key: the workspace's own, or the built-in one.
pub(crate) async fn template(
    tx: &mut Tx,
    key: &str,
    locale: &str,
) -> ApiResult<(String, String, String)> {
    if let Some(t) = core_db::templates::find(tx, key, locale).await?
        && t.kind == Kind::Document.as_str()
    {
        return Ok((t.name, t.subject.unwrap_or_default(), t.body));
    }
    let b = engine::built_in(key).ok_or_else(|| {
        ApiError::invalid(
            "template_key",
            format!("there is no document template `{key}`"),
        )
    })?;
    Ok((b.name.to_owned(), b.subject.to_owned(), b.body.to_owned()))
}

/// The document kind a template makes.
fn template_kind(key: &str) -> &'static str {
    engine::built_in(key).map_or("misc", |b| b.kind)
}

/// What a booking's documents see: the booking, its first talent, the
/// buyer, the contact and the accepted (or latest) quote.
pub(crate) async fn booking_context(
    tx: &mut Tx,
    state: &AppState,
    booking_id: Uuid,
) -> ApiResult<Value> {
    let b = super::bookings::load(tx, booking_id)
        .await?
        .ok_or_else(|| ApiError::invalid("related_id", "there is no such booking"))?;
    let mut booking = super::bookings::to_json(&b);
    let names: Vec<&str> = b.talent.iter().map(|t| t.name.as_str()).collect();
    if let Some(o) = booking.as_object_mut() {
        o.insert("talent_names".into(), json!(join_names(&names)));
        o.insert(
            "venue".into(),
            json!(b.venue_name.clone().or(b.venue_text.clone())),
        );
        o.insert(
            "deposit_bps".into(),
            json!(b.deposit_percent.map(|p| (p * 100.0).round() as i64)),
        );
    }
    let talent = match b.talent.first() {
        Some(t) => {
            let mut v = super::context::load_object(tx, state, "talent", t.id)
                .await?
                .unwrap_or(Value::Null);
            let members: Vec<Value> = core_db::talent::members(tx, t.id)
                .await?
                .into_iter()
                .map(|m| json!({ "name": m.name, "role": m.role }))
                .collect();
            if let Some(o) = v.as_object_mut() {
                o.insert("members".into(), json!(members));
            }
            v
        }
        None => Value::Null,
    };
    let organisation = match b.organisation_id {
        Some(id) => super::organisations::load(tx, id).await?.map(|o| {
            let mut v = super::organisations::to_json(&o);
            if let Some(obj) = v.as_object_mut() {
                obj.insert("address".into(), json!(o.addresses.first()));
            }
            v
        }),
        None => None,
    };
    let contact = match b.contact_id {
        Some(id) => super::contacts::load(tx, id)
            .await?
            .map(|c| super::contacts::to_json(&c)),
        None => None,
    };
    let quotes = core_db::quotes::list(tx, Some(booking_id), &[], 0, 20)
        .await?
        .0;
    let quote = quotes
        .iter()
        .find(|q| q.status == core_domain::QuoteStatus::Accepted)
        .or_else(|| quotes.first())
        .cloned()
        .map(|q| super::quotes::to_json(&q.into()));
    Ok(json!({
        "booking": booking,
        "talent": talent,
        "organisation": organisation,
        "contact": contact,
        "quote": quote,
    }))
}

fn join_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Renders a template into a whole, branded HTML page.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn render_html(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    key: &str,
    title: Option<&str>,
    reference: &str,
    date: NaiveDate,
    record: Value,
) -> ApiResult<(String, String)> {
    let (workspace, options) = super::context::workspace(tx, state).await?;
    let issuer = record
        .get("issuer")
        .cloned()
        .filter(|v| !v.is_null())
        .unwrap_or(super::finance::issuer_json(tx, ctx, None).await?);
    let (_, subject, body) = template(tx, key, &options.locale).await?;
    let mut tree = json!({
        "workspace": workspace,
        "issuer": issuer,
        "document": { "title": title, "reference": reference, "date": date },
        "now": Utc::now(),
    });
    if let (Some(t), Value::Object(r)) = (tree.as_object_mut(), record) {
        for (k, v) in r {
            t.insert(k, v);
        }
    }
    // The title comes from the template's subject unless one is given.
    let rendered_title = match title {
        Some(t) => t.to_owned(),
        None => {
            super::templates::render_in(tx, Kind::Document, Some(&subject), "", &tree, &options)
                .await?
                .subject
                .unwrap_or_default()
        }
    };
    if let Some(d) = tree.get_mut("document").and_then(Value::as_object_mut) {
        d.insert("title".into(), json!(rendered_title));
    }
    let rendered =
        super::templates::render_in(tx, Kind::Document, None, &body, &tree, &options).await?;
    let branding = branding(state, tx, ctx, &issuer).await?;
    let title = if rendered_title.trim().is_empty() {
        "Document".to_owned()
    } else {
        rendered_title
    };
    let html = engine::page(&title, &rendered.body, &branding);
    Ok((title, html))
}

/// Prints HTML to PDF off the async threads.
pub(crate) async fn print(state: &AppState, html: String, title: String) -> ApiResult<Vec<u8>> {
    let renderer = state.renderer.clone();
    tokio::task::spawn_blocking(move || renderer.render(&html, &title))
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::internal(e.to_string()))
}

fn pdf_filename(title: &str, number: i32) -> String {
    let base: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    let base = if base.is_empty() {
        "document".to_owned()
    } else {
        base.chars().take(80).collect()
    };
    if number > 1 {
        format!("{base}-v{number}.pdf")
    } else {
        format!("{base}.pdf")
    }
}

/// Queues the printing of a version, then tries it at once; the job only
/// does the work if this attempt did not.
pub(crate) async fn queue_render(
    tx: &mut Tx,
    ctx: &Ctx,
    document_id: Uuid,
    number: i32,
) -> ApiResult<()> {
    let payload = json!({ "document_id": document_id, "version": number });
    core_db::jobs::enqueue(
        tx,
        NewJob::new(Some(ctx.workspace_id), JOB_RENDER, &payload)
            .dedupe(&format!("{document_id}:{number}")),
    )
    .await?;
    Ok(())
}

/// Prints a version waiting to be printed (the worker's `document.render`,
/// and straight after a request that made one).
pub async fn render_version(
    state: &AppState,
    workspace: WorkspaceId,
    document_id: Uuid,
    number: i32,
) -> ApiResult<()> {
    let ctx = Ctx::system(workspace);
    let mut tx = begin(state, &ctx).await?;
    if core_db::documents::lock(&mut tx, document_id)
        .await?
        .is_none()
    {
        return Ok(());
    }
    let Some(v) = core_db::documents::version(&mut tx, document_id, number).await? else {
        return Ok(());
    };
    if v.status == "ready" {
        return Ok(());
    }
    let doc = load(&mut tx, document_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let html = v.html.clone().unwrap_or_default();
    match print(state, html, doc.title.clone()).await {
        Ok(pdf) => {
            let file = files::store(
                state,
                &mut tx,
                &ctx,
                &pdf_filename(&doc.title, number),
                PDF,
                &pdf,
                true,
            )
            .await?;
            core_db::documents::finish_version(&mut tx, document_id, number, Some(file.id), None)
                .await?;
            let after = load(&mut tx, document_id)
                .await?
                .ok_or(ApiError::NotFound)?;
            record(
                &mut tx,
                &ctx,
                Change {
                    topic: "document.ready",
                    resource: ENTITY,
                    object_id: Some(document_id),
                    before: None,
                    after: Some(to_json(&after)),
                    event_extra: Some(
                        json!({ "version": number, "renderer": state.renderer.name() }),
                    ),
                },
            )
            .await?;
        }
        Err(e) => {
            let message = e.to_string();
            core_db::documents::finish_version(&mut tx, document_id, number, None, Some(&message))
                .await?;
            tx.commit().await?;
            return Err(e);
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Tries printing now; on failure the queued job will try again.
pub(crate) async fn render_now(state: &AppState, ctx: &Ctx, document_id: Uuid, number: i32) {
    if let Err(e) = render_version(state, ctx.workspace_id, document_id, number).await {
        tracing::warn!(%document_id, number, error = %e, "printing a document failed; the worker will retry");
    }
}

// Creating ----------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct GenerateDocument {
    /// `document.contract`, `document.confirmation`, `document.advance_sheet`
    /// or a workspace template's key.
    pub template_key: String,
    /// The record it is made from (a booking for the built-in templates).
    pub related_entity: String,
    pub related_id: Uuid,
    /// Default: the template's subject.
    pub title: Option<String>,
    /// Default: what the template makes, else `misc`.
    pub kind: Option<String>,
    pub document_date: Option<NaiveDate>,
}

/// The record part of a template's context.
async fn record_context(tx: &mut Tx, state: &AppState, entity: &str, id: Uuid) -> ApiResult<Value> {
    if entity == super::bookings::ENTITY {
        return booking_context(tx, state, id).await;
    }
    let object = super::context::load_object(tx, state, entity, id)
        .await?
        .ok_or_else(|| ApiError::invalid("related_id", format!("there is no such {entity}")))?;
    Ok(json!({ entity: object }))
}

/// Makes a document from a template and prints it.
pub async fn generate(
    state: &AppState,
    ctx: &Ctx,
    req: GenerateDocument,
) -> ApiResult<DocumentDetail> {
    ctx.authorize(Permission::DocumentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let entity = req.related_entity.trim();
    if !RELATED.contains(&entity) || entity == super::invoices::ENTITY {
        return Err(ApiError::invalid(
            "related_entity",
            "documents are made from a booking, enquiry, talent, organisation or contact",
        ));
    }
    ctx.authorize(super::context::read_permission(entity))?;
    let mut check = Check::default();
    let kind = check_kind(
        &mut check,
        req.kind
            .as_deref()
            .unwrap_or(template_kind(&req.template_key)),
    );
    let title = check.optional("title", req.title.as_deref(), 200);
    check.finish()?;
    let today = super::finance::today(&mut tx, ctx).await?;
    let date = req.document_date.unwrap_or(today);
    let id = core_domain::ids::new_id();
    let reference = format!("D-{}", id.simple().to_string()[..8].to_uppercase());
    let record_json = record_context(&mut tx, state, entity, req.related_id).await?;
    let (title, html) = render_html(
        state,
        &mut tx,
        ctx,
        &req.template_key,
        title.as_deref(),
        &reference,
        date,
        record_json,
    )
    .await?;
    let detail = insert_generated(
        &mut tx,
        ctx,
        NewGenerated {
            id,
            kind: &kind,
            title: &title,
            related_entity: Some(entity),
            related_id: Some(req.related_id),
            template_key: Some(&req.template_key),
            document_date: Some(date),
            html: &html,
        },
    )
    .await?;
    tx.commit().await?;
    render_now(state, ctx, id, 1).await;
    get(state, ctx, detail.id).await
}

pub(crate) struct NewGenerated<'a> {
    pub id: Uuid,
    pub kind: &'a str,
    pub title: &'a str,
    pub related_entity: Option<&'a str>,
    pub related_id: Option<Uuid>,
    pub template_key: Option<&'a str>,
    pub document_date: Option<NaiveDate>,
    pub html: &'a str,
}

/// Records a generated document with its first version waiting to print.
/// The caller commits, then calls [`render_now`].
pub(crate) async fn insert_generated(
    tx: &mut Tx,
    ctx: &Ctx,
    g: NewGenerated<'_>,
) -> ApiResult<DocumentView> {
    core_db::documents::insert(
        tx,
        core_db::documents::NewDocument {
            id: g.id,
            kind: g.kind,
            title: g.title,
            related_entity: g.related_entity,
            related_id: g.related_id,
            source: "generated",
            template_key: g.template_key,
            document_date: g.document_date,
            expires_on: None,
            notes: None,
            tags: &[],
            custom: &json!({}),
        },
        ctx.user_id(),
    )
    .await?;
    let number = core_db::documents::add_version(
        tx,
        g.id,
        core_db::documents::NewVersion {
            purpose: "original",
            status: "rendering",
            file_id: None,
            html: Some(g.html),
        },
        ctx.user_id(),
    )
    .await?;
    queue_render(tx, ctx, g.id, number).await?;
    let view = load(tx, g.id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "document.created",
            resource: ENTITY,
            object_id: Some(g.id),
            before: None,
            after: Some(to_json(&view)),
            event_extra: None,
        },
    )
    .await?;
    Ok(view)
}

/// Makes a new version of a document from its template again (the record
/// changed). Waiting signatures are cancelled: they were for the old text.
pub async fn regenerate(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    if_match: Option<i64>,
) -> ApiResult<DocumentDetail> {
    ctx.authorize(Permission::DocumentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::documents::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let (Some(key), Some(entity), Some(related)) = (
        before.template_key.clone(),
        before.related_entity.clone(),
        before.related_id,
    ) else {
        return Err(ApiError::conflict(
            "document_not_generated",
            "only documents made from a template can be made again",
        ));
    };
    if entity == super::invoices::ENTITY || core_db::documents::kept_by_record(&mut tx, id).await? {
        return Err(ApiError::conflict(
            "document_locked",
            "an invoice's PDF is fixed when it is issued",
        ));
    }
    if before.signature_status == "signed" {
        return Err(ApiError::conflict(
            "document_signed",
            "a signed document cannot change; make a new one",
        ));
    }
    let record_json = record_context(&mut tx, state, &entity, related).await?;
    let reference = format!("D-{}", id.simple().to_string()[..8].to_uppercase());
    let date = before
        .document_date
        .unwrap_or(super::finance::today(&mut tx, ctx).await?);
    let (_, html) = render_html(
        state,
        &mut tx,
        ctx,
        &key,
        Some(&before.title),
        &reference,
        date,
        record_json,
    )
    .await?;
    core_db::documents::cancel_pending(&mut tx, id).await?;
    core_db::documents::set_signature_status(&mut tx, id, "none", ctx.user_id()).await?;
    let number = core_db::documents::add_version(
        &mut tx,
        id,
        core_db::documents::NewVersion {
            purpose: "original",
            status: "rendering",
            file_id: None,
            html: Some(&html),
        },
        ctx.user_id(),
    )
    .await?;
    queue_render(&mut tx, ctx, id, number).await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "document.updated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: Some(json!({ "version": number })),
        },
    )
    .await?;
    tx.commit().await?;
    render_now(state, ctx, id, number).await;
    get(state, ctx, id).await
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewDocument {
    /// An uploaded file (`POST /files`).
    pub file_id: Uuid,
    pub kind: String,
    /// Default: the file's name.
    pub title: Option<String>,
    pub related_entity: Option<String>,
    pub related_id: Option<Uuid>,
    pub document_date: Option<NaiveDate>,
    pub expires_on: Option<NaiveDate>,
    pub notes: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[schema(value_type = Option<Object>)]
    pub custom: Option<Value>,
}

/// A document made from an uploaded file.
pub async fn create(state: &AppState, ctx: &Ctx, new: NewDocument) -> ApiResult<DocumentDetail> {
    let id = create_in(state, ctx, new, "uploaded").await?;
    get(state, ctx, id).await
}

pub(crate) async fn create_in(
    state: &AppState,
    ctx: &Ctx,
    new: NewDocument,
    source: &str,
) -> ApiResult<Uuid> {
    ctx.authorize(Permission::DocumentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let id = insert_uploaded(state, &mut tx, ctx, new, source).await?;
    tx.commit().await?;
    Ok(id)
}

pub(crate) async fn insert_uploaded(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    new: NewDocument,
    source: &str,
) -> ApiResult<Uuid> {
    let file = core_db::files::get(tx, new.file_id)
        .await?
        .ok_or_else(|| ApiError::invalid("file_id", "there is no such file"))?;
    let mut check = Check::default();
    let kind = check_kind(&mut check, &new.kind);
    let title = check
        .optional("title", new.title.as_deref(), 200)
        .unwrap_or_else(|| file.filename.chars().take(200).collect());
    let notes = check.long("notes", new.notes.as_deref(), 5000);
    let tags = check.tags(&new.tags);
    let custom = new.custom.unwrap_or_else(|| json!({}));
    if !custom.is_object() {
        check.issue("custom", "must be an object");
    }
    if let (Some(d), Some(e)) = (new.document_date, new.expires_on)
        && e < d
    {
        check.issue("expires_on", "must not be before the document's date");
    }
    check.finish()?;
    let related_entity = new.related_entity.as_deref().map(str::trim);
    check_related(tx, state, related_entity, new.related_id).await?;
    let id = core_domain::ids::new_id();
    core_db::documents::insert(
        tx,
        core_db::documents::NewDocument {
            id,
            kind: &kind,
            title: &title,
            related_entity,
            related_id: new.related_id,
            source,
            template_key: None,
            document_date: new.document_date,
            expires_on: new.expires_on,
            notes: notes.as_deref(),
            tags: &tags,
            custom: &custom,
        },
        ctx.user_id(),
    )
    .await?;
    core_db::files::attach(tx, file.id).await?;
    core_db::documents::add_version(
        tx,
        id,
        core_db::documents::NewVersion {
            purpose: "original",
            status: "ready",
            file_id: Some(file.id),
            html: None,
        },
        ctx.user_id(),
    )
    .await?;
    let view = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "document.created",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(to_json(&view)),
            event_extra: None,
        },
    )
    .await?;
    Ok(id)
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewVersionFile {
    pub file_id: Uuid,
}

/// A new version of an uploaded document from another file.
pub async fn add_file_version(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    req: NewVersionFile,
    if_match: Option<i64>,
) -> ApiResult<DocumentDetail> {
    ctx.authorize(Permission::DocumentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::documents::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.source == "generated" {
        return Err(ApiError::conflict(
            "document_generated",
            "a generated document is made again from its template instead",
        ));
    }
    if before.signature_status == "signed" {
        return Err(ApiError::conflict(
            "document_signed",
            "a signed document cannot change",
        ));
    }
    let file = core_db::files::get(&mut tx, req.file_id)
        .await?
        .ok_or_else(|| ApiError::invalid("file_id", "there is no such file"))?;
    core_db::files::attach(&mut tx, file.id).await?;
    core_db::documents::cancel_pending(&mut tx, id).await?;
    core_db::documents::set_signature_status(&mut tx, id, "none", ctx.user_id()).await?;
    let number = core_db::documents::add_version(
        &mut tx,
        id,
        core_db::documents::NewVersion {
            purpose: "original",
            status: "ready",
            file_id: Some(file.id),
            html: None,
        },
        ctx.user_id(),
    )
    .await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "document.updated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: Some(json!({ "version": number })),
        },
    )
    .await?;
    let d = detail(&mut tx, id).await?;
    tx.commit().await?;
    Ok(d)
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct DocumentPatch {
    pub kind: Option<String>,
    pub title: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub related_entity: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub related_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<NaiveDate>)]
    pub document_date: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<NaiveDate>)]
    pub expires_on: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub notes: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
    #[schema(value_type = Option<Object>)]
    pub custom: Option<Value>,
}

/// Changes what is recorded about a document (never its PDF).
pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    p: DocumentPatch,
    if_match: Option<i64>,
) -> ApiResult<DocumentDetail> {
    ctx.authorize(Permission::DocumentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::documents::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let mut check = Check::default();
    let kind = match &p.kind {
        Some(k) => check_kind(&mut check, k),
        None => before.kind.clone(),
    };
    let title = match &p.title {
        Some(t) => check.required("title", t, 200),
        None => before.title.clone(),
    };
    let notes = match &p.notes {
        Some(n) => check.long("notes", n.as_deref(), 5000),
        None => before.notes.clone(),
    };
    let tags = match &p.tags {
        Some(t) => check.tags(t),
        None => before.tags.clone(),
    };
    let custom = p.custom.clone().unwrap_or_else(|| before.custom.clone());
    if !custom.is_object() {
        check.issue("custom", "must be an object");
    }
    let document_date = p.document_date.unwrap_or(before.document_date);
    let expires_on = p.expires_on.unwrap_or(before.expires_on);
    if let (Some(d), Some(e)) = (document_date, expires_on)
        && e < d
    {
        check.issue("expires_on", "must not be before the document's date");
    }
    check.finish()?;
    let related_entity = p
        .related_entity
        .clone()
        .unwrap_or(before.related_entity.clone())
        .map(|e| e.trim().to_owned());
    let related_id = p.related_id.unwrap_or(before.related_id);
    if p.related_entity.is_some() || p.related_id.is_some() {
        let moved = related_entity != before.related_entity || related_id != before.related_id;
        if moved
            && (before.related_entity.as_deref() == Some(super::invoices::ENTITY)
                || core_db::documents::kept_by_record(&mut tx, id).await?)
        {
            return Err(ApiError::conflict(
                "document_locked",
                "an invoice's or settlement's PDF stays with it",
            ));
        }
        check_related(&mut tx, state, related_entity.as_deref(), related_id).await?;
    }
    core_db::documents::update(
        &mut tx,
        id,
        core_db::documents::Edit {
            kind: &kind,
            title: &title,
            related_entity: related_entity.as_deref(),
            related_id,
            document_date,
            expires_on,
            notes: notes.as_deref(),
            tags: &tags,
            custom: &custom,
        },
        ctx.user_id(),
    )
    .await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "document.updated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    let d = detail(&mut tx, id).await?;
    tx.commit().await?;
    Ok(d)
}

pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::DocumentWrite)?;
    let mut tx = begin(state, ctx).await?;
    core_db::documents::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.related_entity.as_deref() == Some(super::invoices::ENTITY)
        && before.source == "generated"
        || core_db::documents::kept_by_record(&mut tx, id).await?
    {
        return Err(ApiError::conflict(
            "document_locked",
            "an issued invoice's or settlement's PDF is kept with it",
        ));
    }
    core_db::documents::cancel_pending(&mut tx, id).await?;
    core_db::documents::soft_delete(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "document.deleted",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// E-signature ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct Signer {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RequestSignatures {
    /// Who signs, in order. Default: the booking's contact.
    #[serde(default)]
    pub signers: Vec<Signer>,
}

/// Sends the current version for signing: each signer gets their own link.
/// Requests still waiting from before are cancelled.
pub async fn request_signatures(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    req: RequestSignatures,
    if_match: Option<i64>,
) -> ApiResult<DocumentDetail> {
    ctx.authorize(Permission::DocumentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::documents::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.signature_status == "signed" {
        return Err(ApiError::conflict(
            "document_signed",
            "this document is already signed",
        ));
    }
    let current = core_db::documents::version(&mut tx, id, before.current_version)
        .await?
        .ok_or(ApiError::NotFound)?;
    if current.status != "ready" || current.content_type.as_deref() != Some(PDF) {
        return Err(ApiError::conflict(
            "document_not_ready",
            "only a finished PDF can be signed; wait for it to be ready",
        ));
    }
    let mut signers = req.signers;
    if signers.is_empty()
        && before.related_entity.as_deref() == Some(super::bookings::ENTITY)
        && let Some(b) = before.related_id
        && let Some(booking) = super::bookings::load(&mut tx, b).await?
        && let Some(c) = booking.contact_id
        && let Some(contact) = core_db::contacts::get(&mut tx, c).await?
        && let Some(email) = contact.emails.first()
    {
        signers.push(Signer {
            name: contact.name.clone(),
            email: email.clone(),
        });
    }
    let mut check = Check::default();
    if signers.is_empty() {
        check.issue("signers", "give at least one signer");
    }
    if signers.len() > MAX_SIGNERS {
        check.issue("signers", format!("give at most {MAX_SIGNERS} signers"));
    }
    let mut clean = Vec::new();
    for (i, s) in signers.iter().enumerate() {
        let name = check.required(&format!("signers[{i}].name"), &s.name, 200);
        let email = s.email.trim().to_lowercase();
        if !core_engines::fields::is_email(&email) || email.len() > 320 {
            check.issue(format!("signers[{i}].email"), "is not an email address");
        }
        clean.push((name, email));
    }
    check.finish()?;
    // The signing certificate is added to the PDF when everyone has signed;
    // find out now if this one cannot take it.
    let file = core_db::files::get(&mut tx, current.file_id.ok_or(ApiError::NotFound)?)
        .await?
        .ok_or(ApiError::NotFound)?;
    let bytes = files::bytes(state, &mut tx, &file).await?;
    let readable = tokio::task::spawn_blocking(move || core_pdf::write::can_append(&bytes))
        .await
        .map_err(ApiError::internal)?;
    if let Err(e) = readable {
        return Err(ApiError::conflict(
            "pdf_unsignable",
            format!("this PDF cannot be signed here ({e}); upload an unprotected copy"),
        ));
    }
    core_db::documents::cancel_pending(&mut tx, id).await?;
    let round_id = core_domain::ids::new_id();
    let w = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    for (position, (name, email)) in clean.iter().enumerate() {
        let sid = core_domain::ids::new_id();
        let token = new_token();
        core_db::documents::insert_signature(
            &mut tx,
            core_db::documents::NewSignature {
                id: sid,
                document_id: id,
                version_number: current.number,
                round_id,
                position: position as i32,
                signer_name: name,
                signer_email: email,
                token_hash: &hash_token(&token),
            },
            ctx.user_id(),
        )
        .await?;
        let link = state.config.app_url(&format!("/sign/{token}"), &[]);
        record_with_secret(
            &mut tx,
            ctx,
            Change {
                topic: "document.signature_requested",
                resource: ENTITY,
                object_id: Some(id),
                before: None,
                after: None,
                event_extra: Some(json!({
                    "signature_id": sid,
                    "signer": { "name": name, "email": email },
                    "document": { "title": before.title, "kind": before.kind },
                    "workspace_name": w.name,
                })),
            },
            &state.config.keyring,
            &json!({ "url": link }),
        )
        .await?;
    }
    core_db::documents::set_signature_status(&mut tx, id, "awaiting", ctx.user_id()).await?;
    let d = detail(&mut tx, id).await?;
    tx.commit().await?;
    Ok(d)
}

/// Stops waiting for signatures.
pub async fn cancel_signatures(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<DocumentDetail> {
    ctx.authorize(Permission::DocumentWrite)?;
    let mut tx = begin(state, ctx).await?;
    core_db::documents::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.signature_status == "signed" {
        return Err(ApiError::conflict(
            "document_signed",
            "this document is already signed",
        ));
    }
    core_db::documents::cancel_pending(&mut tx, id).await?;
    core_db::documents::set_signature_status(&mut tx, id, "none", ctx.user_id()).await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "document.signatures_cancelled",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    let d = detail(&mut tx, id).await?;
    tx.commit().await?;
    Ok(d)
}

/// What a signer sees.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicSigning {
    pub workspace_name: String,
    #[schema(value_type = Object)]
    pub branding: Value,
    pub document_title: String,
    pub signer_name: String,
    /// pending, signed, declined, or cancelled (a newer version replaced it).
    pub status: String,
    pub decided_at: Option<DateTime<Utc>>,
    pub signed_name: Option<String>,
    /// SHA-256 of the PDF being signed, in hex.
    pub sha256: String,
}

async fn open_public(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
    deciding: bool,
) -> ApiResult<(Tx, Ctx, Signature)> {
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let (id, workspace) = core_db::documents::find_public_signature(&mut tx, &hash_token(token))
        .await?
        .ok_or(ApiError::NotFound)?;
    core_db::set_context(&mut tx, Some(workspace), None).await?;
    let mut ctx = Ctx::system(workspace);
    ctx.meta = meta;
    // Signers answer one at a time per document (locked before the
    // request, as everywhere else), so the last one always sees the
    // others' signatures and seals.
    if deciding && let Some(doc) = core_db::documents::signature_document(&mut tx, id).await? {
        core_db::documents::lock(&mut tx, doc).await?;
    }
    let s = core_db::documents::signature(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((tx, ctx, s))
}

async fn signed_version(tx: &mut Tx, s: &Signature) -> ApiResult<Version> {
    let v = core_db::documents::version(tx, s.document_id, s.version_number)
        .await?
        .ok_or(ApiError::NotFound)?;
    if v.file_id.is_none() {
        return Err(ApiError::NotFound);
    }
    Ok(v)
}

async fn public_view(tx: &mut Tx, ctx: &Ctx, s: &Signature) -> ApiResult<PublicSigning> {
    let w = core_db::workspaces::get(tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let doc = load(tx, s.document_id).await?.ok_or(ApiError::NotFound)?;
    let v = signed_version(tx, s).await?;
    Ok(PublicSigning {
        workspace_name: w.name,
        branding: w.branding,
        document_title: doc.title,
        signer_name: s.signer_name.clone(),
        status: s.status.clone(),
        decided_at: s.decided_at,
        signed_name: s.signed_name.clone(),
        sha256: v.sha256.as_deref().map(files::hex).unwrap_or_default(),
    })
}

/// A signer opens their link; the first view is recorded.
pub async fn public_get(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
) -> ApiResult<PublicSigning> {
    let (mut tx, ctx, s) = open_public(state, token, meta, false).await?;
    if s.viewed_at.is_none() && s.status == "pending" {
        core_db::documents::mark_signature_viewed(&mut tx, s.id).await?;
        record(
            &mut tx,
            &ctx,
            Change {
                topic: "document.signature_viewed",
                resource: ENTITY,
                object_id: Some(s.document_id),
                before: None,
                after: None,
                event_extra: Some(json!({ "signature_id": s.id })),
            },
        )
        .await?;
    }
    let view = public_view(&mut tx, &ctx, &s).await?;
    tx.commit().await?;
    Ok(view)
}

/// The PDF the signer is asked to sign (or the signed copy, once made).
pub async fn public_pdf(state: &AppState, token: &str) -> ApiResult<files::Download> {
    let (mut tx, _ctx, s) = open_public(state, token, Default::default(), false).await?;
    let doc = load(&mut tx, s.document_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Once everyone in their round has signed, the signer may keep the
    // signed copy; never one sealed from a later round.
    let sealed = core_db::documents::signed_round(&mut tx, s.document_id).await?;
    let number = if s.status == "signed" && sealed == Some(s.round_id) {
        doc.current_version
    } else {
        s.version_number
    };
    let v = core_db::documents::version(&mut tx, s.document_id, number)
        .await?
        .ok_or(ApiError::NotFound)?;
    let file = core_db::files::get(&mut tx, v.file_id.ok_or(ApiError::NotFound)?)
        .await?
        .ok_or(ApiError::NotFound)?;
    let bytes = files::bytes(state, &mut tx, &file).await?;
    tx.commit().await?;
    Ok(files::Download {
        filename: file.filename,
        content_type: file.content_type,
        bytes,
    })
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SignDecision {
    /// Sign (true) or decline (false).
    pub sign: bool,
    /// The signer's name, typed as their signature. Required to sign.
    pub signed_name: Option<String>,
    /// Must be true to sign: they agree to sign electronically.
    #[serde(default)]
    pub agree: bool,
    pub reason: Option<String>,
}

/// A signer signs or declines.
pub async fn public_decide(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
    d: SignDecision,
) -> ApiResult<PublicSigning> {
    let ip = meta.ip;
    let user_agent = meta.user_agent.clone();
    let (mut tx, ctx, s) = open_public(state, token, meta, true).await?;
    if s.status != "pending" {
        return Err(ApiError::conflict(
            "signature_closed",
            match s.status.as_str() {
                "cancelled" => "this request was replaced; use the newest link you were sent",
                _ => "this request has already been answered",
            },
        ));
    }
    load(&mut tx, s.document_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let v = signed_version(&mut tx, &s).await?;
    let mut check = Check::default();
    let name = if d.sign {
        let name = check.required("signed_name", d.signed_name.as_deref().unwrap_or(""), 200);
        if !d.agree {
            check.issue("agree", "agree to sign electronically to sign");
        }
        name
    } else {
        d.signed_name
            .as_deref()
            .map(str::trim)
            .unwrap_or(&s.signer_name)
            .to_owned()
    };
    let reason = check.long("reason", d.reason.as_deref(), 2000);
    check.finish()?;
    let agent = user_agent
        .as_deref()
        .map(|u| u.chars().take(500).collect::<String>());
    core_db::documents::decide_signature(
        &mut tx,
        s.id,
        core_db::documents::Decision {
            signed: d.sign,
            name: &name,
            ip,
            user_agent: agent.as_deref(),
            pdf_sha256: if d.sign { v.sha256.as_deref() } else { None },
            decline_reason: if d.sign { None } else { reason.as_deref() },
        },
    )
    .await?;
    let topic = if d.sign {
        "document.signature_signed"
    } else {
        "document.signature_declined"
    };
    record(
        &mut tx,
        &ctx,
        Change {
            topic,
            resource: ENTITY,
            object_id: Some(s.document_id),
            before: None,
            after: None,
            event_extra: Some(json!({ "signature_id": s.id, "signer": { "name": s.signer_name, "email": s.signer_email } })),
        },
    )
    .await?;
    if !d.sign {
        core_db::documents::cancel_pending(&mut tx, s.document_id).await?;
        core_db::documents::set_signature_status(&mut tx, s.document_id, "declined", None).await?;
    }
    // The signature is kept even if sealing fails: the seal is a job,
    // tried at once and retried by the worker.
    let complete = d.sign
        && core_db::documents::signatures(&mut tx, s.document_id)
            .await?
            .iter()
            .filter(|x| x.round_id == s.round_id && x.status != "cancelled")
            .all(|x| x.status == "signed");
    if complete {
        core_db::jobs::enqueue(
            &mut tx,
            NewJob::new(
                Some(ctx.workspace_id),
                JOB_SEAL,
                &json!({ "document_id": s.document_id, "round_id": s.round_id }),
            )
            .dedupe(&s.round_id.to_string()),
        )
        .await?;
    }
    let s = core_db::documents::signature(&mut tx, s.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut view = public_view(&mut tx, &ctx, &s).await?;
    tx.commit().await?;
    if complete {
        match seal_round(state, ctx.workspace_id, s.document_id, s.round_id).await {
            Ok(()) => {
                let (mut tx, ctx, s) = open_public(state, token, Default::default(), false).await?;
                view = public_view(&mut tx, &ctx, &s).await?;
                tx.commit().await?;
            }
            Err(e) => tracing::warn!(error = %e, "sealing failed; the worker will retry"),
        }
    }
    Ok(view)
}

/// Seals a document once every signer in a round has signed; does nothing
/// if it is already sealed or the round was replaced (the `document.seal` job).
pub async fn seal_round(
    state: &AppState,
    workspace: WorkspaceId,
    document_id: Uuid,
    round_id: Uuid,
) -> ApiResult<()> {
    let ctx = Ctx::system(workspace);
    let mut tx = begin(state, &ctx).await?;
    if core_db::documents::lock(&mut tx, document_id)
        .await?
        .is_none()
    {
        return Ok(());
    }
    let doc = load(&mut tx, document_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if doc.signature_status != "awaiting" {
        return Ok(());
    }
    let all = core_db::documents::signatures(&mut tx, document_id).await?;
    let round: Vec<&Signature> = all
        .iter()
        .filter(|x| x.round_id == round_id && x.status != "cancelled")
        .collect();
    let Some(first) = round.first() else {
        return Ok(());
    };
    if !round.iter().all(|x| x.status == "signed") {
        return Ok(());
    }
    let v = signed_version(&mut tx, first).await?;
    seal(state, &mut tx, &ctx, &doc, &v, &round).await?;
    tx.commit().await?;
    Ok(())
}

/// Everyone signed: the signed copy is the PDF they signed with the signing
/// certificate appended, kept as a new version.
async fn seal(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    doc: &DocumentView,
    v: &Version,
    round: &[&Signature],
) -> ApiResult<()> {
    let file = core_db::files::get(tx, v.file_id.ok_or(ApiError::NotFound)?)
        .await?
        .ok_or(ApiError::NotFound)?;
    let original = files::bytes(state, tx, &file).await?;
    let sha = files::hex(&Sha256::digest(&original));
    let issuer = super::finance::issuer_json(tx, ctx, None).await?;
    let branding = branding(state, tx, ctx, &issuer).await?;
    let records: Vec<SignatureRecord> = round
        .iter()
        .map(|s| SignatureRecord {
            name: s.signer_name.clone(),
            email: s.signer_email.clone(),
            signed_name: s.signed_name.clone().unwrap_or_default(),
            signed_at: s.decided_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
            ip: s.signed_ip.map(|ip| ip.to_string()),
            user_agent: s.signed_user_agent.clone(),
        })
        .collect();
    let reference = format!("{} v{}", doc.id, v.number);
    let html = engine::certificate(&doc.title, &reference, &sha, &records, &branding);
    let cert = print(state, html, "Signing certificate".into()).await?;
    let signed =
        tokio::task::spawn_blocking(move || core_pdf::write::append_pages(&original, &cert))
            .await
            .map_err(ApiError::internal)?
            .map_err(|e| ApiError::internal(e.to_string()))?;
    let name = file.filename.trim_end_matches(".pdf").to_owned() + "-signed.pdf";
    let stored = files::store(state, tx, ctx, &name, PDF, &signed, true).await?;
    let number = core_db::documents::add_version(
        tx,
        doc.id,
        core_db::documents::NewVersion {
            purpose: "signed",
            status: "ready",
            file_id: Some(stored.id),
            html: None,
        },
        None,
    )
    .await?;
    core_db::documents::set_signed_round(tx, doc.id, round[0].round_id).await?;
    let after = load(tx, doc.id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "document.signed",
            resource: ENTITY,
            object_id: Some(doc.id),
            before: Some(to_json(doc)),
            after: Some(to_json(&after)),
            event_extra: Some(json!({ "version": number, "signed_sha256": sha })),
        },
    )
    .await?;
    Ok(())
}

// Booking pack -----------------------------------------------------------------------------

/// Every current document of a booking and its invoices, zipped.
pub async fn booking_pack(
    state: &AppState,
    ctx: &Ctx,
    booking_id: Uuid,
) -> ApiResult<files::Download> {
    ctx.authorize(Permission::DocumentRead)?;
    let mut tx = begin(state, ctx).await?;
    let booking = super::bookings::load(&mut tx, booking_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut docs = core_db::documents::list(
        &mut tx,
        core_db::documents::Filter {
            related_id: Some(booking_id),
            kinds: &[],
            q: None,
            expires_before: None,
            offset: 0,
            limit: 200,
        },
    )
    .await?
    .0;
    for inv in core_db::invoices::for_booking(&mut tx, booking_id).await? {
        if let Some(d) = inv.document_id
            && let Some(doc) = core_db::documents::get(&mut tx, d).await?
        {
            docs.push(doc);
        }
    }
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let mut seen = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for d in docs {
            let Some(file_id) = d.file_id else { continue };
            if !seen.insert(file_id) {
                continue;
            }
            let Some(file) = core_db::files::get(&mut tx, file_id).await? else {
                continue;
            };
            let bytes = files::bytes(state, &mut tx, &file).await?;
            let mut name = format!("{}-{}", d.kind, file.filename);
            let mut n = 2;
            while !names.insert(name.clone()) {
                name = format!("{}-{n}-{}", d.kind, file.filename);
                n += 1;
            }
            zip.start_file(name, options).map_err(ApiError::internal)?;
            zip.write_all(&bytes).map_err(ApiError::internal)?;
        }
        zip.finish().map_err(ApiError::internal)?;
    }
    tx.commit().await?;
    Ok(files::Download {
        filename: format!(
            "{}.zip",
            pdf_filename(&booking.title, 1).trim_end_matches(".pdf")
        ),
        content_type: "application/zip".into(),
        bytes: buf.into_inner(),
    })
}

/// The document a rule or the system makes for a record from a template;
/// used by the rules' `generate_document` action.
pub(crate) async fn generate_in(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    template_key: &str,
    entity: &str,
    id: Uuid,
) -> ApiResult<(Uuid, i32)> {
    let today = super::finance::today(tx, ctx).await?;
    let doc_id = core_domain::ids::new_id();
    let reference = format!("D-{}", doc_id.simple().to_string()[..8].to_uppercase());
    let record_json = record_context(tx, state, entity, id).await?;
    let (title, html) = render_html(
        state,
        tx,
        ctx,
        template_key,
        None,
        &reference,
        today,
        record_json,
    )
    .await?;
    insert_generated(
        tx,
        ctx,
        NewGenerated {
            id: doc_id,
            kind: template_kind(template_key),
            title: &title,
            related_entity: Some(entity),
            related_id: Some(id),
            template_key: Some(template_key),
            document_date: Some(today),
            html: &html,
        },
    )
    .await?;
    Ok((doc_id, 1))
}

/// Document templates: the built-in ones, with the workspace's own wording
/// where it has saved a template with the same key, and its other ones.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DocumentTemplate {
    pub key: String,
    pub name: String,
    /// What it makes.
    pub kind: String,
    /// The record it is made from.
    pub entity: String,
    /// The workspace's saved template, if it has changed or added one.
    pub template_id: Option<Uuid>,
    pub built_in: bool,
}

pub async fn templates(state: &AppState, ctx: &Ctx) -> ApiResult<Vec<DocumentTemplate>> {
    ctx.authorize(Permission::DocumentRead)?;
    let mut tx = begin(state, ctx).await?;
    let saved = core_db::templates::list(&mut tx, Some(Kind::Document.as_str())).await?;
    tx.commit().await?;
    let mut out: Vec<DocumentTemplate> = engine::BUILT_IN
        .iter()
        .map(|b| DocumentTemplate {
            key: b.key.to_owned(),
            name: b.name.to_owned(),
            kind: b.kind.to_owned(),
            entity: b.entity.to_owned(),
            template_id: saved.iter().find(|t| t.key == b.key).map(|t| t.id),
            built_in: true,
        })
        .collect();
    for t in saved.iter().filter(|t| engine::built_in(&t.key).is_none()) {
        out.push(DocumentTemplate {
            key: t.key.clone(),
            name: t.name.clone(),
            kind: "misc".into(),
            entity: super::bookings::ENTITY.into(),
            template_id: Some(t.id),
            built_in: false,
        });
    }
    Ok(out)
}

/// A built-in template's source, to start a workspace's own from.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TemplateSource {
    pub key: String,
    pub name: String,
    pub subject: String,
    pub body: String,
}

pub fn built_in_source(key: &str) -> ApiResult<TemplateSource> {
    let b = engine::built_in(key).ok_or(ApiError::NotFound)?;
    Ok(TemplateSource {
        key: b.key.into(),
        name: b.name.into(),
        subject: b.subject.into(),
        body: b.body.into(),
    })
}
