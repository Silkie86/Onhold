//! Payments received, refunds and payouts to talent. A payment is
//! allocated to one or more invoices in its currency; each invoice's paid
//! amount and status follow from its live allocations. A payment is never
//! edited: a mistake is voided (taking its allocations back) and recorded
//! again.

use chrono::{DateTime, NaiveDate, Utc};
use core_db::Tx;
use core_db::payments::{Allocation, Payment};
use core_domain::{Ctx, Money, Permission, UserId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::crm::Check;
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const ENTITY: &str = "payment";
pub const METHODS: &[&str] = &["bank_transfer", "card", "cash", "cheque", "stripe", "other"];

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PaymentView {
    pub id: Uuid,
    /// payment (money in), refund or payout (money out to talent).
    pub kind: String,
    pub amount: Money,
    pub method: String,
    pub paid_on: NaiveDate,
    pub reference: Option<String>,
    pub organisation_id: Option<Uuid>,
    pub organisation_name: Option<String>,
    pub talent_id: Option<Uuid>,
    pub talent_name: Option<String>,
    pub booking_id: Option<Uuid>,
    pub refund_of: Option<Uuid>,
    pub settlement_id: Option<Uuid>,
    /// Allocated to invoices or refunded.
    pub allocated: Money,
    /// What is left to allocate.
    pub unallocated: Money,
    pub allocations: Vec<AllocationView>,
    pub notes: Option<String>,
    pub voided_at: Option<DateTime<Utc>>,
    pub void_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AllocationView {
    pub id: Uuid,
    pub payment_id: Uuid,
    pub invoice_id: Uuid,
    pub invoice_reference: Option<String>,
    pub amount_minor: i64,
    pub paid_on: NaiveDate,
    pub method: String,
}

impl From<Allocation> for AllocationView {
    fn from(a: Allocation) -> Self {
        Self {
            id: a.id,
            payment_id: a.payment_id,
            invoice_id: a.invoice_id,
            invoice_reference: a.invoice_reference,
            amount_minor: a.amount_minor,
            paid_on: a.paid_on,
            method: a.method,
        }
    }
}

fn view(p: Payment, allocations: Vec<Allocation>) -> PaymentView {
    let currency = p.currency.trim().to_owned();
    let money = |minor| Money {
        minor,
        currency: currency.clone(),
    };
    PaymentView {
        id: p.id,
        kind: p.kind,
        amount: money(p.amount_minor),
        method: p.method,
        paid_on: p.paid_on,
        reference: p.reference,
        organisation_id: p.organisation_id,
        organisation_name: p.organisation_name,
        talent_id: p.talent_id,
        talent_name: p.talent_name,
        booking_id: p.booking_id,
        refund_of: p.refund_of,
        settlement_id: p.settlement_id,
        allocated: money(p.allocated_minor),
        unallocated: money(p.amount_minor - p.allocated_minor),
        allocations: allocations.into_iter().map(Into::into).collect(),
        notes: p.notes,
        voided_at: p.voided_at,
        void_reason: p.void_reason,
        version: p.version,
        created_at: p.created_at,
        created_by: p.created_by,
    }
}

pub fn to_json(v: &PaymentView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

pub(crate) async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<PaymentView>> {
    let Some(p) = core_db::payments::get(tx, id).await? else {
        return Ok(None);
    };
    let allocations = core_db::payments::allocations(tx, Some(id), None).await?;
    Ok(Some(view(p, allocations)))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaymentPage {
    pub items: Vec<PaymentView>,
    pub total: i64,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct PaymentFilter {
    /// payment, refund or payout.
    pub kind: Option<String>,
    pub booking_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub talent_id: Option<Uuid>,
    /// Payments allocated to this invoice.
    pub invoice_id: Option<Uuid>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn list(state: &AppState, ctx: &Ctx, f: &PaymentFilter) -> ApiResult<PaymentPage> {
    ctx.authorize(Permission::InvoiceRead)?;
    let mut tx = begin(state, ctx).await?;
    let (rows, total) = core_db::payments::list(
        &mut tx,
        core_db::payments::Filter {
            kind: f.kind.as_deref(),
            booking_id: f.booking_id,
            organisation_id: f.organisation_id,
            talent_id: f.talent_id,
            invoice_id: f.invoice_id,
            from: f.from,
            to: f.to,
            offset: f.offset.unwrap_or(0).max(0),
            limit: f.limit.unwrap_or(50).clamp(1, 500),
        },
    )
    .await?;
    let mut items = Vec::new();
    for p in rows {
        let a = core_db::payments::allocations(&mut tx, Some(p.id), None).await?;
        items.push(view(p, a));
    }
    tx.commit().await?;
    Ok(PaymentPage { items, total })
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<PaymentView> {
    ctx.authorize(Permission::InvoiceRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AllocationInput {
    pub invoice_id: Uuid,
    /// Default: what the invoice still owes, up to what is left of the payment.
    pub amount_minor: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewPayment {
    pub amount: Money,
    /// Default bank_transfer.
    pub method: Option<String>,
    /// Default today.
    pub paid_on: Option<NaiveDate>,
    pub reference: Option<String>,
    /// Default: the first allocated invoice's payer.
    pub organisation_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
    /// Invoices it pays, in order.
    #[serde(default)]
    pub allocations: Vec<AllocationInput>,
    pub notes: Option<String>,
}

fn check_method(check: &mut Check, m: Option<&str>) -> String {
    let m = m.unwrap_or("bank_transfer").trim();
    if !METHODS.contains(&m) {
        check.issue("method", format!("must be one of: {}", METHODS.join(", ")));
    }
    m.to_owned()
}

/// Records money received, allocated to the invoices it pays.
pub async fn create(state: &AppState, ctx: &Ctx, new: NewPayment) -> ApiResult<PaymentView> {
    ctx.authorize(Permission::PaymentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let id = create_in(&mut tx, ctx, new, None).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

pub(crate) async fn create_in(
    tx: &mut Tx,
    ctx: &Ctx,
    new: NewPayment,
    method_override: Option<&str>,
) -> ApiResult<Uuid> {
    let mut check = Check::default();
    let amount = check.money("amount", Some(&new.amount));
    if new.amount.minor <= 0 {
        check.issue("amount", "must be more than zero");
    }
    let method = check_method(&mut check, method_override.or(new.method.as_deref()));
    let reference = check.optional("reference", new.reference.as_deref(), 200);
    let notes = check.long("notes", new.notes.as_deref(), 5000);
    check.finish()?;
    let amount = amount.ok_or_else(|| ApiError::invalid("amount", "is not an amount"))?;
    let paid_on = match new.paid_on {
        Some(d) => d,
        None => super::finance::today(tx, ctx).await?,
    };
    // The invoices, locked in id order so concurrent payments cannot deadlock.
    let mut ids: Vec<Uuid> = new.allocations.iter().map(|a| a.invoice_id).collect();
    ids.sort();
    ids.dedup();
    if ids.len() != new.allocations.len() {
        return Err(ApiError::invalid("allocations", "name each invoice once"));
    }
    for id in &ids {
        core_db::invoices::lock(tx, *id).await?;
    }
    let mut plan = Vec::new();
    let mut left = amount.minor;
    let mut organisation = new.organisation_id;
    let mut booking = new.booking_id;
    for (i, a) in new.allocations.iter().enumerate() {
        let inv = super::invoices::load(tx, a.invoice_id)
            .await?
            .ok_or_else(|| {
                ApiError::invalid(
                    format!("allocations[{i}].invoice_id"),
                    "there is no such invoice",
                )
            })?;
        if inv.kind != "invoice" || !inv.status.is_issued() {
            return Err(ApiError::invalid(
                format!("allocations[{i}].invoice_id"),
                "payments go against issued invoices",
            ));
        }
        if inv.currency != amount.currency {
            return Err(ApiError::invalid(
                format!("allocations[{i}].invoice_id"),
                format!(
                    "the invoice is in {}; the payment in {}",
                    inv.currency, amount.currency
                ),
            ));
        }
        let want = a.amount_minor.unwrap_or(inv.outstanding.minor.min(left));
        if want <= 0 || want > inv.outstanding.minor {
            return Err(ApiError::invalid(
                format!("allocations[{i}].amount_minor"),
                format!(
                    "must be more than zero and at most {} (what it still owes)",
                    inv.outstanding.minor
                ),
            ));
        }
        if want > left {
            return Err(ApiError::invalid(
                "allocations",
                "the allocations add up to more than the payment",
            ));
        }
        left -= want;
        organisation = organisation.or(inv.payer_organisation_id);
        booking = booking.or(inv.booking_id);
        plan.push((inv.id, want));
    }
    let id = core_domain::ids::new_id();
    core_db::payments::insert(
        tx,
        core_db::payments::NewPayment {
            id,
            kind: "payment",
            amount_minor: amount.minor,
            currency: &amount.currency,
            method: &method,
            paid_on,
            reference: reference.as_deref(),
            organisation_id: organisation,
            talent_id: None,
            booking_id: booking,
            refund_of: None,
            settlement_id: None,
            notes: notes.as_deref(),
        },
        ctx.user_id(),
    )
    .await?;
    for (invoice, amount) in &plan {
        core_db::payments::allocate(tx, id, *invoice, *amount, ctx.user_id()).await?;
        core_db::payments::add_allocated(tx, id, *amount).await?;
    }
    let v = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "payment.received",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(to_json(&v)),
            event_extra: None,
        },
    )
    .await?;
    for (invoice, _) in &plan {
        super::invoices::recompute(tx, ctx, *invoice).await?;
    }
    Ok(id)
}

/// Allocates what is left of a payment to another invoice.
pub async fn allocate(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    a: AllocationInput,
    if_match: Option<i64>,
) -> ApiResult<PaymentView> {
    ctx.authorize(Permission::PaymentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::payments::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.kind != "payment" || before.voided_at.is_some() {
        return Err(ApiError::conflict(
            "payment_closed",
            "only a live payment can be allocated",
        ));
    }
    core_db::invoices::lock(&mut tx, a.invoice_id).await?;
    let inv = super::invoices::load(&mut tx, a.invoice_id)
        .await?
        .ok_or_else(|| ApiError::invalid("invoice_id", "there is no such invoice"))?;
    if inv.kind != "invoice" || !inv.status.is_issued() || inv.currency != before.amount.currency {
        return Err(ApiError::invalid(
            "invoice_id",
            "payments go against issued invoices in the same currency",
        ));
    }
    let want = a
        .amount_minor
        .unwrap_or(inv.outstanding.minor.min(before.unallocated.minor));
    if want <= 0 || want > inv.outstanding.minor || want > before.unallocated.minor {
        return Err(ApiError::invalid(
            "amount_minor",
            "must be more than zero, and no more than the invoice owes or the payment has left",
        ));
    }
    core_db::payments::allocate(&mut tx, id, inv.id, want, ctx.user_id()).await?;
    core_db::payments::add_allocated(&mut tx, id, want).await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "payment.allocated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    super::invoices::recompute(&mut tx, ctx, inv.id).await?;
    tx.commit().await?;
    Ok(after)
}

/// Takes one allocation back.
pub async fn remove_allocation(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    allocation_id: Uuid,
) -> ApiResult<PaymentView> {
    ctx.authorize(Permission::PaymentWrite)?;
    let mut tx = begin(state, ctx).await?;
    core_db::payments::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let Some(a) = before.allocations.iter().find(|a| a.id == allocation_id) else {
        return Err(ApiError::NotFound);
    };
    core_db::invoices::lock(&mut tx, a.invoice_id).await?;
    let (_, invoice, amount) =
        core_db::payments::remove_allocation(&mut tx, allocation_id, ctx.user_id())
            .await?
            .ok_or(ApiError::NotFound)?;
    core_db::payments::add_allocated(&mut tx, id, -amount).await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "payment.unallocated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    super::invoices::recompute(&mut tx, ctx, invoice).await?;
    tx.commit().await?;
    Ok(after)
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewRefund {
    pub amount_minor: i64,
    pub method: Option<String>,
    pub paid_on: Option<NaiveDate>,
    pub reference: Option<String>,
    pub notes: Option<String>,
}

/// Money returned from a payment. Only what is not allocated can be
/// refunded: take allocations back (or credit the invoice) first.
pub async fn refund(state: &AppState, ctx: &Ctx, id: Uuid, r: NewRefund) -> ApiResult<PaymentView> {
    ctx.authorize(Permission::PaymentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let refund_id = refund_in(&mut tx, ctx, id, r).await?;
    let refund = load(&mut tx, refund_id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(refund)
}

pub(crate) async fn refund_in(tx: &mut Tx, ctx: &Ctx, id: Uuid, r: NewRefund) -> ApiResult<Uuid> {
    core_db::payments::lock(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let before = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.kind != "payment" || before.voided_at.is_some() {
        return Err(ApiError::conflict(
            "payment_closed",
            "only a live payment can be refunded",
        ));
    }
    let mut check = Check::default();
    if r.amount_minor <= 0 || r.amount_minor > before.unallocated.minor {
        check.issue(
            "amount_minor",
            format!(
                "must be more than zero and at most {} (what is not allocated to invoices)",
                before.unallocated.minor
            ),
        );
    }
    let method = check_method(&mut check, r.method.as_deref().or(Some(&before.method)));
    let reference = check.optional("reference", r.reference.as_deref(), 200);
    let notes = check.long("notes", r.notes.as_deref(), 5000);
    check.finish()?;
    let paid_on = match r.paid_on {
        Some(d) => d,
        None => super::finance::today(tx, ctx).await?,
    };
    let refund_id = core_domain::ids::new_id();
    core_db::payments::insert(
        tx,
        core_db::payments::NewPayment {
            id: refund_id,
            kind: "refund",
            amount_minor: r.amount_minor,
            currency: &before.amount.currency,
            method: &method,
            paid_on,
            reference: reference.as_deref(),
            organisation_id: before.organisation_id,
            talent_id: None,
            booking_id: before.booking_id,
            refund_of: Some(id),
            settlement_id: None,
            notes: notes.as_deref(),
        },
        ctx.user_id(),
    )
    .await?;
    core_db::payments::add_allocated(tx, id, r.amount_minor).await?;
    let refund = load(tx, refund_id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "payment.refunded",
            resource: ENTITY,
            object_id: Some(refund_id),
            before: None,
            after: Some(to_json(&refund)),
            event_extra: Some(serde_json::json!({ "refund_of": id })),
        },
    )
    .await?;
    Ok(refund_id)
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct VoidPayment {
    pub reason: Option<String>,
}

/// Voids a payment recorded in error: its allocations are taken back and
/// the invoices owe again. A payment with refunds is not voided.
pub async fn void(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    req: VoidPayment,
    if_match: Option<i64>,
) -> ApiResult<PaymentView> {
    ctx.authorize(Permission::PaymentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::payments::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.voided_at.is_some() {
        return Ok(before);
    }
    if core_db::payments::refunded(&mut tx, id).await? > 0 {
        return Err(ApiError::conflict(
            "payment_refunded",
            "this payment has refunds; void those first",
        ));
    }
    let reason = super::optional_text("reason", req.reason, 500)?;
    let mut invoices = Vec::new();
    for a in &before.allocations {
        core_db::invoices::lock(&mut tx, a.invoice_id).await?;
        core_db::payments::remove_allocation(&mut tx, a.id, ctx.user_id()).await?;
        invoices.push(a.invoice_id);
    }
    if let Some(of) = before.refund_of {
        core_db::payments::lock(&mut tx, of).await?;
        core_db::payments::add_allocated(&mut tx, of, -before.amount.minor).await?;
    }
    core_db::payments::void(&mut tx, id, reason.as_deref(), ctx.user_id()).await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "payment.voided",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    for i in invoices {
        super::invoices::recompute(&mut tx, ctx, i).await?;
    }
    tx.commit().await?;
    Ok(after)
}

/// An invoice's allocations, for its page.
pub async fn for_invoice(
    state: &AppState,
    ctx: &Ctx,
    invoice_id: Uuid,
) -> ApiResult<Vec<AllocationView>> {
    ctx.authorize(Permission::InvoiceRead)?;
    let mut tx = begin(state, ctx).await?;
    if core_db::invoices::get(&mut tx, invoice_id).await?.is_none() {
        return Err(ApiError::NotFound);
    }
    let rows = core_db::payments::allocations(&mut tx, None, Some(invoice_id)).await?;
    tx.commit().await?;
    Ok(rows.into_iter().map(Into::into).collect())
}
