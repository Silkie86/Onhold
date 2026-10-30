//! Projects: a run of dates looked at as one thing, with a budget, crew,
//! costs that belong to no one date, and milestone tasks. A tour is the
//! common case; a theatre season, a speaking circuit, a travelling
//! exhibition or a shoot schedule work the same way.
//!
//! The dates are ordinary bookings, so holds, quotes, contracts, invoices
//! and clash checks are untouched. What a project adds is the view across
//! them: whether each date pays and whether the whole run does once shared
//! costs are counted, the budget against what was spent, and a day-by-day
//! itinerary (shows, travel days, days off, where everyone sleeps) that day
//! sheets and the talent's phone view read from.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use core_db::Tx;
use core_db::projects::{CrewMember, CrewValues, Project};
use core_domain::touring::{self, COST_CATEGORIES, Warning, WarningLevel};
use core_domain::{BookingStatus, Ctx, Money, Permission, UserId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::bookings::{BookingView, ScheduleItem};
use super::crm::Check;
use super::travel::{MoneyCheck, MoneyLineView, TravelView};
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::routes::nullable;

pub const ENTITY: &str = "project";
pub const KINDS: &[&str] = &[
    "tour",
    "season",
    "circuit",
    "exhibition",
    "production",
    "campaign",
    "other",
];
pub const STATUSES: &[&str] = &["planning", "confirmed", "under_way", "done", "cancelled"];
/// The most dates one project may hold, and the longest itinerary shown.
pub const MAX_DATES: usize = 400;
const MAX_DAYS: i64 = 400;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectView {
    pub id: Uuid,
    /// `tour`, `season`, `circuit`, `exhibition`, `production`, `campaign` or `other`.
    pub kind: String,
    pub name: String,
    /// `planning`, `confirmed`, `under_way`, `done` or `cancelled`.
    pub status: String,
    /// What the project's sums are shown in.
    pub currency: String,
    pub owner_id: Option<Uuid>,
    /// Expected costs per cost category, in minor units of `currency`.
    pub budget: BTreeMap<String, i64>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    /// The first and last date's start.
    pub first_at: Option<DateTime<Utc>>,
    pub last_at: Option<DateTime<Utc>>,
    /// How many dates.
    pub dates: i64,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

impl From<Project> for ProjectView {
    fn from(p: Project) -> Self {
        Self {
            id: p.id,
            kind: p.kind,
            name: p.name,
            status: p.status,
            currency: p.currency.trim().to_owned(),
            owner_id: p.owner_id,
            budget: serde_json::from_value(p.budget).unwrap_or_default(),
            notes: p.notes,
            tags: p.tags,
            first_at: p.first_at,
            last_at: p.last_at,
            dates: p.dates,
            version: p.version,
            created_at: p.created_at,
            updated_at: p.updated_at,
            created_by: p.created_by,
            updated_by: p.updated_by,
        }
    }
}

pub fn to_json(v: &ProjectView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<ProjectView> {
    Ok(core_db::projects::get(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into())
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectPage {
    pub items: Vec<ProjectView>,
    pub total: i64,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct ProjectFilter {
    pub status: Option<String>,
    pub offset: Option<i64>,
    /// 1 to 200, default 50.
    pub limit: Option<i64>,
}

pub async fn list(state: &AppState, ctx: &Ctx, f: &ProjectFilter) -> ApiResult<ProjectPage> {
    ctx.authorize(Permission::ProjectRead)?;
    let status = f.status.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(s) = status
        && !STATUSES.contains(&s)
    {
        return Err(ApiError::invalid(
            "status",
            format!("use one of: {}", STATUSES.join(", ")),
        ));
    }
    let mut tx = begin(state, ctx).await?;
    let (rows, total) = core_db::projects::list(
        &mut tx,
        status,
        f.offset.unwrap_or(0).max(0),
        f.limit.unwrap_or(50).clamp(1, 200),
    )
    .await?;
    tx.commit().await?;
    Ok(ProjectPage {
        items: rows.into_iter().map(ProjectView::from).collect(),
        total,
    })
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<ProjectView> {
    ctx.authorize(Permission::ProjectRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NewProject {
    pub name: String,
    /// Default `tour`.
    pub kind: Option<String>,
    /// Default `planning`.
    pub status: Option<String>,
    /// Default: the workspace's currency.
    pub currency: Option<String>,
    /// Default: the member creating it.
    pub owner_id: Option<Uuid>,
    #[serde(default)]
    pub budget: BTreeMap<String, i64>,
    pub notes: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Bookings that are its dates from the start.
    #[serde(default)]
    pub booking_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ProjectPatch {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub currency: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub owner_id: Option<Option<Uuid>>,
    /// Replaces the whole budget.
    pub budget: Option<BTreeMap<String, i64>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub notes: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
}

fn check_choice(check: &mut Check, field: &str, value: &str, allowed: &[&str]) {
    if !allowed.contains(&value) {
        check.issue(field, format!("use one of: {}", allowed.join(", ")));
    }
}

fn check_budget(check: &mut Check, budget: &BTreeMap<String, i64>) -> Value {
    let mut out = Map::new();
    for (k, v) in budget {
        if !COST_CATEGORIES.contains(&k.as_str()) {
            check.issue(
                "budget",
                format!(
                    "`{k}` is not a cost category; use: {}",
                    COST_CATEGORIES.join(", ")
                ),
            );
        } else if *v < 0 {
            check.issue("budget", format!("`{k}` must not be negative"));
        } else if *v > 0 {
            out.insert(k.clone(), json!(v));
        }
    }
    Value::Object(out)
}

async fn check_owner(tx: &mut Tx, check: &mut Check, id: Option<Uuid>) -> ApiResult<()> {
    if let Some(id) = id
        && core_db::members::get(tx, UserId(id)).await?.is_none()
    {
        check.issue("owner_id", "must be a member of the workspace");
    }
    Ok(())
}

pub async fn create(state: &AppState, ctx: &Ctx, new: NewProject) -> ApiResult<ProjectView> {
    ctx.authorize(Permission::ProjectWrite)?;
    if !new.booking_ids.is_empty() {
        ctx.authorize(Permission::BookingWrite)?;
    }
    let mut tx = begin(state, ctx).await?;
    let mut check = Check::default();
    let name = check.required("name", &new.name, 200);
    let kind = new.kind.as_deref().map_or("tour", str::trim).to_owned();
    check_choice(&mut check, "kind", &kind, KINDS);
    let status = new
        .status
        .as_deref()
        .map_or("planning", str::trim)
        .to_owned();
    check_choice(&mut check, "status", &status, STATUSES);
    let currency = match check.currency("currency", new.currency.as_deref()) {
        Some(c) => c,
        None => super::finance::base_currency(&mut tx, ctx).await?,
    };
    let owner_id = new.owner_id.or(ctx.user_id().map(|u| u.0));
    check_owner(&mut tx, &mut check, owner_id).await?;
    let budget = check_budget(&mut check, &new.budget);
    let notes = check.long("notes", new.notes.as_deref(), 20000);
    let tags = check.tags(&new.tags);
    if new.booking_ids.len() > MAX_DATES {
        check.issue("booking_ids", format!("give at most {MAX_DATES} dates"));
    }
    check.finish()?;
    let id = core_domain::ids::new_id();
    core_db::projects::insert(
        &mut tx,
        id,
        core_db::projects::Values {
            kind: &kind,
            name: &name,
            status: &status,
            currency: &currency,
            owner_id,
            budget: &budget,
            notes: notes.as_deref(),
            tags: &tags,
        },
        ctx.user_id(),
    )
    .await?;
    for b in &new.booking_ids {
        attach_in_tx(&mut tx, id, *b).await?;
    }
    let v = load(&mut tx, id).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "project.created",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(to_json(&v)),
            event_extra: Some(json!({ "booking_ids": new.booking_ids })),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(v)
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    p: ProjectPatch,
    if_match: Option<i64>,
) -> ApiResult<ProjectView> {
    ctx.authorize(Permission::ProjectWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::projects::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let before = load(&mut tx, id).await?;
    let mut check = Check::default();
    let name = match &p.name {
        Some(n) => check.required("name", n, 200),
        None => before.name.clone(),
    };
    let kind = p
        .kind
        .as_deref()
        .map_or(before.kind.clone(), |k| k.trim().to_owned());
    check_choice(&mut check, "kind", &kind, KINDS);
    let status = p
        .status
        .as_deref()
        .map_or(before.status.clone(), |s| s.trim().to_owned());
    check_choice(&mut check, "status", &status, STATUSES);
    let currency = check
        .currency("currency", p.currency.as_deref())
        .unwrap_or_else(|| before.currency.clone());
    let owner_id = p.owner_id.unwrap_or(before.owner_id);
    if p.owner_id.is_some() {
        check_owner(&mut tx, &mut check, owner_id).await?;
    }
    // Budget amounts are in the project's currency: a new currency needs them again.
    if currency != before.currency && p.budget.is_none() && !before.budget.is_empty() {
        check.issue(
            "budget",
            "give the budget again in the new currency, or clear it",
        );
    }
    let budget = check_budget(&mut check, p.budget.as_ref().unwrap_or(&before.budget));
    let notes = match &p.notes {
        Some(n) => check.long("notes", n.as_deref(), 20000),
        None => before.notes.clone(),
    };
    let tags = match &p.tags {
        Some(t) => check.tags(t),
        None => before.tags.clone(),
    };
    check.finish()?;
    core_db::projects::update(
        &mut tx,
        id,
        core_db::projects::Values {
            kind: &kind,
            name: &name,
            status: &status,
            currency: &currency,
            owner_id,
            budget: &budget,
            notes: notes.as_deref(),
            tags: &tags,
        },
        ctx.user_id(),
    )
    .await?;
    let after = load(&mut tx, id).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "project.updated",
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

/// Deletes a project. Its dates stay, as bookings of their own.
pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::ProjectWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = load(&mut tx, id).await?;
    core_db::projects::soft_delete(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "project.deleted",
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

// Dates ------------------------------------------------------------------------------

async fn attach_in_tx(tx: &mut Tx, project_id: Uuid, booking_id: Uuid) -> ApiResult<()> {
    if super::bookings::load(tx, booking_id).await?.is_none() {
        return Err(ApiError::invalid("booking_id", "there is no such booking"));
    }
    if core_db::projects::booking_ids(tx, project_id).await?.len() >= MAX_DATES {
        return Err(ApiError::invalid(
            "booking_id",
            format!("a project holds at most {MAX_DATES} dates"),
        ));
    }
    core_db::projects::attach(tx, project_id, booking_id).await?;
    Ok(())
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DateLink {
    pub booking_id: Uuid,
}

/// Makes a booking one of the project's dates (taking it out of any other
/// project).
pub async fn add_date(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    link: DateLink,
) -> ApiResult<ProjectView> {
    ctx.authorize(Permission::ProjectWrite)?;
    ctx.authorize(Permission::BookingWrite)?;
    let mut tx = begin(state, ctx).await?;
    load(&mut tx, id).await?;
    let previous = core_db::projects::of_booking(&mut tx, link.booking_id).await?;
    attach_in_tx(&mut tx, id, link.booking_id).await?;
    let v = load(&mut tx, id).await?;
    if previous != Some(id) {
        record(
            &mut tx,
            ctx,
            Change {
                topic: "project.date_added",
                resource: ENTITY,
                object_id: Some(id),
                before: None,
                after: None,
                event_extra: Some(json!({ "booking_id": link.booking_id, "moved_from": previous })),
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(v)
}

pub async fn remove_date(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    booking_id: Uuid,
) -> ApiResult<ProjectView> {
    ctx.authorize(Permission::ProjectWrite)?;
    let mut tx = begin(state, ctx).await?;
    load(&mut tx, id).await?;
    if !core_db::projects::detach(&mut tx, id, booking_id).await? {
        return Err(ApiError::NotFound);
    }
    let v = load(&mut tx, id).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "project.date_removed",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: None,
            event_extra: Some(json!({ "booking_id": booking_id })),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(v)
}

// Crew -------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CrewView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub role: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub notes: Option<String>,
    pub version: i64,
}

impl From<CrewMember> for CrewView {
    fn from(c: CrewMember) -> Self {
        Self {
            id: c.id,
            project_id: c.project_id,
            name: c.name,
            role: c.role,
            phone: c.phone,
            email: c.email,
            notes: c.notes,
            version: c.version,
        }
    }
}

/// A crew member, as given when adding or changing one; changing replaces
/// every field.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CrewInput {
    pub name: String,
    pub role: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub notes: Option<String>,
}

struct CheckedCrew {
    name: String,
    role: Option<String>,
    phone: Option<String>,
    email: Option<String>,
    notes: Option<String>,
}

impl CheckedCrew {
    fn values(&self) -> CrewValues<'_> {
        CrewValues {
            name: &self.name,
            role: self.role.as_deref(),
            phone: self.phone.as_deref(),
            email: self.email.as_deref(),
            notes: self.notes.as_deref(),
        }
    }
}

fn check_crew(input: &CrewInput) -> ApiResult<CheckedCrew> {
    let mut check = Check::default();
    let c = CheckedCrew {
        name: check.required("name", &input.name, 100),
        role: check.optional("role", input.role.as_deref(), 100),
        phone: check.optional("phone", input.phone.as_deref(), 50),
        email: check.optional("email", input.email.as_deref(), 320),
        notes: check.long("notes", input.notes.as_deref(), 2000),
    };
    if let Some(e) = &c.email
        && !e.contains('@')
    {
        check.issue("email", "is not an email address");
    }
    check.finish()?;
    Ok(c)
}

const CREW: &str = "project_crew";

pub async fn add_crew(
    state: &AppState,
    ctx: &Ctx,
    project_id: Uuid,
    input: CrewInput,
) -> ApiResult<CrewView> {
    ctx.authorize(Permission::ProjectWrite)?;
    let c = check_crew(&input)?;
    let mut tx = begin(state, ctx).await?;
    load(&mut tx, project_id).await?;
    let id = core_domain::ids::new_id();
    core_db::projects::insert_crew(&mut tx, id, project_id, c.values(), ctx.user_id()).await?;
    let v: CrewView = core_db::projects::crew_member(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    record(
        &mut tx,
        ctx,
        Change {
            topic: "project.crew_added",
            resource: CREW,
            object_id: Some(id),
            before: None,
            after: Some(serde_json::to_value(&v).unwrap_or(Value::Null)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(v)
}

pub async fn update_crew(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    input: CrewInput,
) -> ApiResult<CrewView> {
    ctx.authorize(Permission::ProjectWrite)?;
    let c = check_crew(&input)?;
    let mut tx = begin(state, ctx).await?;
    let before: CrewView = core_db::projects::crew_member(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    core_db::projects::update_crew(&mut tx, id, c.values(), ctx.user_id()).await?;
    let after: CrewView = core_db::projects::crew_member(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    record(
        &mut tx,
        ctx,
        Change {
            topic: "project.crew_updated",
            resource: CREW,
            object_id: Some(id),
            before: Some(serde_json::to_value(&before).unwrap_or(Value::Null)),
            after: Some(serde_json::to_value(&after).unwrap_or(Value::Null)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

pub async fn remove_crew(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::ProjectWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before: CrewView = core_db::projects::crew_member(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    core_db::projects::delete_crew(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "project.crew_removed",
            resource: CREW,
            object_id: Some(id),
            before: Some(serde_json::to_value(&before).unwrap_or(Value::Null)),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// The overview: every date, and whether the whole thing pays -------------------------------

/// A date in the project, as the overview lists it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DateSummary {
    pub booking_id: Uuid,
    pub title: String,
    pub status: BookingStatus,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub timezone: String,
    pub venue: Option<String>,
    pub talent: Vec<String>,
    /// The date's own money check, in its fee's currency.
    pub income: Money,
    pub commission: Money,
    pub costs: Money,
    pub net: Money,
    /// The net in the project's currency; `None` without an exchange rate.
    pub net_in_currency: Option<Money>,
    pub warnings: Vec<Warning>,
}

/// One cost category: what was planned and what is booked or spent.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BudgetLine {
    pub category: String,
    pub budget: Money,
    pub actual: Money,
    /// Budget less actual; negative when over.
    pub remaining: Money,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectOverview {
    pub project: ProjectView,
    pub dates: Vec<DateSummary>,
    /// Across every date plus the project's own income and costs, in the
    /// project's currency.
    pub total_income: Money,
    pub total_commission: Money,
    pub total_costs: Money,
    pub net: Money,
    pub budget: Vec<BudgetLine>,
    pub warnings: Vec<Warning>,
    pub crew: Vec<CrewView>,
    /// Travel and money that belong to the project rather than one date.
    pub travel: Vec<TravelView>,
    pub lines: Vec<MoneyLineView>,
}

/// The budget category travel of this kind counts under.
pub(crate) fn travel_category(kind: &str) -> &'static str {
    match kind {
        "hotel" => "accommodation",
        "car" | "taxi" => "ground",
        _ => "travel",
    }
}

fn add(a: i64, b: i64) -> ApiResult<i64> {
    a.checked_add(b)
        .ok_or_else(|| ApiError::invalid("amount", "is too large"))
}

async fn bookings_of(tx: &mut Tx, project_id: Uuid) -> ApiResult<Vec<BookingView>> {
    let ids = core_db::projects::booking_ids(tx, project_id).await?;
    super::bookings::load_many(tx, &ids).await
}

/// Whether a date still stands: cancelled bookings and lapsed holds stay
/// listed but count for nothing.
fn stands(b: &BookingView) -> bool {
    !matches!(b.status, BookingStatus::Cancelled | BookingStatus::Expired)
}

/// The last day a booking covers: an end at midnight belongs to the day before.
fn last_day(b: &BookingView) -> NaiveDate {
    let end = b.ends_at - Duration::seconds(1);
    local_date(end.max(b.starts_at), &b.timezone)
}

fn local_date(at: DateTime<Utc>, tz: &str) -> NaiveDate {
    let tz: chrono_tz::Tz = tz.parse().unwrap_or(chrono_tz::UTC);
    at.with_timezone(&tz).date_naive()
}

pub async fn overview(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<ProjectOverview> {
    // Fees, commission and costs are the dates' own: reading them needs bookings too.
    ctx.authorize(Permission::ProjectRead)?;
    ctx.authorize(Permission::BookingRead)?;
    let mut tx = begin(state, ctx).await?;
    let project = load(&mut tx, id).await?;
    let currency = project.currency.clone();
    let bookings = bookings_of(&mut tx, id).await?;
    let ids: Vec<Uuid> = bookings.iter().map(|b| b.id).collect();
    let inputs = super::travel::CheckInputs::load(&mut tx, ctx, &ids).await?;
    let today = inputs.today;
    let mut rates = super::travel::Rates::default();
    let mut unconverted: Vec<String> = Vec::new();
    let (mut income, mut commission, mut costs) = (0i64, 0i64, 0i64);
    let mut actual: BTreeMap<String, i64> = BTreeMap::new();
    let mut dates = Vec::with_capacity(bookings.len());
    let mut losing = 0usize;

    for b in &bookings {
        let m: MoneyCheck = super::travel::check_with(&mut tx, &inputs, &mut rates, b).await?;
        let counts = stands(b);
        let on = local_date(b.starts_at, &b.timezone).min(today);
        let mut convert = async |x: &Money| -> ApiResult<Option<i64>> {
            super::travel::convert(&mut tx, &mut rates, x, &currency, on).await
        };
        let parts = (
            convert(&m.total_income).await?,
            convert(&m.total_commission).await?,
            convert(&m.total_costs).await?,
            convert(&m.net).await?,
        );
        let net_in_currency = match parts {
            (Some(i), Some(c), Some(k), Some(n)) => {
                if counts {
                    income = add(income, i)?;
                    commission = add(commission, c)?;
                    costs = add(costs, k)?;
                }
                Some(Money {
                    minor: n,
                    currency: currency.clone(),
                })
            }
            _ => {
                if counts && !unconverted.contains(&m.currency) {
                    unconverted.push(m.currency.clone());
                }
                None
            }
        };
        let trips: Vec<&TravelView> = inputs.travel_of(b.id).collect();
        for c in m.costs.iter().filter(|_| counts) {
            let category = match (c.category.as_str(), c.source_id) {
                // Travel entered as travel counts under its kind.
                ("travel", Some(source)) => trips
                    .iter()
                    .find(|t| t.id == source)
                    .map_or("travel", |t| travel_category(&t.kind)),
                (other, _) => COST_CATEGORIES
                    .iter()
                    .copied()
                    .find(|k| *k == other)
                    .unwrap_or("other"),
            };
            if let Some(minor) =
                super::travel::convert(&mut tx, &mut rates, &c.amount, &currency, on).await?
            {
                let slot = actual.entry(category.to_owned()).or_insert(0);
                *slot = add(*slot, minor)?;
            }
        }
        if counts && m.net.minor < 0 {
            losing += 1;
        }
        dates.push(DateSummary {
            booking_id: b.id,
            title: b.title.clone(),
            status: b.status,
            starts_at: b.starts_at,
            ends_at: b.ends_at,
            timezone: b.timezone.clone(),
            venue: b.venue_name.clone().or(b.venue_text.clone()),
            talent: b.talent.iter().map(|t| t.name.clone()).collect(),
            income: m.total_income,
            commission: m.total_commission,
            costs: m.total_costs,
            net: m.net,
            net_in_currency,
            warnings: m.warnings,
        });
    }

    // The project's own income and costs: shared travel, wages, insurance.
    let travel: Vec<TravelView> = core_db::travel::list(&mut tx, &[], Some(id))
        .await?
        .into_iter()
        .map(TravelView::from)
        .collect();
    let lines: Vec<MoneyLineView> = core_db::travel::lines(&mut tx, &[], Some(id))
        .await?
        .into_iter()
        .map(MoneyLineView::from)
        .collect();
    for t in &travel {
        let Some(cost) = t.our_cost() else { continue };
        let on = local_date(t.starts_at, &t.start_tz).min(today);
        match super::travel::convert(&mut tx, &mut rates, cost, &currency, on).await? {
            Some(minor) => {
                costs = add(costs, minor)?;
                let slot = actual
                    .entry(travel_category(&t.kind).to_owned())
                    .or_insert(0);
                *slot = add(*slot, minor)?;
            }
            None if !unconverted.contains(&cost.currency) => {
                unconverted.push(cost.currency.clone());
            }
            None => {}
        }
    }
    for l in &lines {
        let on = l.spent_on.unwrap_or(today).min(today);
        match super::travel::convert(&mut tx, &mut rates, &l.amount, &currency, on).await? {
            Some(minor) if l.direction == "income" => income = add(income, minor)?,
            Some(minor) => {
                costs = add(costs, minor)?;
                let slot = actual.entry(l.category.clone()).or_insert(0);
                *slot = add(*slot, minor)?;
            }
            None if !unconverted.contains(&l.amount.currency) => {
                unconverted.push(l.amount.currency.clone());
            }
            None => {}
        }
    }

    let (net, mut warnings) = touring::assess(income, commission, costs)
        .ok_or_else(|| ApiError::invalid("amount", "is too large"))?;
    if losing > 0 {
        warnings.push(Warning::new(
            WarningLevel::Caution,
            "losing_dates",
            if losing == 1 {
                "One date loses money on its own.".to_owned()
            } else {
                format!("{losing} dates lose money on their own.")
            },
        ));
    }
    let money = |minor: i64| Money {
        minor,
        currency: currency.clone(),
    };
    let mut budget = Vec::new();
    let mut over = Vec::new();
    for category in COST_CATEGORIES {
        let planned = project.budget.get(*category).copied().unwrap_or(0);
        let spent = actual.get(*category).copied().unwrap_or(0);
        if planned == 0 && spent == 0 {
            continue;
        }
        if planned > 0 && spent > planned {
            over.push(category.replace('_', " "));
        }
        budget.push(BudgetLine {
            category: (*category).to_owned(),
            budget: money(planned),
            actual: money(spent),
            remaining: money(planned.saturating_sub(spent)),
        });
    }
    if !over.is_empty() {
        warnings.push(Warning::new(
            WarningLevel::Caution,
            "over_budget",
            format!("Over budget on {}.", over.join(", ")),
        ));
    }
    if !unconverted.is_empty() {
        warnings.push(Warning::new(
            WarningLevel::Caution,
            "unconverted",
            format!(
                "Amounts in {} are left out of the totals: there is no exchange rate to {currency}.",
                unconverted.join(", ")
            ),
        ));
    }
    let crew = core_db::projects::crew(&mut tx, id)
        .await?
        .into_iter()
        .map(CrewView::from)
        .collect();
    tx.commit().await?;
    Ok(ProjectOverview {
        total_income: money(income),
        total_commission: money(commission),
        total_costs: money(costs),
        net: money(net),
        project,
        dates,
        budget,
        warnings,
        crew,
        travel,
        lines,
    })
}

// The itinerary: day by day ----------------------------------------------------------------

/// A date on a day of the itinerary, with what the day sheet needs.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ItineraryDate {
    pub booking_id: Uuid,
    pub title: String,
    pub status: BookingStatus,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub timezone: String,
    pub all_day: bool,
    pub venue: Option<String>,
    pub talent: Vec<String>,
    pub schedule: Vec<ScheduleItem>,
    pub contact_name: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ItineraryDay {
    pub date: NaiveDate,
    /// `show` (a date is on), `travel` (moving, nothing on) or `off`.
    pub kind: String,
    pub dates: Vec<ItineraryDate>,
    /// Journeys that leave this day (hotels not included).
    pub travel: Vec<TravelView>,
    /// Where people sleep tonight.
    pub hotels: Vec<TravelView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Itinerary {
    pub project_id: Uuid,
    pub name: String,
    pub days: Vec<ItineraryDay>,
    pub crew: Vec<CrewView>,
}

pub async fn itinerary(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<Itinerary> {
    ctx.authorize(Permission::ProjectRead)?;
    ctx.authorize(Permission::BookingRead)?;
    let mut tx = begin(state, ctx).await?;
    let project = load(&mut tx, id).await?;
    let bookings: Vec<BookingView> = bookings_of(&mut tx, id)
        .await?
        .into_iter()
        .filter(stands)
        .collect();
    let ids: Vec<Uuid> = bookings.iter().map(|b| b.id).collect();
    let travel: Vec<TravelView> = core_db::travel::list(&mut tx, &ids, Some(id))
        .await?
        .into_iter()
        .map(TravelView::from)
        .filter(|t| t.status != "cancelled")
        .map(|mut t| {
            // The itinerary is for the people going: what it costs stays off.
            t.cost = None;
            t
        })
        .collect();
    let crew = core_db::projects::crew(&mut tx, id)
        .await?
        .into_iter()
        .map(CrewView::from)
        .collect();
    let mut contacts = BTreeMap::new();
    let see_contacts = ctx.authorize(Permission::ContactRead).is_ok();
    for b in bookings.iter().filter(|_| see_contacts) {
        if let Some(c) = b.contact_id
            && let Some(found) = super::contacts::load(&mut tx, c).await?
        {
            contacts.insert(b.id, found.name);
        }
    }
    tx.commit().await?;

    let starts = bookings
        .iter()
        .map(|b| local_date(b.starts_at, &b.timezone))
        .chain(travel.iter().map(|t| local_date(t.starts_at, &t.start_tz)));
    let ends =
        bookings
            .iter()
            .map(last_day)
            .chain(travel.iter().map(|t| match (t.ends_at, &t.end_tz) {
                (Some(at), Some(tz)) => local_date(at, tz),
                _ => local_date(t.starts_at, &t.start_tz),
            }));
    let (Some(first), Some(last)) = (starts.min(), ends.max()) else {
        return Ok(Itinerary {
            project_id: id,
            name: project.name,
            days: Vec::new(),
            crew,
        });
    };
    let last = last.min(first + Duration::days(MAX_DAYS - 1));
    let mut days = Vec::new();
    let mut day = first;
    while day <= last {
        let on_day: Vec<ItineraryDate> = bookings
            .iter()
            .filter(|b| local_date(b.starts_at, &b.timezone) <= day && day <= last_day(b))
            .map(|b| ItineraryDate {
                booking_id: b.id,
                title: b.title.clone(),
                status: b.status,
                starts_at: b.starts_at,
                ends_at: b.ends_at,
                timezone: b.timezone.clone(),
                all_day: b.all_day,
                venue: b.venue_name.clone().or(b.venue_text.clone()),
                talent: b.talent.iter().map(|t| t.name.clone()).collect(),
                schedule: b.schedule.clone(),
                contact_name: contacts.get(&b.id).cloned(),
                notes: b.notes.clone(),
            })
            .collect();
        let moving: Vec<TravelView> = travel
            .iter()
            .filter(|t| t.kind != "hotel" && local_date(t.starts_at, &t.start_tz) == day)
            .cloned()
            .collect();
        // A hotel is tonight's when the night starting today falls between
        // check-in and check-out.
        let hotels: Vec<TravelView> = travel
            .iter()
            .filter(|t| {
                t.kind == "hotel" && local_date(t.starts_at, &t.start_tz) <= day && {
                    let out = match (t.ends_at, &t.end_tz) {
                        (Some(at), Some(tz)) => local_date(at, tz),
                        _ => local_date(t.starts_at, &t.start_tz) + Duration::days(1),
                    };
                    day < out
                }
            })
            .cloned()
            .collect();
        let kind = if !on_day.is_empty() {
            "show"
        } else if !moving.is_empty() {
            "travel"
        } else {
            "off"
        };
        days.push(ItineraryDay {
            date: day,
            kind: kind.to_owned(),
            dates: on_day,
            travel: moving,
            hotels,
        });
        day += Duration::days(1);
    }
    Ok(Itinerary {
        project_id: id,
        name: project.name,
        days,
        crew,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn travel_counts_under_its_kind() {
        assert_eq!(travel_category("hotel"), "accommodation");
        assert_eq!(travel_category("taxi"), "ground");
        assert_eq!(travel_category("flight"), "travel");
        assert_eq!(travel_category("ferry"), "travel");
    }
}
