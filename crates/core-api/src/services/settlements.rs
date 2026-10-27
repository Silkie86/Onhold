//! Settlements: what each talent on a booking is owed. The gross is their
//! share of the booking (the accepted quote's lines for them, or the fee
//! split evenly), less the agency's commission (the talent's rate) and any
//! expenses. Finalising prints a statement; under the talent invoicing
//! model it also drafts the agency's commission invoice to the talent.
//! Payouts are recorded against it as payments out.

use chrono::{DateTime, NaiveDate, Utc};
use core_db::Tx;
use core_domain::finance::settle;
use core_domain::{Ctx, Money, Permission};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::crm::Check;
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const ENTITY: &str = "settlement";

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Expense {
    pub description: String,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SettlementView {
    pub id: Uuid,
    pub booking_id: Uuid,
    pub booking_title: String,
    pub talent_id: Uuid,
    pub talent_name: String,
    /// draft or final.
    pub status: String,
    pub gross: Money,
    pub commission_bps: i32,
    pub commission: Money,
    pub expenses: Vec<Expense>,
    pub expenses_total: Money,
    /// What the talent is owed.
    pub net: Money,
    pub paid_out: Money,
    pub notes: Option<String>,
    pub finalised_at: Option<DateTime<Utc>>,
    /// The statement.
    pub document_id: Option<Uuid>,
    pub commission_invoice_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<core_db::settlements::Settlement> for SettlementView {
    fn from(s: core_db::settlements::Settlement) -> Self {
        let currency = s.currency.trim().to_owned();
        let money = |minor| Money {
            minor,
            currency: currency.clone(),
        };
        Self {
            id: s.id,
            booking_id: s.booking_id,
            booking_title: s.booking_title,
            talent_id: s.talent_id,
            talent_name: s.talent_name,
            status: s.status,
            gross: money(s.gross_minor),
            commission_bps: s.commission_bps,
            commission: money(s.commission_minor),
            expenses: serde_json::from_value(s.expenses).unwrap_or_default(),
            expenses_total: money(s.expenses_minor),
            net: money(s.net_minor),
            paid_out: money(s.paid_out_minor),
            notes: s.notes,
            finalised_at: s.finalised_at,
            document_id: s.document_id,
            commission_invoice_id: s.commission_invoice_id,
            version: s.version,
            created_at: s.created_at,
            updated_at: s.updated_at,
        }
    }
}

pub fn to_json(v: &SettlementView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<SettlementView>> {
    Ok(core_db::settlements::get(tx, id).await?.map(Into::into))
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct SettlementFilter {
    pub booking_id: Option<Uuid>,
    pub talent_id: Option<Uuid>,
    /// draft or final.
    pub status: Option<String>,
}

pub async fn list(state: &AppState, ctx: &Ctx, f: &SettlementFilter) -> ApiResult<Vec<SettlementView>> {
    ctx.authorize(Permission::InvoiceRead)?;
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::settlements::list(&mut tx, f.booking_id, f.talent_id, f.status.as_deref()).await?;
    tx.commit().await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<SettlementView> {
    ctx.authorize(Permission::InvoiceRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

/// Each talent's gross share of a booking, net of tax.
async fn shares(tx: &mut Tx, booking: &super::bookings::BookingView) -> ApiResult<(String, Vec<(Uuid, i64)>)> {
    let quotes = core_db::quotes::list(tx, Some(booking.id), &[core_domain::QuoteStatus::Accepted], 0, 1)
        .await?
        .0;
    let talent: Vec<Uuid> = booking.talent.iter().map(|t| t.id).collect();
    let (currency, lines): (String, Vec<(Option<Uuid>, i64)>) = match quotes.into_iter().next() {
        Some(q) => {
            let v: super::quotes::QuoteView = q.into();
            (v.currency.clone(), v.line_items.iter().map(|l| (l.talent_id, l.amount_minor)).collect())
        }
        None => match &booking.fee {
            Some(f) => (f.currency.clone(), vec![(None, f.minor)]),
            None => (String::new(), vec![]),
        },
    };
    let mut out: Vec<(Uuid, i64)> = talent.iter().map(|t| (*t, 0)).collect();
    let mut unassigned = 0i64;
    for (t, amount) in lines {
        match t.and_then(|t| out.iter_mut().find(|(id, _)| *id == t)) {
            Some(slot) => slot.1 += amount,
            None => unassigned += amount,
        }
    }
    // Lines for no one in particular are shared evenly; the first talent
    // takes any odd unit.
    if !out.is_empty() && unassigned != 0 {
        let n = out.len() as i64;
        let each = unassigned / n;
        let rest = unassigned - each * n;
        for (i, slot) in out.iter_mut().enumerate() {
            slot.1 += each + if i == 0 { rest } else { 0 };
        }
    }
    Ok((currency, out))
}

/// Drafts a settlement for each talent on a booking that has none yet.
pub async fn create_for_booking(state: &AppState, ctx: &Ctx, booking_id: Uuid) -> ApiResult<Vec<SettlementView>> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let booking = super::bookings::load(&mut tx, booking_id).await?.ok_or(ApiError::NotFound)?;
    let (currency, shares) = shares(&mut tx, &booking).await?;
    if currency.is_empty() || shares.is_empty() {
        return Err(ApiError::conflict(
            "booking_unpriced",
            "the booking needs talent and a fee or accepted quote to settle",
        ));
    }
    for (talent_id, gross) in shares {
        if core_db::settlements::find(&mut tx, booking_id, talent_id).await?.is_some() {
            continue;
        }
        let talent = core_db::talent::get(&mut tx, talent_id).await?.ok_or(ApiError::NotFound)?;
        let bps = talent.commission_bps.unwrap_or(0).clamp(0, 10_000);
        let gross = gross.max(0);
        let s = settle(gross, bps, &[]).ok_or_else(|| ApiError::invalid("gross", "is too large"))?;
        let id = core_domain::ids::new_id();
        core_db::settlements::insert(
            &mut tx,
            id,
            booking_id,
            talent_id,
            core_db::settlements::Values {
                currency: &currency,
                gross_minor: gross,
                commission_bps: bps,
                commission_minor: s.commission_minor,
                expenses: &json!([]),
                expenses_minor: 0,
                net_minor: s.net_minor,
                notes: None,
            },
            ctx.user_id(),
        )
        .await?;
        let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
        record(
            &mut tx,
            ctx,
            Change {
                topic: "settlement.created",
                resource: ENTITY,
                object_id: Some(id),
                before: None,
                after: Some(to_json(&v)),
                event_extra: None,
            },
        )
        .await?;
    }
    let rows = core_db::settlements::list(&mut tx, Some(booking_id), None, None).await?;
    tx.commit().await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct SettlementPatch {
    pub gross_minor: Option<i64>,
    pub commission_bps: Option<i32>,
    pub expenses: Option<Vec<Expense>>,
    pub notes: Option<String>,
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    p: SettlementPatch,
    if_match: Option<i64>,
) -> ApiResult<SettlementView> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::settlements::lock(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.status != "draft" {
        return Err(ApiError::conflict("settlement_final", "reopen the settlement to change it"));
    }
    let mut check = Check::default();
    let gross = p.gross_minor.unwrap_or(before.gross.minor);
    if gross < 0 || gross > core_domain::bookings::MAX_AMOUNT_MINOR {
        check.issue("gross_minor", "must be zero or more");
    }
    let bps = p.commission_bps.unwrap_or(before.commission_bps);
    if !(0..=10_000).contains(&bps) {
        check.issue("commission_bps", "must be 0 to 10000");
    }
    let expenses = p.expenses.clone().unwrap_or(before.expenses.clone());
    let mut clean = Vec::new();
    for (i, e) in expenses.iter().enumerate() {
        let d = check.required(&format!("expenses[{i}].description"), &e.description, 200);
        if e.amount_minor <= 0 {
            check.issue(format!("expenses[{i}].amount_minor"), "must be more than zero");
        }
        clean.push(Expense { description: d, amount_minor: e.amount_minor });
    }
    if clean.len() > 50 {
        check.issue("expenses", "give at most 50 expenses");
    }
    let notes = match &p.notes {
        Some(n) => check.long("notes", Some(n), 5000),
        None => before.notes.clone(),
    };
    check.finish()?;
    let amounts: Vec<i64> = clean.iter().map(|e| e.amount_minor).collect();
    let s = settle(gross, bps, &amounts).ok_or_else(|| ApiError::invalid("expenses", "the amounts are too large"))?;
    let expenses_json = serde_json::to_value(&clean).map_err(ApiError::internal)?;
    core_db::settlements::update(
        &mut tx,
        id,
        core_db::settlements::Values {
            currency: &before.gross.currency,
            gross_minor: gross,
            commission_bps: bps,
            commission_minor: s.commission_minor,
            expenses: &expenses_json,
            expenses_minor: s.expenses_minor,
            net_minor: s.net_minor,
            notes: notes.as_deref(),
        },
        ctx.user_id(),
    )
    .await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "settlement.updated",
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

/// Fixes a settlement: prints its statement and, when the talent invoices
/// the buyer themselves, drafts the commission invoice to them.
pub async fn finalise(state: &AppState, ctx: &Ctx, id: Uuid, if_match: Option<i64>) -> ApiResult<SettlementView> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::settlements::lock(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.status != "draft" {
        return Err(ApiError::conflict("settlement_final", "this settlement is already final"));
    }
    let today = super::finance::today(&mut tx, ctx).await?;
    let mut record_json = super::documents::booking_context(&mut tx, state, before.booking_id).await?;
    if let Some(o) = record_json.as_object_mut() {
        o.insert(
            "settlement".into(),
            json!({
                "talent_name": before.talent_name,
                "currency": before.gross.currency,
                "gross": before.gross,
                "commission_bps": before.commission_bps,
                "commission": before.commission,
                "expenses": before.expenses,
                "net": before.net,
                "notes": before.notes,
            }),
        );
    }
    let doc_id = core_domain::ids::new_id();
    let title = format!("Statement for {}: {}", before.talent_name, before.booking_title);
    let reference = format!("S-{}", &id.simple().to_string()[..8].to_uppercase());
    let (title, html) = super::documents::render_html(
        state,
        &mut tx,
        ctx,
        core_engines::documents::STATEMENT,
        Some(&title),
        &reference,
        today,
        record_json,
    )
    .await?;
    super::documents::insert_generated(
        &mut tx,
        ctx,
        super::documents::NewGenerated {
            id: doc_id,
            kind: "statement",
            title: &title,
            related_entity: Some(super::bookings::ENTITY),
            related_id: Some(before.booking_id),
            template_key: Some(core_engines::documents::STATEMENT),
            document_date: Some(today),
            html: &html,
        },
    )
    .await?;
    let model = core_db::finance::settings(&mut tx).await?.invoicing_model;
    let commission_invoice = if model != "agency" && before.commission.minor > 0 {
        Some(
            super::invoices::insert_draft(
                &mut tx,
                ctx,
                super::invoices::NewInvoice {
                    purpose: Some("commission".into()),
                    booking_id: Some(before.booking_id),
                    payer: Some(super::invoices::Payer {
                        talent_id: Some(before.talent_id),
                        ..Default::default()
                    }),
                    currency: Some(before.gross.currency.clone()),
                    line_items: vec![super::invoices::LineInput {
                        description: format!(
                            "Commission ({}) on {}",
                            core_engines::templates::percent(i64::from(before.commission_bps)),
                            before.booking_title
                        ),
                        quantity: Some(1.0),
                        unit_minor: before.commission.minor,
                        tax_code_id: None,
                        talent_id: Some(before.talent_id),
                    }],
                    ..Default::default()
                },
                None,
            )
            .await?,
        )
    } else {
        None
    };
    core_db::settlements::finalise(&mut tx, id, Some(doc_id), commission_invoice, ctx.user_id()).await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "settlement.finalised",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    super::documents::render_now(state, ctx, doc_id, 1).await;
    Ok(after)
}

/// Back to draft, to correct it. Its statement stays on file.
pub async fn reopen(state: &AppState, ctx: &Ctx, id: Uuid, if_match: Option<i64>) -> ApiResult<SettlementView> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::settlements::lock(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.status != "final" {
        return Ok(before);
    }
    if before.paid_out.minor > 0 {
        return Err(ApiError::conflict(
            "settlement_paid",
            "payouts are recorded against it; void them first",
        ));
    }
    core_db::settlements::reopen(&mut tx, id, ctx.user_id()).await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "settlement.reopened",
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
    core_db::settlements::lock(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if !core_db::settlements::soft_delete(&mut tx, id, ctx.user_id()).await? {
        return Err(ApiError::conflict("settlement_final", "reopen the settlement to delete it"));
    }
    record(
        &mut tx,
        ctx,
        Change {
            topic: "settlement.deleted",
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

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NewPayout {
    /// Default: what is still owed to the talent.
    pub amount_minor: Option<i64>,
    pub method: Option<String>,
    pub paid_on: Option<NaiveDate>,
    pub reference: Option<String>,
}

/// Records paying the talent what a final settlement says they are owed.
pub async fn payout(state: &AppState, ctx: &Ctx, id: Uuid, p: NewPayout) -> ApiResult<SettlementView> {
    ctx.authorize(Permission::PaymentWrite)?;
    let mut tx = begin(state, ctx).await?;
    core_db::settlements::lock(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.status != "final" {
        return Err(ApiError::conflict("settlement_draft", "finalise the settlement first"));
    }
    let owed = before.net.minor - before.paid_out.minor;
    if owed <= 0 {
        return Err(ApiError::conflict("settlement_paid", "the talent has been paid everything owed"));
    }
    let amount = p.amount_minor.unwrap_or(owed);
    if amount <= 0 || amount > owed {
        return Err(ApiError::invalid(
            "amount_minor",
            format!("must be more than zero and at most {owed} (what is still owed)"),
        ));
    }
    let mut check = Check::default();
    let method = p.method.as_deref().unwrap_or("bank_transfer").to_owned();
    if !super::payments::METHODS.contains(&method.as_str()) {
        check.issue("method", "is not a payment method");
    }
    let reference = check.optional("reference", p.reference.as_deref(), 200);
    check.finish()?;
    let paid_on = match p.paid_on {
        Some(d) => d,
        None => super::finance::today(&mut tx, ctx).await?,
    };
    let pid = core_domain::ids::new_id();
    core_db::payments::insert(
        &mut tx,
        core_db::payments::NewPayment {
            id: pid,
            kind: "payout",
            amount_minor: amount,
            currency: &before.net.currency,
            method: &method,
            paid_on,
            reference: reference.as_deref(),
            organisation_id: None,
            talent_id: Some(before.talent_id),
            booking_id: Some(before.booking_id),
            refund_of: None,
            settlement_id: Some(id),
            notes: None,
        },
        ctx.user_id(),
    )
    .await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "settlement.paid_out",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: Some(json!({ "payment_id": pid, "amount_minor": amount })),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}
