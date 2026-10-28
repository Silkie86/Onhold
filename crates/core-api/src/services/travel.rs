//! Travel on a booking and whether the booking pays.
//!
//! Travel items are flights, trains, ground transport and hotels, each with
//! its own times in its own place's zone, so a 07:10 departure from London
//! and a 10:25 arrival in Barcelona read as the tickets do. Money lines are
//! income beside the fee (a hotel or catering buyout) and costs (per diems,
//! crew, production). Together with the fee and the talent's commission
//! they give the booking's money check: what is left once everything is
//! paid, with a warning when the costs eat the fee.

use chrono::{DateTime, NaiveDate, Utc};
use core_db::Tx;
pub use core_db::travel::Owner;
use core_db::travel::{LineValues, MoneyLine, TravelItem, TravelValues};
use core_domain::touring::{self, COST_CATEGORIES, INCOME_CATEGORIES, TRAVEL_KINDS, Warning};
use core_domain::{Ctx, Money, Permission, UserId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use super::bookings::BookingView;
use super::crm::Check;
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const TRAVEL: &str = "travel";
pub const MONEY_LINE: &str = "money_line";
const MAX_TRAVELLERS: usize = 40;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TravelView {
    pub id: Uuid,
    /// The booking it is for, or
    pub booking_id: Option<Uuid>,
    /// the project it belongs to (travel on a day between dates).
    pub project_id: Option<Uuid>,
    /// `flight`, `train`, `bus`, `car`, `ferry`, `taxi`, `hotel` or `other`.
    pub kind: String,
    /// `planned`, `booked` or `cancelled`.
    pub status: String,
    /// The airline, rail company, driver or hotel.
    pub carrier: Option<String>,
    /// A flight or train number, e.g. `BA 1442`.
    pub number: Option<String>,
    /// The booking reference or confirmation code.
    pub reference: Option<String>,
    pub from_place: Option<String>,
    /// IATA airport code.
    pub from_code: Option<String>,
    /// Where to; for a hotel, the hotel's address.
    pub to_place: Option<String>,
    pub to_code: Option<String>,
    /// Departure, or check-in for a hotel.
    pub starts_at: DateTime<Utc>,
    /// The IANA zone of the departure.
    pub start_tz: String,
    /// Arrival, or check-out for a hotel.
    pub ends_at: Option<DateTime<Utc>>,
    pub end_tz: Option<String>,
    pub travellers: Vec<String>,
    pub notes: Option<String>,
    pub cost: Option<Money>,
    /// `us` (a cost against the fee) or `buyer` (the client provides it).
    pub paid_by: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

impl From<TravelItem> for TravelView {
    fn from(t: TravelItem) -> Self {
        Self {
            id: t.id,
            booking_id: t.booking_id,
            project_id: t.project_id,
            kind: t.kind,
            status: t.status,
            carrier: t.carrier,
            number: t.number,
            reference: t.reference,
            from_place: t.from_place,
            from_code: t.from_code,
            to_place: t.to_place,
            to_code: t.to_code,
            starts_at: t.starts_at,
            start_tz: t.start_tz,
            ends_at: t.ends_at,
            end_tz: t.end_tz,
            travellers: t.travellers,
            notes: t.notes,
            cost: match (t.cost_minor, t.cost_currency) {
                (Some(minor), Some(currency)) => Some(Money {
                    minor,
                    currency: currency.trim().to_owned(),
                }),
                _ => None,
            },
            paid_by: t.paid_by,
            version: t.version,
            created_at: t.created_at,
            updated_at: t.updated_at,
            created_by: t.created_by,
            updated_by: t.updated_by,
        }
    }
}

impl TravelView {
    /// What this item costs the booking, if anything.
    pub(crate) fn our_cost(&self) -> Option<&Money> {
        (self.paid_by == "us" && self.status != "cancelled")
            .then_some(self.cost.as_ref())
            .flatten()
    }
}

pub fn travel_json(v: &TravelView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// A travel item, as given when adding or changing one. Changing replaces
/// every field.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TravelInput {
    pub kind: String,
    /// Default `planned`.
    pub status: Option<String>,
    pub carrier: Option<String>,
    pub number: Option<String>,
    pub reference: Option<String>,
    pub from_place: Option<String>,
    /// An IATA airport code; any case.
    pub from_code: Option<String>,
    pub to_place: Option<String>,
    pub to_code: Option<String>,
    pub starts_at: DateTime<Utc>,
    /// Default: the booking's zone.
    pub start_tz: Option<String>,
    pub ends_at: Option<DateTime<Utc>>,
    /// Default: the departure's zone.
    pub end_tz: Option<String>,
    #[serde(default)]
    pub travellers: Vec<String>,
    pub notes: Option<String>,
    pub cost: Option<Money>,
    /// Default `us`.
    pub paid_by: Option<String>,
}

struct CheckedTravel {
    kind: String,
    status: String,
    carrier: Option<String>,
    number: Option<String>,
    reference: Option<String>,
    from_place: Option<String>,
    from_code: Option<String>,
    to_place: Option<String>,
    to_code: Option<String>,
    starts_at: DateTime<Utc>,
    start_tz: String,
    ends_at: Option<DateTime<Utc>>,
    end_tz: Option<String>,
    travellers: Vec<String>,
    notes: Option<String>,
    cost: Option<Money>,
    paid_by: String,
}

impl CheckedTravel {
    fn values(&self) -> TravelValues<'_> {
        TravelValues {
            kind: &self.kind,
            status: &self.status,
            carrier: self.carrier.as_deref(),
            number: self.number.as_deref(),
            reference: self.reference.as_deref(),
            from_place: self.from_place.as_deref(),
            from_code: self.from_code.as_deref(),
            to_place: self.to_place.as_deref(),
            to_code: self.to_code.as_deref(),
            starts_at: self.starts_at,
            start_tz: &self.start_tz,
            ends_at: self.ends_at,
            end_tz: self.end_tz.as_deref(),
            travellers: &self.travellers,
            notes: self.notes.as_deref(),
            cost_minor: self.cost.as_ref().map(|m| m.minor),
            cost_currency: self.cost.as_ref().map(|m| m.currency.as_str()),
            paid_by: &self.paid_by,
        }
    }
}

fn check_zone(check: &mut Check, field: &str, tz: &str) {
    if core_domain::workspace::validate_timezone(tz).is_err() {
        check.issue(field, format!("`{tz}` is not an IANA time zone"));
    }
}

fn check_code(check: &mut Check, field: &str, raw: Option<&str>) -> Option<String> {
    let raw = raw.map(str::trim).filter(|s| !s.is_empty())?;
    let code = touring::airport_code(raw);
    if code.is_none() {
        check.issue(field, "must be a three-letter airport code, e.g. LHR");
    }
    code
}

fn check_travel(input: TravelInput, booking_tz: &str) -> ApiResult<CheckedTravel> {
    let mut check = Check::default();
    let kind = input.kind.trim().to_owned();
    if !TRAVEL_KINDS.contains(&kind.as_str()) {
        check.issue("kind", format!("use one of: {}", TRAVEL_KINDS.join(", ")));
    }
    let status = input
        .status
        .as_deref()
        .map(str::trim)
        .unwrap_or("planned")
        .to_owned();
    if !["planned", "booked", "cancelled"].contains(&status.as_str()) {
        check.issue("status", "use planned, booked or cancelled");
    }
    let paid_by = input
        .paid_by
        .as_deref()
        .map(str::trim)
        .unwrap_or("us")
        .to_owned();
    if !["us", "buyer"].contains(&paid_by.as_str()) {
        check.issue("paid_by", "use us or buyer");
    }
    let carrier = check.optional("carrier", input.carrier.as_deref(), 100);
    // Only flight numbers follow the airline-code pattern; a bus "X90" stays as typed.
    let number = check
        .optional("number", input.number.as_deref(), 20)
        .map(|n| {
            if kind == "flight" {
                touring::transport_number(&n)
            } else {
                n
            }
        });
    let reference = check.optional("reference", input.reference.as_deref(), 50);
    let from_place = check.optional("from_place", input.from_place.as_deref(), 200);
    let to_place = check.optional("to_place", input.to_place.as_deref(), 200);
    let from_code = check_code(&mut check, "from_code", input.from_code.as_deref());
    let to_code = check_code(&mut check, "to_code", input.to_code.as_deref());
    let start_tz = input
        .start_tz
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(booking_tz)
        .to_owned();
    check_zone(&mut check, "start_tz", &start_tz);
    let end_tz = input.ends_at.map(|_| {
        input
            .end_tz
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&start_tz)
            .to_owned()
    });
    if let Some(tz) = &end_tz {
        check_zone(&mut check, "end_tz", tz);
    }
    if let Some(end) = input.ends_at {
        if end < input.starts_at {
            check.issue("ends_at", "must not be before the start");
        } else if (end - input.starts_at).num_days() > 60 {
            check.issue("ends_at", "must be within 60 days of the start");
        }
    }
    let mut travellers: Vec<String> = Vec::new();
    if input.travellers.len() > MAX_TRAVELLERS * 4 {
        check.issue(
            "travellers",
            format!("list at most {MAX_TRAVELLERS} travellers"),
        );
    }
    for t in input.travellers.iter().take(MAX_TRAVELLERS * 4) {
        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.is_empty() || travellers.contains(&t) {
            continue;
        }
        if t.chars().count() > 100 {
            check.issue("travellers", "each name must be at most 100 characters");
        }
        travellers.push(t);
    }
    if travellers.len() > MAX_TRAVELLERS {
        check.issue(
            "travellers",
            format!("list at most {MAX_TRAVELLERS} travellers"),
        );
    }
    let notes = check.long("notes", input.notes.as_deref(), 2000);
    let cost = check.money("cost", input.cost.as_ref());
    check.finish()?;
    Ok(CheckedTravel {
        kind,
        status,
        carrier,
        number,
        reference,
        from_place,
        from_code,
        to_place,
        to_code,
        starts_at: input.starts_at,
        start_tz,
        ends_at: input.ends_at,
        end_tz,
        travellers,
        notes,
        cost,
        paid_by,
    })
}

async fn booking(tx: &mut Tx, id: Uuid) -> ApiResult<BookingView> {
    super::bookings::load(tx, id)
        .await?
        .ok_or(ApiError::NotFound)
}

/// The travel of a booking, in time order.
pub(crate) async fn travel_of(tx: &mut Tx, booking_id: Uuid) -> ApiResult<Vec<TravelView>> {
    Ok(core_db::travel::list(tx, &[booking_id], None)
        .await?
        .into_iter()
        .map(TravelView::from)
        .collect())
}

pub async fn list_travel(
    state: &AppState,
    ctx: &Ctx,
    booking_id: Uuid,
) -> ApiResult<Vec<TravelView>> {
    ctx.authorize(Permission::BookingRead)?;
    let mut tx = begin(state, ctx).await?;
    booking(&mut tx, booking_id).await?;
    let items = travel_of(&mut tx, booking_id).await?;
    tx.commit().await?;
    Ok(items)
}

/// Who may change an item, and the zone its times default to.
async fn owner_rules(tx: &mut Tx, ctx: &Ctx, owner: Owner) -> ApiResult<(Permission, String)> {
    match owner {
        Owner::Booking(id) => Ok((Permission::BookingWrite, booking(tx, id).await?.timezone)),
        Owner::Project(id) => {
            core_db::projects::get(tx, id)
                .await?
                .ok_or(ApiError::NotFound)?;
            let tz = core_db::workspaces::get(tx, ctx.workspace_id)
                .await?
                .map_or_else(|| "UTC".to_owned(), |w| w.default_timezone);
            Ok((Permission::ProjectWrite, tz))
        }
    }
}

fn owner_of(booking_id: Option<Uuid>, project_id: Option<Uuid>) -> ApiResult<Owner> {
    match (booking_id, project_id) {
        (Some(b), _) => Ok(Owner::Booking(b)),
        (None, Some(p)) => Ok(Owner::Project(p)),
        (None, None) => Err(ApiError::NotFound),
    }
}

/// Adds travel to a booking, or to a project for a day between its dates.
pub async fn create_travel(
    state: &AppState,
    ctx: &Ctx,
    owner: Owner,
    input: TravelInput,
) -> ApiResult<TravelView> {
    ctx.authorize(match owner {
        Owner::Booking(_) => Permission::BookingWrite,
        Owner::Project(_) => Permission::ProjectWrite,
    })?;
    let mut tx = begin(state, ctx).await?;
    let (_, tz) = owner_rules(&mut tx, ctx, owner).await?;
    let c = check_travel(input, &tz)?;
    let id = core_domain::ids::new_id();
    core_db::travel::insert(&mut tx, id, owner, c.values(), ctx.user_id()).await?;
    let v: TravelView = core_db::travel::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    record(
        &mut tx,
        ctx,
        Change {
            topic: "travel.created",
            resource: TRAVEL,
            object_id: Some(id),
            before: None,
            after: Some(travel_json(&v)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(v)
}

pub async fn update_travel(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    input: TravelInput,
    if_match: Option<i64>,
) -> ApiResult<TravelView> {
    let mut tx = begin(state, ctx).await?;
    let version = core_db::travel::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let before: TravelView = core_db::travel::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    let (permission, tz) = owner_rules(
        &mut tx,
        ctx,
        owner_of(before.booking_id, before.project_id)?,
    )
    .await?;
    ctx.authorize(permission)?;
    check_version(version, if_match)?;
    let c = check_travel(input, &tz)?;
    core_db::travel::update(&mut tx, id, c.values(), ctx.user_id()).await?;
    let after: TravelView = core_db::travel::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    record(
        &mut tx,
        ctx,
        Change {
            topic: "travel.updated",
            resource: TRAVEL,
            object_id: Some(id),
            before: Some(travel_json(&before)),
            after: Some(travel_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

pub async fn delete_travel(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    let mut tx = begin(state, ctx).await?;
    let before: TravelView = core_db::travel::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    let (permission, _) = owner_rules(
        &mut tx,
        ctx,
        owner_of(before.booking_id, before.project_id)?,
    )
    .await?;
    ctx.authorize(permission)?;
    core_db::travel::soft_delete(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "travel.deleted",
            resource: TRAVEL,
            object_id: Some(id),
            before: Some(travel_json(&before)),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// Money lines -------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MoneyLineView {
    pub id: Uuid,
    pub booking_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    /// `income` or `cost`.
    pub direction: String,
    /// Income: `buyout`, `merchandise`, `other`. Cost: `travel`,
    /// `accommodation`, `ground`, `per_diem`, `crew`, `production`, `visa`,
    /// `marketing`, `other`.
    pub category: String,
    pub description: String,
    pub amount: Money,
    pub spent_on: Option<NaiveDate>,
    pub notes: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

impl From<MoneyLine> for MoneyLineView {
    fn from(l: MoneyLine) -> Self {
        Self {
            id: l.id,
            booking_id: l.booking_id,
            project_id: l.project_id,
            direction: l.direction,
            category: l.category,
            description: l.description,
            amount: Money {
                minor: l.amount_minor,
                currency: l.currency.trim().to_owned(),
            },
            spent_on: l.spent_on,
            notes: l.notes,
            version: l.version,
            created_at: l.created_at,
            updated_at: l.updated_at,
            created_by: l.created_by,
            updated_by: l.updated_by,
        }
    }
}

pub fn line_json(v: &MoneyLineView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// A money line, as given when adding or changing one. Changing replaces
/// every field.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MoneyLineInput {
    /// `income` or `cost`.
    pub direction: String,
    pub category: String,
    pub description: String,
    pub amount: Money,
    pub spent_on: Option<NaiveDate>,
    pub notes: Option<String>,
}

struct CheckedLine {
    direction: String,
    category: String,
    description: String,
    amount: Money,
    spent_on: Option<NaiveDate>,
    notes: Option<String>,
}

impl CheckedLine {
    fn values(&self) -> LineValues<'_> {
        LineValues {
            direction: &self.direction,
            category: &self.category,
            description: &self.description,
            amount_minor: self.amount.minor,
            currency: &self.amount.currency,
            spent_on: self.spent_on,
            notes: self.notes.as_deref(),
        }
    }
}

fn check_line(input: MoneyLineInput) -> ApiResult<CheckedLine> {
    let mut check = Check::default();
    let direction = input.direction.trim().to_owned();
    let category = input.category.trim().to_owned();
    match direction.as_str() {
        "income" if !INCOME_CATEGORIES.contains(&category.as_str()) => check.issue(
            "category",
            format!("use one of: {}", INCOME_CATEGORIES.join(", ")),
        ),
        "cost" if !COST_CATEGORIES.contains(&category.as_str()) => check.issue(
            "category",
            format!("use one of: {}", COST_CATEGORIES.join(", ")),
        ),
        "income" | "cost" => {}
        _ => check.issue("direction", "use income or cost"),
    }
    let description = check.required("description", &input.description, 200);
    let amount = check
        .money("amount", Some(&input.amount))
        .unwrap_or(input.amount);
    let notes = check.long("notes", input.notes.as_deref(), 2000);
    check.finish()?;
    Ok(CheckedLine {
        direction,
        category,
        description,
        amount,
        spent_on: input.spent_on,
        notes,
    })
}

/// Adds income or a cost to a booking, or to a project as a whole.
pub async fn create_line(
    state: &AppState,
    ctx: &Ctx,
    owner: Owner,
    input: MoneyLineInput,
) -> ApiResult<MoneyLineView> {
    ctx.authorize(match owner {
        Owner::Booking(_) => Permission::BookingWrite,
        Owner::Project(_) => Permission::ProjectWrite,
    })?;
    let c = check_line(input)?;
    let mut tx = begin(state, ctx).await?;
    owner_rules(&mut tx, ctx, owner).await?;
    let id = core_domain::ids::new_id();
    core_db::travel::insert_line(&mut tx, id, owner, c.values(), ctx.user_id()).await?;
    let v: MoneyLineView = core_db::travel::line(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    record(
        &mut tx,
        ctx,
        Change {
            topic: "money_line.created",
            resource: MONEY_LINE,
            object_id: Some(id),
            before: None,
            after: Some(line_json(&v)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(v)
}

pub async fn update_line(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    input: MoneyLineInput,
    if_match: Option<i64>,
) -> ApiResult<MoneyLineView> {
    let c = check_line(input)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::travel::lock_line(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let before: MoneyLineView = core_db::travel::line(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    let (permission, _) = owner_rules(
        &mut tx,
        ctx,
        owner_of(before.booking_id, before.project_id)?,
    )
    .await?;
    ctx.authorize(permission)?;
    check_version(version, if_match)?;
    core_db::travel::update_line(&mut tx, id, c.values(), ctx.user_id()).await?;
    let after: MoneyLineView = core_db::travel::line(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    record(
        &mut tx,
        ctx,
        Change {
            topic: "money_line.updated",
            resource: MONEY_LINE,
            object_id: Some(id),
            before: Some(line_json(&before)),
            after: Some(line_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

pub async fn delete_line(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    let mut tx = begin(state, ctx).await?;
    let before: MoneyLineView = core_db::travel::line(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    let (permission, _) = owner_rules(
        &mut tx,
        ctx,
        owner_of(before.booking_id, before.project_id)?,
    )
    .await?;
    ctx.authorize(permission)?;
    core_db::travel::delete_line(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "money_line.deleted",
            resource: MONEY_LINE,
            object_id: Some(id),
            before: Some(line_json(&before)),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// The money check ----------------------------------------------------------------------

/// One amount in the money check, in the check's currency.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MoneyItem {
    /// `fee`, `commission`, `travel`, or a money line's category.
    pub category: String,
    pub description: String,
    /// In the check's currency.
    pub amount: Money,
    /// The amount as entered, when it was in another currency.
    pub original: Option<Money>,
    /// The travel item or money line it comes from.
    pub source_id: Option<Uuid>,
}

/// Whether a booking pays: income less commission and costs, all in one
/// currency.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MoneyCheck {
    pub booking_id: Uuid,
    /// The fee's currency, or the workspace's when there is no fee.
    pub currency: String,
    pub income: Vec<MoneyItem>,
    pub commission: Vec<MoneyItem>,
    pub costs: Vec<MoneyItem>,
    pub total_income: Money,
    pub total_commission: Money,
    pub total_costs: Money,
    /// What is left: income less commission and costs.
    pub net: Money,
    pub warnings: Vec<Warning>,
    /// The income and cost lines themselves, to edit.
    pub lines: Vec<MoneyLineView>,
}

/// Converts `m` to `to` at the reference rate on `on`; `None` when there is
/// no rate.
pub(crate) async fn convert(
    tx: &mut Tx,
    m: &Money,
    to: &str,
    on: NaiveDate,
) -> ApiResult<Option<i64>> {
    if m.currency == to {
        return Ok(Some(m.minor));
    }
    let Some(q) = super::finance::reference_rate(tx, &m.currency, to, on).await? else {
        return Ok(None);
    };
    Ok(core_domain::finance::convert(
        m.minor,
        &m.currency,
        to,
        q.rate,
    ))
}

/// Adds `items` into `out` in `currency`, noting the currencies that could
/// not be converted.
async fn gather(
    tx: &mut Tx,
    currency: &str,
    on: NaiveDate,
    items: Vec<(String, String, Money, Option<Uuid>)>,
    out: &mut Vec<MoneyItem>,
    unconverted: &mut Vec<String>,
) -> ApiResult<()> {
    for (category, description, original, source_id) in items {
        match convert(tx, &original, currency, on).await? {
            Some(minor) => out.push(MoneyItem {
                category,
                description,
                amount: Money {
                    minor,
                    currency: currency.to_owned(),
                },
                original: (original.currency != currency).then_some(original),
                source_id,
            }),
            None => {
                if !unconverted.contains(&original.currency) {
                    unconverted.push(original.currency);
                }
            }
        }
    }
    Ok(())
}

fn sum(items: &[MoneyItem]) -> ApiResult<i64> {
    items
        .iter()
        .try_fold(0i64, |acc, i| acc.checked_add(i.amount.minor))
        .ok_or_else(|| ApiError::invalid("amount", "is too large"))
}

/// "Flight · BA 1442": the kind, then the carrier and number.
pub(crate) fn travel_title(t: &TravelView) -> String {
    let mut c = t.kind.chars();
    let mut label = c
        .next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default();
    let what = [t.carrier.as_deref(), t.number.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    if !what.is_empty() && t.kind != "hotel" {
        label.push_str(" · ");
        label.push_str(&what);
    }
    label
}

/// The title plus the route, for a line in the money check.
fn travel_label(t: &TravelView) -> String {
    let mut label = travel_title(t);
    if t.kind == "hotel" {
        if let Some(name) = &t.carrier {
            label.push_str(" · ");
            label.push_str(name);
        }
        return label;
    }
    match (
        t.from_code.as_ref().or(t.from_place.as_ref()),
        t.to_code.as_ref().or(t.to_place.as_ref()),
    ) {
        (Some(a), Some(b)) => label.push_str(&format!(" · {a} to {b}")),
        (Some(a), None) | (None, Some(a)) => label.push_str(&format!(" · {a}")),
        (None, None) => {}
    }
    label
}

/// The money check of a loaded booking, inside the caller's transaction.
pub(crate) async fn check_booking(
    tx: &mut Tx,
    ctx: &Ctx,
    b: &BookingView,
) -> ApiResult<MoneyCheck> {
    // The whole fee counts even when no talent is on the booking to share it.
    let (fee_currency, shares, fee_total) = super::settlements::shares(tx, b).await?;
    let currency = if fee_currency.is_empty() {
        super::finance::base_currency(tx, ctx).await?
    } else {
        fee_currency.clone()
    };
    let tz: chrono_tz::Tz = b.timezone.parse().unwrap_or(chrono_tz::UTC);
    // Rates for a date still to come are not known yet: today's stand in.
    let on = b
        .starts_at
        .with_timezone(&tz)
        .date_naive()
        .min(super::finance::today(tx, ctx).await?);
    let lines: Vec<MoneyLineView> = core_db::travel::lines(tx, &[b.id], None)
        .await?
        .into_iter()
        .map(MoneyLineView::from)
        .collect();
    let travel = travel_of(tx, b.id).await?;
    let mut unconverted = Vec::new();

    let mut income = Vec::new();
    let mut commission = Vec::new();
    if !fee_currency.is_empty() {
        income.push(MoneyItem {
            category: "fee".into(),
            description: "Fee".into(),
            amount: Money {
                minor: fee_total,
                currency: currency.clone(),
            },
            original: None,
            source_id: None,
        });
        // Commission is taken from each talent's share of the fee, at their
        // rate; buyouts and other income are left out of it.
        for (talent_id, gross) in &shares {
            // Talent deleted since the booking was made still take their commission.
            let Some(t) = core_db::talent::commission(tx, *talent_id).await? else {
                continue;
            };
            let bps = t.commission_bps.unwrap_or(0).clamp(0, 10_000);
            let minor = core_domain::finance::percent_of(*gross, i64::from(bps))
                .ok_or_else(|| ApiError::invalid("fee", "is too large"))?;
            if minor > 0 {
                commission.push(MoneyItem {
                    category: "commission".into(),
                    description: format!("Commission, {} at {}%", t.name, f64::from(bps) / 100.0),
                    amount: Money {
                        minor,
                        currency: currency.clone(),
                    },
                    original: None,
                    source_id: None,
                });
            }
        }
    }
    let extra_income = lines
        .iter()
        .filter(|l| l.direction == "income")
        .map(|l| {
            (
                l.category.clone(),
                l.description.clone(),
                l.amount.clone(),
                Some(l.id),
            )
        })
        .collect();
    gather(
        tx,
        &currency,
        on,
        extra_income,
        &mut income,
        &mut unconverted,
    )
    .await?;

    let mut costs = Vec::new();
    let travel_costs = travel
        .iter()
        .filter_map(|t| {
            t.our_cost()
                .map(|c| ("travel".to_owned(), travel_label(t), c.clone(), Some(t.id)))
        })
        .collect();
    gather(
        tx,
        &currency,
        on,
        travel_costs,
        &mut costs,
        &mut unconverted,
    )
    .await?;
    let line_costs = lines
        .iter()
        .filter(|l| l.direction == "cost")
        .map(|l| {
            (
                l.category.clone(),
                l.description.clone(),
                l.amount.clone(),
                Some(l.id),
            )
        })
        .collect();
    gather(tx, &currency, on, line_costs, &mut costs, &mut unconverted).await?;

    let (total_income, total_commission, total_costs) =
        (sum(&income)?, sum(&commission)?, sum(&costs)?);
    let (net, mut warnings) = touring::assess(total_income, total_commission, total_costs)
        .ok_or_else(|| ApiError::invalid("amount", "is too large"))?;
    if fee_currency.is_empty() {
        warnings.push(Warning::new(
            touring::WarningLevel::Caution,
            "unpriced",
            "There is no fee or accepted quote yet.",
        ));
    }
    if !unconverted.is_empty() {
        warnings.push(Warning::new(
            touring::WarningLevel::Caution,
            "unconverted",
            format!(
                "Amounts in {} are left out: there is no exchange rate to {currency}.",
                unconverted.join(", ")
            ),
        ));
    }
    let money = |minor| Money {
        minor,
        currency: currency.clone(),
    };
    Ok(MoneyCheck {
        booking_id: b.id,
        total_income: money(total_income),
        total_commission: money(total_commission),
        total_costs: money(total_costs),
        net: money(net),
        currency: currency.clone(),
        income,
        commission,
        costs,
        warnings,
        lines,
    })
}

pub async fn money_check(state: &AppState, ctx: &Ctx, booking_id: Uuid) -> ApiResult<MoneyCheck> {
    ctx.authorize(Permission::BookingRead)?;
    let mut tx = begin(state, ctx).await?;
    let b = booking(&mut tx, booking_id).await?;
    let v = check_booking(&mut tx, ctx, &b).await?;
    tx.commit().await?;
    Ok(v)
}
