//! Bringing in past PDFs (contracts, invoices, riders) in bulk, without AI.
//! Each upload is read for its text (the `file.read` job), its fields are
//! suggested by fixed patterns and the workspace's own issuer templates,
//! and the records it may belong to are matched by name, email and
//! invoice number. A person checks the suggestions and confirms: that
//! makes a document on the record, and for an invoice, a past invoice kept
//! as it was. Nothing is saved from a suggestion until then.

use chrono::{DateTime, NaiveDate, Utc};
use core_db::Tx;
use core_db::pdf_imports::{PdfImport, PdfTemplate};
use core_domain::{Ctx, Permission};
use core_engines::import::DateOrder;
use core_pdf::suggest::{self, Options, Suggestions};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const ENTITY: &str = "pdf_import";
pub const MAX_BATCH: usize = 200;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PdfImportView {
    pub id: Uuid,
    pub file_id: Uuid,
    pub filename: String,
    pub size_bytes: i64,
    pub page_count: Option<i32>,
    /// Pages look scanned: only OCR can read them.
    pub needs_ocr: bool,
    /// reading, ready, confirmed, discarded or failed.
    pub status: String,
    /// What reading found: `kinds` (likely kinds of document, best first),
    /// `fields` (suggestions with the line each came from) and `templates`
    /// (the workspace's issuer templates that applied).
    #[schema(value_type = Object)]
    pub found: Value,
    /// Records it may belong to, best first: [{entity, id, name, reason}].
    #[schema(value_type = Vec<Object>)]
    pub matches: Value,
    pub error: Option<String>,
    pub document_id: Option<Uuid>,
    pub invoice_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

impl From<PdfImport> for PdfImportView {
    fn from(p: PdfImport) -> Self {
        Self {
            id: p.id,
            file_id: p.file_id,
            filename: p.filename,
            size_bytes: p.size_bytes,
            page_count: p.page_count,
            needs_ocr: p.needs_ocr,
            status: p.status,
            found: p.found,
            matches: p.matches,
            error: p.error,
            document_id: p.document_id,
            invoice_id: p.invoice_id,
            version: p.version,
            created_at: p.created_at,
        }
    }
}

async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<PdfImportView>> {
    Ok(core_db::pdf_imports::get(tx, id).await?.map(Into::into))
}

pub async fn list(
    state: &AppState,
    ctx: &Ctx,
    status: Option<&str>,
) -> ApiResult<Vec<PdfImportView>> {
    ctx.authorize(Permission::DocumentRead)?;
    let statuses: Vec<String> = status
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::pdf_imports::list(&mut tx, &statuses, 500).await?;
    tx.commit().await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<PdfImportView> {
    ctx.authorize(Permission::DocumentRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewPdfImports {
    /// Uploaded PDFs (`POST /files`), up to 200 at a time.
    pub file_ids: Vec<Uuid>,
}

/// Starts reading a batch of uploaded PDFs.
pub async fn create(
    state: &AppState,
    ctx: &Ctx,
    req: NewPdfImports,
) -> ApiResult<Vec<PdfImportView>> {
    ctx.authorize(Permission::DocumentWrite)?;
    if req.file_ids.is_empty() || req.file_ids.len() > MAX_BATCH {
        return Err(ApiError::invalid(
            "file_ids",
            format!("give 1 to {MAX_BATCH} files"),
        ));
    }
    let mut tx = begin(state, ctx).await?;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, file_id) in req.file_ids.iter().enumerate() {
        if !seen.insert(*file_id) {
            continue;
        }
        // Attaching locks the file first, so a read finishing meanwhile is
        // either seen below or finds this import when it commits.
        core_db::files::attach(&mut tx, *file_id).await?;
        let file = core_db::files::get(&mut tx, *file_id)
            .await?
            .ok_or_else(|| ApiError::invalid(format!("file_ids[{i}]"), "there is no such file"))?;
        if file.content_type != super::files::PDF {
            return Err(ApiError::invalid(
                format!("file_ids[{i}]"),
                format!("{} is not a PDF", file.filename),
            ));
        }
        let id = core_domain::ids::new_id();
        core_db::pdf_imports::insert(&mut tx, id, file.id, ctx.user_id()).await?;
        // Already read (a file used twice): match it now.
        if file.read_state != "pending" {
            let text = core_db::files::text(&mut tx, file.id).await?;
            after_text(
                &mut tx,
                ctx,
                id,
                text.as_deref(),
                file.read_error.as_deref(),
            )
            .await?;
        }
        record(
            &mut tx,
            ctx,
            Change {
                topic: "pdf_import.created",
                resource: ENTITY,
                object_id: Some(id),
                before: None,
                after: Some(json!({ "file_id": file.id, "filename": file.filename })),
                event_extra: None,
            },
        )
        .await?;
        out.push(id);
    }
    let mut views = Vec::new();
    for id in out {
        views.push(load(&mut tx, id).await?.ok_or(ApiError::NotFound)?);
    }
    tx.commit().await?;
    Ok(views)
}

/// After `file.read`: suggests fields and finds matching records.
pub(crate) async fn after_read(
    tx: &mut Tx,
    ctx: &Ctx,
    id: Uuid,
    extracted: Option<&core_pdf::Extracted>,
) -> ApiResult<()> {
    let text = extracted.map(core_pdf::Extracted::text);
    let error = match extracted {
        Some(_) => None,
        None => Some("the PDF could not be read"),
    };
    after_text(tx, ctx, id, text.as_deref(), error).await
}

async fn options(tx: &mut Tx, ctx: &Ctx) -> ApiResult<Options> {
    let w = core_db::workspaces::get(tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let us_style =
        matches!(w.country.trim(), "US" | "PH" | "FM" | "PW") || w.locale.ends_with("-US");
    Ok(Options {
        date_order: if us_style {
            DateOrder::Mdy
        } else {
            DateOrder::Dmy
        },
        default_currency: w.default_currency.trim().to_owned(),
    })
}

/// The suggestions for a text, with the workspace's issuer templates.
pub(crate) async fn suggestions(
    tx: &mut Tx,
    ctx: &Ctx,
    text: &str,
) -> ApiResult<(Suggestions, Vec<String>)> {
    let pages: Vec<&str> = text.split('\x0c').collect();
    let opts = options(tx, ctx).await?;
    let mut found = suggest::suggest(&pages, &opts);
    let templates: Vec<core_pdf::templates::Template> = core_db::pdf_imports::templates(tx)
        .await?
        .into_iter()
        .filter_map(|t| serde_json::from_value(t.definition).ok())
        .collect();
    let compiled: Vec<_> = templates
        .iter()
        .filter_map(|t| core_pdf::templates::compile(t).ok())
        .collect();
    let applied = core_pdf::templates::apply(&compiled, &pages, &opts, &mut found);
    Ok((found, applied))
}

fn text_of(v: &suggest::Value) -> Option<&str> {
    match v {
        suggest::Value::Text(t) => Some(t),
        _ => None,
    }
}

/// Records a PDF may belong to, best first.
async fn matches(tx: &mut Tx, found: &Suggestions) -> ApiResult<Vec<Value>> {
    let mut out: Vec<(f32, Value)> = Vec::new();
    fn push(out: &mut Vec<(f32, Value)>, score: f32, v: Value) {
        let id = v.get("id").cloned();
        if !out.iter().any(|(_, o)| o.get("id") == id.as_ref()) {
            out.push((score, v));
        }
    }
    // The same invoice number already recorded.
    for s in found.all("invoice_number") {
        if let Some(n) = text_of(&s.value) {
            let hits = sqlx::query!(
                r#"SELECT id, reference AS "reference!" FROM invoices
                   WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL AND lower(reference) = lower($1)
                   LIMIT 3"#,
                n
            )
            .fetch_all(&mut **tx)
            .await?;
            for h in hits {
                push(
                    &mut out,
                    1.0,
                    json!({ "entity": "invoice", "id": h.id, "name": h.reference,
                                   "reason": format!("invoice number {n} is already recorded") }),
                );
            }
        }
    }
    for field in ["party", "issuer"] {
        for s in found.all(field).take(3) {
            let Some(name) = text_of(&s.value) else {
                continue;
            };
            let lower = name.to_lowercase();
            for (c, _) in core_db::organisations::similar(tx, &lower, None, 0.4, None, 3).await? {
                push(
                    &mut out,
                    c.similarity,
                    json!({ "entity": "organisation", "id": c.id, "name": c.name,
                                            "reason": format!("“{name}” appears in the document") }),
                );
            }
            for c in core_db::talent::similar(tx, &lower, 0.4, None, 3).await? {
                push(
                    &mut out,
                    c.similarity,
                    json!({ "entity": "talent", "id": c.id, "name": c.name,
                                            "reason": format!("“{name}” appears in the document") }),
                );
            }
        }
    }
    let emails: Vec<String> = found
        .all("email")
        .filter_map(|s| text_of(&s.value).map(str::to_lowercase))
        .take(10)
        .collect();
    if !emails.is_empty() {
        for c in core_db::contacts::similar(tx, "", &emails, &[], 1.1, None, 5).await? {
            push(
                &mut out,
                0.9,
                json!({ "entity": "contact", "id": c.id, "name": c.name,
                               "reason": format!("{} appears in the document", c.emails.join(", ")) }),
            );
        }
    }
    // A booking on a date the document names, with a matched buyer.
    let orgs: Vec<Uuid> = out
        .iter()
        .filter(|(_, v)| v.get("entity").and_then(Value::as_str) == Some("organisation"))
        .filter_map(|(_, v)| {
            v.get("id")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok())
        })
        .collect();
    let dates: Vec<NaiveDate> = ["start_date", "issue_date", "signed_date"]
        .iter()
        .flat_map(|f| found.all(f))
        .filter_map(|s| match &s.value {
            suggest::Value::Date(d) => Some(*d),
            _ => None,
        })
        .take(5)
        .collect();
    if !orgs.is_empty() && !dates.is_empty() {
        let hits = sqlx::query!(
            r#"SELECT id, title, starts_at FROM bookings
               WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL AND organisation_id = ANY($1)
                 AND (starts_at AT TIME ZONE timezone)::date = ANY($2)
               LIMIT 5"#,
            &orgs,
            &dates
        )
        .fetch_all(&mut **tx)
        .await?;
        for h in hits {
            push(
                &mut out,
                0.95,
                json!({ "entity": "booking", "id": h.id, "name": h.title,
                                "reason": format!("a booking for this buyer on {}", h.starts_at.date_naive()) }),
            );
        }
    }
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out.into_iter().take(10).map(|(_, v)| v).collect())
}

async fn after_text(
    tx: &mut Tx,
    ctx: &Ctx,
    id: Uuid,
    text: Option<&str>,
    error: Option<&str>,
) -> ApiResult<()> {
    let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
        let message = error.unwrap_or("the PDF has no text to read (it may be a scan)");
        core_db::pdf_imports::set_read(
            tx,
            id,
            if error.is_some() { "failed" } else { "ready" },
            &json!({ "kinds": [], "fields": [] }),
            &json!([]),
            Some(message),
        )
        .await?;
        return Ok(());
    };
    let (found, applied) = suggestions(tx, ctx, text).await?;
    let matched = matches(tx, &found).await?;
    let found_json = json!({
        "kinds": found.kinds,
        "fields": found.fields,
        "templates": applied,
    });
    core_db::pdf_imports::set_read(tx, id, "ready", &found_json, &json!(matched), None).await?;
    Ok(())
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ConfirmPdfImport {
    /// The kind of document (contract, invoice, rider…).
    pub kind: String,
    pub title: Option<String>,
    pub related_entity: Option<String>,
    pub related_id: Option<Uuid>,
    pub document_date: Option<NaiveDate>,
    pub expires_on: Option<NaiveDate>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// For a past invoice: also record it as one.
    pub invoice: Option<super::invoices::HistoricalInvoice>,
}

/// Keeps the PDF as a document on the chosen record, with what the person
/// confirmed, and for an invoice, records it as a past invoice.
pub async fn confirm(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    req: ConfirmPdfImport,
    if_match: Option<i64>,
) -> ApiResult<PdfImportView> {
    ctx.authorize(Permission::DocumentWrite)?;
    if req.invoice.is_some() {
        ctx.authorize(Permission::InvoiceWrite)?;
        if req.kind != "invoice" {
            return Err(ApiError::invalid(
                "invoice",
                "only an invoice can be kept as a past invoice",
            ));
        }
    }
    let mut tx = begin(state, ctx).await?;
    let (version, status) = core_db::pdf_imports::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    if !matches!(status.as_str(), "ready" | "failed") {
        return Err(ApiError::conflict(
            "import_not_ready",
            format!("a {status} import cannot be confirmed"),
        ));
    }
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let mut doc_entity = req.related_entity.clone();
    let mut doc_related = req.related_id;
    let doc_id = super::documents::insert_uploaded(
        state,
        &mut tx,
        ctx,
        super::documents::NewDocument {
            file_id: before.file_id,
            kind: req.kind.clone(),
            title: req.title.clone(),
            related_entity: doc_entity.clone(),
            related_id: doc_related,
            document_date: req.document_date,
            expires_on: req.expires_on,
            notes: None,
            tags: req.tags.clone(),
            custom: None,
        },
        "imported",
    )
    .await?;
    let invoice_id = match req.invoice {
        Some(mut h) => {
            h.document_id = Some(doc_id);
            let inv = super::invoices::insert_historical(&mut tx, ctx, h).await?;
            // The PDF belongs with its invoice when it had no other record.
            if doc_entity.is_none() {
                doc_entity = Some(super::invoices::ENTITY.into());
                doc_related = Some(inv);
                let d = core_db::documents::get(&mut tx, doc_id)
                    .await?
                    .ok_or(ApiError::NotFound)?;
                core_db::documents::update(
                    &mut tx,
                    doc_id,
                    core_db::documents::Edit {
                        kind: &d.kind,
                        title: &d.title,
                        related_entity: doc_entity.as_deref(),
                        related_id: doc_related,
                        document_date: d.document_date,
                        expires_on: d.expires_on,
                        notes: d.notes.as_deref(),
                        tags: &d.tags,
                        custom: &d.custom,
                    },
                    ctx.user_id(),
                )
                .await?;
            }
            Some(inv)
        }
        None => None,
    };
    core_db::pdf_imports::confirm(&mut tx, id, doc_id, invoice_id, ctx.user_id()).await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "pdf_import.confirmed",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(json!({ "document_id": doc_id, "invoice_id": invoice_id })),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

pub async fn discard(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<PdfImportView> {
    ctx.authorize(Permission::DocumentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let (_, status) = core_db::pdf_imports::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if status == "confirmed" {
        return Err(ApiError::conflict(
            "import_confirmed",
            "delete the document it made instead",
        ));
    }
    core_db::pdf_imports::discard(&mut tx, id, ctx.user_id()).await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(after)
}

/// Re-reads a ready import's text with the current templates (after a
/// template was added).
pub async fn rematch(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<PdfImportView> {
    ctx.authorize(Permission::DocumentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let (_, status) = core_db::pdf_imports::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if status != "ready" {
        return Err(ApiError::conflict(
            "import_not_ready",
            "only a ready import can be read again",
        ));
    }
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let text = core_db::files::text(&mut tx, before.file_id).await?;
    after_text(&mut tx, ctx, id, text.as_deref(), None).await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(after)
}

// Issuer templates ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PdfTemplateView {
    pub id: Uuid,
    pub name: String,
    pub definition: core_pdf::templates::Template,
    pub version: i64,
}

fn template_view(t: PdfTemplate) -> Option<PdfTemplateView> {
    Some(PdfTemplateView {
        id: t.id,
        name: t.name,
        definition: serde_json::from_value(t.definition).ok()?,
        version: t.version,
    })
}

pub async fn templates(state: &AppState, ctx: &Ctx) -> ApiResult<Vec<PdfTemplateView>> {
    ctx.authorize(Permission::DocumentRead)?;
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::pdf_imports::templates(&mut tx).await?;
    tx.commit().await?;
    Ok(rows.into_iter().filter_map(template_view).collect())
}

/// Adds field patterns for an issuer whose PDFs arrive often.
pub async fn create_template(
    state: &AppState,
    ctx: &Ctx,
    t: core_pdf::templates::Template,
) -> ApiResult<PdfTemplateView> {
    ctx.authorize(Permission::FinanceConfig)?;
    if let Err(issues) = core_pdf::templates::compile(&t) {
        return Err(ApiError::Invalid(
            issues
                .into_iter()
                .map(|i| core_engines::Issue::new(format!("definition.{}", i.field), i.message))
                .collect(),
        ));
    }
    let name = super::required_text("name", &t.name, 200)?;
    let mut tx = begin(state, ctx).await?;
    let id = core_domain::ids::new_id();
    let definition = serde_json::to_value(&t).map_err(ApiError::internal)?;
    core_db::pdf_imports::insert_template(&mut tx, id, &name, &definition, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "pdf_template.created",
            resource: "pdf_template",
            object_id: Some(id),
            before: None,
            after: Some(definition.clone()),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(PdfTemplateView {
        id,
        name,
        definition: t,
        version: 1,
    })
}

pub async fn archive_template(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::FinanceConfig)?;
    let mut tx = begin(state, ctx).await?;
    if !core_db::pdf_imports::archive_template(&mut tx, id, ctx.user_id()).await? {
        return Err(ApiError::NotFound);
    }
    record(
        &mut tx,
        ctx,
        Change {
            topic: "pdf_template.archived",
            resource: "pdf_template",
            object_id: Some(id),
            before: None,
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Search inside documents' text: the documents list does this with `q`;
/// this answers what a PDF says without making a document of it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Reading {
    pub suggestions: Suggestions,
    pub templates: Vec<String>,
}

/// Suggests fields for any stored PDF (a document's, say) on demand.
pub async fn read_file(state: &AppState, ctx: &Ctx, file_id: Uuid) -> ApiResult<Reading> {
    ctx.authorize(Permission::DocumentRead)?;
    let mut tx = begin(state, ctx).await?;
    let file = core_db::files::get(&mut tx, file_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if file.read_state == "pending" {
        return Err(ApiError::conflict(
            "file_not_read",
            "the file is still being read; try again shortly",
        ));
    }
    let text = core_db::files::text(&mut tx, file_id)
        .await?
        .unwrap_or_default();
    let (suggestions, templates) = suggestions(&mut tx, ctx, &text).await?;
    tx.commit().await?;
    Ok(Reading {
        suggestions,
        templates,
    })
}
