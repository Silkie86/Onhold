//! The pitch log: what was done to win an organisation over (pitched,
//! followed up, called… from the workspace's own list of actions) and the
//! answers it gave (from its own list of outcomes). Logged by hand here;
//! everything later in pitching (follow-ups, rest periods, the ranking)
//! reads it, so a hand-logged entry counts the same as any other.

use chrono::{DateTime, Duration, Utc};
use core_db::Tx;
use core_db::pitches::Entry;
use core_domain::{Ctx, Permission, UserId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::crm::Check;
use super::organisations::{ItemInput, ItemRef};
use super::pitching::{ItemKind, OutcomeTag, PitchingSettings, settings_in};
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::routes::nullable;

pub const ENTITY: &str = "pitch";
pub const MAX_NOTE: usize = 2000;

/// Whether an entry is something done or an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PitchKind {
    Action,
    Outcome,
}

impl PitchKind {
    fn as_str(self) -> &'static str {
        match self {
            PitchKind::Action => "action",
            PitchKind::Outcome => "outcome",
        }
    }
}

/// How it was done or how the answer came.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PitchChannel {
    Email,
    Phone,
    Text,
    Whatsapp,
    Social,
    InPerson,
    Other,
}

impl PitchChannel {
    fn as_str(self) -> &'static str {
        match self {
            PitchChannel::Email => "email",
            PitchChannel::Phone => "phone",
            PitchChannel::Text => "text",
            PitchChannel::Whatsapp => "whatsapp",
            PitchChannel::Social => "social",
            PitchChannel::InPerson => "in_person",
            PitchChannel::Other => "other",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(Value::String(s.to_owned())).ok()
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PitchView {
    pub id: Uuid,
    pub organisation_id: Uuid,
    /// The person there it was with, if anyone in particular.
    pub contact_id: Option<Uuid>,
    pub contact_name: Option<String>,
    pub kind: PitchKind,
    /// A key from the workspace's actions or outcomes.
    pub type_key: String,
    /// Its label, while the workspace still lists it.
    pub type_label: Option<String>,
    /// For an answer: what it means for pitching them again.
    pub tag: Option<OutcomeTag>,
    pub channel: Option<PitchChannel>,
    /// What was pitched, if it still exists.
    pub item: Option<ItemRef>,
    pub note: Option<String>,
    pub happened_at: DateTime<Utc>,
    /// `manual`: logged by hand.
    pub source: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

fn view(settings: &PitchingSettings, e: Entry) -> PitchView {
    let kind = if e.kind == "outcome" {
        PitchKind::Outcome
    } else {
        PitchKind::Action
    };
    let (type_label, tag) = match kind {
        PitchKind::Action => (settings.action(&e.type_key).map(|a| a.label.clone()), None),
        PitchKind::Outcome => match settings.outcome(&e.type_key) {
            Some(o) => (Some(o.label.clone()), Some(o.tag)),
            None => (None, None),
        },
    };
    let item = match (e.talent_id, e.project_id, e.item_name) {
        (Some(id), _, Some(name)) => Some(ItemRef {
            kind: ItemKind::Talent,
            id,
            name,
        }),
        (None, Some(id), Some(name)) => Some(ItemRef {
            kind: ItemKind::Project,
            id,
            name,
        }),
        _ => None,
    };
    PitchView {
        id: e.id,
        organisation_id: e.organisation_id,
        contact_id: e.contact_id,
        contact_name: e.contact_name,
        kind,
        type_key: e.type_key,
        type_label,
        tag,
        channel: e.channel.as_deref().and_then(PitchChannel::parse),
        item,
        note: e.note,
        happened_at: e.happened_at,
        source: e.source,
        version: e.version,
        created_at: e.created_at,
        updated_at: e.updated_at,
        created_by: e.created_by,
        updated_by: e.updated_by,
    }
}

fn to_json(v: &PitchView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

async fn load(tx: &mut Tx, settings: &PitchingSettings, id: Uuid) -> ApiResult<Option<PitchView>> {
    Ok(core_db::pitches::get(tx, id)
        .await?
        .map(|e| view(settings, e)))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PitchPage {
    pub items: Vec<PitchView>,
    pub total: i64,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct PitchFilter {
    pub offset: Option<i64>,
    /// 1 to 200, default 50.
    pub limit: Option<i64>,
}

/// An organisation's log, latest first.
pub async fn list(
    state: &AppState,
    ctx: &Ctx,
    organisation: Uuid,
    f: &PitchFilter,
) -> ApiResult<PitchPage> {
    ctx.authorize(Permission::OrganisationRead)?;
    let mut tx = begin(state, ctx).await?;
    if core_db::organisations::get(&mut tx, organisation)
        .await?
        .is_none()
    {
        return Err(ApiError::NotFound);
    }
    let (settings, _) = settings_in(&mut tx).await?;
    let (rows, total) = core_db::pitches::list(
        &mut tx,
        organisation,
        f.offset.unwrap_or(0).max(0),
        f.limit.unwrap_or(50).clamp(1, 200),
    )
    .await?;
    tx.commit().await?;
    Ok(PitchPage {
        items: rows.into_iter().map(|e| view(&settings, e)).collect(),
        total,
    })
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewPitch {
    pub organisation_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub kind: PitchKind,
    /// A key from the workspace's actions or outcomes, as `kind` says.
    pub type_key: String,
    pub channel: Option<PitchChannel>,
    pub item: Option<ItemInput>,
    pub note: Option<String>,
    /// When it happened; default now. Can be in the past.
    pub happened_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct PitchPatch {
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub contact_id: Option<Option<Uuid>>,
    pub type_key: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<PitchChannel>)]
    pub channel: Option<Option<PitchChannel>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<ItemInput>)]
    pub item: Option<Option<ItemInput>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub note: Option<Option<String>>,
    pub happened_at: Option<DateTime<Utc>>,
}

/// The parts of an entry, checked.
struct Parts {
    contact_id: Option<Uuid>,
    type_key: String,
    channel: Option<PitchChannel>,
    item: Option<ItemInput>,
    note: Option<String>,
    happened_at: DateTime<Utc>,
}

/// Checks an entry against the workspace's lists and records: the type is
/// one it lists (unless unchanged, so an entry whose type was since removed
/// can still be edited), the person is at the organisation, what was pitched
/// exists, and it did not happen tomorrow or later.
#[allow(clippy::too_many_arguments)]
async fn check(
    tx: &mut Tx,
    settings: &PitchingSettings,
    organisation: Uuid,
    kind: PitchKind,
    parts: &Parts,
    type_changed: bool,
    contact_changed: bool,
    item_changed: bool,
) -> ApiResult<()> {
    let mut check = Check::default();
    if type_changed {
        let listed = match kind {
            PitchKind::Action => settings.action(&parts.type_key).is_some(),
            PitchKind::Outcome => settings.outcome(&parts.type_key).is_some(),
        };
        if !listed {
            check.issue(
                "type_key",
                match kind {
                    PitchKind::Action => "is not one of the workspace's actions",
                    PitchKind::Outcome => "is not one of the workspace's answers",
                },
            );
        }
    }
    if contact_changed && let Some(id) = parts.contact_id {
        match core_db::contacts::get(tx, id).await? {
            Some(c) if c.organisation_id == Some(organisation) => {}
            Some(_) => check.issue("contact_id", "is not at this organisation"),
            None => check.issue("contact_id", "there is no such contact"),
        }
    }
    if item_changed && let Some(item) = parts.item {
        let exists = match item.kind {
            ItemKind::Talent => core_db::talent::get(tx, item.id).await?.is_some(),
            ItemKind::Project => core_db::projects::get(tx, item.id).await?.is_some(),
        };
        if !exists {
            check.issue("item", format!("there is no such {}", item.kind.as_str()));
        }
    }
    if parts.happened_at > Utc::now() + Duration::hours(24) {
        check.issue("happened_at", "cannot be in the future");
    }
    check.finish()
}

fn values(organisation: Uuid, kind: PitchKind, p: &Parts) -> core_db::pitches::Values<'_> {
    core_db::pitches::Values {
        organisation_id: organisation,
        contact_id: p.contact_id,
        kind: kind.as_str(),
        type_key: &p.type_key,
        channel: p.channel.map(PitchChannel::as_str),
        talent_id: p.item.filter(|i| i.kind == ItemKind::Talent).map(|i| i.id),
        project_id: p.item.filter(|i| i.kind == ItemKind::Project).map(|i| i.id),
        note: p.note.as_deref(),
        happened_at: p.happened_at,
    }
}

pub async fn create(state: &AppState, ctx: &Ctx, new: NewPitch) -> ApiResult<PitchView> {
    ctx.authorize(Permission::OrganisationWrite)?;
    let mut tx = begin(state, ctx).await?;
    if core_db::organisations::get(&mut tx, new.organisation_id)
        .await?
        .is_none()
    {
        return Err(ApiError::invalid(
            "organisation_id",
            "there is no such organisation",
        ));
    }
    let (settings, _) = settings_in(&mut tx).await?;
    let mut note_check = Check::default();
    let note = note_check.long("note", new.note.as_deref(), MAX_NOTE);
    note_check.finish()?;
    let parts = Parts {
        contact_id: new.contact_id,
        type_key: new.type_key.trim().to_owned(),
        channel: new.channel,
        item: new.item,
        note,
        happened_at: new.happened_at.unwrap_or_else(Utc::now),
    };
    check(
        &mut tx,
        &settings,
        new.organisation_id,
        new.kind,
        &parts,
        true,
        true,
        true,
    )
    .await?;
    let id = core_domain::ids::new_id();
    core_db::pitches::insert(
        &mut tx,
        id,
        values(new.organisation_id, new.kind, &parts),
        ctx.user_id(),
    )
    .await?;
    let view = load(&mut tx, &settings, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "pitch.logged",
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

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    p: PitchPatch,
    if_match: Option<i64>,
) -> ApiResult<PitchView> {
    ctx.authorize(Permission::OrganisationWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::pitches::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let (settings, _) = settings_in(&mut tx).await?;
    let before = load(&mut tx, &settings, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut note_check = Check::default();
    let note = match &p.note {
        Some(n) => note_check.long("note", n.as_deref(), MAX_NOTE),
        None => before.note.clone(),
    };
    note_check.finish()?;
    // An item that has since gone is kept unless replaced.
    let stored = core_db::pitches::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let current_item = match (stored.talent_id, stored.project_id) {
        (Some(id), _) => Some(ItemInput {
            kind: ItemKind::Talent,
            id,
        }),
        (None, Some(id)) => Some(ItemInput {
            kind: ItemKind::Project,
            id,
        }),
        _ => None,
    };
    let parts = Parts {
        contact_id: p.contact_id.unwrap_or(stored.contact_id),
        type_key: p
            .type_key
            .as_deref()
            .map_or_else(|| before.type_key.clone(), |t| t.trim().to_owned()),
        channel: p.channel.unwrap_or(before.channel),
        item: p.item.unwrap_or(current_item),
        note,
        happened_at: p.happened_at.unwrap_or(before.happened_at),
    };
    check(
        &mut tx,
        &settings,
        before.organisation_id,
        before.kind,
        &parts,
        parts.type_key != before.type_key,
        parts.contact_id != stored.contact_id,
        parts.item != current_item,
    )
    .await?;
    core_db::pitches::update(
        &mut tx,
        id,
        values(before.organisation_id, before.kind, &parts),
        ctx.user_id(),
    )
    .await?;
    let after = load(&mut tx, &settings, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "pitch.updated",
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
    ctx.authorize(Permission::OrganisationWrite)?;
    let mut tx = begin(state, ctx).await?;
    let (settings, _) = settings_in(&mut tx).await?;
    let before = load(&mut tx, &settings, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    core_db::pitches::soft_delete(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "pitch.deleted",
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
