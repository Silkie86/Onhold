//! Talent: the generic bookable (a person, an act, a space or equipment),
//! and the members of an act or crew.

use chrono::{DateTime, Utc};
use core_db::Tx;
use core_db::talent::{Member, Talent};
use core_domain::{Ctx, Money, Permission, TalentKind, UserId};
use core_engines::fields::{FieldDef, Mode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::crm::{Check, Duplicate, ListArgs, ListQuery};
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::routes::nullable;

pub const ENTITY: &str = "talent";
pub const MEMBER: &str = "talent_member";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TalentStatus {
    #[default]
    Active,
    Inactive,
}

impl TalentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TalentStatus::Active => "active",
            TalentStatus::Inactive => "inactive",
        }
    }

    fn parse(s: &str) -> Self {
        if s == "inactive" {
            TalentStatus::Inactive
        } else {
            TalentStatus::Active
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TalentView {
    pub id: Uuid,
    pub kind: TalentKind,
    /// The stage or display name.
    pub name: String,
    pub legal_name: Option<String>,
    /// Free text: solo, band, DJ, speaker, studio…
    pub talent_type: Option<String>,
    pub bio: Option<String>,
    pub base_location: Option<String>,
    pub default_fee: Option<Money>,
    /// 0 to 100, up to two decimal places.
    pub commission_percent: Option<f64>,
    pub status: TalentStatus,
    /// The member who signs in to the talent portal as this talent.
    pub portal_user_id: Option<Uuid>,
    pub tags: Vec<String>,
    /// Custom field values by key.
    #[schema(value_type = Object)]
    pub custom: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

impl From<Talent> for TalentView {
    fn from(t: Talent) -> Self {
        Self {
            id: t.id,
            kind: t.kind,
            name: t.name,
            legal_name: t.legal_name,
            talent_type: t.talent_type,
            bio: t.bio,
            base_location: t.base_location,
            default_fee: t
                .default_fee_minor
                .zip(t.default_fee_currency)
                .map(|(minor, currency)| Money { minor, currency }),
            commission_percent: t.commission_bps.map(|b| f64::from(b) / 100.0),
            status: TalentStatus::parse(&t.status),
            portal_user_id: t.portal_user_id,
            tags: t.tags,
            custom: t.custom,
            version: t.version,
            created_at: t.created_at,
            updated_at: t.updated_at,
            created_by: t.created_by,
            updated_by: t.updated_by,
        }
    }
}

/// The JSON events, the audit log, rules and templates see.
pub fn to_json(v: &TalentView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TalentPage {
    pub items: Vec<TalentView>,
    /// How many records match, across all pages.
    pub total: i64,
}

pub async fn list(
    state: &AppState,
    ctx: &Ctx,
    q: &ListQuery,
    kind: Option<TalentKind>,
    status: Option<TalentStatus>,
) -> ApiResult<TalentPage> {
    ctx.authorize(Permission::TalentRead)?;
    let mut tx = begin(state, ctx).await?;
    let args = ListArgs::parse(&mut tx, ENTITY, core_db::talent::SORTS, q).await?;
    let (rows, total) = core_db::talent::list(
        &mut tx,
        core_db::talent::Filter {
            list: args.params(),
            kind,
            status: status.map(TalentStatus::as_str),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(TalentPage {
        items: rows.into_iter().map(Into::into).collect(),
        total,
    })
}

pub(crate) async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<TalentView>> {
    Ok(core_db::talent::get(tx, id).await?.map(Into::into))
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<TalentView> {
    ctx.authorize(Permission::TalentRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NewTalent {
    /// Default `person`.
    pub kind: Option<TalentKind>,
    pub name: String,
    pub legal_name: Option<String>,
    pub talent_type: Option<String>,
    pub bio: Option<String>,
    pub base_location: Option<String>,
    pub default_fee: Option<Money>,
    pub commission_percent: Option<f64>,
    /// Default `active`.
    pub status: Option<TalentStatus>,
    pub portal_user_id: Option<Uuid>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Custom field values by key. Missing fields take their defaults.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub custom: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct TalentPatch {
    pub kind: Option<TalentKind>,
    pub name: Option<String>,
    /// `null` clears it; likewise for the other optional fields.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub legal_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub talent_type: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub bio: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub base_location: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Money>)]
    pub default_fee: Option<Option<Money>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<f64>)]
    pub commission_percent: Option<Option<f64>>,
    pub status: Option<TalentStatus>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub portal_user_id: Option<Option<Uuid>>,
    /// Replaces the tags.
    pub tags: Option<Vec<String>>,
    /// Only the keys sent change; `null` clears a value.
    #[schema(value_type = Option<Object>)]
    pub custom: Option<Map<String, Value>>,
}

/// Every value of a talent record, checked.
pub(crate) struct Checked {
    kind: TalentKind,
    name: String,
    legal_name: Option<String>,
    talent_type: Option<String>,
    bio: Option<String>,
    base_location: Option<String>,
    default_fee: Option<Money>,
    commission_bps: Option<i32>,
    status: TalentStatus,
    portal_user_id: Option<Uuid>,
    tags: Vec<String>,
    custom: Value,
}

impl Checked {
    fn values(&self) -> core_db::talent::Values<'_> {
        core_db::talent::Values {
            kind: self.kind,
            name: &self.name,
            legal_name: self.legal_name.as_deref(),
            talent_type: self.talent_type.as_deref(),
            bio: self.bio.as_deref(),
            base_location: self.base_location.as_deref(),
            default_fee_minor: self.default_fee.as_ref().map(|m| m.minor),
            default_fee_currency: self.default_fee.as_ref().map(|m| m.currency.as_str()),
            commission_bps: self.commission_bps,
            status: self.status.as_str(),
            portal_user_id: self.portal_user_id,
            tags: &self.tags,
            custom: &self.custom,
        }
    }
}

fn commission(check: &mut Check, percent: Option<f64>) -> Option<i32> {
    let p = percent?;
    let bps = (p * 100.0).round();
    if !(0.0..=100.0).contains(&p) || (p * 100.0 - bps).abs() > 1e-6 {
        check.issue(
            "commission_percent",
            "must be 0 to 100 with at most two decimal places",
        );
        return None;
    }
    Some(bps as i32)
}

async fn check_portal_user(tx: &mut Tx, check: &mut Check, user: Option<Uuid>) -> ApiResult<()> {
    if let Some(u) = user
        && core_db::members::get(tx, UserId(u)).await?.is_none()
    {
        check.issue("portal_user_id", "is not a member of this workspace");
    }
    Ok(())
}

/// Checks a new record's values against each other and the custom field
/// definitions, collecting every problem.
pub(crate) fn check_new(new: &NewTalent, defs: &[FieldDef]) -> (Checked, Check) {
    let mut check = Check::default();
    let checked = Checked {
        kind: new.kind.unwrap_or_default(),
        name: check.required("name", &new.name, 200),
        legal_name: check.optional("legal_name", new.legal_name.as_deref(), 200),
        talent_type: check.optional("talent_type", new.talent_type.as_deref(), 100),
        bio: check.long("bio", new.bio.as_deref(), 20_000),
        base_location: check.optional("base_location", new.base_location.as_deref(), 200),
        default_fee: check.money("default_fee", new.default_fee.as_ref()),
        commission_bps: commission(&mut check, new.commission_percent),
        status: new.status.unwrap_or_default(),
        portal_user_id: new.portal_user_id,
        tags: check.tags(&new.tags),
        custom: Value::Object(check.custom(defs, &Map::new(), &new.custom, Mode::Create)),
    };
    (checked, check)
}

pub async fn create(state: &AppState, ctx: &Ctx, new: NewTalent) -> ApiResult<TalentView> {
    ctx.authorize(Permission::TalentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let view = create_in_tx(&mut tx, ctx, new, None).await?;
    tx.commit().await?;
    Ok(view)
}

/// The create, inside the caller's transaction (imports use this).
pub(crate) async fn create_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    new: NewTalent,
    event_extra: Option<Value>,
) -> ApiResult<TalentView> {
    let defs = super::fields::engine_defs(tx, ENTITY).await?;
    let (checked, mut check) = check_new(&new, &defs);
    check_portal_user(tx, &mut check, new.portal_user_id).await?;
    check.finish()?;

    let id = core_domain::ids::new_id();
    core_db::talent::insert(tx, id, checked.values(), ctx.user_id()).await?;
    let view = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "talent.created",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(to_json(&view)),
            event_extra,
        },
    )
    .await?;
    Ok(view)
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    patch: TalentPatch,
    if_match: Option<i64>,
) -> ApiResult<TalentView> {
    ctx.authorize(Permission::TalentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let view = update_in_tx(&mut tx, ctx, id, patch, if_match, None).await?;
    tx.commit().await?;
    Ok(view)
}

/// The update, inside the caller's transaction (rules and imports use this).
pub(crate) async fn update_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    id: Uuid,
    p: TalentPatch,
    if_match: Option<i64>,
    event_extra: Option<Value>,
) -> ApiResult<TalentView> {
    let current = core_db::talent::get_for_update(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(current.version, if_match)?;
    let before = TalentView::from(current.clone());

    let mut check = Check::default();
    let current_custom = current.custom.as_object().cloned().unwrap_or_default();
    let custom = match &p.custom {
        Some(changes) => {
            let defs = super::fields::engine_defs(tx, ENTITY).await?;
            check.custom(&defs, &current_custom, changes, Mode::Update)
        }
        None => current_custom,
    };
    let checked = Checked {
        kind: p.kind.unwrap_or(current.kind),
        name: match &p.name {
            Some(n) => check.required("name", n, 200),
            None => current.name.clone(),
        },
        legal_name: match &p.legal_name {
            Some(v) => check.optional("legal_name", v.as_deref(), 200),
            None => current.legal_name.clone(),
        },
        talent_type: match &p.talent_type {
            Some(v) => check.optional("talent_type", v.as_deref(), 100),
            None => current.talent_type.clone(),
        },
        bio: match &p.bio {
            Some(v) => check.long("bio", v.as_deref(), 20_000),
            None => current.bio.clone(),
        },
        base_location: match &p.base_location {
            Some(v) => check.optional("base_location", v.as_deref(), 200),
            None => current.base_location.clone(),
        },
        default_fee: match &p.default_fee {
            Some(v) => check.money("default_fee", v.as_ref()),
            None => before.default_fee.clone(),
        },
        commission_bps: match p.commission_percent {
            Some(v) => commission(&mut check, v),
            None => current.commission_bps,
        },
        status: p.status.unwrap_or(before.status),
        portal_user_id: p.portal_user_id.unwrap_or(current.portal_user_id),
        tags: match &p.tags {
            Some(t) => check.tags(t),
            None => current.tags.clone(),
        },
        custom: Value::Object(custom),
    };
    if checked.portal_user_id != current.portal_user_id {
        check_portal_user(tx, &mut check, checked.portal_user_id).await?;
    }
    check.finish()?;

    let unchanged = checked.kind == current.kind
        && checked.name == current.name
        && checked.legal_name == current.legal_name
        && checked.talent_type == current.talent_type
        && checked.bio == current.bio
        && checked.base_location == current.base_location
        && checked.default_fee == before.default_fee
        && checked.commission_bps == current.commission_bps
        && checked.status == before.status
        && checked.portal_user_id == current.portal_user_id
        && checked.tags == current.tags
        && checked.custom == current.custom;
    if unchanged {
        return Ok(before);
    }

    core_db::talent::update(tx, id, checked.values(), ctx.user_id()).await?;
    let after = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "talent.updated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra,
        },
    )
    .await?;
    Ok(after)
}

pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::TalentWrite)?;
    let mut tx = begin(state, ctx).await?;
    delete_in_tx(&mut tx, ctx, id).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn delete_in_tx(tx: &mut Tx, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    let before = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    core_db::talent::soft_delete(tx, id, ctx.user_id()).await?;
    super::talent_calendars::disconnect_all(tx, ctx, Some(id), "talent_removed").await?;
    record(
        tx,
        ctx,
        Change {
            topic: "talent.deleted",
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

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct TalentDuplicateQuery {
    pub name: String,
    /// The record being edited, which is not its own duplicate.
    pub except: Option<Uuid>,
}

/// Talent that may be the one being entered: similar names.
pub async fn duplicates(
    state: &AppState,
    ctx: &Ctx,
    q: &TalentDuplicateQuery,
) -> ApiResult<Vec<Duplicate>> {
    ctx.authorize(Permission::TalentRead)?;
    let name = q.name.trim().to_lowercase();
    if name.is_empty() {
        return Ok(Vec::new());
    }
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::talent::similar(
        &mut tx,
        &name,
        super::crm::SIMILAR_NAME,
        q.except,
        super::crm::MAX_CANDIDATES,
    )
    .await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(|c| Duplicate {
            id: c.id,
            name: c.name,
            reasons: super::crm::name_reason(c.similarity).into_iter().collect(),
        })
        .collect())
}

// Members ----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TalentMemberView {
    pub id: Uuid,
    pub talent_id: Uuid,
    pub name: String,
    pub role: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    /// Dietary, travel and other notes.
    pub notes: Option<String>,
    pub position: i32,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Member> for TalentMemberView {
    fn from(m: Member) -> Self {
        Self {
            id: m.id,
            talent_id: m.talent_id,
            name: m.name,
            role: m.role,
            email: m.email,
            phone: m.phone,
            notes: m.notes,
            position: m.position,
            version: m.version,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct TalentMemberInput {
    pub name: String,
    pub role: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub notes: Option<String>,
    /// Order in the list; default last.
    pub position: Option<i32>,
}

struct CheckedMember {
    name: String,
    role: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    notes: Option<String>,
}

fn check_member(m: &TalentMemberInput) -> ApiResult<CheckedMember> {
    let mut check = Check::default();
    let out = CheckedMember {
        name: check.required("name", &m.name, 200),
        role: check.optional("role", m.role.as_deref(), 200),
        email: check
            .optional("email", m.email.as_deref(), 320)
            .map(|e| core_domain::crm::email_key(&e)),
        phone: check.optional("phone", m.phone.as_deref(), 50),
        notes: check.long("notes", m.notes.as_deref(), 5_000),
    };
    if let Some(e) = &out.email
        && !core_engines::fields::is_email(e)
    {
        check.issue("email", "must be an email address");
    }
    check.finish()?;
    Ok(out)
}

pub async fn members(
    state: &AppState,
    ctx: &Ctx,
    talent_id: Uuid,
) -> ApiResult<Vec<TalentMemberView>> {
    ctx.authorize(Permission::TalentRead)?;
    let mut tx = begin(state, ctx).await?;
    load(&mut tx, talent_id).await?.ok_or(ApiError::NotFound)?;
    let rows = core_db::talent::members(&mut tx, talent_id).await?;
    tx.commit().await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn add_member(
    state: &AppState,
    ctx: &Ctx,
    talent_id: Uuid,
    input: TalentMemberInput,
) -> ApiResult<TalentMemberView> {
    ctx.authorize(Permission::TalentWrite)?;
    let m = check_member(&input)?;
    let mut tx = begin(state, ctx).await?;
    load(&mut tx, talent_id).await?.ok_or(ApiError::NotFound)?;
    let existing = core_db::talent::members(&mut tx, talent_id).await?;
    let position = input
        .position
        .unwrap_or_else(|| existing.iter().map(|m| m.position + 1).max().unwrap_or(0));
    let id = core_domain::ids::new_id();
    core_db::talent::insert_member(
        &mut tx,
        id,
        talent_id,
        core_db::talent::MemberValues {
            name: &m.name,
            role: m.role.as_deref(),
            email: m.email.as_deref(),
            phone: m.phone.as_deref(),
            notes: m.notes.as_deref(),
            position,
        },
        ctx.user_id(),
    )
    .await?;
    let view: TalentMemberView = core_db::talent::member_for_update(&mut tx, talent_id, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    record(
        &mut tx,
        ctx,
        Change {
            topic: "talent_member.created",
            resource: MEMBER,
            object_id: Some(id),
            before: None,
            after: Some(json!(view)),
            event_extra: Some(json!({ "talent_id": talent_id })),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(view)
}

pub async fn update_member(
    state: &AppState,
    ctx: &Ctx,
    talent_id: Uuid,
    id: Uuid,
    input: TalentMemberInput,
    if_match: Option<i64>,
) -> ApiResult<TalentMemberView> {
    ctx.authorize(Permission::TalentWrite)?;
    let m = check_member(&input)?;
    let mut tx = begin(state, ctx).await?;
    let current = core_db::talent::member_for_update(&mut tx, talent_id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(current.version, if_match)?;
    core_db::talent::update_member(
        &mut tx,
        id,
        core_db::talent::MemberValues {
            name: &m.name,
            role: m.role.as_deref(),
            email: m.email.as_deref(),
            phone: m.phone.as_deref(),
            notes: m.notes.as_deref(),
            position: input.position.unwrap_or(current.position),
        },
        ctx.user_id(),
    )
    .await?;
    let after: TalentMemberView = core_db::talent::member_for_update(&mut tx, talent_id, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    record(
        &mut tx,
        ctx,
        Change {
            topic: "talent_member.updated",
            resource: MEMBER,
            object_id: Some(id),
            before: Some(json!(TalentMemberView::from(current))),
            after: Some(json!(after)),
            event_extra: Some(json!({ "talent_id": talent_id })),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

pub async fn remove_member(
    state: &AppState,
    ctx: &Ctx,
    talent_id: Uuid,
    id: Uuid,
) -> ApiResult<()> {
    ctx.authorize(Permission::TalentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let current = core_db::talent::member_for_update(&mut tx, talent_id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    core_db::talent::delete_member(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "talent_member.deleted",
            resource: MEMBER,
            object_id: Some(id),
            before: Some(json!(TalentMemberView::from(current))),
            after: None,
            event_extra: Some(json!({ "talent_id": talent_id })),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
