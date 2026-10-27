//! Bookings: holds, pending and confirmed engagements of one or more talent.
//!
//! A hold pencils a date in and lapses by itself after the workspace's hold
//! period. Confirming checks for clashes: another confirmed booking of the
//! same talent at an overlapping time, or time the talent marked
//! unavailable. Either blocks, unless someone with `booking:override` gives
//! a reason; the override is audited. The database refuses a double booking
//! of confirmed bookings outright, so two people confirming at once cannot
//! both succeed.

use chrono::{DateTime, Duration, Utc};
use core_db::Tx;
use core_db::bookings::{Booking, NO_DOUBLE_BOOKING};
use core_domain::{AvailabilityKind, BookingStatus, Ctx, Money, Permission, UserId, WorkspaceId};
use core_engines::fields::Mode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::crm::{Check, ListArgs, ListQuery};
use super::links::{self, Named};
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::routes::nullable;

pub const ENTITY: &str = "booking";
pub const MAX_SCHEDULE: usize = 30;
/// The longest a single booking may run.
pub const MAX_LENGTH_DAYS: i64 = 366;
/// The widest calendar window one request may ask for.
pub const MAX_CALENDAR_DAYS: i64 = 400;

/// A time in the running order: load-in, soundcheck, doors, set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScheduleItem {
    pub label: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BookingView {
    pub id: Uuid,
    pub enquiry_id: Option<Uuid>,
    pub title: String,
    pub status: BookingStatus,
    pub status_changed_at: DateTime<Utc>,
    /// The talent booked, in order.
    pub talent: Vec<Named>,
    pub organisation_id: Option<Uuid>,
    pub organisation_name: Option<String>,
    pub contact_id: Option<Uuid>,
    pub contact_name: Option<String>,
    pub venue_id: Option<Uuid>,
    pub venue_name: Option<String>,
    pub venue_text: Option<String>,
    /// The IANA zone times are shown and entered in.
    pub timezone: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    pub schedule: Vec<ScheduleItem>,
    /// When a hold lapses by itself.
    pub hold_expires_at: Option<DateTime<Utc>>,
    pub fee: Option<Money>,
    /// The share of the fee due as a deposit.
    pub deposit_percent: Option<f64>,
    pub cancel_reason: Option<String>,
    /// Confirmed despite a clash.
    pub clash_override: bool,
    pub override_reason: Option<String>,
    pub override_by: Option<Uuid>,
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

pub(crate) fn view(b: Booking, talent: Vec<Named>) -> BookingView {
    BookingView {
        id: b.id,
        enquiry_id: b.enquiry_id,
        title: b.title,
        status: b.status,
        status_changed_at: b.status_changed_at,
        talent,
        organisation_id: b.organisation_id,
        organisation_name: b.organisation_name,
        contact_id: b.contact_id,
        contact_name: b.contact_name,
        venue_id: b.venue_id,
        venue_name: b.venue_name,
        venue_text: b.venue_text,
        timezone: b.timezone,
        starts_at: b.starts_at,
        ends_at: b.ends_at,
        all_day: b.all_day,
        schedule: serde_json::from_value(b.schedule).unwrap_or_default(),
        hold_expires_at: b.hold_expires_at,
        fee: match (b.fee_minor, b.fee_currency) {
            (Some(minor), Some(currency)) => Some(Money {
                minor,
                currency: currency.trim().to_owned(),
            }),
            _ => None,
        },
        deposit_percent: b.deposit_bps.map(|v| f64::from(v) / 100.0),
        cancel_reason: b.cancel_reason,
        clash_override: b.clash_override,
        override_reason: b.override_reason,
        override_by: b.override_by,
        notes: b.notes,
        tags: b.tags,
        custom: b.custom,
        version: b.version,
        created_at: b.created_at,
        updated_at: b.updated_at,
        created_by: b.created_by,
        updated_by: b.updated_by,
    }
}

/// The JSON events, the audit log, rules and templates see.
pub fn to_json(v: &BookingView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

pub(crate) async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<BookingView>> {
    let Some(b) = core_db::bookings::get(tx, id).await? else {
        return Ok(None);
    };
    let talent = links::talent_by_owner(core_db::bookings::talent_of(tx, &[id]).await?, id);
    Ok(Some(view(b, talent)))
}

async fn with_talent(tx: &mut Tx, rows: Vec<Booking>) -> ApiResult<Vec<BookingView>> {
    let ids: Vec<Uuid> = rows.iter().map(|b| b.id).collect();
    let talent = core_db::bookings::talent_of(tx, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|b| {
            let t = links::talent_by_owner(talent.clone(), b.id);
            view(b, t)
        })
        .collect())
}

// Clashes ----------------------------------------------------------------------------

/// Something else taking a talent's time.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Clash {
    /// `booking` or `availability`.
    pub kind: String,
    /// Whether it stops the booking being confirmed: a confirmed booking or
    /// unavailable time. Holds, pending bookings and tentative time only warn.
    pub blocking: bool,
    pub talent_id: Uuid,
    pub talent_name: String,
    pub booking_id: Option<Uuid>,
    pub availability_id: Option<Uuid>,
    /// The other booking's title, or the availability's note.
    pub title: Option<String>,
    /// The other booking's status, or the availability's kind.
    pub status: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClashReport {
    /// True when at least one clash blocks confirming.
    pub blocking: bool,
    pub clashes: Vec<Clash>,
}

impl ClashReport {
    fn summary(&self) -> String {
        let parts: Vec<String> = self
            .clashes
            .iter()
            .filter(|c| c.blocking)
            .map(|c| match c.kind.as_str() {
                "booking" => format!(
                    "{} is already booked for “{}”",
                    c.talent_name,
                    c.title.as_deref().unwrap_or("another booking")
                ),
                _ => format!("{} is unavailable then", c.talent_name),
            })
            .collect();
        parts.join("; ")
    }
}

/// Everything overlapping `[starts, ends)` for these talent, other than the
/// booking `except`.
pub(crate) async fn find_clashes(
    tx: &mut Tx,
    talent: &[Uuid],
    starts: DateTime<Utc>,
    ends: DateTime<Utc>,
    except: Option<Uuid>,
) -> ApiResult<ClashReport> {
    let mut clashes = Vec::new();
    if talent.is_empty() {
        return Ok(ClashReport {
            blocking: false,
            clashes,
        });
    }
    for o in core_db::bookings::overlapping(tx, talent, starts, ends, except).await? {
        clashes.push(Clash {
            kind: "booking".into(),
            blocking: o.status == BookingStatus::Confirmed,
            talent_id: o.talent_id,
            talent_name: o.talent_name,
            booking_id: Some(o.booking_id),
            availability_id: None,
            title: Some(o.title),
            status: o.status.as_str().into(),
            starts_at: o.starts_at,
            ends_at: o.ends_at,
        });
    }
    let kinds = [AvailabilityKind::Unavailable, AvailabilityKind::Tentative];
    for a in core_db::availability::list(tx, talent, starts, ends, &kinds).await? {
        clashes.push(Clash {
            kind: "availability".into(),
            blocking: a.kind == AvailabilityKind::Unavailable,
            talent_id: a.talent_id,
            talent_name: a.talent_name,
            booking_id: None,
            availability_id: Some(a.id),
            title: a.note,
            status: a.kind.as_str().into(),
            starts_at: a.starts_at,
            ends_at: a.ends_at,
        });
    }
    clashes.sort_by_key(|c| (!c.blocking, c.starts_at));
    Ok(ClashReport {
        blocking: clashes.iter().any(|c| c.blocking),
        clashes,
    })
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ClashQuery {
    pub talent_ids: Vec<Uuid>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// The booking being edited, which does not clash with itself.
    pub except_id: Option<Uuid>,
}

/// What a booking of these talent at this time would clash with.
pub async fn clash_check(state: &AppState, ctx: &Ctx, q: &ClashQuery) -> ApiResult<ClashReport> {
    ctx.authorize(Permission::BookingRead)?;
    if q.ends_at <= q.starts_at {
        return Err(ApiError::invalid("ends_at", "must be after starts_at"));
    }
    if q.talent_ids.len() > links::MAX_TALENT {
        return Err(ApiError::invalid(
            "talent_ids",
            format!("name at most {} talent", links::MAX_TALENT),
        ));
    }
    let mut tx = begin(state, ctx).await?;
    let report = find_clashes(&mut tx, &q.talent_ids, q.starts_at, q.ends_at, q.except_id).await?;
    tx.commit().await?;
    Ok(report)
}

/// Confirming (or keeping confirmed) at this time: fails on a blocking
/// clash unless an override reason is given by someone allowed to.
/// Returns the override reason to record, if one was needed.
async fn check_firm(
    tx: &mut Tx,
    ctx: &Ctx,
    talent: &[Uuid],
    starts: DateTime<Utc>,
    ends: DateTime<Utc>,
    except: Option<Uuid>,
    override_reason: Option<&str>,
) -> ApiResult<Option<String>> {
    let report = find_clashes(tx, talent, starts, ends, except).await?;
    if !report.blocking {
        return Ok(None);
    }
    match override_reason.map(str::trim).filter(|r| !r.is_empty()) {
        None => Err(clash_error(&report.summary())),
        Some(reason) => {
            ctx.authorize(Permission::BookingOverride)?;
            if reason.chars().count() > 500 {
                return Err(ApiError::invalid(
                    "override_reason",
                    "must be at most 500 characters",
                ));
            }
            Ok(Some(reason.to_owned()))
        }
    }
}

fn clash_error(summary: &str) -> ApiError {
    ApiError::conflict(
        "booking_clash",
        if summary.is_empty() {
            "this would double-book someone".to_owned()
        } else {
            format!("{summary}. Choose another time, or an admin can confirm anyway with a reason.")
        },
    )
}

/// Maps the database's double-booking refusal to the same error people get
/// from the check (it only fires when two confirmations race).
fn refused(err: core_db::DbError) -> ApiError {
    if err.is_exclusion_violation(Some(NO_DOUBLE_BOOKING)) {
        clash_error("someone else just confirmed a booking for the same talent at this time")
    } else {
        err.into()
    }
}

// Listing ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct BookingPage {
    pub items: Vec<BookingView>,
    pub total: i64,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct BookingFilter {
    /// Bookings ending after this…
    pub from: Option<DateTime<Utc>>,
    /// …and starting before this.
    pub to: Option<DateTime<Utc>>,
    /// Comma-separated statuses, e.g. `hold,confirmed`.
    pub status: Option<String>,
    pub talent_id: Option<Uuid>,
    /// Bookings for (or at) this organisation.
    pub organisation_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub enquiry_id: Option<Uuid>,
}

fn parse_statuses(s: Option<&str>) -> ApiResult<Vec<BookingStatus>> {
    let mut out = Vec::new();
    for part in s
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let status = BookingStatus::parse(part).ok_or_else(|| {
            ApiError::invalid(
                "status",
                format!(
                    "`{part}` is not a status; use {}",
                    BookingStatus::ALL
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })?;
        if !out.contains(&status) {
            out.push(status);
        }
    }
    Ok(out)
}

pub async fn list(
    state: &AppState,
    ctx: &Ctx,
    q: &ListQuery,
    f: &BookingFilter,
) -> ApiResult<BookingPage> {
    ctx.authorize(Permission::BookingRead)?;
    let statuses = parse_statuses(f.status.as_deref())?;
    let mut tx = begin(state, ctx).await?;
    let q = ListQuery {
        q: q.q.clone(),
        tag: q.tag.clone(),
        custom: q.custom.clone(),
        // Soonest first unless asked otherwise.
        sort: q.sort.clone().or_else(|| {
            q.q.as_deref()
                .is_none_or(|s| s.trim().is_empty())
                .then(|| "starts_at".to_owned())
        }),
        offset: q.offset,
        limit: q.limit,
    };
    let args = ListArgs::parse(&mut tx, ENTITY, core_db::bookings::SORTS, &q).await?;
    let (rows, total) = core_db::bookings::list(
        &mut tx,
        core_db::bookings::Filter {
            list: args.params(),
            from: f.from,
            to: f.to,
            statuses: &statuses,
            talent_id: f.talent_id,
            organisation_id: f.organisation_id,
            contact_id: f.contact_id,
            enquiry_id: f.enquiry_id,
        },
    )
    .await?;
    let items = with_talent(&mut tx, rows).await?;
    tx.commit().await?;
    Ok(BookingPage { items, total })
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<BookingView> {
    ctx.authorize(Permission::BookingRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

// Calendar --------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct CalendarQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// One talent's lane only.
    pub talent_id: Option<Uuid>,
    /// Comma-separated statuses; default everything but cancelled and expired.
    pub status: Option<String>,
    /// Only bookings with this tag.
    pub tag: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CalendarView {
    pub bookings: Vec<BookingView>,
    /// Unavailable, tentative and available-only time in the window.
    pub availability: Vec<super::availability::AvailabilityView>,
}

/// Bookings and availability overlapping a window, for the calendar.
pub async fn calendar(state: &AppState, ctx: &Ctx, q: &CalendarQuery) -> ApiResult<CalendarView> {
    ctx.authorize(Permission::BookingRead)?;
    if q.to <= q.from {
        return Err(ApiError::invalid("to", "must be after from"));
    }
    if q.to - q.from > Duration::days(MAX_CALENDAR_DAYS) {
        return Err(ApiError::invalid(
            "to",
            format!("ask for at most {MAX_CALENDAR_DAYS} days at a time"),
        ));
    }
    let mut statuses = parse_statuses(q.status.as_deref())?;
    if statuses.is_empty() {
        statuses = vec![
            BookingStatus::Hold,
            BookingStatus::Pending,
            BookingStatus::Confirmed,
            BookingStatus::Completed,
        ];
    }
    let tag = q
        .tag
        .as_deref()
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty());
    let mut tx = begin(state, ctx).await?;
    let (rows, _) = core_db::bookings::list(
        &mut tx,
        core_db::bookings::Filter {
            list: core_db::crm::ListParams {
                q: None,
                searchable: &[],
                tag: tag.as_deref(),
                custom: None,
                sort: "starts_at",
                sort_custom: None,
                desc: false,
                offset: 0,
                limit: 5000,
            },
            from: Some(q.from),
            to: Some(q.to),
            statuses: &statuses,
            talent_id: q.talent_id,
            organisation_id: None,
            contact_id: None,
            enquiry_id: None,
        },
    )
    .await?;
    let bookings = with_talent(&mut tx, rows).await?;
    let talent: Vec<Uuid> = q.talent_id.into_iter().collect();
    let availability = core_db::availability::list(&mut tx, &talent, q.from, q.to, &[])
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    tx.commit().await?;
    Ok(CalendarView {
        bookings,
        availability,
    })
}

// Writing ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NewBooking {
    pub title: String,
    /// The enquiry it answers. Its talent, organisation, contact and venue
    /// are used for any not given here.
    pub enquiry_id: Option<Uuid>,
    /// `hold` (default), `pending` or `confirmed`.
    pub status: Option<BookingStatus>,
    #[serde(default)]
    pub talent_ids: Vec<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub venue_id: Option<Uuid>,
    pub venue_text: Option<String>,
    /// Default: the workspace's timezone.
    pub timezone: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub schedule: Vec<ScheduleItem>,
    /// When a hold lapses. Default: the workspace's hold period from now.
    pub hold_expires_at: Option<DateTime<Utc>>,
    pub fee: Option<Money>,
    pub deposit_percent: Option<f64>,
    pub notes: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub custom: Map<String, Value>,
    /// Confirm despite a clash (needs `booking:override`).
    pub override_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct BookingPatch {
    pub title: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub enquiry_id: Option<Option<Uuid>>,
    /// Replaces the talent booked.
    pub talent_ids: Option<Vec<Uuid>>,
    /// `null` clears it; likewise for the other optional fields.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub organisation_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub contact_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub venue_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub venue_text: Option<Option<String>>,
    pub timezone: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub all_day: Option<bool>,
    pub schedule: Option<Vec<ScheduleItem>>,
    /// Holds only; `null` means it never lapses.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<DateTime<Utc>>)]
    pub hold_expires_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Money>)]
    pub fee: Option<Option<Money>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<f64>)]
    pub deposit_percent: Option<Option<f64>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub notes: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
    /// Only the keys sent change; `null` clears a value.
    #[schema(value_type = Option<Object>)]
    pub custom: Option<Map<String, Value>>,
    /// Keep a confirmed booking at a time that clashes (needs `booking:override`).
    pub override_reason: Option<String>,
}

fn deposit(check: &mut Check, percent: Option<f64>) -> Option<i32> {
    let p = percent?;
    let bps = (p * 100.0).round();
    if !(0.0..=100.0).contains(&p) || (p * 100.0 - bps).abs() > 1e-6 {
        check.issue(
            "deposit_percent",
            "must be 0 to 100 with at most two decimal places",
        );
        return None;
    }
    Some(bps as i32)
}

fn check_times(check: &mut Check, starts: DateTime<Utc>, ends: DateTime<Utc>) {
    if ends <= starts {
        check.issue("ends_at", "must be after starts_at");
    } else if ends - starts > Duration::days(MAX_LENGTH_DAYS) {
        check.issue(
            "ends_at",
            format!("a booking may last at most {MAX_LENGTH_DAYS} days"),
        );
    }
}

fn check_timezone(check: &mut Check, tz: &str) -> String {
    let tz = tz.trim();
    if core_domain::workspace::validate_timezone(tz).is_err() {
        check.issue("timezone", format!("`{tz}` is not an IANA timezone"));
    }
    tz.to_owned()
}

fn check_schedule(check: &mut Check, items: &[ScheduleItem]) -> Value {
    if items.len() > MAX_SCHEDULE {
        check.issue("schedule", format!("give at most {MAX_SCHEDULE} times"));
    }
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let label = check.required(&format!("schedule[{i}].label"), &item.label, 100);
        out.push(ScheduleItem { label, at: item.at });
    }
    out.sort_by_key(|s| s.at);
    serde_json::to_value(out).unwrap_or_else(|_| json!([]))
}

async fn hold_days(tx: &mut Tx, ctx: &Ctx) -> ApiResult<i32> {
    Ok(core_db::workspaces::get(tx, ctx.workspace_id)
        .await?
        .map_or(7, |w| w.hold_days))
}

/// When a hold placed now lapses, by the workspace's setting.
async fn default_expiry(tx: &mut Tx, ctx: &Ctx) -> ApiResult<Option<DateTime<Utc>>> {
    let days = hold_days(tx, ctx).await?;
    Ok((days > 0).then(|| Utc::now() + Duration::days(i64::from(days))))
}

pub async fn create(state: &AppState, ctx: &Ctx, new: NewBooking) -> ApiResult<BookingView> {
    ctx.authorize(Permission::BookingWrite)?;
    let mut tx = begin(state, ctx).await?;
    let view = create_in_tx(&mut tx, ctx, new).await?;
    tx.commit().await?;
    Ok(view)
}

pub(crate) async fn create_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    mut new: NewBooking,
) -> ApiResult<BookingView> {
    let status = new.status.unwrap_or(BookingStatus::Hold);
    if !matches!(
        status,
        BookingStatus::Hold | BookingStatus::Pending | BookingStatus::Confirmed
    ) {
        return Err(ApiError::invalid(
            "status",
            "a new booking is a hold, pending or confirmed",
        ));
    }
    let mut check = Check::default();
    if let Some(enquiry_id) = new.enquiry_id {
        match core_db::enquiries::get(tx, enquiry_id).await? {
            None => check.issue("enquiry_id", "there is no such enquiry"),
            Some(e) => {
                if new.talent_ids.is_empty() {
                    new.talent_ids = core_db::enquiries::talent_of(tx, &[enquiry_id])
                        .await?
                        .into_iter()
                        .map(|(_, t, _)| t)
                        .collect();
                }
                new.organisation_id = new.organisation_id.or(e.organisation_id);
                new.contact_id = new.contact_id.or(e.contact_id);
                new.venue_id = new.venue_id.or(e.venue_id);
                new.venue_text = new.venue_text.or(e.venue_text);
            }
        }
    }
    let defs = super::fields::engine_defs(tx, ENTITY).await?;
    let title = check.required("title", &new.title, 200);
    let talent = links::check_talent(tx, &mut check, &new.talent_ids).await?;
    links::check_crm(
        tx,
        &mut check,
        new.organisation_id,
        new.contact_id,
        new.venue_id,
    )
    .await?;
    let venue_text = check.optional("venue_text", new.venue_text.as_deref(), 200);
    let timezone = match &new.timezone {
        Some(tz) => check_timezone(&mut check, tz),
        None => core_db::workspaces::get(tx, ctx.workspace_id)
            .await?
            .map_or_else(|| "UTC".to_owned(), |w| w.default_timezone),
    };
    check_times(&mut check, new.starts_at, new.ends_at);
    let schedule = check_schedule(&mut check, &new.schedule);
    let fee = check.money("fee", new.fee.as_ref());
    let deposit_bps = deposit(&mut check, new.deposit_percent);
    let notes = check.long("notes", new.notes.as_deref(), 20_000);
    let tags = check.tags(&new.tags);
    let custom = check.custom(&defs, &Map::new(), &new.custom, Mode::Create);
    check.finish()?;

    let override_reason = if status == BookingStatus::Confirmed {
        check_firm(
            tx,
            ctx,
            &talent,
            new.starts_at,
            new.ends_at,
            None,
            new.override_reason.as_deref(),
        )
        .await?
    } else {
        None
    };
    let hold_expires_at = match status {
        BookingStatus::Hold => match new.hold_expires_at {
            Some(at) => Some(at),
            None => default_expiry(tx, ctx).await?,
        },
        _ => None,
    };

    let id = core_domain::ids::new_id();
    let custom = Value::Object(custom);
    core_db::bookings::insert(
        tx,
        id,
        status,
        hold_expires_at,
        core_db::bookings::Values {
            enquiry_id: new.enquiry_id,
            title: &title,
            organisation_id: new.organisation_id,
            contact_id: new.contact_id,
            venue_id: new.venue_id,
            venue_text: venue_text.as_deref(),
            timezone: &timezone,
            starts_at: new.starts_at,
            ends_at: new.ends_at,
            all_day: new.all_day,
            schedule: &schedule,
            fee_minor: fee.as_ref().map(|m| m.minor),
            fee_currency: fee.as_ref().map(|m| m.currency.as_str()),
            deposit_bps,
            notes: notes.as_deref(),
            tags: &tags,
            custom: &custom,
        },
        ctx.user_id(),
    )
    .await?;
    if let Some(reason) = &override_reason {
        core_db::bookings::set_override(tx, id, reason, ctx.user_id()).await?;
    }
    core_db::bookings::set_talent(tx, id, &talent)
        .await
        .map_err(refused)?;
    let view = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "booking.created",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(to_json(&view)),
            event_extra: None,
        },
    )
    .await?;
    record(
        tx,
        ctx,
        Change {
            topic: status.topic(),
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(to_json(&view)),
            event_extra: None,
        },
    )
    .await?;
    if override_reason.is_some() {
        record_override(tx, ctx, &view).await?;
    }
    Ok(view)
}

async fn record_override(tx: &mut Tx, ctx: &Ctx, v: &BookingView) -> ApiResult<()> {
    record(
        tx,
        ctx,
        Change {
            topic: "booking.clash_overridden",
            resource: ENTITY,
            object_id: Some(v.id),
            before: None,
            after: Some(json!({
                "override_reason": v.override_reason,
                "starts_at": v.starts_at,
                "ends_at": v.ends_at,
                "talent": v.talent,
            })),
            event_extra: None,
        },
    )
    .await?;
    Ok(())
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    patch: BookingPatch,
    if_match: Option<i64>,
) -> ApiResult<BookingView> {
    ctx.authorize(Permission::BookingWrite)?;
    let mut tx = begin(state, ctx).await?;
    let view = update_in_tx(&mut tx, ctx, id, patch, if_match).await?;
    tx.commit().await?;
    Ok(view)
}

/// The update, inside the caller's transaction (rules use this).
pub(crate) async fn update_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    id: Uuid,
    p: BookingPatch,
    if_match: Option<i64>,
) -> ApiResult<BookingView> {
    let version = core_db::bookings::lock(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let current = core_db::bookings::get(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let before = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    if current.status.is_final() {
        return Err(ApiError::conflict(
            "booking_closed",
            format!("a {} booking cannot be changed", current.status),
        ));
    }

    let mut check = Check::default();
    let title = match &p.title {
        Some(t) => check.required("title", t, 200),
        None => current.title.clone(),
    };
    let enquiry_id = p.enquiry_id.unwrap_or(current.enquiry_id);
    if let Some(Some(e)) = p.enquiry_id
        && core_db::enquiries::get(tx, e).await?.is_none()
    {
        check.issue("enquiry_id", "there is no such enquiry");
    }
    let before_talent: Vec<Uuid> = before.talent.iter().map(|n| n.id).collect();
    let talent = match &p.talent_ids {
        Some(ids) => links::check_talent(tx, &mut check, ids).await?,
        None => before_talent.clone(),
    };
    let organisation_id = p.organisation_id.unwrap_or(current.organisation_id);
    let contact_id = p.contact_id.unwrap_or(current.contact_id);
    let venue_id = p.venue_id.unwrap_or(current.venue_id);
    links::check_crm(
        tx,
        &mut check,
        p.organisation_id.flatten(),
        p.contact_id.flatten(),
        p.venue_id.flatten(),
    )
    .await?;
    let venue_text = match &p.venue_text {
        Some(v) => check.optional("venue_text", v.as_deref(), 200),
        None => current.venue_text.clone(),
    };
    let timezone = match &p.timezone {
        Some(tz) => check_timezone(&mut check, tz),
        None => current.timezone.clone(),
    };
    let starts_at = p.starts_at.unwrap_or(current.starts_at);
    let ends_at = p.ends_at.unwrap_or(current.ends_at);
    check_times(&mut check, starts_at, ends_at);
    let all_day = p.all_day.unwrap_or(current.all_day);
    let schedule = match &p.schedule {
        Some(s) => check_schedule(&mut check, s),
        None => current.schedule.clone(),
    };
    let hold_expires_at = match p.hold_expires_at {
        Some(at) if current.status == BookingStatus::Hold => at,
        Some(_) => {
            check.issue("hold_expires_at", "only a hold lapses");
            current.hold_expires_at
        }
        None => current.hold_expires_at,
    };
    let fee = match &p.fee {
        Some(f) => check.money("fee", f.as_ref()),
        None => before.fee.clone(),
    };
    let deposit_bps = match p.deposit_percent {
        Some(d) => deposit(&mut check, d),
        None => current.deposit_bps,
    };
    let notes = match &p.notes {
        Some(v) => check.long("notes", v.as_deref(), 20_000),
        None => current.notes.clone(),
    };
    let tags = match &p.tags {
        Some(t) => check.tags(t),
        None => current.tags.clone(),
    };
    let current_custom = current.custom.as_object().cloned().unwrap_or_default();
    let custom = match &p.custom {
        Some(changes) => {
            let defs = super::fields::engine_defs(tx, ENTITY).await?;
            check.custom(&defs, &current_custom, changes, Mode::Update)
        }
        None => current_custom,
    };
    check.finish()?;

    let moved = starts_at != current.starts_at || ends_at != current.ends_at;
    let talent_changed = talent != before_talent;
    // A confirmed booking that moves, or takes new talent, is checked again.
    let rechecked = current.status == BookingStatus::Confirmed && (moved || talent_changed);
    let override_reason = if rechecked {
        check_firm(
            tx,
            ctx,
            &talent,
            starts_at,
            ends_at,
            Some(id),
            p.override_reason.as_deref(),
        )
        .await?
    } else {
        None
    };

    let custom = Value::Object(custom);
    let fee_minor = fee.as_ref().map(|m| m.minor);
    let fee_currency = fee.as_ref().map(|m| m.currency.clone());
    let unchanged = title == current.title
        && enquiry_id == current.enquiry_id
        && !talent_changed
        && organisation_id == current.organisation_id
        && contact_id == current.contact_id
        && venue_id == current.venue_id
        && venue_text == current.venue_text
        && timezone == current.timezone
        && !moved
        && all_day == current.all_day
        && schedule == current.schedule
        && hold_expires_at == current.hold_expires_at
        && fee_minor == current.fee_minor
        && fee_currency.as_deref() == current.fee_currency.as_deref().map(str::trim)
        && deposit_bps == current.deposit_bps
        && notes == current.notes
        && tags == current.tags
        && custom == current.custom;
    if unchanged {
        return Ok(before);
    }

    if let Some(reason) = &override_reason {
        core_db::bookings::set_override(tx, id, reason, ctx.user_id()).await?;
    }
    core_db::bookings::update(
        tx,
        id,
        core_db::bookings::Values {
            enquiry_id,
            title: &title,
            organisation_id,
            contact_id,
            venue_id,
            venue_text: venue_text.as_deref(),
            timezone: &timezone,
            starts_at,
            ends_at,
            all_day,
            schedule: &schedule,
            fee_minor,
            fee_currency: fee_currency.as_deref(),
            deposit_bps,
            notes: notes.as_deref(),
            tags: &tags,
            custom: &custom,
        },
        ctx.user_id(),
    )
    .await
    .map_err(refused)?;
    if hold_expires_at != current.hold_expires_at {
        core_db::bookings::set_hold_expiry(tx, id, hold_expires_at, ctx.user_id()).await?;
    }
    if talent_changed {
        core_db::bookings::set_talent(tx, id, &talent)
            .await
            .map_err(refused)?;
    }
    // An override that no longer has a clash to excuse ends, which puts the
    // booking back under the double-booking constraint. Only once it is at
    // its new time and talent: at the old ones it still clashes.
    if rechecked && override_reason.is_none() && current.clash_override {
        core_db::bookings::clear_override(tx, id, ctx.user_id())
            .await
            .map_err(refused)?;
    }
    let after = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "booking.updated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    if moved {
        record(
            tx,
            ctx,
            Change {
                topic: "booking.rescheduled",
                resource: ENTITY,
                object_id: Some(id),
                before: Some(json!({ "starts_at": before.starts_at, "ends_at": before.ends_at })),
                after: Some(json!({ "starts_at": after.starts_at, "ends_at": after.ends_at })),
                event_extra: None,
            },
        )
        .await?;
    }
    if override_reason.is_some() {
        record_override(tx, ctx, &after).await?;
    }
    Ok(after)
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct StatusRequest {
    pub status: BookingStatus,
    /// Why it was cancelled.
    pub reason: Option<String>,
    /// Confirm despite a clash (needs `booking:override`).
    pub override_reason: Option<String>,
}

pub async fn change_status(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    req: StatusRequest,
    if_match: Option<i64>,
) -> ApiResult<BookingView> {
    ctx.authorize(Permission::BookingWrite)?;
    let mut tx = begin(state, ctx).await?;
    let view = change_status_in_tx(&mut tx, ctx, id, req, if_match).await?;
    tx.commit().await?;
    Ok(view)
}

/// Moves a booking along one of [`BookingStatus::can_become`]'s arrows.
pub(crate) async fn change_status_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    id: Uuid,
    req: StatusRequest,
    if_match: Option<i64>,
) -> ApiResult<BookingView> {
    let version = core_db::bookings::lock(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let before = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    let to = req.status;
    if before.status == to {
        return Ok(before);
    }
    if !before.status.can_become(to) {
        return Err(ApiError::conflict(
            "status_change_refused",
            format!("a {} booking cannot become {to}", before.status),
        ));
    }
    let talent: Vec<Uuid> = before.talent.iter().map(|t| t.id).collect();
    let override_reason = if to == BookingStatus::Confirmed {
        check_firm(
            tx,
            ctx,
            &talent,
            before.starts_at,
            before.ends_at,
            Some(id),
            req.override_reason.as_deref(),
        )
        .await?
    } else {
        None
    };
    let cancel_reason = match to {
        BookingStatus::Cancelled => {
            let mut check = Check::default();
            let reason = check.optional("reason", req.reason.as_deref(), 500);
            check.finish()?;
            reason
        }
        _ => None,
    };
    let hold_expires_at = match to {
        BookingStatus::Hold => default_expiry(tx, ctx).await?,
        _ => None,
    };
    let keeps_override = to == BookingStatus::Confirmed && override_reason.is_some();
    // An earlier override stays on record unless the booking is confirmed afresh.
    let (clash_override, reason, by) = if to == BookingStatus::Confirmed {
        (
            keeps_override,
            override_reason.clone(),
            override_reason.as_ref().and(ctx.user_id().map(|u| u.0)),
        )
    } else {
        (
            before.clash_override,
            before.override_reason.clone(),
            before.override_by,
        )
    };
    core_db::bookings::set_status(
        tx,
        id,
        core_db::bookings::StatusChange {
            status: to,
            hold_expires_at,
            cancel_reason: cancel_reason.as_deref(),
            clash_override,
            override_reason: reason.as_deref(),
            override_by: by,
        },
        ctx.user_id(),
    )
    .await
    .map_err(refused)?;
    if matches!(to, BookingStatus::Cancelled | BookingStatus::Expired) {
        super::quotes::withdraw_open(tx, ctx, id).await?;
    }
    let after = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: to.topic(),
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    if keeps_override {
        record_override(tx, ctx, &after).await?;
    }
    Ok(after)
}

/// Deleting a booking withdraws its open quotes; the quotes stay readable.
pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::BookingWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    super::quotes::withdraw_open(&mut tx, ctx, id).await?;
    core_db::bookings::soft_delete(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "booking.deleted",
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

// The worker's sweeps -----------------------------------------------------------------

pub const JOB_EXPIRE_HOLD: &str = "booking.expire_hold";
pub const JOB_COMPLETE: &str = "booking.complete";

/// Lapses a hold whose expiry has passed (the worker's `booking.expire_hold`).
pub async fn expire_hold(state: &AppState, workspace: WorkspaceId, id: Uuid) -> ApiResult<()> {
    let ctx = Ctx::system(workspace);
    let mut tx = begin(state, &ctx).await?;
    if core_db::bookings::lock(&mut tx, id).await?.is_none() {
        return Ok(());
    }
    let Some(b) = core_db::bookings::get(&mut tx, id).await? else {
        return Ok(());
    };
    let due =
        b.status == BookingStatus::Hold && b.hold_expires_at.is_some_and(|at| at <= Utc::now());
    if due {
        let req = StatusRequest {
            status: BookingStatus::Expired,
            reason: None,
            override_reason: None,
        };
        change_status_in_tx(&mut tx, &ctx, id, req, None).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Completes a confirmed booking that has ended (the worker's `booking.complete`).
pub async fn complete(state: &AppState, workspace: WorkspaceId, id: Uuid) -> ApiResult<()> {
    let ctx = Ctx::system(workspace);
    let mut tx = begin(state, &ctx).await?;
    if core_db::bookings::lock(&mut tx, id).await?.is_none() {
        return Ok(());
    }
    let Some(b) = core_db::bookings::get(&mut tx, id).await? else {
        return Ok(());
    };
    if b.status == BookingStatus::Confirmed && b.ends_at <= Utc::now() {
        let req = StatusRequest {
            status: BookingStatus::Completed,
            reason: None,
            override_reason: None,
        };
        change_status_in_tx(&mut tx, &ctx, id, req, None).await?;
    }
    tx.commit().await?;
    Ok(())
}
