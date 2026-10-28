//! The talent portal (M7): the free viewer for the artists, performers and
//! other creatives a workspace represents. Staff invite a talent from their
//! record; the talent then sees their own dates, times, travel and
//! documents, answers dates, keeps their availability (or connects their
//! own calendar) and, when the workspace allows, sees fees and statements.
//!
//! Every portal read goes through `core_db::portal`, which narrows to the
//! talent records opened for the signed-in user; what else they may see is
//! the workspace's [`PortalSettings`].

use chrono::{DateTime, Duration, NaiveDate, Utc};
use core_db::Tx;
use core_domain::{BookingStatus, Ctx, Money, Permission, Role, UserId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::availability::{AvailabilityView, NewAvailability};
use super::crm::Check;
use super::files;
use super::talent_calendars::{self, CreatedConnectLink, TalentCalendarView};
use super::{Change, begin, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// What a workspace lets its talent see and do in the portal. Everything
/// is a switch the owner or an admin sets; a cautious agency can start with
/// read-only dates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PortalSettings {
    /// Fees on their dates. Off until the agency turns it on.
    pub show_fees: bool,
    /// Holds (pencilled dates) as well as pending and confirmed ones.
    pub show_holds: bool,
    /// Confirm or decline a date; the agent is told either way.
    pub answer_dates: bool,
    /// Add and remove their own unavailable or tentative times, and connect
    /// their own calendar.
    pub edit_availability: bool,
    /// Contracts, advance sheets, riders and the like on their dates and record.
    pub show_documents: bool,
    /// Flights, trains, rides and hotels on their dates (never what they cost).
    pub show_travel: bool,
    /// Their statements: what each date paid them, and what was paid out.
    pub show_money: bool,
    /// Hide the paid features shown locked in the free viewer.
    pub hide_locked: bool,
    /// Write to the agency from the portal; the conversation sits in the
    /// agency's inbox.
    pub messages: bool,
    /// Change their own bio and where they are based.
    pub edit_profile: bool,
}

impl Default for PortalSettings {
    fn default() -> Self {
        Self {
            show_fees: false,
            show_holds: true,
            answer_dates: true,
            edit_availability: true,
            show_documents: true,
            show_travel: true,
            show_money: false,
            hide_locked: false,
            messages: true,
            edit_profile: true,
        }
    }
}

impl PortalSettings {
    /// The stored switches; unknown keys are ignored and missing ones default.
    pub fn from_json(v: &Value) -> Self {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }
}

async fn settings_in(tx: &mut Tx) -> ApiResult<PortalSettings> {
    Ok(PortalSettings::from_json(
        &core_db::portal::settings(tx).await?,
    ))
}

// For the agency -------------------------------------------------------------------------------

pub async fn get_settings(state: &AppState, ctx: &Ctx) -> ApiResult<PortalSettings> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let s = settings_in(&mut tx).await?;
    tx.commit().await?;
    Ok(s)
}

pub async fn update_settings(
    state: &AppState,
    ctx: &Ctx,
    new: PortalSettings,
) -> ApiResult<PortalSettings> {
    ctx.authorize(Permission::WorkspaceUpdate)?;
    let mut tx = begin(state, ctx).await?;
    let before = settings_in(&mut tx).await?;
    let stored = serde_json::to_value(&new).map_err(ApiError::internal)?;
    core_db::portal::set_settings(&mut tx, ctx.workspace_id, &stored, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "workspace.portal_updated",
            resource: "workspace",
            object_id: Some(ctx.workspace_id.0),
            before: Some(json!(before)),
            after: Some(stored),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(new)
}

/// Who a talent record's portal opens for, and any invitation still waiting.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalAccess {
    pub user: Option<PortalUser>,
    pub invitation: Option<PendingInvitation>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalUser {
    pub id: Uuid,
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PendingInvitation {
    pub email: String,
    pub expires_at: DateTime<Utc>,
}

async fn access_in(tx: &mut Tx, talent_id: Uuid) -> ApiResult<PortalAccess> {
    let talent = core_db::talent::get(tx, talent_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let user = match talent.portal_user_id {
        Some(id) => core_db::members::list(tx)
            .await?
            .into_iter()
            .find(|m| m.user_id.0 == id)
            .map(|m| PortalUser {
                id,
                name: m.name,
                email: m.email,
            }),
        None => None,
    };
    let invitation = core_db::portal::open_invitation(tx, talent_id)
        .await?
        .map(|(email, expires_at)| PendingInvitation { email, expires_at });
    Ok(PortalAccess { user, invitation })
}

pub async fn access(state: &AppState, ctx: &Ctx, talent_id: Uuid) -> ApiResult<PortalAccess> {
    ctx.authorize(Permission::TalentRead)?;
    let mut tx = begin(state, ctx).await?;
    let a = access_in(&mut tx, talent_id).await?;
    tx.commit().await?;
    Ok(a)
}

/// Invites a talent to the portal by email, for this talent record. An
/// earlier invitation still waiting is replaced. Whoever looks after the
/// talent's record may do this (agents too): the membership it makes sees
/// only that record's portal.
pub async fn invite(
    state: &AppState,
    ctx: &Ctx,
    talent_id: Uuid,
    email: &str,
) -> ApiResult<PortalAccess> {
    ctx.authorize(Permission::TalentWrite)?;
    let email = super::tenancy::validate_email(email)?;
    let mut tx = begin(state, ctx).await?;
    let talent = core_db::talent::get(&mut tx, talent_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if talent.portal_user_id.is_some() {
        return Err(ApiError::conflict(
            "portal_open",
            "this talent's portal is already open; remove that access first to invite someone else",
        ));
    }
    check_existing_member(&mut tx, &email).await?;
    let workspace = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    core_db::portal::revoke_invitations(&mut tx, talent_id).await?;
    super::tenancy::invite_in_tx(
        state,
        &mut tx,
        ctx,
        &email,
        Role::Talent,
        Some(talent_id),
        &workspace.name,
    )
    .await?;
    let a = access_in(&mut tx, talent_id).await?;
    tx.commit().await?;
    Ok(a)
}

/// Ends a talent record's portal access and cancels waiting invitations.
/// A talent member left with no talent record here leaves the workspace.
pub async fn remove_access(state: &AppState, ctx: &Ctx, talent_id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::TalentWrite)?;
    let mut tx = begin(state, ctx).await?;
    core_db::talent::get(&mut tx, talent_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !end_access_in(&mut tx, ctx, talent_id).await? {
        return Err(ApiError::NotFound);
    }
    tx.commit().await?;
    Ok(())
}

/// Ends a talent record's portal: waiting invitations are cancelled and
/// whoever it opened for is unlinked; a talent member left with no record
/// leaves the workspace. False when there was nothing to end. Also used
/// when the record itself is deleted.
pub(crate) async fn end_access_in(tx: &mut Tx, ctx: &Ctx, talent_id: Uuid) -> ApiResult<bool> {
    let revoked = core_db::portal::revoke_invitations(tx, talent_id).await?;
    let user = core_db::portal::unlink_talent(tx, talent_id, ctx.user_id()).await?;
    if let Some(user) = user {
        record(
            tx,
            ctx,
            Change {
                topic: "talent.portal_unlinked",
                resource: "talent",
                object_id: Some(talent_id),
                before: Some(json!({ "portal_user_id": user })),
                after: Some(json!({ "portal_user_id": null })),
                event_extra: None,
            },
        )
        .await?;
        let user = UserId(user);
        let is_talent_member = core_db::members::role_of(tx, user).await? == Some(Role::Talent);
        if is_talent_member && core_db::portal::talent_count(tx, user.0).await? == 0 {
            core_db::members::soft_delete(tx, user, ctx.user_id()).await?;
            record(
                tx,
                ctx,
                Change {
                    topic: "member.removed",
                    resource: "member",
                    object_id: Some(user.0),
                    before: Some(json!({ "role": Role::Talent })),
                    after: None,
                    event_extra: Some(json!({ "via": "portal_access_removed" })),
                },
            )
            .await?;
        }
    }
    Ok(user.is_some() || revoked > 0)
}

/// Refuses a portal for someone already in the workspace whose role does
/// not open the portal (an agent, say): the link would lead nowhere.
pub(crate) async fn check_existing_member(tx: &mut Tx, email: &str) -> ApiResult<()> {
    let member = core_db::members::list(tx)
        .await?
        .into_iter()
        .find(|m| m.email.eq_ignore_ascii_case(email));
    if let Some(m) = member
        && !core_db::workspaces::role_permissions(tx, m.role)
            .await?
            .contains(&Permission::PortalUse)
    {
        return Err(ApiError::conflict(
            "already_member",
            format!(
                "{email} is already in this workspace as {}, which has no talent portal",
                m.role
            ),
        ));
    }
    Ok(())
}

// For the talent -------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalTalent {
    pub id: Uuid,
    pub name: String,
}

/// What the portal opens with.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalHome {
    pub workspace_name: String,
    /// The workspace's logo and colours, for the portal's look.
    #[schema(value_type = Object)]
    pub branding: Value,
    pub settings: PortalSettings,
    /// The talent records opened for this person here (usually one; none
    /// once the agency ends their access).
    pub talent: Vec<PortalTalent>,
    /// Dates waiting for their answer.
    pub awaiting_answer: i64,
    /// The website page where they can upgrade to the full app; none until
    /// OnHold's team sets one, or when the agency hides locked features.
    pub upgrade_url: Option<String>,
    /// Messages from the agency they have not read yet.
    pub unread_messages: i64,
}

/// The talent records opened for the signed-in person (none for staff, keys
/// and anyone whose access ended, who then see an empty portal).
async fn mine(tx: &mut Tx) -> ApiResult<Vec<(Uuid, String)>> {
    Ok(core_db::portal::my_talent(tx).await?)
}

pub async fn home(state: &AppState, ctx: &Ctx) -> ApiResult<PortalHome> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    let talent = mine(&mut tx).await?;
    let settings = settings_in(&mut tx).await?;
    let workspace = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let awaiting_answer = if settings.answer_dates {
        core_db::portal::dates(
            &mut tx,
            Some(Utc::now()),
            None,
            &answerable(&settings),
            false,
            MAX_DATES,
        )
        .await?
        .iter()
        .filter(|d| d.answer.is_none())
        .count() as i64
    } else {
        0
    };
    let unread_messages = if settings.messages {
        core_db::portal::unread(&mut tx).await?
    } else {
        0
    };
    let upgrade_url = if settings.hide_locked {
        None
    } else {
        super::setup::upgrade_url(&mut tx).await?
    };
    tx.commit().await?;
    Ok(PortalHome {
        workspace_name: workspace.name,
        branding: workspace.branding,
        upgrade_url,
        unread_messages,
        settings,
        talent: talent
            .into_iter()
            .map(|(id, name)| PortalTalent { id, name })
            .collect(),
        awaiting_answer,
    })
}

const MAX_DATES: i64 = 500;

/// The statuses the portal shows.
fn shown(s: &PortalSettings) -> Vec<BookingStatus> {
    let mut v = vec![
        BookingStatus::Pending,
        BookingStatus::Confirmed,
        BookingStatus::Completed,
        BookingStatus::Cancelled,
    ];
    if s.show_holds {
        v.push(BookingStatus::Hold);
    }
    v
}

/// The statuses a talent may answer.
fn answerable(s: &PortalSettings) -> Vec<BookingStatus> {
    shown(s)
        .into_iter()
        .filter(|st| {
            matches!(
                st,
                BookingStatus::Hold | BookingStatus::Pending | BookingStatus::Confirmed
            )
        })
        .collect()
}

/// One of the talent's dates.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalDate {
    pub id: Uuid,
    /// Which of their talent records it is for.
    pub talent_id: Uuid,
    pub talent_name: String,
    pub title: String,
    #[schema(value_type = String)]
    pub status: BookingStatus,
    pub venue: Option<String>,
    /// Who booked them (the promoter, brand or venue).
    pub organisation_name: Option<String>,
    /// The IANA zone the date's times are shown in.
    pub timezone: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    /// Load-in, soundcheck, set times: `[{label, at}]`.
    pub schedule: Value,
    /// When a hold lapses by itself.
    pub hold_expires_at: Option<DateTime<Utc>>,
    /// Only when the workspace shows fees.
    pub fee: Option<Money>,
    /// `confirmed` or `declined`, once they answered.
    pub answer: Option<String>,
    pub answer_note: Option<String>,
    pub answered_at: Option<DateTime<Utc>>,
    /// Whether they may answer (or change their answer) now.
    pub can_answer: bool,
}

fn date_view(d: core_db::portal::Date, s: &PortalSettings) -> PortalDate {
    // A fee shared by several acts is not any one act's to see.
    let fee = match (s.show_fees && d.acts == 1, d.fee_minor, d.fee_currency) {
        (true, Some(minor), Some(currency)) => Some(Money { minor, currency }),
        _ => None,
    };
    let can_answer = s.answer_dates && answerable(s).contains(&d.status) && d.ends_at > Utc::now();
    PortalDate {
        id: d.id,
        talent_id: d.talent_id,
        talent_name: d.talent_name,
        title: d.title,
        status: d.status,
        venue: d.venue,
        organisation_name: d.organisation_name,
        timezone: d.timezone,
        starts_at: d.starts_at,
        ends_at: d.ends_at,
        all_day: d.all_day,
        schedule: d.schedule,
        hold_expires_at: d.hold_expires_at,
        fee,
        answer: d.answer,
        answer_note: d.answer_note,
        answered_at: d.answered_at,
        can_answer,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum When {
    /// From today on, soonest first.
    #[default]
    Upcoming,
    /// Before today, latest first (the last two years).
    Past,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct DatesQuery {
    /// `upcoming` (default) or `past`.
    #[param(inline)]
    pub when: Option<When>,
}

pub async fn dates(state: &AppState, ctx: &Ctx, q: &DatesQuery) -> ApiResult<Vec<PortalDate>> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    mine(&mut tx).await?;
    let s = settings_in(&mut tx).await?;
    let now = Utc::now();
    let rows = match q.when.unwrap_or_default() {
        When::Upcoming => {
            core_db::portal::dates(&mut tx, Some(now), None, &shown(&s), false, MAX_DATES).await?
        }
        When::Past => core_db::portal::dates(
            &mut tx,
            Some(now - Duration::days(731)),
            Some(now),
            &shown(&s),
            true,
            MAX_DATES,
        )
        .await?
        .into_iter()
        .filter(|d| d.ends_at <= now)
        .collect(),
    };
    tx.commit().await?;
    Ok(rows.into_iter().map(|d| date_view(d, &s)).collect())
}

/// Travel as the talent sees it: where and when, never what it cost.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalTravel {
    pub id: Uuid,
    /// `flight`, `train`, `bus`, `car`, `ferry`, `taxi`, `hotel` or `other`.
    pub kind: String,
    /// `planned`, `booked` or `cancelled`.
    pub status: String,
    pub carrier: Option<String>,
    pub number: Option<String>,
    pub reference: Option<String>,
    pub from_place: Option<String>,
    pub from_code: Option<String>,
    pub to_place: Option<String>,
    pub to_code: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub start_tz: String,
    pub ends_at: Option<DateTime<Utc>>,
    pub end_tz: Option<String>,
    pub travellers: Vec<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalDocument {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    /// `booking` or `talent`.
    pub related_entity: String,
    pub related_id: Uuid,
    pub expires_on: Option<NaiveDate>,
    /// `none`, `awaiting`, `signed` or `declined`.
    pub signature_status: String,
    pub updated_at: DateTime<Utc>,
}

impl From<core_db::portal::Document> for PortalDocument {
    fn from(d: core_db::portal::Document) -> Self {
        Self {
            id: d.id,
            kind: d.kind,
            title: d.title,
            related_entity: d.related_entity,
            related_id: d.related_id,
            expires_on: d.expires_on,
            signature_status: d.signature_status,
            updated_at: d.updated_at,
        }
    }
}

/// A date with everything the talent needs for it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalDateDetail {
    /// The date, once for each of their talent records on it.
    pub entries: Vec<PortalDate>,
    pub travel: Vec<PortalTravel>,
    pub documents: Vec<PortalDocument>,
}

pub async fn date(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<PortalDateDetail> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    mine(&mut tx).await?;
    let s = settings_in(&mut tx).await?;
    let detail = detail_in(&mut tx, id, &s).await?;
    tx.commit().await?;
    Ok(detail)
}

async fn detail_in(tx: &mut Tx, id: Uuid, s: &PortalSettings) -> ApiResult<PortalDateDetail> {
    let shown = shown(s);
    let rows: Vec<core_db::portal::Date> = core_db::portal::date(tx, id)
        .await?
        .into_iter()
        .filter(|d| shown.contains(&d.status))
        .collect();
    // On a booking with other acts, only travel naming one of theirs.
    let shared = rows.first().is_some_and(|d| d.acts > rows.len() as i64);
    let names: Vec<String> = rows.iter().map(|d| d.talent_name.to_lowercase()).collect();
    let entries: Vec<PortalDate> = rows.into_iter().map(|d| date_view(d, s)).collect();
    if entries.is_empty() {
        return Err(ApiError::NotFound);
    }
    let travel = if s.show_travel {
        core_db::travel::list(tx, &[id], None)
            .await?
            .into_iter()
            .filter(|t| !shared || travels_with(&t.travellers, &names))
            .map(|t| PortalTravel {
                id: t.id,
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
            })
            .collect()
    } else {
        Vec::new()
    };
    let documents = if s.show_documents {
        core_db::portal::documents(tx, &[id], core_db::portal::date_kinds(s.show_fees))
            .await?
            .into_iter()
            .filter(|d| d.related_entity == "booking")
            .map(Into::into)
            .collect()
    } else {
        Vec::new()
    };
    Ok(PortalDateDetail {
        entries,
        travel,
        documents,
    })
}

/// Whether a travel item names one of these acts among its travellers.
fn travels_with(travellers: &[String], names: &[String]) -> bool {
    travellers.iter().any(|t| {
        let t = t.to_lowercase();
        names.iter().any(|n| t.contains(n.as_str()))
    })
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AnswerInput {
    /// `confirmed` or `declined`.
    #[serde(default)]
    pub answer: String,
    /// Which of their talent records answers; needed only when several of
    /// theirs are on the date.
    pub talent_id: Option<Uuid>,
    /// A few words for the agent, e.g. why they cannot do it.
    pub note: Option<String>,
}

/// The talent confirms or declines a date. The booking's own status stays
/// the agency's to move; the agent gets a task saying what the talent said.
pub async fn answer(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    input: AnswerInput,
) -> ApiResult<PortalDateDetail> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    mine(&mut tx).await?;
    let s = settings_in(&mut tx).await?;
    // A date that is not theirs does not exist, whatever the body says.
    let before = detail_in(&mut tx, id, &s).await?;
    let user = ctx
        .user_id()
        .ok_or_else(|| ApiError::forbidden("person_required", "only the talent can answer"))?;
    let mut check = Check::default();
    let answer = input.answer.trim().to_owned();
    if answer != "confirmed" && answer != "declined" {
        check.issue("answer", "must be confirmed or declined");
    }
    let note = check.optional("note", input.note.as_deref(), 500);
    check.finish()?;
    if !s.answer_dates {
        return Err(ApiError::forbidden(
            "portal_switched_off",
            "your agency answers dates for you here",
        ));
    }
    let entry = match input.talent_id {
        Some(t) => before.entries.iter().find(|e| e.talent_id == t),
        None if before.entries.len() == 1 => before.entries.first(),
        None => {
            return Err(ApiError::invalid(
                "talent_id",
                "say which of your acts is answering",
            ));
        }
    }
    .ok_or(ApiError::NotFound)?
    .clone();
    if !entry.can_answer {
        return Err(ApiError::conflict(
            "not_answerable",
            "this date can no longer be answered",
        ));
    }
    // The same answer sent again (a double tap, a retry) changes nothing
    // and gives the agent no second task.
    if entry.answer.as_deref() == Some(answer.as_str()) && entry.answer_note == note {
        tx.commit().await?;
        return Ok(before);
    }
    if !core_db::portal::set_answer(&mut tx, id, entry.talent_id, &answer, note.as_deref(), user)
        .await?
    {
        return Err(ApiError::NotFound);
    }
    // The agent hears about it: a task on the booking, due now.
    let assignee = core_db::portal::date_owner(&mut tx, id).await?;
    let verb = if answer == "confirmed" {
        "confirmed"
    } else {
        "declined"
    };
    let title = format!("{} {} {}", entry.talent_name, verb, entry.title);
    let title: String = title.chars().take(300).collect();
    let task_id = core_domain::ids::new_id();
    core_db::tasks::insert(
        &mut tx,
        task_id,
        core_db::tasks::Values {
            title: &title,
            notes: note.as_deref(),
            due_at: Some(Utc::now()),
            assignee_id: assignee,
            related_entity: Some("booking"),
            related_id: Some(id),
            status: "open",
            done_at: None,
            done_by: None,
        },
        None,
        Some(user),
    )
    .await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "booking.talent_answered",
            resource: "booking",
            object_id: Some(id),
            before: Some(json!({ "talent_id": entry.talent_id, "answer": entry.answer })),
            after: Some(json!({ "talent_id": entry.talent_id, "answer": answer, "note": note, "task_id": task_id })),
            event_extra: None,
        },
    )
    .await?;
    let after = detail_in(&mut tx, id, &s).await?;
    tx.commit().await?;
    Ok(after)
}

/// How far ahead the portal lists availability: holidays and tours are
/// often planned well over a year out.
const AVAILABILITY_DAYS_AHEAD: i64 = 3 * 366;

/// The talent's availability and busy times from their own calendars.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalAvailability {
    /// From a month ago to three years ahead.
    pub entries: Vec<AvailabilityView>,
    /// Their connected calendars.
    pub calendars: Vec<TalentCalendarView>,
    /// Whether they may change it here.
    pub can_edit: bool,
}

pub async fn availability(state: &AppState, ctx: &Ctx) -> ApiResult<PortalAvailability> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    let ids: Vec<Uuid> = mine(&mut tx).await?.into_iter().map(|(id, _)| id).collect();
    let s = settings_in(&mut tx).await?;
    let from = Utc::now() - Duration::days(31);
    let to = Utc::now() + Duration::days(AVAILABILITY_DAYS_AHEAD);
    // An empty list would mean every talent's.
    let entries = if ids.is_empty() {
        Vec::new()
    } else {
        core_db::availability::list(&mut tx, &ids, from, to, &[])
            .await?
            .into_iter()
            .map(Into::into)
            .collect()
    };
    let mut calendars = Vec::new();
    for id in &ids {
        calendars.extend(
            core_db::talent_calendars::list(&mut tx, Some(*id))
                .await?
                .iter()
                .map(TalentCalendarView::from),
        );
    }
    tx.commit().await?;
    Ok(PortalAvailability {
        entries,
        calendars,
        can_edit: s.edit_availability,
    })
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PortalNewAvailability {
    /// Which of their talent records; needed only when they have several.
    pub talent_id: Option<Uuid>,
    #[serde(flatten)]
    pub entry: NewAvailability,
}

/// The one of `mine` a request names, or the only one.
fn pick(mine: &[(Uuid, String)], talent_id: Option<Uuid>) -> ApiResult<Uuid> {
    match talent_id {
        Some(t) if mine.iter().any(|(id, _)| *id == t) => Ok(t),
        Some(_) => Err(ApiError::NotFound),
        None if mine.len() == 1 => Ok(mine[0].0),
        None => Err(ApiError::invalid(
            "talent_id",
            "say which of your acts this is for",
        )),
    }
}

async fn may_edit_availability(tx: &mut Tx) -> ApiResult<()> {
    if settings_in(tx).await?.edit_availability {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "portal_switched_off",
            "your agency keeps your availability for you here",
        ))
    }
}

pub async fn add_availability(
    state: &AppState,
    ctx: &Ctx,
    input: PortalNewAvailability,
) -> ApiResult<AvailabilityView> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    let talent_id = pick(&mine(&mut tx).await?, input.talent_id)?;
    may_edit_availability(&mut tx).await?;
    let view =
        super::availability::insert_in(&mut tx, ctx, talent_id, "portal", input.entry).await?;
    tx.commit().await?;
    Ok(view)
}

/// Removes an entry the talent added here. Entries the agency added stay
/// the agency's.
pub async fn remove_availability(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    let ids: Vec<Uuid> = mine(&mut tx).await?.into_iter().map(|(id, _)| id).collect();
    may_edit_availability(&mut tx).await?;
    let entry = core_db::availability::get(&mut tx, id)
        .await?
        .filter(|a| ids.contains(&a.talent_id))
        .ok_or(ApiError::NotFound)?;
    if entry.source != "portal" {
        return Err(ApiError::conflict(
            "agency_entry",
            "your agency added this; ask them to change it",
        ));
    }
    super::availability::delete_in(&mut tx, ctx, id).await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct PortalCalendarLink {
    /// Which of their talent records; needed only when they have several.
    pub talent_id: Option<Uuid>,
}

/// A link to connect the talent's own calendar (Google, Microsoft or an
/// iCal link), the same one an agency sends.
pub async fn calendar_link(
    state: &AppState,
    ctx: &Ctx,
    input: PortalCalendarLink,
) -> ApiResult<CreatedConnectLink> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    let talent_id = pick(&mine(&mut tx).await?, input.talent_id)?;
    may_edit_availability(&mut tx).await?;
    let link = talent_calendars::make_link(state, &mut tx, ctx, talent_id).await?;
    tx.commit().await?;
    Ok(link)
}

pub async fn documents(state: &AppState, ctx: &Ctx) -> ApiResult<Vec<PortalDocument>> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    mine(&mut tx).await?;
    let s = settings_in(&mut tx).await?;
    if !s.show_documents {
        tx.commit().await?;
        return Ok(Vec::new());
    }
    let now = Utc::now();
    let ids: Vec<Uuid> = core_db::portal::dates(
        &mut tx,
        Some(now - Duration::days(92)),
        None,
        &shown(&s),
        false,
        MAX_DATES,
    )
    .await?
    .into_iter()
    .map(|d| d.id)
    .collect();
    let docs =
        core_db::portal::documents(&mut tx, &ids, core_db::portal::date_kinds(s.show_fees)).await?;
    tx.commit().await?;
    Ok(docs.into_iter().map(Into::into).collect())
}

/// The current file of a document the portal shows.
pub async fn download(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<files::Download> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    mine(&mut tx).await?;
    let s = settings_in(&mut tx).await?;
    let may = core_db::portal::may_open_document(
        &mut tx,
        id,
        core_db::portal::date_kinds(s.show_fees),
        &shown(&s),
        s.show_documents,
        s.show_money,
    )
    .await?;
    if !may {
        return Err(ApiError::NotFound);
    }
    let doc = core_db::documents::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let v = core_db::documents::version(&mut tx, id, doc.current_version)
        .await?
        .ok_or(ApiError::NotFound)?;
    let file_id = v.file_id.ok_or_else(|| {
        ApiError::conflict("document_not_ready", "this document is not ready yet")
    })?;
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

/// What a date paid the talent.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalStatement {
    pub id: Uuid,
    pub booking_id: Uuid,
    pub booking_title: String,
    pub starts_at: DateTime<Utc>,
    pub talent_id: Uuid,
    pub talent_name: String,
    pub gross: Money,
    pub commission: Money,
    pub expenses: Money,
    pub net: Money,
    /// Paid out to them so far.
    pub paid: Money,
    pub finalised_at: Option<DateTime<Utc>>,
    /// The statement's PDF, opened through the portal's documents.
    pub document_id: Option<Uuid>,
}

pub async fn money(state: &AppState, ctx: &Ctx) -> ApiResult<Vec<PortalStatement>> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    mine(&mut tx).await?;
    // Not shared: nothing to show (the portal hides the page too).
    if !settings_in(&mut tx).await?.show_money {
        tx.commit().await?;
        return Ok(Vec::new());
    }
    let rows = core_db::portal::statements(&mut tx, 200).await?;
    tx.commit().await?;
    let m = |minor: i64, c: &str| Money {
        minor,
        currency: c.to_owned(),
    };
    Ok(rows
        .into_iter()
        .map(|r| PortalStatement {
            id: r.id,
            booking_id: r.booking_id,
            booking_title: r.booking_title,
            starts_at: r.starts_at,
            talent_id: r.talent_id,
            talent_name: r.talent_name,
            gross: m(r.gross_minor, &r.currency),
            commission: m(r.commission_minor, &r.currency),
            expenses: m(r.expenses_minor, &r.currency),
            net: m(r.net_minor, &r.currency),
            paid: m(r.paid_minor, &r.currency),
            finalised_at: r.finalised_at,
            document_id: r.document_id,
        })
        .collect())
}

// Messages ------------------------------------------------------------------------------------

/// The longest message either side may write.
const MESSAGE_MAX: usize = 4000;
/// Messages the portal shows, the latest ones.
const MESSAGES_SHOWN: i64 = 200;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalMessage {
    pub id: Uuid,
    /// Written by the talent (else by someone at the agency).
    pub from_me: bool,
    pub author_name: Option<String>,
    pub body: String,
    pub sent_at: DateTime<Utc>,
}

/// A talent record's conversation with the agency.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalConversation {
    /// None for someone with no talent record open here.
    pub talent_id: Option<Uuid>,
    pub talent_name: String,
    /// The latest messages, oldest first.
    pub messages: Vec<PortalMessage>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct MessagesQuery {
    /// Which of their talent records; needed only when they have several.
    pub talent_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PortalMessageInput {
    pub talent_id: Option<Uuid>,
    #[serde(default)]
    pub body: String,
}

fn message_view(m: core_db::portal::Message) -> PortalMessage {
    PortalMessage {
        id: m.id,
        from_me: m.inbound,
        author_name: m.author_name,
        body: m.body,
        sent_at: m.sent_at,
    }
}

async fn messages_switched_on(tx: &mut Tx) -> ApiResult<()> {
    if settings_in(tx).await?.messages {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "portal_switched_off",
            "your agency does not take messages here",
        ))
    }
}

/// The talent's conversation with the agency; opening it marks it read.
pub async fn messages(
    state: &AppState,
    ctx: &Ctx,
    q: &MessagesQuery,
) -> ApiResult<PortalConversation> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    let mine = mine(&mut tx).await?;
    if mine.is_empty() && q.talent_id.is_none() {
        tx.commit().await?;
        return Ok(PortalConversation {
            talent_id: None,
            talent_name: String::new(),
            messages: Vec::new(),
        });
    }
    let talent_id = pick(&mine, q.talent_id)?;
    messages_switched_on(&mut tx).await?;
    let messages = match core_db::portal::thread_of(&mut tx, talent_id).await? {
        Some(thread) => {
            let messages = core_db::portal::messages(&mut tx, thread, MESSAGES_SHOWN).await?;
            // Read up to the latest they are shown, not to the clock: a
            // reply still being written is not passed over.
            if let Some(last) = messages.iter().map(|m| m.sent_at).max() {
                core_db::portal::mark_read(&mut tx, thread, last).await?;
            }
            messages
        }
        None => Vec::new(),
    };
    tx.commit().await?;
    Ok(PortalConversation {
        talent_id: Some(talent_id),
        talent_name: name_of(&mine, talent_id),
        messages: messages.into_iter().map(message_view).collect(),
    })
}

fn name_of(mine: &[(Uuid, String)], talent_id: Uuid) -> String {
    mine.iter()
        .find(|(id, _)| *id == talent_id)
        .map(|(_, n)| n.clone())
        .unwrap_or_default()
}

/// The talent writes to the agency. The conversation opens (or reopens)
/// unread in the agency's inbox.
pub async fn send_message(
    state: &AppState,
    ctx: &Ctx,
    input: PortalMessageInput,
) -> ApiResult<PortalMessage> {
    ctx.authorize(Permission::PortalUse)?;
    let user = ctx
        .user_id()
        .ok_or_else(|| ApiError::forbidden("person_required", "only the talent can write here"))?;
    let mut tx = begin(state, ctx).await?;
    let mine = mine(&mut tx).await?;
    let talent_id = pick(&mine, input.talent_id)?;
    messages_switched_on(&mut tx).await?;
    let body = super::required_text("body", &input.body, MESSAGE_MAX)?;
    let me = core_db::members::list(&mut tx)
        .await?
        .into_iter()
        .find(|m| m.user_id == user)
        .ok_or(ApiError::NotFound)?;
    let talent_name = name_of(&mine, talent_id);
    let thread = core_db::portal::open_thread(&mut tx, talent_id, user, &talent_name).await?;
    core_db::inbox::lock(&mut tx, thread).await?;
    let id = core_domain::ids::new_id();
    let now = Utc::now();
    core_db::inbox::insert_message(
        &mut tx,
        core_db::inbox::NewMessage {
            id,
            thread_id: thread,
            direction: "inbound",
            integration_id: None,
            external_id: None,
            message_id: None,
            in_reply_to: None,
            references: &[],
            from_address: Some(&me.email),
            from_name: Some(&me.name),
            reply_to: None,
            to_addresses: &[],
            cc_addresses: &[],
            subject: None,
            body_text: &body,
            body_html: None,
            sent_at: now,
            status: "received",
            automatic: false,
            bulk: false,
            author_id: Some(user),
            template_key: None,
        },
    )
    .await?;
    core_db::inbox::refresh(&mut tx, thread, true).await?;
    core_db::portal::mark_read(&mut tx, thread, now).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "message.received",
            resource: super::inbox::ENTITY,
            object_id: Some(thread),
            before: None,
            after: Some(json!({
                "message_id": id,
                "channel": "portal",
                "talent_id": talent_id,
            })),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(PortalMessage {
        id,
        from_me: true,
        author_name: Some(me.name),
        body,
        sent_at: now,
    })
}

/// Someone at the agency writes in a talent's conversation (a reply in the
/// inbox, or a first message from the talent's record). It reaches the
/// portal at once, and the talent gets an email saying so.
pub(crate) async fn reply_in(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    thread: Uuid,
    r: super::inbox::Reply,
) -> ApiResult<Uuid> {
    if !r.attachments.is_empty() || !r.cc.is_empty() || r.to.as_ref().is_some_and(|t| !t.is_empty())
    {
        return Err(ApiError::invalid(
            "body",
            "a portal message is text only and goes to the talent",
        ));
    }
    let body = match (
        r.body.as_deref().map(str::trim).filter(|b| !b.is_empty()),
        &r.template_key,
    ) {
        (Some(b), None) => b.to_owned(),
        _ => return Err(ApiError::invalid("body", "write a message")),
    };
    if body.chars().count() > MESSAGE_MAX {
        return Err(ApiError::invalid(
            "body",
            format!("must be at most {MESSAGE_MAX} characters"),
        ));
    }
    let (talent_id, talent_name, with, current) = core_db::portal::thread_talent(tx, thread)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !current {
        return Err(ApiError::conflict(
            "portal_access_ended",
            "this conversation was with someone whose portal access has ended",
        ));
    }
    if !settings_in(tx).await?.messages {
        return Err(ApiError::forbidden(
            "portal_switched_off",
            "portal messages are switched off in Settings, Talent portal",
        ));
    }
    let members = core_db::members::list(tx).await?;
    let to = members.iter().find(|m| m.user_id == with).ok_or_else(|| {
        ApiError::conflict(
            "no_portal",
            "their portal is not open; invite them from their record first",
        )
    })?;
    let author = ctx
        .user_id()
        .and_then(|u| members.iter().find(|m| m.user_id == u));
    let author_name = author.map(|m| m.name.clone());
    let id = core_domain::ids::new_id();
    core_db::inbox::insert_message(
        tx,
        core_db::inbox::NewMessage {
            id,
            thread_id: thread,
            direction: "outbound",
            integration_id: None,
            external_id: None,
            message_id: None,
            in_reply_to: None,
            references: &[],
            from_address: None,
            from_name: author_name.as_deref(),
            reply_to: None,
            to_addresses: std::slice::from_ref(&to.email),
            cc_addresses: &[],
            subject: None,
            body_text: &body,
            body_html: None,
            sent_at: Utc::now(),
            status: "sent",
            automatic: false,
            bulk: false,
            author_id: ctx.user_id(),
            template_key: None,
        },
    )
    .await?;
    core_db::inbox::refresh(tx, thread, false).await?;
    record(
        tx,
        ctx,
        Change {
            topic: "thread.replied",
            resource: super::inbox::ENTITY,
            object_id: Some(thread),
            before: None,
            after: Some(json!({ "message_id": id, "channel": "portal", "talent_id": talent_id })),
            event_extra: None,
        },
    )
    .await?;
    let preview: String = body.chars().take(300).collect();
    let url = state
        .config
        .public_base_url
        .join("/portal/messages")
        .map_err(ApiError::internal)?;
    super::record_with_secret(
        tx,
        ctx,
        Change {
            topic: "portal.message_sent",
            resource: super::inbox::ENTITY,
            object_id: Some(thread),
            before: None,
            after: Some(json!({ "message_id": id, "talent_id": talent_id })),
            event_extra: Some(json!({
                "to": to.email,
                "message": {
                    "talent": talent_name,
                    "sender": author_name,
                    "preview": preview,
                },
            })),
        },
        &state.config.keyring,
        &json!({ "url": url.as_str() }),
    )
    .await?;
    Ok(id)
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AgencyMessageInput {
    #[serde(default)]
    pub body: String,
}

/// From the talent's record: write to them, starting the conversation if
/// it has not begun. Returns the conversation's id in the inbox.
pub async fn write_to_talent(
    state: &AppState,
    ctx: &Ctx,
    talent_id: Uuid,
    input: AgencyMessageInput,
) -> ApiResult<Uuid> {
    ctx.authorize(Permission::InboxWrite)?;
    let mut tx = begin(state, ctx).await?;
    let talent = core_db::talent::get(&mut tx, talent_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let Some(with) = talent.portal_user_id else {
        return Err(ApiError::conflict(
            "no_portal",
            "their portal is not open; invite them first",
        ));
    };
    let thread =
        core_db::portal::open_thread(&mut tx, talent_id, UserId(with), &talent.name).await?;
    core_db::inbox::lock(&mut tx, thread).await?;
    reply_in(
        state,
        &mut tx,
        ctx,
        thread,
        super::inbox::Reply {
            body: Some(input.body),
            template_key: None,
            to: None,
            cc: Vec::new(),
            attachments: Vec::new(),
            signature: false,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(thread)
}

// Profile -------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PortalProfile {
    pub talent_id: Uuid,
    pub version: i64,
    pub name: String,
    pub bio: Option<String>,
    pub base_location: Option<String>,
    pub can_edit: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PortalProfileInput {
    pub bio: Option<String>,
    pub base_location: Option<String>,
    /// The record's version as read; a change made since is not overwritten.
    pub version: i64,
}

fn profile_view(p: core_db::portal::Profile, can_edit: bool) -> PortalProfile {
    PortalProfile {
        talent_id: p.id,
        version: p.version,
        name: p.name,
        bio: p.bio,
        base_location: p.base_location,
        can_edit,
    }
}

/// The talent's own records, as they may see and change them.
pub async fn profiles(state: &AppState, ctx: &Ctx) -> ApiResult<Vec<PortalProfile>> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    let can_edit = settings_in(&mut tx).await?.edit_profile;
    let out = core_db::portal::profiles(&mut tx).await?;
    tx.commit().await?;
    Ok(out.into_iter().map(|p| profile_view(p, can_edit)).collect())
}

/// The talent changes their bio and where they are based. The record's
/// history shows who changed it.
pub async fn update_profile(
    state: &AppState,
    ctx: &Ctx,
    talent_id: Uuid,
    input: PortalProfileInput,
) -> ApiResult<PortalProfile> {
    ctx.authorize(Permission::PortalUse)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::portal::profiles(&mut tx)
        .await?
        .into_iter()
        .find(|p| p.id == talent_id)
        .ok_or(ApiError::NotFound)?;
    let user = ctx
        .user_id()
        .ok_or_else(|| ApiError::forbidden("person_required", "only the talent can change this"))?;
    let mut check = Check::default();
    let bio = check.long("bio", input.bio.as_deref(), 20000);
    let base_location = check.optional("base_location", input.base_location.as_deref(), 200);
    check.finish()?;
    if !settings_in(&mut tx).await?.edit_profile {
        return Err(ApiError::forbidden(
            "portal_switched_off",
            "your agency keeps your profile for you here",
        ));
    }
    if bio == before.bio && base_location == before.base_location {
        tx.commit().await?;
        return Ok(profile_view(before, true));
    }
    let after = core_db::portal::set_profile(
        &mut tx,
        talent_id,
        bio.as_deref(),
        base_location.as_deref(),
        input.version,
        user,
    )
    .await?
    .ok_or_else(ApiError::version_conflict)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "talent.updated",
            resource: "talent",
            object_id: Some(talent_id),
            before: Some(json!({ "bio": before.bio, "base_location": before.base_location })),
            after: Some(json!({
                "bio": after.bio,
                "base_location": after.base_location,
                "by_talent": true,
            })),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(profile_view(after, true))
}

#[cfg(test)]
mod tests {

    #[test]
    fn shared_travel_is_shown_only_to_the_acts_it_names() {
        let names = vec!["ada".to_owned()];
        assert!(travels_with(&["Ada Lovelace".into()], &names));
        assert!(travels_with(&["Crew".into(), "ADA".into()], &names));
        assert!(!travels_with(&["Grace".into()], &names));
        assert!(!travels_with(&[], &names));
    }

    use super::*;

    #[test]
    fn settings_default_to_cautious_fees_and_open_dates() {
        let s = PortalSettings::from_json(&json!({}));
        assert!(!s.show_fees && !s.show_money && !s.hide_locked);
        assert!(s.answer_dates && s.edit_availability && s.show_documents && s.show_travel);
        let s = PortalSettings::from_json(&json!({ "show_fees": true, "unknown": 1 }));
        assert!(s.show_fees && s.show_holds);
    }

    #[test]
    fn holds_are_answerable_only_when_shown() {
        let mut s = PortalSettings::default();
        assert!(answerable(&s).contains(&BookingStatus::Hold));
        s.show_holds = false;
        assert!(!answerable(&s).contains(&BookingStatus::Hold));
        assert!(!answerable(&s).contains(&BookingStatus::Cancelled));
    }
}
