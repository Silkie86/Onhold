//! Invoices and credit notes. A draft can be changed freely; issuing it
//! takes the issuer's next number, freezes who it is from and to, fixes
//! the exchange rate into the base currency and prints the PDF. From then
//! on only its payment state moves: payments are allocated to it, credit
//! notes reduce it, and the worker marks it overdue once its due date has
//! passed. Mistakes are put right with a credit note; an invoice nothing
//! has been paid or credited on can be voided instead.
//!
//! Who invoices depends on the workspace's invoicing model: the agency
//! (`agency`), the talent in their own name (`talent`), or the agency for
//! the deposit and the talent for the balance (`split`).

use chrono::{DateTime, NaiveDate, Utc};
use core_db::Tx;
use core_db::invoices::Invoice;
use core_domain::crypto::{hash_token, new_token};
use core_domain::finance::{Rate, TaxedAmount, convert, format_number, percent_of, status_from_amounts, totals};
use core_domain::{Ctx, InvoiceStatus, Money, Permission, UserId, WorkspaceId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::crm::Check;
use super::quotes::LineInput as QuoteLineInput;
use super::{Change, begin, check_version, record, record_with_secret};
use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::routes::nullable;

pub const ENTITY: &str = "invoice";
pub const JOB_OVERDUE: &str = "invoice.overdue";
pub const MAX_LINES: usize = 100;
pub const PURPOSES: &[&str] = &["deposit", "balance", "full", "commission", "other"];

// Views ------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InvoiceLine {
    pub description: String,
    pub quantity: f64,
    pub unit_minor: i64,
    /// Net of tax.
    pub amount_minor: i64,
    pub tax_code_id: Option<Uuid>,
    pub tax_name: Option<String>,
    pub tax_rate_bps: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub talent_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TaxLine {
    pub tax_code_id: Option<Uuid>,
    pub name: String,
    pub rate_bps: i32,
    pub net_minor: i64,
    pub tax_minor: i64,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InvoiceView {
    pub id: Uuid,
    /// invoice or credit_note.
    pub kind: String,
    /// deposit, balance, full, commission or other.
    pub purpose: String,
    pub status: InvoiceStatus,
    pub number: Option<i64>,
    /// As printed: `INV-2026-0042`. None for a draft.
    pub reference: Option<String>,
    /// Brought in from before OnHold, kept as it was.
    pub historical: bool,
    /// workspace, or talent (invoicing in their own name).
    pub issuer_kind: String,
    pub issuer_talent_id: Option<Uuid>,
    /// organisation, talent or other.
    pub payer_kind: String,
    pub payer_organisation_id: Option<Uuid>,
    pub payer_talent_id: Option<Uuid>,
    pub payer_contact_id: Option<Uuid>,
    pub payer_name: String,
    /// As printed, fixed at issue.
    #[schema(value_type = Option<Object>)]
    pub issuer: Option<Value>,
    #[schema(value_type = Option<Object>)]
    pub payer: Option<Value>,
    pub booking_id: Option<Uuid>,
    pub booking_title: Option<String>,
    pub credits_invoice_id: Option<Uuid>,
    pub currency: String,
    pub line_items: Vec<InvoiceLine>,
    pub tax_lines: Vec<TaxLine>,
    pub subtotal: Money,
    pub tax: Money,
    pub total: Money,
    pub paid: Money,
    pub credited: Money,
    /// What is still owed: total less payments and credit notes.
    pub outstanding: Money,
    pub issue_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub base_currency: Option<String>,
    /// Units of the base currency per unit of the invoice's, fixed at issue.
    #[schema(value_type = Option<String>)]
    pub fx_rate: Option<Rate>,
    pub fx_date: Option<NaiveDate>,
    pub fx_source: Option<String>,
    pub base_total: Option<Money>,
    pub notes: Option<String>,
    pub terms: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
    pub sent_to: Option<String>,
    pub viewed_at: Option<DateTime<Utc>>,
    /// The PDF made on issue.
    pub document_id: Option<Uuid>,
    pub voided_at: Option<DateTime<Utc>>,
    pub void_reason: Option<String>,
    pub tags: Vec<String>,
    #[schema(value_type = Object)]
    pub custom: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

impl From<Invoice> for InvoiceView {
    fn from(i: Invoice) -> Self {
        let currency = i.currency.trim().to_owned();
        let money = |minor: i64| Money {
            minor,
            currency: currency.clone(),
        };
        let outstanding = if i.kind == "invoice" && i.status.is_issued() {
            (i.total_minor - i.paid_minor - i.credited_minor).max(0)
        } else {
            0
        };
        let base_currency = i.base_currency.map(|c| c.trim().to_owned());
        Self {
            id: i.id,
            kind: i.kind,
            purpose: i.purpose,
            status: i.status,
            number: i.number,
            reference: i.reference,
            historical: i.historical,
            issuer_kind: i.issuer_kind,
            issuer_talent_id: i.issuer_talent_id,
            payer_kind: i.payer_kind,
            payer_organisation_id: i.payer_organisation_id,
            payer_talent_id: i.payer_talent_id,
            payer_contact_id: i.payer_contact_id,
            payer_name: i.payer_display,
            issuer: i.issuer,
            payer: i.payer,
            booking_id: i.booking_id,
            booking_title: i.booking_title,
            credits_invoice_id: i.credits_invoice_id,
            line_items: serde_json::from_value(i.line_items).unwrap_or_default(),
            tax_lines: serde_json::from_value(i.tax_lines).unwrap_or_default(),
            subtotal: money(i.subtotal_minor),
            tax: money(i.tax_minor),
            total: money(i.total_minor),
            paid: money(i.paid_minor),
            credited: money(i.credited_minor),
            outstanding: money(outstanding),
            currency: currency.clone(),
            issue_date: i.issue_date,
            due_date: i.due_date,
            base_total: match (&base_currency, i.base_total_minor) {
                (Some(c), Some(m)) => Some(Money {
                    minor: m,
                    currency: c.clone(),
                }),
                _ => None,
            },
            base_currency,
            fx_rate: i.fx_rate.and_then(|r| r.parse().ok()),
            fx_date: i.fx_date,
            fx_source: i.fx_source,
            notes: i.notes,
            terms: i.terms,
            sent_at: i.sent_at,
            sent_to: i.sent_to,
            viewed_at: i.viewed_at,
            document_id: i.document_id,
            voided_at: i.voided_at,
            void_reason: i.void_reason,
            tags: i.tags,
            custom: i.custom,
            version: i.version,
            created_at: i.created_at,
            updated_at: i.updated_at,
            created_by: i.created_by,
            updated_by: i.updated_by,
        }
    }
}

pub fn to_json(v: &InvoiceView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

pub(crate) async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<InvoiceView>> {
    Ok(core_db::invoices::get(tx, id).await?.map(Into::into))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InvoicePage {
    pub items: Vec<InvoiceView>,
    pub total: i64,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct InvoiceFilter {
    /// Comma-separated statuses.
    pub status: Option<String>,
    /// invoice or credit_note.
    pub kind: Option<String>,
    pub booking_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub talent_id: Option<Uuid>,
    /// Words in the reference, payer or tags.
    pub q: Option<String>,
    pub issued_from: Option<NaiveDate>,
    pub issued_to: Option<NaiveDate>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

fn parse_statuses(s: Option<&str>) -> ApiResult<Vec<InvoiceStatus>> {
    let mut out = Vec::new();
    for part in s.unwrap_or_default().split(',').map(str::trim).filter(|p| !p.is_empty()) {
        out.push(
            InvoiceStatus::parse(part)
                .ok_or_else(|| ApiError::invalid("status", format!("`{part}` is not a status")))?,
        );
    }
    Ok(out)
}

pub async fn list(state: &AppState, ctx: &Ctx, f: &InvoiceFilter) -> ApiResult<InvoicePage> {
    ctx.authorize(Permission::InvoiceRead)?;
    let statuses = parse_statuses(f.status.as_deref())?;
    let mut tx = begin(state, ctx).await?;
    let (rows, total) = core_db::invoices::list(
        &mut tx,
        core_db::invoices::Filter {
            statuses: &statuses,
            kind: f.kind.as_deref(),
            booking_id: f.booking_id,
            organisation_id: f.organisation_id,
            talent_id: f.talent_id,
            q: f.q.as_deref().map(str::trim).filter(|q| !q.is_empty()),
            issued_from: f.issued_from,
            issued_to: f.issued_to,
            offset: f.offset.unwrap_or(0).max(0),
            limit: f.limit.unwrap_or(50).clamp(1, 500),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(InvoicePage {
        items: rows.into_iter().map(Into::into).collect(),
        total,
    })
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<InvoiceView> {
    ctx.authorize(Permission::InvoiceRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

// Drafts -----------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct LineInput {
    pub description: String,
    /// Up to three decimal places. Default 1.
    pub quantity: Option<f64>,
    /// Net price of one, in minor units. Negative for a discount.
    pub unit_minor: i64,
    /// Default: the workspace's default tax code.
    pub tax_code_id: Option<Uuid>,
    pub talent_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct Payer {
    pub organisation_id: Option<Uuid>,
    pub talent_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    /// For someone with no record: their name as it should print.
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NewInvoice {
    /// Default `other`.
    pub purpose: Option<String>,
    pub booking_id: Option<Uuid>,
    /// Default: the booking's buyer.
    pub payer: Option<Payer>,
    /// A talent issuing in their own name; default the workspace.
    pub issuer_talent_id: Option<Uuid>,
    /// Default: the booking's fee currency, else the workspace's.
    pub currency: Option<String>,
    #[serde(default)]
    pub line_items: Vec<LineInput>,
    pub issue_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub notes: Option<String>,
    pub terms: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[schema(value_type = Option<Object>)]
    pub custom: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct InvoicePatch {
    pub purpose: Option<String>,
    pub payer: Option<Payer>,
    pub currency: Option<String>,
    pub line_items: Option<Vec<LineInput>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<NaiveDate>)]
    pub issue_date: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<NaiveDate>)]
    pub due_date: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub notes: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub terms: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
    #[schema(value_type = Option<Object>)]
    pub custom: Option<Value>,
}

/// Lines with their tax worked out, and the totals.
pub(crate) struct Priced {
    pub lines: Vec<InvoiceLine>,
    pub tax_lines: Vec<TaxLine>,
    pub subtotal: i64,
    pub tax: i64,
    pub total: i64,
}

pub(crate) async fn price(
    tx: &mut Tx,
    ctx: &Ctx,
    check: &mut Check,
    inputs: &[LineInput],
    allow_negative: bool,
) -> ApiResult<Priced> {
    super::finance::ensure_tax_codes(tx, ctx).await?;
    let codes = core_db::finance::tax_codes(tx, true).await?;
    let default = codes.iter().find(|c| c.is_default && c.archived_at.is_none());
    if inputs.len() > MAX_LINES {
        check.issue("line_items", format!("give at most {MAX_LINES} lines"));
    }
    let as_quote: Vec<QuoteLineInput> = inputs
        .iter()
        .map(|l| QuoteLineInput {
            description: l.description.clone(),
            quantity: l.quantity,
            unit_minor: l.unit_minor,
            talent_id: l.talent_id,
        })
        .collect();
    let mut sub = Check::default();
    let (lines, _) = super::quotes::check_lines(&mut sub, &as_quote);
    for issue in sub.issues {
        if allow_negative && issue.field == "line_items" && issue.message.contains("negative") {
            continue;
        }
        check.issue(issue.field, issue.message);
    }
    let mut out = Vec::new();
    let mut taxed = Vec::new();
    for (i, (l, input)) in lines.iter().zip(inputs).enumerate() {
        let code = match input.tax_code_id {
            Some(id) => match codes.iter().find(|c| c.id == id) {
                Some(c) if c.archived_at.is_none() => Some(c),
                Some(_) => {
                    check.issue(format!("line_items[{i}].tax_code_id"), "that tax code is archived");
                    None
                }
                None => {
                    check.issue(format!("line_items[{i}].tax_code_id"), "there is no such tax code");
                    None
                }
            },
            None => default,
        };
        let rate = code.map_or(0, |c| c.rate_bps);
        taxed.push(TaxedAmount {
            amount_minor: l.amount_minor,
            tax_code_id: code.map(|c| c.id),
            rate_bps: rate,
        });
        out.push(InvoiceLine {
            description: l.description.clone(),
            quantity: l.quantity,
            unit_minor: l.unit_minor,
            amount_minor: l.amount_minor,
            tax_code_id: code.map(|c| c.id),
            tax_name: code.map(|c| c.name.clone()),
            tax_rate_bps: rate,
            talent_id: l.talent_id,
        });
    }
    let Some(t) = totals(&taxed) else {
        check.issue("line_items", "the amounts are too large");
        return Ok(Priced { lines: out, tax_lines: vec![], subtotal: 0, tax: 0, total: 0 });
    };
    if t.total_minor < 0 {
        check.issue("line_items", "the total must not be negative");
    }
    let tax_lines = t
        .groups
        .iter()
        .filter(|g| g.tax_code_id.is_some())
        .map(|g| {
            let code = codes.iter().find(|c| Some(c.id) == g.tax_code_id);
            TaxLine {
                tax_code_id: g.tax_code_id,
                name: code.map_or_else(|| "Tax".into(), |c| c.name.clone()),
                rate_bps: g.rate_bps,
                net_minor: g.net_minor,
                tax_minor: g.tax_minor,
                note: code.and_then(|c| c.note.clone()),
            }
        })
        .collect();
    Ok(Priced {
        lines: out,
        tax_lines,
        subtotal: t.subtotal_minor,
        tax: t.tax_minor,
        total: t.total_minor,
    })
}

struct PayerChoice {
    kind: &'static str,
    organisation_id: Option<Uuid>,
    talent_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    name: Option<String>,
}

async fn check_payer(tx: &mut Tx, check: &mut Check, p: &Payer) -> ApiResult<PayerChoice> {
    let name = check.optional("payer.name", p.name.as_deref(), 200);
    if let Some(o) = p.organisation_id
        && core_db::organisations::get(tx, o).await?.is_none()
    {
        check.issue("payer.organisation_id", "there is no such organisation");
    }
    if let Some(t) = p.talent_id
        && core_db::talent::get(tx, t).await?.is_none()
    {
        check.issue("payer.talent_id", "there is no such talent");
    }
    if let Some(c) = p.contact_id
        && core_db::contacts::get(tx, c).await?.is_none()
    {
        check.issue("payer.contact_id", "there is no such contact");
    }
    let kind = match (p.organisation_id, p.talent_id) {
        (Some(_), None) => "organisation",
        (None, Some(_)) => "talent",
        (None, None) if name.is_some() => "other",
        (Some(_), Some(_)) => {
            check.issue("payer", "give an organisation or a talent, not both");
            "organisation"
        }
        (None, None) => {
            check.issue("payer", "say who the invoice is to");
            "other"
        }
    };
    Ok(PayerChoice {
        kind,
        organisation_id: p.organisation_id,
        talent_id: p.talent_id,
        contact_id: p.contact_id,
        name,
    })
}

/// The buyer of a booking as a payer.
async fn booking_payer(tx: &mut Tx, booking: &super::bookings::BookingView) -> ApiResult<Payer> {
    if booking.organisation_id.is_some() {
        return Ok(Payer {
            organisation_id: booking.organisation_id,
            contact_id: booking.contact_id,
            ..Payer::default()
        });
    }
    if let Some(c) = booking.contact_id
        && let Some(contact) = core_db::contacts::get(tx, c).await?
    {
        return Ok(Payer {
            contact_id: Some(c),
            name: Some(contact.name),
            ..Payer::default()
        });
    }
    Ok(Payer::default())
}

fn check_purpose(check: &mut Check, p: Option<&str>) -> String {
    let p = p.unwrap_or("other").trim();
    if !PURPOSES.contains(&p) {
        check.issue("purpose", format!("must be one of: {}", PURPOSES.join(", ")));
    }
    p.to_owned()
}

/// A new draft invoice.
pub async fn create(state: &AppState, ctx: &Ctx, new: NewInvoice) -> ApiResult<InvoiceView> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let id = insert_draft(&mut tx, ctx, new, None).await?;
    let view = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(view)
}

pub(crate) async fn insert_draft(
    tx: &mut Tx,
    ctx: &Ctx,
    new: NewInvoice,
    credits: Option<&InvoiceView>,
) -> ApiResult<Uuid> {
    let mut check = Check::default();
    let purpose = check_purpose(&mut check, new.purpose.as_deref());
    let booking = match new.booking_id {
        Some(b) => Some(
            super::bookings::load(tx, b)
                .await?
                .ok_or_else(|| ApiError::invalid("booking_id", "there is no such booking"))?,
        ),
        None => None,
    };
    let payer_in = match (new.payer.clone(), &booking) {
        (Some(p), _) => p,
        (None, Some(b)) => booking_payer(tx, b).await?,
        (None, None) => Payer::default(),
    };
    let payer = check_payer(tx, &mut check, &payer_in).await?;
    if let Some(t) = new.issuer_talent_id
        && core_db::talent::get(tx, t).await?.is_none()
    {
        check.issue("issuer_talent_id", "there is no such talent");
    }
    let currency = match check.currency("currency", new.currency.as_deref()) {
        Some(c) => c,
        None => match booking.as_ref().and_then(|b| b.fee.as_ref()) {
            Some(f) => f.currency.clone(),
            None => super::finance::base_currency(tx, ctx).await?,
        },
    };
    let priced = price(tx, ctx, &mut check, &new.line_items, false).await?;
    let notes = check.long("notes", new.notes.as_deref(), 5000);
    let terms = check.long("terms", new.terms.as_deref(), 20_000);
    let tags = check.tags(&new.tags);
    let custom = new.custom.clone().unwrap_or_else(|| json!({}));
    if !custom.is_object() {
        check.issue("custom", "must be an object");
    }
    if let (Some(i), Some(d)) = (new.issue_date, new.due_date)
        && d < i
    {
        check.issue("due_date", "must not be before the issue date");
    }
    check.finish()?;
    let id = core_domain::ids::new_id();
    let lines = serde_json::to_value(&priced.lines).map_err(ApiError::internal)?;
    let tax_lines = serde_json::to_value(&priced.tax_lines).map_err(ApiError::internal)?;
    core_db::invoices::insert(
        tx,
        core_db::invoices::Creation {
            id,
            kind: if credits.is_some() { "credit_note" } else { "invoice" },
            issuer_talent_id: new.issuer_talent_id,
            credits_invoice_id: credits.map(|c| c.id),
            historical: None,
        },
        core_db::invoices::Values {
            purpose: &purpose,
            payer_kind: payer.kind,
            payer_organisation_id: payer.organisation_id,
            payer_talent_id: payer.talent_id,
            payer_contact_id: payer.contact_id,
            payer_name: payer.name.as_deref(),
            booking_id: new.booking_id,
            currency: &currency,
            line_items: &lines,
            tax_lines: &tax_lines,
            subtotal_minor: priced.subtotal,
            tax_minor: priced.tax,
            total_minor: priced.total,
            issue_date: new.issue_date,
            due_date: new.due_date,
            notes: notes.as_deref(),
            terms: terms.as_deref(),
            tags: &tags,
            custom: &custom,
        },
        ctx.user_id(),
    )
    .await?;
    let view = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "invoice.created",
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

async fn locked(tx: &mut Tx, id: Uuid, if_match: Option<i64>) -> ApiResult<InvoiceView> {
    let version = core_db::invoices::lock(tx, id).await?.ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    load(tx, id).await?.ok_or(ApiError::NotFound)
}

fn line_inputs(lines: &[InvoiceLine]) -> Vec<LineInput> {
    lines
        .iter()
        .map(|l| LineInput {
            description: l.description.clone(),
            quantity: Some(l.quantity),
            unit_minor: l.unit_minor,
            tax_code_id: l.tax_code_id,
            talent_id: l.talent_id,
        })
        .collect()
}

/// Changes a draft; on an issued invoice, only its tags, custom fields and
/// internal notes.
pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    p: InvoicePatch,
    if_match: Option<i64>,
) -> ApiResult<InvoiceView> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = locked(&mut tx, id, if_match).await?;
    let mut check = Check::default();
    let tags = match &p.tags {
        Some(t) => check.tags(t),
        None => before.tags.clone(),
    };
    let custom = p.custom.clone().unwrap_or_else(|| before.custom.clone());
    if !custom.is_object() {
        check.issue("custom", "must be an object");
    }
    let notes = match &p.notes {
        Some(n) => check.long("notes", n.as_deref(), 5000),
        None => before.notes.clone(),
    };
    if before.status != InvoiceStatus::Draft {
        if p.purpose.is_some()
            || p.payer.is_some()
            || p.currency.is_some()
            || p.line_items.is_some()
            || p.issue_date.is_some()
            || p.due_date.is_some()
            || p.terms.is_some()
        {
            return Err(ApiError::conflict(
                "invoice_issued",
                "an issued invoice cannot change; credit it and issue a new one",
            ));
        }
        check.finish()?;
        core_db::invoices::update_labels(&mut tx, id, &tags, &custom, notes.as_deref(), ctx.user_id()).await?;
    } else {
        let purpose = match &p.purpose {
            Some(x) => check_purpose(&mut check, Some(x)),
            None => before.purpose.clone(),
        };
        let payer_in = p.payer.clone().unwrap_or(Payer {
            organisation_id: before.payer_organisation_id,
            talent_id: before.payer_talent_id,
            contact_id: before.payer_contact_id,
            name: (before.payer_kind == "other").then(|| before.payer_name.clone()),
        });
        let payer = check_payer(&mut tx, &mut check, &payer_in).await?;
        let currency = match &p.currency {
            Some(c) => check.currency("currency", Some(c)).unwrap_or(before.currency.clone()),
            None => before.currency.clone(),
        };
        let inputs = p.line_items.clone().unwrap_or_else(|| line_inputs(&before.line_items));
        let priced = price(&mut tx, ctx, &mut check, &inputs, before.kind == "credit_note").await?;
        let terms = match &p.terms {
            Some(t) => check.long("terms", t.as_deref(), 20_000),
            None => before.terms.clone(),
        };
        let issue_date = p.issue_date.unwrap_or(before.issue_date);
        let due_date = p.due_date.unwrap_or(before.due_date);
        if let (Some(i), Some(d)) = (issue_date, due_date)
            && d < i
        {
            check.issue("due_date", "must not be before the issue date");
        }
        check.finish()?;
        let lines = serde_json::to_value(&priced.lines).map_err(ApiError::internal)?;
        let tax_lines = serde_json::to_value(&priced.tax_lines).map_err(ApiError::internal)?;
        core_db::invoices::update(
            &mut tx,
            id,
            core_db::invoices::Values {
                purpose: &purpose,
                payer_kind: payer.kind,
                payer_organisation_id: payer.organisation_id,
                payer_talent_id: payer.talent_id,
                payer_contact_id: payer.contact_id,
                payer_name: payer.name.as_deref(),
                booking_id: before.booking_id,
                currency: &currency,
                line_items: &lines,
                tax_lines: &tax_lines,
                subtotal_minor: priced.subtotal,
                tax_minor: priced.tax,
                total_minor: priced.total,
                issue_date,
                due_date,
                notes: notes.as_deref(),
                terms: terms.as_deref(),
                tags: &tags,
                custom: &custom,
            },
            ctx.user_id(),
        )
        .await?;
    }
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "invoice.updated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = locked(&mut tx, id, None).await?;
    if before.status != InvoiceStatus::Draft || before.historical {
        return Err(ApiError::conflict(
            "invoice_issued",
            "only drafts are deleted; void or credit an issued invoice",
        ));
    }
    core_db::invoices::delete_draft(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "invoice.deleted",
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

// From a booking -----------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct InvoiceBooking {
    /// deposit, balance or full.
    pub purpose: String,
    /// Default: what the workspace's invoicing model says.
    pub issuer_talent_id: Option<Uuid>,
    /// Issue it straight away (default false: a draft to check first).
    #[serde(default)]
    pub issue: bool,
}

/// What a booking is sold for: the accepted quote's lines, else its fee.
async fn booking_lines(tx: &mut Tx, booking: &super::bookings::BookingView) -> ApiResult<(String, Vec<LineInput>)> {
    let quotes = core_db::quotes::list(tx, Some(booking.id), &[core_domain::QuoteStatus::Accepted], 0, 1)
        .await?
        .0;
    if let Some(q) = quotes.into_iter().next() {
        let v: super::quotes::QuoteView = q.into();
        let lines = v
            .line_items
            .iter()
            .map(|l| LineInput {
                description: l.description.clone(),
                quantity: Some(l.quantity),
                unit_minor: l.unit_minor,
                tax_code_id: None,
                talent_id: l.talent_id,
            })
            .collect();
        return Ok((v.currency, lines));
    }
    let fee = booking.fee.as_ref().ok_or_else(|| {
        ApiError::conflict(
            "booking_unpriced",
            "the booking has no fee or accepted quote to invoice",
        )
    })?;
    Ok((
        fee.currency.clone(),
        vec![LineInput {
            description: format!("Fee: {}", booking.title),
            quantity: Some(1.0),
            unit_minor: fee.minor,
            tax_code_id: None,
            talent_id: booking.talent.first().map(|t| t.id),
        }],
    ))
}

/// A deposit, balance or full invoice for a booking, from its accepted
/// quote (or fee). The balance is the full amount less every deposit
/// invoiced so far.
pub async fn from_booking(
    state: &AppState,
    ctx: &Ctx,
    booking_id: Uuid,
    req: InvoiceBooking,
) -> ApiResult<InvoiceView> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let id = from_booking_in(state, &mut tx, ctx, booking_id, &req.purpose, req.issuer_talent_id).await?;
    let view = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    if req.issue {
        return issue(state, ctx, id, IssueInvoice::default(), None).await;
    }
    Ok(view)
}

pub(crate) async fn from_booking_in(
    _state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    booking_id: Uuid,
    purpose: &str,
    issuer_talent_id: Option<Uuid>,
) -> ApiResult<Uuid> {
    let booking = super::bookings::load(tx, booking_id).await?.ok_or(ApiError::NotFound)?;
    if !matches!(purpose, "deposit" | "balance" | "full") {
        return Err(ApiError::invalid("purpose", "must be deposit, balance or full"));
    }
    let settings = core_db::finance::settings(tx).await?;
    let (currency, full) = booking_lines(tx, &booking).await?;
    let existing = core_db::invoices::for_booking(tx, booking_id).await?;
    let live: Vec<InvoiceView> = existing
        .into_iter()
        .map(InvoiceView::from)
        .filter(|i| i.kind == "invoice" && i.status != InvoiceStatus::Void)
        .collect();
    let first_talent = booking.talent.first().map(|t| t.id);
    let issuer = issuer_talent_id.or(match (settings.invoicing_model.as_str(), purpose) {
        ("talent", _) | ("split", "balance") => first_talent,
        _ => None,
    });
    let lines = match purpose {
        "full" => full,
        "deposit" => {
            let pct = booking.deposit_percent.ok_or_else(|| {
                ApiError::conflict("no_deposit", "the booking has no deposit percentage")
            })?;
            let bps = (pct * 100.0).round() as i64;
            full.into_iter()
                .filter_map(|l| {
                    let amount = core_domain::bookings::line_amount(
                        (l.quantity.unwrap_or(1.0) * 1000.0).round() as i64,
                        l.unit_minor,
                    )?;
                    Some(LineInput {
                        description: format!("Deposit ({}): {}", core_engines::templates::percent(bps), l.description),
                        quantity: Some(1.0),
                        unit_minor: percent_of(amount, bps)?,
                        tax_code_id: l.tax_code_id,
                        talent_id: l.talent_id,
                    })
                })
                .collect()
        }
        _ => {
            let mut lines = full;
            for d in live.iter().filter(|i| i.purpose == "deposit") {
                for l in &d.line_items {
                    lines.push(LineInput {
                        description: format!(
                            "Less deposit {}",
                            d.reference.as_deref().unwrap_or("(draft)")
                        ),
                        quantity: Some(1.0),
                        unit_minor: -l.amount_minor,
                        tax_code_id: l.tax_code_id,
                        talent_id: l.talent_id,
                    });
                }
            }
            lines
        }
    };
    if live.iter().any(|i| i.purpose == purpose || (i.purpose == "full" && purpose != "full")) {
        return Err(ApiError::conflict(
            "already_invoiced",
            format!("this booking already has a {purpose} invoice (or a full one); void or credit it first"),
        ));
    }
    let payer = booking_payer(tx, &booking).await?;
    insert_draft(
        tx,
        ctx,
        NewInvoice {
            purpose: Some(purpose.into()),
            booking_id: Some(booking_id),
            payer: Some(payer),
            issuer_talent_id: issuer,
            currency: Some(currency),
            line_items: lines,
            ..NewInvoice::default()
        },
        None,
    )
    .await
}

// Issuing -----------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct IssueInvoice {
    /// Default today.
    pub issue_date: Option<NaiveDate>,
    /// Default: the payer's or workspace's payment terms after the issue date.
    pub due_date: Option<NaiveDate>,
    /// Units of the base currency per unit of the invoice's, when there is
    /// no reference rate (or to use another).
    #[schema(value_type = Option<String>)]
    pub fx_rate: Option<Rate>,
}

/// The payer as printed.
async fn payer_json(tx: &mut Tx, inv: &InvoiceView) -> ApiResult<Value> {
    let contact = match inv.payer_contact_id {
        Some(c) => core_db::contacts::get(tx, c).await?.map(|c| c.name),
        None => None,
    };
    Ok(match (inv.payer_organisation_id, inv.payer_talent_id) {
        (Some(o), _) => {
            let org = super::organisations::load(tx, o).await?.ok_or(ApiError::NotFound)?;
            json!({
                "kind": "organisation",
                "name": org.name,
                "address": org.addresses.first(),
                "tax_number": org.tax_id,
                "contact": contact,
            })
        }
        (None, Some(t)) => {
            let issuer = super::finance::issuer_json(tx, &Ctx::system(WorkspaceId(Uuid::nil())), Some(t)).await?;
            json!({
                "kind": "talent",
                "name": issuer.get("name"),
                "address": issuer.get("address"),
                "tax_number": issuer.get("tax_number"),
                "contact": contact,
            })
        }
        _ => json!({ "kind": "other", "name": inv.payer_name, "contact": contact }),
    })
}

/// Issues a draft: numbers it, fixes its parties and exchange rate, and
/// prints its PDF. A credit note reduces what its invoice owes.
pub async fn issue(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    req: IssueInvoice,
    if_match: Option<i64>,
) -> ApiResult<InvoiceView> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let (doc_id, number) = issue_in(state, &mut tx, ctx, id, req, if_match).await?;
    let view = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    super::documents::render_now(state, ctx, doc_id, number).await;
    Ok(view)
}

pub(crate) async fn issue_in(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    id: Uuid,
    req: IssueInvoice,
    if_match: Option<i64>,
) -> ApiResult<(Uuid, i32)> {
    let before = locked(tx, id, if_match).await?;
    if before.status != InvoiceStatus::Draft {
        return Err(ApiError::conflict("invoice_issued", "this invoice is already issued"));
    }
    if before.line_items.is_empty() {
        return Err(ApiError::invalid("line_items", "add at least one line before issuing"));
    }
    let today = super::finance::today(tx, ctx).await?;
    let issue_date = req.issue_date.or(before.issue_date).unwrap_or(today);
    let settings = core_db::finance::settings(tx).await?;
    let mut terms = settings.payment_terms_days;
    if let Some(o) = before.payer_organisation_id
        && let Some(org) = core_db::organisations::get(tx, o).await?
        && let Some(t) = org.payment_terms_days
    {
        terms = t;
    }
    let due_date = req
        .due_date
        .or(before.due_date)
        .unwrap_or(issue_date + chrono::Duration::days(i64::from(terms)));
    if due_date < issue_date {
        return Err(ApiError::invalid("due_date", "must not be before the issue date"));
    }
    // Credit notes: against an issued invoice, for no more than it still owes.
    let original = match before.credits_invoice_id {
        Some(c) => {
            core_db::invoices::lock(tx, c).await?;
            let o = load(tx, c).await?.ok_or(ApiError::NotFound)?;
            if !o.status.is_issued() {
                return Err(ApiError::conflict("invoice_not_issued", "the credited invoice is not issued"));
            }
            if o.currency != before.currency {
                return Err(ApiError::invalid("currency", "a credit note is in its invoice's currency"));
            }
            let room = o.total.minor - o.credited.minor;
            if before.total.minor > room {
                return Err(ApiError::invalid(
                    "line_items",
                    format!("the invoice has {} left to credit", room),
                ));
            }
            Some(o)
        }
        None => None,
    };
    // The exchange rate into the base currency, fixed now.
    let base = super::finance::base_currency(tx, ctx).await?;
    let (rate, fx_date, fx_source) = if before.currency == base {
        (Rate::ONE, issue_date, "same")
    } else if let Some(r) = req.fx_rate {
        (r, issue_date, "manual")
    } else if let Some(o) = &original
        && let Some(r) = o.fx_rate
    {
        // A credit note reverses at its invoice's rate.
        (r, o.fx_date.unwrap_or(issue_date), "booking")
    } else if let Some(r) = booking_rate(tx, before.booking_id).await? {
        (r, issue_date, "booking")
    } else {
        match super::finance::reference_rate(tx, &before.currency, &base, issue_date).await? {
            Some(q) => (q.rate, q.date, "ecb"),
            None => {
                return Err(ApiError::unprocessable(
                    "no_rate",
                    format!(
                        "there is no reference rate from {} to {base}; give fx_rate",
                        before.currency
                    ),
                ));
            }
        }
    };
    let base_total = convert(before.total.minor, &before.currency, &base, rate)
        .ok_or_else(|| ApiError::invalid("fx_rate", "the converted total is too large"))?;
    let kind = before.kind.as_str();
    let issuer_key = before.issuer_talent_id.map_or_else(|| "workspace".to_owned(), |t| t.to_string());
    let number = core_db::finance::next_number(tx, &issuer_key, kind).await?;
    let format = if kind == "credit_note" { &settings.credit_note_format } else { &settings.invoice_format };
    let reference = format_number(format, number, issue_date);
    let issuer = super::finance::issuer_json(tx, ctx, before.issuer_talent_id).await?;
    let payer = payer_json(tx, &before).await?;
    core_db::invoices::issue(
        tx,
        id,
        core_db::invoices::Issue {
            status: InvoiceStatus::Issued,
            number,
            reference: &reference,
            issue_date,
            due_date,
            issuer: &issuer,
            payer: &payer,
            base_currency: &base,
            fx_rate: &rate.to_string(),
            fx_date,
            fx_source,
            base_total_minor: base_total,
        },
        ctx.user_id(),
    )
    .await
    .map_err(|e| {
        if e.is_unique_violation(Some(core_db::invoices::NUMBER_TAKEN)) {
            ApiError::conflict("number_taken", "that number is already used; try again")
        } else {
            e.into()
        }
    })?;
    let after = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    let (doc_id, version) = make_pdf(state, tx, ctx, &after, original.as_ref()).await?;
    core_db::invoices::set_document(tx, id, doc_id).await?;
    let after = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "invoice.issued",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    if let Some(o) = original {
        recompute(tx, ctx, o.id).await?;
    }
    Ok((doc_id, version))
}

/// A rate fixed on the booking, if it has one.
async fn booking_rate(tx: &mut Tx, booking_id: Option<Uuid>) -> ApiResult<Option<Rate>> {
    let Some(b) = booking_id else { return Ok(None) };
    let r = sqlx::query_scalar!(
        r#"SELECT fx_rate::text FROM bookings WHERE workspace_id = app_workspace_id() AND id = $1"#,
        b
    )
    .fetch_optional(&mut **tx)
    .await?
    .flatten();
    Ok(r.and_then(|r| r.parse().ok()))
}

/// What the invoice template sees.
pub(crate) async fn template_context(tx: &mut Tx, inv: &InvoiceView, original: Option<&InvoiceView>) -> ApiResult<Value> {
    let settings = core_db::finance::settings(tx).await?;
    let bank = match inv.issuer_talent_id {
        Some(t) => super::finance::talent_bank(tx, t, &inv.currency).await?,
        None => {
            let accounts: Vec<super::finance::BankAccount> =
                serde_json::from_value(settings.bank_accounts.clone()).unwrap_or_default();
            accounts
                .into_iter()
                .find(|a| a.currency == inv.currency)
                .map(|a| json!({ "label": a.label, "details": a.details }))
        }
    };
    let booking_date = match inv.booking_id {
        Some(b) => super::bookings::load(tx, b).await?.map(|b| b.starts_at.date_naive()),
        None => None,
    };
    Ok(json!({
        "invoice": {
            "kind": inv.kind,
            "reference": inv.reference,
            "issue_date": inv.issue_date,
            "due_date": inv.due_date,
            "currency": inv.currency,
            "issuer": inv.issuer,
            "payer": inv.payer,
            "line_items": inv.line_items,
            "tax_lines": inv.tax_lines,
            "subtotal": inv.subtotal,
            "tax": inv.tax,
            "total": inv.total,
            "bank_account": bank,
            "payment_terms_days": inv.issue_date.zip(inv.due_date).map(|(i, d)| (d - i).num_days()),
            "late_fee_text": settings.late_fee_text,
            "terms": inv.terms,
            "footer": settings.invoice_footer,
            "notes": inv.notes,
            "credits_reference": original.and_then(|o| o.reference.clone()),
            "booking_title": inv.booking_title,
            "booking_date": booking_date,
        },
        "issuer": inv.issuer,
    }))
}

async fn make_pdf(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    inv: &InvoiceView,
    original: Option<&InvoiceView>,
) -> ApiResult<(Uuid, i32)> {
    let key = if inv.kind == "credit_note" {
        core_engines::documents::CREDIT_NOTE
    } else {
        core_engines::documents::INVOICE
    };
    let context = template_context(tx, inv, original).await?;
    let reference = inv.reference.clone().unwrap_or_default();
    let title = format!(
        "{} {reference}",
        if inv.kind == "credit_note" { "Credit note" } else { "Invoice" }
    );
    let (title, html) = super::documents::render_html(
        state,
        tx,
        ctx,
        key,
        Some(&title),
        &reference,
        inv.issue_date.unwrap_or_default(),
        context,
    )
    .await?;
    let doc_id = core_domain::ids::new_id();
    super::documents::insert_generated(
        tx,
        ctx,
        super::documents::NewGenerated {
            id: doc_id,
            kind: &inv.kind,
            title: &title,
            related_entity: Some(ENTITY),
            related_id: Some(inv.id),
            template_key: Some(key),
            document_date: inv.issue_date,
            html: &html,
        },
    )
    .await?;
    Ok((doc_id, 1))
}

/// Moves an issued invoice's payment state on from its allocations and
/// credit notes, emitting the event for a new status.
pub(crate) async fn recompute(tx: &mut Tx, ctx: &Ctx, id: Uuid) -> ApiResult<InvoiceView> {
    let before = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.kind != "invoice" || !before.status.is_issued() {
        return Ok(before);
    }
    let paid = core_db::payments::paid_towards(tx, id).await?;
    let credited = core_db::invoices::credited_total(tx, id).await?;
    let today = super::finance::today(tx, ctx).await?;
    let status = status_from_amounts(before.total.minor, paid, credited, before.due_date, today);
    if paid == before.paid.minor && credited == before.credited.minor && status == before.status {
        return Ok(before);
    }
    core_db::invoices::set_amounts(tx, id, paid, credited, status).await?;
    let after = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    let topic = if status == before.status { "invoice.updated" } else { status.topic() };
    record(
        tx,
        ctx,
        Change {
            topic,
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    if status == InvoiceStatus::Paid
        && before.status != InvoiceStatus::Paid
        && let Some(b) = after.booking_id
    {
        super::bookings::invoice_paid(tx, ctx, b, &after.purpose).await?;
    }
    Ok(after)
}

/// Marks an invoice overdue once its due date has passed (the worker's
/// `invoice.overdue`); `invoice.overdue` events start chasing rules.
pub async fn mark_overdue(state: &AppState, workspace: WorkspaceId, id: Uuid) -> ApiResult<()> {
    let ctx = Ctx::system(workspace);
    let mut tx = begin(state, &ctx).await?;
    if core_db::invoices::lock(&mut tx, id).await?.is_none() {
        return Ok(());
    }
    recompute(&mut tx, &ctx, id).await?;
    tx.commit().await?;
    Ok(())
}

// Credit notes and voiding -------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NewCreditNote {
    /// Default: every line of the invoice (a full credit).
    pub line_items: Option<Vec<LineInput>>,
    pub notes: Option<String>,
    /// Issue it straight away (default true).
    pub issue: Option<bool>,
}

/// A credit note against an issued invoice.
pub async fn credit(state: &AppState, ctx: &Ctx, id: Uuid, req: NewCreditNote) -> ApiResult<InvoiceView> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let original = locked(&mut tx, id, None).await?;
    if original.kind != "invoice" || !original.status.is_issued() {
        return Err(ApiError::conflict(
            "invoice_not_issued",
            "only an issued invoice can be credited",
        ));
    }
    let lines = req.line_items.unwrap_or_else(|| line_inputs(&original.line_items));
    let note_id = insert_draft(
        &mut tx,
        ctx,
        NewInvoice {
            purpose: Some(original.purpose.clone()),
            booking_id: original.booking_id,
            payer: Some(Payer {
                organisation_id: original.payer_organisation_id,
                talent_id: original.payer_talent_id,
                contact_id: original.payer_contact_id,
                name: (original.payer_kind == "other").then(|| original.payer_name.clone()),
            }),
            issuer_talent_id: original.issuer_talent_id,
            currency: Some(original.currency.clone()),
            line_items: lines,
            notes: req.notes,
            ..NewInvoice::default()
        },
        Some(&original),
    )
    .await?;
    let mut render = None;
    if req.issue.unwrap_or(true) {
        render = Some(issue_in(state, &mut tx, ctx, note_id, IssueInvoice::default(), None).await?);
    }
    let view = load(&mut tx, note_id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    if let Some((doc, n)) = render {
        super::documents::render_now(state, ctx, doc, n).await;
    }
    Ok(view)
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct VoidInvoice {
    pub reason: Option<String>,
}

/// Voids an issued invoice nothing has been paid or credited on (its
/// number stays used), or an issued credit note.
pub async fn void(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    req: VoidInvoice,
    if_match: Option<i64>,
) -> ApiResult<InvoiceView> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = locked(&mut tx, id, if_match).await?;
    if !before.status.is_issued() || before.historical && before.status == InvoiceStatus::Void {
        return Err(ApiError::conflict("invoice_not_issued", "only an issued invoice can be voided"));
    }
    if before.paid.minor > 0 || before.credited.minor > 0 {
        return Err(ApiError::conflict(
            "invoice_has_payments",
            "payments or credit notes are recorded against this invoice; credit it instead",
        ));
    }
    let reason = super::optional_text("reason", req.reason, 500)?;
    core_db::invoices::void(&mut tx, id, reason.as_deref(), ctx.user_id()).await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "invoice.voided",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    if let Some(o) = before.credits_invoice_id {
        core_db::invoices::lock(&mut tx, o).await?;
        recompute(&mut tx, ctx, o).await?;
    }
    tx.commit().await?;
    Ok(after)
}

// Sending ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct SendInvoice {
    /// Default: the payer contact's first email address.
    pub to: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SentInvoice {
    pub invoice: InvoiceView,
    /// The payer's link to view and download it. Shown once.
    pub link: String,
}

/// Emails an issued invoice with its PDF attached and a link to view it.
/// Each send makes a new link; earlier ones stop working.
pub async fn send(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    req: SendInvoice,
    if_match: Option<i64>,
) -> ApiResult<SentInvoice> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = locked(&mut tx, id, if_match).await?;
    if !before.status.is_issued() || before.historical {
        return Err(ApiError::conflict(
            "invoice_not_issued",
            "only an issued invoice made in OnHold can be sent",
        ));
    }
    let to = match req.to.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t.to_lowercase(),
        None => {
            let mut found = None;
            if let Some(c) = before.payer_contact_id
                && let Some(contact) = core_db::contacts::get(&mut tx, c).await?
            {
                found = contact.emails.first().cloned();
            }
            found.ok_or_else(|| ApiError::invalid("to", "the payer has no contact email; give an address"))?
        }
    };
    if !core_engines::fields::is_email(&to) || to.len() > 320 {
        return Err(ApiError::invalid("to", "is not an email address"));
    }
    let token = new_token();
    core_db::invoices::mark_sent(&mut tx, id, &hash_token(&token), &to, ctx.user_id()).await?;
    let link = state.config.app_url(&format!("/invoice/{token}"), &[]);
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let w = core_db::workspaces::get(&mut tx, ctx.workspace_id).await?.ok_or(ApiError::NotFound)?;
    record_with_secret(
        &mut tx,
        ctx,
        Change {
            topic: "invoice.sent",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: Some(json!({ "workspace_name": w.name })),
        },
        &state.config.keyring,
        &json!({ "url": link }),
    )
    .await?;
    tx.commit().await?;
    Ok(SentInvoice { invoice: after, link })
}

// The payer's side ------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicInvoice {
    pub workspace_name: String,
    #[schema(value_type = Object)]
    pub branding: Value,
    pub kind: String,
    pub reference: String,
    pub status: InvoiceStatus,
    #[schema(value_type = Object)]
    pub issuer: Value,
    #[schema(value_type = Object)]
    pub payer: Value,
    pub issue_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub line_items: Vec<InvoiceLine>,
    pub tax_lines: Vec<TaxLine>,
    pub subtotal: Money,
    pub tax: Money,
    pub total: Money,
    pub outstanding: Money,
    /// How to pay, from the issuer's bank details.
    #[schema(value_type = Option<Object>)]
    pub bank_account: Option<Value>,
}

async fn open_public(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
) -> ApiResult<(Tx, Ctx, Uuid)> {
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let (id, workspace) = core_db::invoices::find_public(&mut tx, &hash_token(token))
        .await?
        .ok_or(ApiError::NotFound)?;
    core_db::set_context(&mut tx, Some(workspace), None).await?;
    let mut ctx = Ctx::system(workspace);
    ctx.meta = meta;
    Ok((tx, ctx, id))
}

/// The payer opens their link; the first view is recorded.
pub async fn public_get(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
) -> ApiResult<PublicInvoice> {
    let (mut tx, ctx, id) = open_public(state, token, meta).await?;
    let inv = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if core_db::invoices::mark_viewed(&mut tx, id).await? {
        let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
        record(
            &mut tx,
            &ctx,
            Change {
                topic: "invoice.viewed",
                resource: ENTITY,
                object_id: Some(id),
                before: Some(to_json(&inv)),
                after: Some(to_json(&after)),
                event_extra: None,
            },
        )
        .await?;
    }
    let w = core_db::workspaces::get(&mut tx, ctx.workspace_id).await?.ok_or(ApiError::NotFound)?;
    let context = template_context(&mut tx, &inv, None).await?;
    tx.commit().await?;
    Ok(PublicInvoice {
        workspace_name: w.name,
        branding: w.branding,
        kind: inv.kind,
        reference: inv.reference.unwrap_or_default(),
        status: inv.status,
        issuer: inv.issuer.unwrap_or(Value::Null),
        payer: inv.payer.unwrap_or(Value::Null),
        issue_date: inv.issue_date,
        due_date: inv.due_date,
        line_items: inv.line_items,
        tax_lines: inv.tax_lines,
        subtotal: inv.subtotal,
        tax: inv.tax,
        total: inv.total,
        outstanding: inv.outstanding,
        bank_account: context.pointer("/invoice/bank_account").cloned().filter(|v| !v.is_null()),
    })
}

/// The invoice's PDF, for the payer.
pub async fn public_pdf(state: &AppState, token: &str) -> ApiResult<super::files::Download> {
    let (mut tx, _ctx, id) = open_public(state, token, Default::default()).await?;
    let pdf = pdf_in(state, &mut tx, id).await?;
    tx.commit().await?;
    Ok(pdf)
}

/// An issued invoice's PDF, printing it now if the worker has not yet.
pub(crate) async fn pdf_in(state: &AppState, tx: &mut Tx, id: Uuid) -> ApiResult<super::files::Download> {
    let inv = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    let doc_id = inv.document_id.ok_or(ApiError::NotFound)?;
    let doc = core_db::documents::get(tx, doc_id).await?.ok_or(ApiError::NotFound)?;
    let v = core_db::documents::version(tx, doc_id, doc.current_version)
        .await?
        .ok_or(ApiError::NotFound)?;
    if let Some(file_id) = v.file_id {
        let file = core_db::files::get(tx, file_id).await?.ok_or(ApiError::NotFound)?;
        let bytes = super::files::bytes(state, tx, &file).await?;
        return Ok(super::files::Download {
            filename: file.filename,
            content_type: file.content_type,
            bytes,
        });
    }
    // Not printed yet: print it for this answer (the job will store it).
    let bytes = super::documents::print(state, v.html.unwrap_or_default(), doc.title.clone()).await?;
    Ok(super::files::Download {
        filename: format!("{}.pdf", inv.reference.unwrap_or_else(|| "invoice".into())),
        content_type: super::files::PDF.into(),
        bytes,
    })
}

pub async fn pdf(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<super::files::Download> {
    ctx.authorize(Permission::InvoiceRead)?;
    let mut tx = begin(state, ctx).await?;
    let pdf = pdf_in(state, &mut tx, id).await?;
    tx.commit().await?;
    Ok(pdf)
}

// Past invoices ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct HistoricalInvoice {
    /// Its own number as printed.
    pub reference: String,
    /// Default invoice.
    pub kind: Option<String>,
    pub payer: Payer,
    pub booking_id: Option<Uuid>,
    pub issuer_talent_id: Option<Uuid>,
    pub currency: String,
    pub issue_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    /// Default: one line for the whole amount, untaxed.
    pub line_items: Option<Vec<LineInput>>,
    /// When there are no lines: the total, tax included.
    pub total_minor: Option<i64>,
    /// How much was paid (default: none; all of it when `paid` is true).
    pub paid_minor: Option<i64>,
    #[serde(default)]
    pub paid: bool,
    /// The PDF it came from, if any.
    pub document_id: Option<Uuid>,
    pub notes: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Records an invoice from before OnHold, as it was: its own number, no
/// new number taken, never sent. It counts in reports and statements.
pub async fn create_historical(state: &AppState, ctx: &Ctx, h: HistoricalInvoice) -> ApiResult<InvoiceView> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let id = insert_historical(&mut tx, ctx, h).await?;
    let view = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(view)
}

pub(crate) async fn insert_historical(tx: &mut Tx, ctx: &Ctx, h: HistoricalInvoice) -> ApiResult<Uuid> {
    let mut check = Check::default();
    let reference = check.required("reference", &h.reference, 60);
    let kind = h.kind.as_deref().unwrap_or("invoice");
    if !matches!(kind, "invoice" | "credit_note") {
        check.issue("kind", "must be invoice or credit_note");
    }
    let currency = check.currency("currency", Some(&h.currency)).unwrap_or_default();
    let payer = check_payer(tx, &mut check, &h.payer).await?;
    let priced = match &h.line_items {
        Some(lines) => price(tx, ctx, &mut check, lines, false).await?,
        None => {
            let total = h.total_minor.unwrap_or(0);
            if total <= 0 {
                check.issue("total_minor", "give the lines or a total above zero");
            }
            Priced {
                lines: vec![InvoiceLine {
                    description: format!("Invoice {reference}"),
                    quantity: 1.0,
                    unit_minor: total,
                    amount_minor: total,
                    tax_code_id: None,
                    tax_name: None,
                    tax_rate_bps: 0,
                    talent_id: None,
                }],
                tax_lines: vec![],
                subtotal: total,
                tax: 0,
                total,
            }
        }
    };
    let paid = if h.paid { priced.total } else { h.paid_minor.unwrap_or(0) };
    if paid < 0 || paid > priced.total {
        check.issue("paid_minor", "must be between 0 and the total");
    }
    let today = super::finance::today(tx, ctx).await?;
    let status = if kind == "credit_note" {
        InvoiceStatus::Issued
    } else {
        status_from_amounts(priced.total, paid, 0, h.due_date, today)
    };
    if let Some(d) = h.document_id
        && core_db::documents::get(tx, d).await?.is_none()
    {
        check.issue("document_id", "there is no such document");
    }
    let notes = check.long("notes", h.notes.as_deref(), 5000);
    let tags = check.tags(&h.tags);
    check.finish()?;
    let id = core_domain::ids::new_id();
    let lines = serde_json::to_value(&priced.lines).map_err(ApiError::internal)?;
    let tax_lines = serde_json::to_value(&priced.tax_lines).map_err(ApiError::internal)?;
    core_db::invoices::insert(
        tx,
        core_db::invoices::Creation {
            id,
            kind,
            issuer_talent_id: h.issuer_talent_id,
            credits_invoice_id: None,
            historical: Some(core_db::invoices::Historical {
                reference: &reference,
                paid_minor: paid,
                document_id: h.document_id,
            }),
        },
        core_db::invoices::Values {
            purpose: "other",
            payer_kind: payer.kind,
            payer_organisation_id: payer.organisation_id,
            payer_talent_id: payer.talent_id,
            payer_contact_id: payer.contact_id,
            payer_name: payer.name.as_deref(),
            booking_id: h.booking_id,
            currency: &currency,
            line_items: &lines,
            tax_lines: &tax_lines,
            subtotal_minor: priced.subtotal,
            tax_minor: priced.tax,
            total_minor: priced.total,
            issue_date: Some(h.issue_date),
            due_date: h.due_date,
            notes: notes.as_deref(),
            terms: None,
            tags: &tags,
            custom: &json!({}),
        },
        ctx.user_id(),
    )
    .await?;
    // The base-currency total at the day's reference rate when known, and
    // its status: both set while the row is still a draft, before the
    // freeze on issued invoices applies.
    let base = super::finance::base_currency(tx, ctx).await?;
    let q = super::finance::reference_rate(tx, &currency, &base, h.issue_date).await?;
    let base_total = q.as_ref().and_then(|q| convert(priced.total, &currency, &base, q.rate));
    sqlx::query!(
        r#"UPDATE invoices SET status = $2, base_currency = $3, fx_rate = $4::text::numeric, fx_date = $5,
                  fx_source = $6, base_total_minor = $7
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        status as InvoiceStatus,
        base_total.map(|_| base.clone()),
        q.as_ref().filter(|_| base_total.is_some()).map(|q| q.rate.to_string()),
        q.as_ref().filter(|_| base_total.is_some()).map(|q| q.date),
        q.as_ref().filter(|_| base_total.is_some()).map(|q| q.source.clone()),
        base_total
    )
    .execute(&mut **tx)
    .await?;
    let view = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "invoice.imported",
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
