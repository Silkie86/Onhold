//! Availability: when talent cannot work (blocks confirming a booking),
//! might not (warns), or works only within set times (shown on the
//! calendar). Staff edit it now; the talent portal (M7) and external
//! calendars (M6) add their own entries with their own source.

use chrono::{DateTime, Duration, Utc};
use core_db::Tx;
use core_db::availability::Availability;
use core_domain::{AvailabilityKind, Ctx, Permission};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::crm::Check;
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::routes::nullable;

pub const ENTITY: &str = "availability";

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AvailabilityView {
    pub id: Uuid,
    pub talent_id: Uuid,
    pub talent_name: String,
    pub kind: AvailabilityKind,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    /// `manual`, `portal` or `external`.
    pub source: String,
    pub note: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Availability> for AvailabilityView {
    fn from(a: Availability) -> Self {
        Self {
            id: a.id,
            talent_id: a.talent_id,
            talent_name: a.talent_name,
            kind: a.kind,
            starts_at: a.starts_at,
            ends_at: a.ends_at,
            all_day: a.all_day,
            source: a.source,
            note: a.note,
            version: a.version,
            created_at: a.created_at,
            updated_at: a.updated_at,
        }
    }
}

fn to_json(v: &AvailabilityView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<AvailabilityView>> {
    Ok(core_db::availability::get(tx, id).await?.map(Into::into))
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct AvailabilityQuery {
    /// Default: today minus a month.
    pub from: Option<DateTime<Utc>>,
    /// Default: a year after `from`.
    pub to: Option<DateTime<Utc>>,
}

/// A talent's availability overlapping a window.
pub async fn list(
    state: &AppState,
    ctx: &Ctx,
    talent_id: Uuid,
    q: &AvailabilityQuery,
) -> ApiResult<Vec<AvailabilityView>> {
    ctx.authorize(Permission::BookingRead)?;
    let from = q.from.unwrap_or_else(|| Utc::now() - Duration::days(31));
    let to = q.to.unwrap_or(from + Duration::days(366));
    if to <= from || to - from > Duration::days(super::bookings::MAX_CALENDAR_DAYS) {
        return Err(ApiError::invalid(
            "to",
            format!(
                "must be after from, and at most {} days later",
                super::bookings::MAX_CALENDAR_DAYS
            ),
        ));
    }
    let mut tx = begin(state, ctx).await?;
    if core_db::talent::get(&mut tx, talent_id).await?.is_none() {
        return Err(ApiError::NotFound);
    }
    let rows = core_db::availability::list(&mut tx, &[talent_id], from, to, &[]).await?;
    tx.commit().await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewAvailability {
    /// Default `unavailable`.
    pub kind: Option<AvailabilityKind>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    #[serde(default)]
    pub all_day: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct AvailabilityPatch {
    pub kind: Option<AvailabilityKind>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub all_day: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub note: Option<Option<String>>,
}

fn check_times(check: &mut Check, starts: DateTime<Utc>, ends: DateTime<Utc>) {
    if ends <= starts {
        check.issue("ends_at", "must be after starts_at");
    } else if ends - starts > Duration::days(super::bookings::MAX_LENGTH_DAYS) {
        check.issue("ends_at", "may cover at most a year");
    }
}

pub async fn create(
    state: &AppState,
    ctx: &Ctx,
    talent_id: Uuid,
    new: NewAvailability,
) -> ApiResult<AvailabilityView> {
    ctx.authorize(Permission::BookingWrite)?;
    let mut tx = begin(state, ctx).await?;
    if core_db::talent::get(&mut tx, talent_id).await?.is_none() {
        return Err(ApiError::NotFound);
    }
    let view = insert_in(&mut tx, ctx, talent_id, "manual", new).await?;
    tx.commit().await?;
    Ok(view)
}

/// Adds an entry for a talent already checked to exist, from `source`
/// (`manual` by staff, `portal` by the talent).
pub(crate) async fn insert_in(
    tx: &mut Tx,
    ctx: &Ctx,
    talent_id: Uuid,
    source: &str,
    new: NewAvailability,
) -> ApiResult<AvailabilityView> {
    let mut check = Check::default();
    check_times(&mut check, new.starts_at, new.ends_at);
    let note = check.optional("note", new.note.as_deref(), 500);
    check.finish()?;
    let id = core_domain::ids::new_id();
    core_db::availability::insert(
        tx,
        id,
        source,
        core_db::availability::Values {
            talent_id,
            kind: new.kind.unwrap_or(AvailabilityKind::Unavailable),
            starts_at: new.starts_at,
            ends_at: new.ends_at,
            all_day: new.all_day,
            note: note.as_deref(),
        },
        ctx.user_id(),
    )
    .await?;
    let view = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "availability.created",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(to_json(&view)),
            event_extra: None,
        },
    )
    .await?;
    Ok(view)
}

/// Why an entry cannot be changed: busy time read from the talent's own
/// calendar changes there, or there is no such entry.
async fn missing(tx: &mut Tx, id: Uuid) -> ApiError {
    match core_db::availability::is_external(tx, id).await {
        Ok(true) => ApiError::conflict(
            "from_their_calendar",
            "this busy time comes from the talent's own calendar; it changes when their calendar does",
        ),
        Ok(false) => ApiError::NotFound,
        Err(e) => e.into(),
    }
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    p: AvailabilityPatch,
    if_match: Option<i64>,
) -> ApiResult<AvailabilityView> {
    ctx.authorize(Permission::BookingWrite)?;
    let mut tx = begin(state, ctx).await?;
    let Some(version) = core_db::availability::lock(&mut tx, id).await? else {
        return Err(missing(&mut tx, id).await);
    };
    check_version(version, if_match)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let mut check = Check::default();
    let starts_at = p.starts_at.unwrap_or(before.starts_at);
    let ends_at = p.ends_at.unwrap_or(before.ends_at);
    check_times(&mut check, starts_at, ends_at);
    let note = match &p.note {
        Some(n) => check.optional("note", n.as_deref(), 500),
        None => before.note.clone(),
    };
    check.finish()?;
    let kind = p.kind.unwrap_or(before.kind);
    let all_day = p.all_day.unwrap_or(before.all_day);
    if kind == before.kind
        && starts_at == before.starts_at
        && ends_at == before.ends_at
        && all_day == before.all_day
        && note == before.note
    {
        return Ok(before);
    }
    core_db::availability::update(
        &mut tx,
        id,
        core_db::availability::Values {
            talent_id: before.talent_id,
            kind,
            starts_at,
            ends_at,
            all_day,
            note: note.as_deref(),
        },
        ctx.user_id(),
    )
    .await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "availability.updated",
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
    ctx.authorize(Permission::BookingWrite)?;
    let mut tx = begin(state, ctx).await?;
    delete_in(&mut tx, ctx, id).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn delete_in(tx: &mut Tx, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    let Some(before) = load(tx, id).await? else {
        return Err(missing(tx, id).await);
    };
    core_db::availability::soft_delete(tx, id, ctx.user_id()).await?;
    record(
        tx,
        ctx,
        Change {
            topic: "availability.deleted",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    Ok(())
}
