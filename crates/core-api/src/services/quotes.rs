//! Quotes: a priced offer on a hold, sent to the buyer as an email with a
//! secret link. The buyer opens it on the web (no account) and accepts or
//! declines. Accepting freezes exactly what was accepted, moves the hold to
//! `pending`, and moves the enquiry to its won stage; rules take it from
//! there (contract, deposit invoice). A sent quote is never edited: a
//! revision keeps the number, counts up, and replaces it when sent.

use chrono::{DateTime, NaiveDate, Utc};
use core_db::Tx;
use core_db::quotes::{ACCEPTED_FROZEN, Quote};
use core_domain::crypto::{hash_token, new_token};
use core_domain::workspace::WorkspaceStatus;
use core_domain::{BookingStatus, Ctx, Money, Permission, QuoteStatus, UserId, WorkspaceId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::crm::Check;
use super::links::Named;
use super::{Change, begin, check_version, record_with_secret};
use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::routes::nullable;

pub const ENTITY: &str = "quote";
pub const MAX_LINES: usize = 50;
/// How long a new quote is valid, in days, unless set.
pub const DEFAULT_VALID_DAYS: i64 = 14;
const COUNTER: &str = "quote";

/// A line as given: what, how many, at what price each.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct LineInput {
    pub description: String,
    /// Up to three decimal places (1.5 hours). Default 1.
    pub quantity: Option<f64>,
    /// Price of one, in minor units of the quote's currency. Negative for a discount.
    pub unit_minor: i64,
    /// The talent the line is for, if any.
    pub talent_id: Option<Uuid>,
}

/// A line with its amount worked out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Line {
    pub description: String,
    pub quantity: f64,
    pub unit_minor: i64,
    pub amount_minor: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub talent_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QuoteView {
    pub id: Uuid,
    pub booking_id: Uuid,
    pub booking_title: String,
    pub number: i64,
    pub revision: i32,
    /// How people refer to it: `Q-0007`, or `Q-0007-R2` for a revision.
    pub reference: String,
    pub supersedes_id: Option<Uuid>,
    pub status: QuoteStatus,
    pub currency: String,
    pub line_items: Vec<Line>,
    pub total: Money,
    /// The last day it can be accepted.
    pub valid_until: Option<NaiveDate>,
    pub message: Option<String>,
    pub terms: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
    pub sent_to: Option<String>,
    pub viewed_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_name: Option<String>,
    pub decided_email: Option<String>,
    pub decline_reason: Option<String>,
    /// Exactly what was accepted.
    #[schema(value_type = Option<Object>)]
    pub snapshot: Option<Value>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

pub fn reference(number: i64, revision: i32) -> String {
    if revision > 1 {
        format!("Q-{number:04}-R{revision}")
    } else {
        format!("Q-{number:04}")
    }
}

impl From<Quote> for QuoteView {
    fn from(q: Quote) -> Self {
        let currency = q.currency.trim().to_owned();
        Self {
            id: q.id,
            booking_id: q.booking_id,
            booking_title: q.booking_title,
            number: q.number,
            revision: q.revision,
            reference: reference(q.number, q.revision),
            supersedes_id: q.supersedes_id,
            status: q.status,
            line_items: serde_json::from_value(q.line_items).unwrap_or_default(),
            total: Money {
                minor: q.total_minor,
                currency: currency.clone(),
            },
            currency,
            valid_until: q.valid_until,
            message: q.message,
            terms: q.terms,
            sent_at: q.sent_at,
            sent_to: q.sent_to,
            viewed_at: q.viewed_at,
            decided_at: q.decided_at,
            decided_name: q.decided_name,
            decided_email: q.decided_email,
            decline_reason: q.decline_reason,
            snapshot: q.snapshot,
            version: q.version,
            created_at: q.created_at,
            updated_at: q.updated_at,
            created_by: q.created_by,
            updated_by: q.updated_by,
        }
    }
}

/// The JSON events, the audit log, rules and templates see (the snapshot
/// is left out: it repeats the rest).
pub fn to_json(v: &QuoteView) -> Value {
    let mut json = serde_json::to_value(v).unwrap_or(Value::Null);
    if let Some(o) = json.as_object_mut() {
        o.remove("snapshot");
    }
    json
}

pub(crate) async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<QuoteView>> {
    Ok(core_db::quotes::get(tx, id).await?.map(Into::into))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct QuotePage {
    pub items: Vec<QuoteView>,
    pub total: i64,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct QuoteFilter {
    pub booking_id: Option<Uuid>,
    /// Comma-separated statuses, e.g. `sent,viewed`.
    pub status: Option<String>,
    pub offset: Option<i64>,
    /// 1 to 200, default 50.
    pub limit: Option<i64>,
}

pub async fn list(state: &AppState, ctx: &Ctx, f: &QuoteFilter) -> ApiResult<QuotePage> {
    ctx.authorize(Permission::QuoteRead)?;
    let mut statuses = Vec::new();
    for part in f
        .status
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        statuses.push(
            QuoteStatus::parse(part)
                .ok_or_else(|| ApiError::invalid("status", format!("`{part}` is not a status")))?,
        );
    }
    let mut tx = begin(state, ctx).await?;
    if let Some(b) = f.booking_id
        && core_db::bookings::get(&mut tx, b).await?.is_none()
    {
        return Err(ApiError::NotFound);
    }
    let (rows, total) = core_db::quotes::list(
        &mut tx,
        f.booking_id,
        &statuses,
        f.offset.unwrap_or(0).max(0),
        f.limit.unwrap_or(50).clamp(1, 200),
    )
    .await?;
    tx.commit().await?;
    Ok(QuotePage {
        items: rows.into_iter().map(Into::into).collect(),
        total,
    })
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<QuoteView> {
    ctx.authorize(Permission::QuoteRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

/// Checks lines and works out their amounts and the total.
fn check_lines(check: &mut Check, lines: &[LineInput]) -> (Vec<Line>, i64) {
    if lines.len() > MAX_LINES {
        check.issue("line_items", format!("give at most {MAX_LINES} lines"));
    }
    let mut out = Vec::new();
    let mut total: i64 = 0;
    for (i, l) in lines.iter().enumerate() {
        let description =
            check.required(&format!("line_items[{i}].description"), &l.description, 500);
        let quantity = l.quantity.unwrap_or(1.0);
        let milli = (quantity * 1000.0).round();
        if !quantity.is_finite()
            || quantity < 0.0
            || (quantity * 1000.0 - milli).abs() > 1e-6
            || milli > 1e9
        {
            check.issue(
                format!("line_items[{i}].quantity"),
                "must be a positive number with at most three decimal places",
            );
            continue;
        }
        let Some(amount) = core_domain::bookings::line_amount(milli as i64, l.unit_minor) else {
            check.issue(format!("line_items[{i}].unit_minor"), "is too large");
            continue;
        };
        total = total.saturating_add(amount);
        out.push(Line {
            description,
            quantity: milli / 1000.0,
            unit_minor: l.unit_minor,
            amount_minor: amount,
            talent_id: l.talent_id,
        });
    }
    if total < 0 {
        check.issue("line_items", "the total must not be negative");
    } else if total > core_domain::bookings::MAX_AMOUNT_MINOR {
        check.issue("line_items", "the total is too large");
    }
    (out, total)
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NewQuote {
    /// Default: the booking's fee currency, else the first talent's, else the workspace's.
    pub currency: Option<String>,
    /// Default: the booking's fee as one line, else each talent's default fee.
    pub line_items: Option<Vec<LineInput>>,
    /// Default: two weeks from today.
    pub valid_until: Option<NaiveDate>,
    pub message: Option<String>,
    pub terms: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct QuotePatch {
    pub currency: Option<String>,
    /// Replaces the lines.
    pub line_items: Option<Vec<LineInput>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<NaiveDate>)]
    pub valid_until: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub message: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub terms: Option<Option<String>>,
}

async fn today(tx: &mut Tx, ctx: &Ctx) -> ApiResult<NaiveDate> {
    let tz: chrono_tz::Tz = core_db::workspaces::get(tx, ctx.workspace_id)
        .await?
        .and_then(|w| w.default_timezone.parse().ok())
        .unwrap_or(chrono_tz::UTC);
    Ok(Utc::now().with_timezone(&tz).date_naive())
}

/// The lines a new quote starts with when none are given.
async fn suggested_lines(
    tx: &mut Tx,
    booking: &super::bookings::BookingView,
    currency: &str,
) -> ApiResult<Vec<LineInput>> {
    if let Some(fee) = &booking.fee
        && fee.currency == currency
    {
        return Ok(vec![LineInput {
            description: format!("Fee: {}", booking.title),
            quantity: Some(1.0),
            unit_minor: fee.minor,
            talent_id: booking.talent.first().map(|t| t.id),
        }]);
    }
    let mut lines = Vec::new();
    for t in &booking.talent {
        if let Some(talent) = core_db::talent::get(tx, t.id).await?
            && let (Some(minor), Some(cur)) =
                (talent.default_fee_minor, talent.default_fee_currency)
            && cur.trim() == currency
        {
            lines.push(LineInput {
                description: format!("Fee: {}", t.name),
                quantity: Some(1.0),
                unit_minor: minor,
                talent_id: Some(t.id),
            });
        }
    }
    Ok(lines)
}

/// A new draft quote for a booking, priced from the booking or its talent.
pub async fn create(
    state: &AppState,
    ctx: &Ctx,
    booking_id: Uuid,
    new: NewQuote,
) -> ApiResult<QuoteView> {
    ctx.authorize(Permission::QuoteWrite)?;
    let mut tx = begin(state, ctx).await?;
    let booking = super::bookings::load(&mut tx, booking_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if booking.status.is_final() {
        return Err(ApiError::conflict(
            "booking_closed",
            format!("a {} booking cannot be quoted", booking.status),
        ));
    }
    let mut check = Check::default();
    let currency = match check.currency("currency", new.currency.as_deref()) {
        Some(c) => c,
        None => match &booking.fee {
            Some(f) => f.currency.clone(),
            None => {
                let mut from_talent = None;
                for t in &booking.talent {
                    if let Some(row) = core_db::talent::get(&mut tx, t.id).await?
                        && let Some(c) = row.default_fee_currency
                    {
                        from_talent = Some(c.trim().to_owned());
                        break;
                    }
                }
                match from_talent {
                    Some(c) => c,
                    None => core_db::workspaces::get(&mut tx, ctx.workspace_id)
                        .await?
                        .map_or_else(|| "GBP".into(), |w| w.default_currency.trim().to_owned()),
                }
            }
        },
    };
    let inputs = match new.line_items {
        Some(l) => l,
        None => suggested_lines(&mut tx, &booking, &currency).await?,
    };
    let (lines, total) = check_lines(&mut check, &inputs);
    let message = check.long("message", new.message.as_deref(), 5000);
    let terms = check.long("terms", new.terms.as_deref(), 20_000);
    let today = today(&mut tx, ctx).await?;
    let valid_until = new
        .valid_until
        .unwrap_or(today + chrono::Duration::days(DEFAULT_VALID_DAYS));
    if valid_until < today {
        check.issue("valid_until", "must not be in the past");
    }
    check.finish()?;

    let number = core_db::counters::next(&mut tx, COUNTER).await?;
    let id = core_domain::ids::new_id();
    let lines = serde_json::to_value(&lines).map_err(ApiError::internal)?;
    core_db::quotes::insert(
        &mut tx,
        id,
        booking_id,
        number,
        1,
        None,
        core_db::quotes::Values {
            currency: &currency,
            line_items: &lines,
            total_minor: total,
            valid_until: Some(valid_until),
            message: message.as_deref(),
            terms: terms.as_deref(),
        },
        ctx.user_id(),
    )
    .await?;
    let view = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    super::record(
        &mut tx,
        ctx,
        Change {
            topic: "quote.created",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(to_json(&view)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(view)
}

async fn locked_draft(tx: &mut Tx, id: Uuid, if_match: Option<i64>) -> ApiResult<QuoteView> {
    let version = core_db::quotes::lock(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let q = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    if q.status != QuoteStatus::Draft {
        return Err(ApiError::conflict(
            "quote_not_draft",
            "a sent quote cannot be changed; make a revision instead",
        ));
    }
    Ok(q)
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    p: QuotePatch,
    if_match: Option<i64>,
) -> ApiResult<QuoteView> {
    ctx.authorize(Permission::QuoteWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = locked_draft(&mut tx, id, if_match).await?;
    let mut check = Check::default();
    let currency = match &p.currency {
        Some(c) => check
            .currency("currency", Some(c))
            .unwrap_or_else(|| before.currency.clone()),
        None => before.currency.clone(),
    };
    let (lines, total) = match &p.line_items {
        Some(l) => check_lines(&mut check, l),
        None => (before.line_items.clone(), before.total.minor),
    };
    let message = match &p.message {
        Some(m) => check.long("message", m.as_deref(), 5000),
        None => before.message.clone(),
    };
    let terms = match &p.terms {
        Some(t) => check.long("terms", t.as_deref(), 20_000),
        None => before.terms.clone(),
    };
    let valid_until = p.valid_until.unwrap_or(before.valid_until);
    if p.valid_until.is_some()
        && let Some(v) = valid_until
        && v < today(&mut tx, ctx).await?
    {
        check.issue("valid_until", "must not be in the past");
    }
    check.finish()?;
    let lines_json = serde_json::to_value(&lines).map_err(ApiError::internal)?;
    core_db::quotes::update(
        &mut tx,
        id,
        core_db::quotes::Values {
            currency: &currency,
            line_items: &lines_json,
            total_minor: total,
            valid_until,
            message: message.as_deref(),
            terms: terms.as_deref(),
        },
        ctx.user_id(),
    )
    .await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    super::record(
        &mut tx,
        ctx,
        Change {
            topic: "quote.updated",
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

/// Only drafts are deleted; anything sent stays on record.
pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::QuoteWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = locked_draft(&mut tx, id, None).await?;
    core_db::quotes::soft_delete(&mut tx, id, ctx.user_id()).await?;
    super::record(
        &mut tx,
        ctx,
        Change {
            topic: "quote.deleted",
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

/// A new draft revision of a sent (or answered) quote, with the same lines.
pub async fn revise(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    if_match: Option<i64>,
) -> ApiResult<QuoteView> {
    ctx.authorize(Permission::QuoteWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::quotes::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let source = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if source.status == QuoteStatus::Draft {
        return Err(ApiError::conflict(
            "quote_is_draft",
            "this quote has not been sent; edit it instead",
        ));
    }
    let booking = super::bookings::load(&mut tx, source.booking_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if booking.status.is_final() {
        return Err(ApiError::conflict(
            "booking_closed",
            format!("a {} booking cannot be quoted", booking.status),
        ));
    }
    let revision = core_db::quotes::latest_revision(&mut tx, source.number).await? + 1;
    let today = today(&mut tx, ctx).await?;
    let valid_until = source
        .valid_until
        .filter(|v| *v >= today)
        .unwrap_or(today + chrono::Duration::days(DEFAULT_VALID_DAYS));
    let new_id = core_domain::ids::new_id();
    let lines = serde_json::to_value(&source.line_items).map_err(ApiError::internal)?;
    core_db::quotes::insert(
        &mut tx,
        new_id,
        source.booking_id,
        source.number,
        revision,
        Some(source.id),
        core_db::quotes::Values {
            currency: &source.currency,
            line_items: &lines,
            total_minor: source.total.minor,
            valid_until: Some(valid_until),
            message: source.message.as_deref(),
            terms: source.terms.as_deref(),
        },
        ctx.user_id(),
    )
    .await?;
    let view = load(&mut tx, new_id).await?.ok_or(ApiError::NotFound)?;
    super::record(
        &mut tx,
        ctx,
        Change {
            topic: "quote.created",
            resource: ENTITY,
            object_id: Some(new_id),
            before: None,
            after: Some(to_json(&view)),
            event_extra: Some(json!({ "revises": source.id })),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(view)
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct SendQuote {
    /// Default: the booking contact's first email address.
    pub to: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SentQuote {
    pub quote: QuoteView,
    /// The buyer's link, to copy into another message if needed. Shown once.
    pub link: String,
}

/// Sends a draft: emails the buyer a link to view and accept it. Any other
/// quote still open on the booking is withdrawn, so only one can be accepted.
pub async fn send(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    req: SendQuote,
    if_match: Option<i64>,
) -> ApiResult<SentQuote> {
    ctx.authorize(Permission::QuoteWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = locked_draft(&mut tx, id, if_match).await?;
    let booking = super::bookings::load(&mut tx, before.booking_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_not_accepted(&mut tx, &before).await?;
    if !matches!(
        booking.status,
        BookingStatus::Hold | BookingStatus::Pending | BookingStatus::Confirmed
    ) {
        return Err(ApiError::conflict(
            "booking_closed",
            format!(
                "the booking is {}; put it on hold again first",
                booking.status
            ),
        ));
    }
    if before.line_items.is_empty() {
        return Err(ApiError::invalid(
            "line_items",
            "add at least one line before sending",
        ));
    }
    if let Some(v) = before.valid_until
        && v < today(&mut tx, ctx).await?
    {
        return Err(ApiError::invalid(
            "valid_until",
            "the quote's last valid day has passed; change it before sending",
        ));
    }
    let to = match req.to.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t.to_lowercase(),
        None => match booking.contact_id {
            Some(c) => core_db::contacts::get(&mut tx, c)
                .await?
                .and_then(|c| c.emails.first().cloned())
                .ok_or_else(|| {
                    ApiError::invalid("to", "the booking's contact has no email; give an address")
                })?,
            None => {
                return Err(ApiError::invalid(
                    "to",
                    "the booking has no contact; give an address",
                ));
            }
        },
    };
    if !core_engines::fields::is_email(&to) || to.len() > 320 {
        return Err(ApiError::invalid("to", "is not an email address"));
    }

    for other in core_db::quotes::open_for_booking(&mut tx, before.booking_id).await? {
        if other != id {
            close(&mut tx, ctx, other, QuoteStatus::Withdrawn).await?;
        }
    }
    let token = new_token();
    core_db::quotes::mark_sent(&mut tx, id, &hash_token(&token), &to, ctx.user_id()).await?;
    let link = state.config.app_url(&format!("/quote/{token}"), &[]);
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record_with_secret(
        &mut tx,
        ctx,
        Change {
            topic: "quote.sent",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: Some(json!({ "booking": booking_summary(&booking) })),
        },
        &state.config.keyring,
        &json!({ "url": link }),
    )
    .await?;
    if let Some(enquiry) = booking.enquiry_id {
        super::enquiries::reach(&mut tx, ctx, enquiry, super::enquiries::Milestone::Quoted).await?;
    }
    tx.commit().await?;
    Ok(SentQuote { quote: after, link })
}

/// What the buyer and the email are told about the booking.
fn booking_summary(b: &super::bookings::BookingView) -> Value {
    json!({
        "id": b.id,
        "title": b.title,
        "starts_at": b.starts_at,
        "ends_at": b.ends_at,
        "all_day": b.all_day,
        "timezone": b.timezone,
        "venue": b.venue_name.clone().or(b.venue_text.clone()),
        "talent": b.talent.iter().map(|t| &t.name).collect::<Vec<_>>(),
    })
}

/// Closes an open quote without an answer: withdrawn or expired.
async fn close(tx: &mut Tx, ctx: &Ctx, id: Uuid, status: QuoteStatus) -> ApiResult<()> {
    let Some(before) = load(tx, id).await? else {
        return Ok(());
    };
    if !before.status.is_open() {
        return Ok(());
    }
    core_db::quotes::close(tx, id, status, ctx.user_id()).await?;
    let after = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    super::record(
        tx,
        ctx,
        Change {
            topic: status.topic(),
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    Ok(())
}

/// Withdraws a booking's open quotes (it was cancelled, lapsed or deleted).
pub(crate) async fn withdraw_open(tx: &mut Tx, ctx: &Ctx, booking_id: Uuid) -> ApiResult<()> {
    for id in core_db::quotes::open_for_booking(tx, booking_id).await? {
        close(tx, ctx, id, QuoteStatus::Withdrawn).await?;
    }
    Ok(())
}

/// Takes back a sent quote before the buyer answers.
pub async fn withdraw(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    if_match: Option<i64>,
) -> ApiResult<QuoteView> {
    ctx.authorize(Permission::QuoteWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::quotes::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let q = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if !q.status.is_open() {
        return Err(ApiError::conflict(
            "quote_closed",
            format!("a {} quote cannot be withdrawn", q.status.as_str()),
        ));
    }
    close(&mut tx, ctx, id, QuoteStatus::Withdrawn).await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(after)
}

pub const JOB_EXPIRE: &str = "quote.expire";

/// Closes an open quote whose last valid day has passed (the worker's `quote.expire`).
pub async fn expire(state: &AppState, workspace: WorkspaceId, id: Uuid) -> ApiResult<()> {
    let ctx = Ctx::system(workspace);
    let mut tx = begin(state, &ctx).await?;
    if core_db::quotes::lock(&mut tx, id).await?.is_none() {
        return Ok(());
    }
    let today = today(&mut tx, &ctx).await?;
    if let Some(q) = load(&mut tx, id).await?
        && q.status.is_open()
        && q.valid_until.is_some_and(|v| v < today)
    {
        close(&mut tx, &ctx, id, QuoteStatus::Expired).await?;
    }
    tx.commit().await?;
    Ok(())
}

// The buyer's side --------------------------------------------------------------------

/// A quote as the buyer sees it: no internal notes, ids or history.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicQuote {
    pub workspace_name: String,
    /// The workspace's logo and colours, for a page in its own style.
    #[schema(value_type = Object)]
    pub branding: Value,
    pub reference: String,
    pub status: QuoteStatus,
    pub line_items: Vec<PublicLine>,
    pub total: Money,
    pub valid_until: Option<NaiveDate>,
    pub message: Option<String>,
    pub terms: Option<String>,
    pub booking_title: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    pub timezone: String,
    pub venue: Option<String>,
    pub talent: Vec<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_name: Option<String>,
    /// True while the buyer can still accept or decline.
    pub can_decide: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicLine {
    pub description: String,
    pub quantity: f64,
    pub unit: Money,
    pub amount: Money,
}

/// The workspace and quote behind a buyer's link, with a context acting for
/// the buyer (as the system, with their address recorded).
async fn open_public(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
) -> ApiResult<(Tx, Ctx, Uuid)> {
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let (id, workspace) = core_db::quotes::find_public(&mut tx, &hash_token(token))
        .await?
        .ok_or(ApiError::NotFound)?;
    core_db::set_context(&mut tx, Some(workspace), None).await?;
    // A closed or suspended workspace's links stop working, as if withdrawn.
    core_db::workspaces::get(&mut tx, workspace)
        .await?
        .filter(|w| matches!(w.status, WorkspaceStatus::Trial | WorkspaceStatus::Active))
        .ok_or(ApiError::NotFound)?;
    let mut ctx = Ctx::system(workspace);
    ctx.meta = meta;
    Ok((tx, ctx, id))
}

async fn public_view(tx: &mut Tx, ctx: &Ctx, q: &QuoteView) -> ApiResult<PublicQuote> {
    let w = core_db::workspaces::get(tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let b = super::bookings::load(tx, q.booking_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let today = today(tx, ctx).await?;
    let money = |minor: i64| Money {
        minor,
        currency: q.currency.clone(),
    };
    Ok(PublicQuote {
        workspace_name: w.name,
        branding: w.branding,
        reference: q.reference.clone(),
        status: q.status,
        line_items: q
            .line_items
            .iter()
            .map(|l| PublicLine {
                description: l.description.clone(),
                quantity: l.quantity,
                unit: money(l.unit_minor),
                amount: money(l.amount_minor),
            })
            .collect(),
        total: q.total.clone(),
        valid_until: q.valid_until,
        message: q.message.clone(),
        terms: q.terms.clone(),
        booking_title: b.title,
        starts_at: b.starts_at,
        ends_at: b.ends_at,
        all_day: b.all_day,
        timezone: b.timezone,
        venue: b.venue_name.or(b.venue_text),
        talent: b.talent.into_iter().map(|t: Named| t.name).collect(),
        decided_at: q.decided_at,
        decided_name: q.decided_name.clone(),
        can_decide: q.status.is_open() && q.valid_until.is_none_or(|v| v >= today),
    })
}

/// The buyer opens their link. The first view is recorded.
pub async fn public_get(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
) -> ApiResult<PublicQuote> {
    let (mut tx, ctx, id) = open_public(state, token, meta).await?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    if before.status == QuoteStatus::Draft {
        return Err(ApiError::NotFound);
    }
    let q = if core_db::quotes::mark_viewed(&mut tx, id).await? {
        let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
        super::record(
            &mut tx,
            &ctx,
            Change {
                topic: "quote.viewed",
                resource: ENTITY,
                object_id: Some(id),
                before: Some(to_json(&before)),
                after: Some(to_json(&after)),
                event_extra: None,
            },
        )
        .await?;
        after
    } else {
        before
    };
    let view = public_view(&mut tx, &ctx, &q).await?;
    tx.commit().await?;
    Ok(view)
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AcceptQuote {
    /// The buyer's name, as their acceptance.
    pub name: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DeclineQuote {
    pub name: String,
    pub reason: Option<String>,
}

/// Refuses when the booking already has an accepted quote that `q` is not a
/// revision of, so a booking never has two acceptances standing. The booking
/// is locked first, so two quotes answered at once are taken in turn.
async fn check_not_accepted(tx: &mut Tx, q: &QuoteView) -> ApiResult<()> {
    core_db::bookings::lock(tx, q.booking_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let accepted = core_db::quotes::accepted_for_booking(tx, q.booking_id).await?;
    if accepted.iter().any(|a| Some(*a) != q.supersedes_id) {
        return Err(ApiError::conflict(
            "quote_already_accepted",
            "another quote for this booking has already been accepted",
        ));
    }
    Ok(())
}

async fn decidable(tx: &mut Tx, ctx: &Ctx, id: Uuid) -> ApiResult<QuoteView> {
    core_db::quotes::lock(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let q = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    if !q.status.is_open() {
        return Err(ApiError::conflict(
            "quote_closed",
            match q.status {
                QuoteStatus::Accepted => "this quote has already been accepted",
                QuoteStatus::Declined => "this quote has already been declined",
                QuoteStatus::Withdrawn => "this quote was replaced or withdrawn",
                QuoteStatus::Expired => "this quote has expired",
                _ => "this quote cannot be answered",
            },
        ));
    }
    if let Some(v) = q.valid_until
        && v < today(tx, ctx).await?
    {
        return Err(ApiError::conflict("quote_closed", "this quote has expired"));
    }
    check_not_accepted(tx, &q).await?;
    Ok(q)
}

/// The buyer accepts: the quote is frozen as accepted, the hold becomes
/// pending, and the enquiry is won.
pub async fn accept(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
    req: AcceptQuote,
) -> ApiResult<PublicQuote> {
    let mut check = Check::default();
    let name = check.required("name", &req.name, 200);
    let email = check.optional("email", req.email.as_deref(), 320);
    if let Some(e) = &email
        && !core_engines::fields::is_email(e)
    {
        check.issue("email", "is not an email address");
    }
    check.finish()?;
    let (mut tx, ctx, id) = open_public(state, token, meta).await?;
    let before = decidable(&mut tx, &ctx, id).await?;
    let booking = super::bookings::load(&mut tx, before.booking_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let snapshot = json!({
        "quote": to_json(&before),
        "booking": booking_summary(&booking),
        "accepted_by": { "name": name, "email": email },
        "accepted_at": Utc::now(),
        "ip": ctx.meta.ip.map(|ip| ip.to_string()),
        "user_agent": ctx.meta.user_agent,
    });
    core_db::quotes::decide(
        &mut tx,
        id,
        core_db::quotes::Decision {
            status: QuoteStatus::Accepted,
            name: &name,
            email: email.as_deref(),
            ip: ctx.meta.ip,
            user_agent: ctx.meta.user_agent.as_deref().map(|u| truncate(u, 500)),
            decline_reason: None,
            snapshot: Some(&snapshot),
        },
    )
    .await
    .map_err(|e| {
        if e.is_check_violation(ACCEPTED_FROZEN) {
            ApiError::conflict("quote_closed", "this quote has already been accepted")
        } else {
            e.into()
        }
    })?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    super::record(
        &mut tx,
        &ctx,
        Change {
            topic: "quote.accepted",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: Some(json!({ "booking": booking_summary(&booking) })),
        },
    )
    .await?;
    if booking.status == BookingStatus::Hold {
        super::bookings::change_status_in_tx(
            &mut tx,
            &ctx,
            booking.id,
            super::bookings::StatusRequest {
                status: BookingStatus::Pending,
                reason: None,
                override_reason: None,
            },
            None,
        )
        .await?;
    }
    if let Some(enquiry) = booking.enquiry_id {
        super::enquiries::reach(&mut tx, &ctx, enquiry, super::enquiries::Milestone::Won).await?;
    }
    let view = public_view(&mut tx, &ctx, &after).await?;
    tx.commit().await?;
    Ok(view)
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// The buyer declines, optionally saying why.
pub async fn decline(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
    req: DeclineQuote,
) -> ApiResult<PublicQuote> {
    let mut check = Check::default();
    let name = check.required("name", &req.name, 200);
    let reason = check.long("reason", req.reason.as_deref(), 2000);
    check.finish()?;
    let (mut tx, ctx, id) = open_public(state, token, meta).await?;
    let before = decidable(&mut tx, &ctx, id).await?;
    core_db::quotes::decide(
        &mut tx,
        id,
        core_db::quotes::Decision {
            status: QuoteStatus::Declined,
            name: &name,
            email: None,
            ip: ctx.meta.ip,
            user_agent: ctx.meta.user_agent.as_deref().map(|u| truncate(u, 500)),
            decline_reason: reason.as_deref(),
            snapshot: None,
        },
    )
    .await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    super::record(
        &mut tx,
        &ctx,
        Change {
            topic: "quote.declined",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    let view = public_view(&mut tx, &ctx, &after).await?;
    tx.commit().await?;
    Ok(view)
}
