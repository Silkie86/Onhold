//! Enquiries: requests for talent, by hand, form, email or portal. Each one
//! moves through the workspace's enquiry pipeline; the first is made on
//! first use (new → checking → quoted → won / lost) and can be changed
//! like any pipeline.

use chrono::{DateTime, NaiveDate, Utc};
use core_db::Tx;
use core_db::enquiries::Enquiry;
use core_db::pipelines::Stage;
use core_domain::{Ctx, Money, Permission, UserId};
use core_engines::fields::Mode;
use core_engines::pipeline::Outcome;
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

pub const ENTITY: &str = "enquiry";

/// Where an enquiry came from.
pub const SOURCES: &[&str] = &["manual", "email", "form", "portal"];

/// The pipeline a workspace starts with: (key, name, outcome, colour).
pub(crate) const DEFAULT_STAGES: &[(&str, &str, Outcome, &str)] = &[
    ("new", "New", Outcome::Open, "#6366f1"),
    (
        "checking",
        "Checking availability",
        Outcome::Open,
        "#0ea5e9",
    ),
    ("quoted", "Quoted", Outcome::Open, "#f59e0b"),
    ("won", "Won", Outcome::Won, "#16a34a"),
    ("lost", "Lost", Outcome::Lost, "#9ca3af"),
];

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EnquiryView {
    pub id: Uuid,
    pub title: String,
    /// `manual`, `email`, `form` or `portal`.
    pub source: String,
    /// The talent asked for, in order.
    pub talent: Vec<Named>,
    pub organisation_id: Option<Uuid>,
    pub organisation_name: Option<String>,
    pub contact_id: Option<Uuid>,
    pub contact_name: Option<String>,
    pub venue_id: Option<Uuid>,
    pub venue_name: Option<String>,
    /// A venue not in the CRM.
    pub venue_text: Option<String>,
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub budget: Option<Money>,
    pub pipeline_id: Option<Uuid>,
    /// The stage's key.
    pub stage: Option<String>,
    pub stage_name: Option<String>,
    /// `open`, `won` or `lost`.
    pub stage_outcome: Option<String>,
    pub stage_entered_at: Option<DateTime<Utc>>,
    pub owner_id: Option<Uuid>,
    pub lost_reason: Option<String>,
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

fn view(e: Enquiry, talent: Vec<Named>, stages: &[Stage]) -> EnquiryView {
    let stage = stages.iter().find(|s| Some(s.id) == e.stage_id);
    EnquiryView {
        id: e.id,
        title: e.title,
        source: e.source,
        talent,
        organisation_id: e.organisation_id,
        organisation_name: e.organisation_name,
        contact_id: e.contact_id,
        contact_name: e.contact_name,
        venue_id: e.venue_id,
        venue_name: e.venue_name,
        venue_text: e.venue_text,
        starts_on: e.starts_on,
        ends_on: e.ends_on,
        budget: match (e.budget_minor, e.budget_currency) {
            (Some(minor), Some(currency)) => Some(Money {
                minor,
                currency: currency.trim().to_owned(),
            }),
            _ => None,
        },
        pipeline_id: e.pipeline_id,
        stage: stage.map(|s| s.key.clone()),
        stage_name: stage.map(|s| s.name.clone()),
        stage_outcome: stage.map(|s| s.outcome.clone()),
        stage_entered_at: e.stage_entered_at,
        owner_id: e.owner_id,
        lost_reason: e.lost_reason,
        notes: e.notes,
        tags: e.tags,
        custom: e.custom,
        version: e.version,
        created_at: e.created_at,
        updated_at: e.updated_at,
        created_by: e.created_by,
        updated_by: e.updated_by,
    }
}

/// The JSON events, the audit log, rules and templates see.
pub fn to_json(v: &EnquiryView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

pub(crate) async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<EnquiryView>> {
    let Some(e) = core_db::enquiries::get(tx, id).await? else {
        return Ok(None);
    };
    let talent = links::talent_by_owner(core_db::enquiries::talent_of(tx, &[id]).await?, id);
    let stages = match e.pipeline_id {
        Some(p) => core_db::pipelines::stages(tx, &[p]).await?,
        None => Vec::new(),
    };
    Ok(Some(view(e, talent, &stages)))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EnquiryPage {
    pub items: Vec<EnquiryView>,
    pub total: i64,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct EnquiryFilter {
    /// Only enquiries in this stage (by key) of the default pipeline.
    pub stage: Option<String>,
    /// Only enquiries asking for this talent.
    pub talent_id: Option<Uuid>,
    /// Only enquiries this member owns.
    pub owner_id: Option<Uuid>,
    /// Only enquiries from (or at) this organisation.
    pub organisation_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
}

pub async fn list(
    state: &AppState,
    ctx: &Ctx,
    q: &ListQuery,
    f: &EnquiryFilter,
) -> ApiResult<EnquiryPage> {
    ctx.authorize(Permission::EnquiryRead)?;
    let mut tx = begin(state, ctx).await?;
    // Newest first unless asked otherwise.
    let mut q = clone_query(q);
    if q.sort.is_none() && q.q.as_deref().is_none_or(|s| s.trim().is_empty()) {
        q.sort = Some("-created_at".into());
    }
    let args = ListArgs::parse(&mut tx, ENTITY, core_db::enquiries::SORTS, &q).await?;
    let pipelines = core_db::pipelines::list(&mut tx, Some(ENTITY)).await?;
    let ids: Vec<Uuid> = pipelines.iter().map(|p| p.id).collect();
    let stages = core_db::pipelines::stages(&mut tx, &ids).await?;
    let stage_id = match &f.stage {
        None => None,
        Some(key) => {
            let default = pipelines.iter().find(|p| p.is_default).map(|p| p.id);
            Some(
                stages
                    .iter()
                    .find(|s| Some(s.pipeline_id) == default && &s.key == key)
                    .map(|s| s.id)
                    .ok_or_else(|| {
                        ApiError::invalid("stage", format!("there is no stage `{key}`"))
                    })?,
            )
        }
    };
    let (rows, total) = core_db::enquiries::list(
        &mut tx,
        core_db::enquiries::Filter {
            list: args.params(),
            stage_id,
            talent_id: f.talent_id,
            owner_id: f.owner_id,
            organisation_id: f.organisation_id,
            contact_id: f.contact_id,
        },
    )
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|e| e.id).collect();
    let talent = core_db::enquiries::talent_of(&mut tx, &ids).await?;
    tx.commit().await?;
    let items = rows
        .into_iter()
        .map(|e| {
            let t = links::talent_by_owner(talent.clone(), e.id);
            view(e, t, &stages)
        })
        .collect();
    Ok(EnquiryPage { items, total })
}

fn clone_query(q: &ListQuery) -> ListQuery {
    ListQuery {
        q: q.q.clone(),
        tag: q.tag.clone(),
        custom: q.custom.clone(),
        sort: q.sort.clone(),
        offset: q.offset,
        limit: q.limit,
    }
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<EnquiryView> {
    ctx.authorize(Permission::EnquiryRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NewEnquiry {
    pub title: String,
    /// `manual` (default), `email`, `form` or `portal`.
    pub source: Option<String>,
    #[serde(default)]
    pub talent_ids: Vec<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub venue_id: Option<Uuid>,
    pub venue_text: Option<String>,
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub budget: Option<Money>,
    /// A stage key of the default pipeline; default its first stage.
    pub stage: Option<String>,
    /// Default: the member creating it.
    pub owner_id: Option<Uuid>,
    pub notes: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub custom: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct EnquiryPatch {
    pub title: Option<String>,
    /// Replaces the talent asked for.
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
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<NaiveDate>)]
    pub starts_on: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<NaiveDate>)]
    pub ends_on: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Money>)]
    pub budget: Option<Option<Money>>,
    /// Move to this stage (by key) of the enquiry's pipeline.
    pub stage: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub owner_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub lost_reason: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub notes: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
    /// Only the keys sent change; `null` clears a value.
    #[schema(value_type = Option<Object>)]
    pub custom: Option<Map<String, Value>>,
}

/// The workspace's default enquiry pipeline, made on first use.
pub(crate) async fn default_pipeline(tx: &mut Tx, ctx: &Ctx) -> ApiResult<(Uuid, Vec<Stage>)> {
    if let Some(p) = core_db::pipelines::default_for(tx, ENTITY).await? {
        let stages = core_db::pipelines::stages(tx, &[p.id]).await?;
        return Ok((p.id, stages));
    }
    let id = core_domain::ids::new_id();
    core_db::pipelines::insert(tx, id, ENTITY, "Enquiries", ctx.user_id()).await?;
    for (i, (key, name, outcome, colour)) in DEFAULT_STAGES.iter().enumerate() {
        core_db::pipelines::insert_stage(
            tx,
            core_domain::ids::new_id(),
            id,
            key,
            core_db::pipelines::StageValues {
                name,
                position: i as i32,
                colour: Some(colour),
                sla_days: None,
                outcome: outcome.as_str(),
                required_fields: &[],
                allowed_next: None,
            },
        )
        .await?;
    }
    core_db::pipelines::set_default(tx, id, ENTITY).await?;
    let stages = core_db::pipelines::stages(tx, &[id]).await?;
    record(
        tx,
        ctx,
        Change {
            topic: "pipeline.created",
            resource: "pipeline",
            object_id: Some(id),
            before: None,
            after: Some(json!({
                "entity": ENTITY,
                "name": "Enquiries",
                "is_default": true,
                "stages": DEFAULT_STAGES.iter().map(|(k, n, o, _)| json!({
                    "key": k, "name": n, "outcome": o.as_str(),
                })).collect::<Vec<_>>(),
            })),
            event_extra: None,
        },
    )
    .await?;
    Ok((id, stages))
}

fn find_stage<'a>(stages: &'a [Stage], key: &str) -> ApiResult<&'a Stage> {
    stages
        .iter()
        .find(|s| s.key == key)
        .ok_or_else(|| ApiError::invalid("stage", format!("there is no stage `{key}`")))
}

fn check_move(from: Option<&Stage>, to: &Stage, custom: &Map<String, Value>) -> ApiResult<()> {
    let from = from.map(super::pipelines::to_engine);
    core_engines::pipeline::check_move(from.as_ref(), &super::pipelines::to_engine(to), custom)
        .map_err(|e| ApiError::unprocessable("stage_move_refused", e.to_string()))
}

fn check_dates(check: &mut Check, starts: Option<NaiveDate>, ends: Option<NaiveDate>) {
    match (starts, ends) {
        (None, Some(_)) => check.issue("starts_on", "give the first date as well as the last"),
        (Some(s), Some(e)) if e < s => check.issue("ends_on", "must not be before starts_on"),
        _ => {}
    }
}

async fn check_owner(tx: &mut Tx, check: &mut Check, owner: Option<Uuid>) -> ApiResult<()> {
    if let Some(owner) = owner
        && core_db::members::get(tx, UserId(owner)).await?.is_none()
    {
        check.issue("owner_id", "must be a member of the workspace");
    }
    Ok(())
}

pub async fn create(state: &AppState, ctx: &Ctx, new: NewEnquiry) -> ApiResult<EnquiryView> {
    ctx.authorize(Permission::EnquiryWrite)?;
    let mut tx = begin(state, ctx).await?;
    let view = create_in_tx(&mut tx, ctx, new).await?;
    tx.commit().await?;
    Ok(view)
}

pub(crate) async fn create_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    new: NewEnquiry,
) -> ApiResult<EnquiryView> {
    let defs = super::fields::engine_defs(tx, ENTITY).await?;
    let mut check = Check::default();
    let title = check.required("title", &new.title, 200);
    let source = new.source.as_deref().map(str::trim).unwrap_or("manual");
    if !SOURCES.contains(&source) {
        check.issue("source", format!("use one of: {}", SOURCES.join(", ")));
    }
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
    check_dates(&mut check, new.starts_on, new.ends_on);
    let budget = check.money("budget", new.budget.as_ref());
    let owner = new.owner_id.or(ctx.user_id().map(|u| u.0));
    check_owner(tx, &mut check, owner).await?;
    let notes = check.long("notes", new.notes.as_deref(), 20_000);
    let tags = check.tags(&new.tags);
    let custom = check.custom(&defs, &Map::new(), &new.custom, Mode::Create);
    check.finish()?;

    let (pipeline_id, stages) = default_pipeline(tx, ctx).await?;
    let stage = match &new.stage {
        Some(key) => find_stage(&stages, key)?,
        None => stages
            .iter()
            .find(|s| s.archived_at.is_none())
            .ok_or_else(|| ApiError::invalid("stage", "the enquiry pipeline has no stages"))?,
    };
    check_move(None, stage, &custom)?;

    let id = core_domain::ids::new_id();
    let custom = Value::Object(custom);
    core_db::enquiries::insert(
        tx,
        id,
        core_db::enquiries::Values {
            title: &title,
            source,
            organisation_id: new.organisation_id,
            contact_id: new.contact_id,
            venue_id: new.venue_id,
            venue_text: venue_text.as_deref(),
            starts_on: new.starts_on,
            ends_on: new.ends_on,
            budget_minor: budget.as_ref().map(|m| m.minor),
            budget_currency: budget.as_ref().map(|m| m.currency.as_str()),
            pipeline_id: Some(pipeline_id),
            stage_id: Some(stage.id),
            stage_entered_at: Some(Utc::now()),
            owner_id: owner,
            lost_reason: None,
            notes: notes.as_deref(),
            tags: &tags,
            custom: &custom,
        },
        ctx.user_id(),
    )
    .await?;
    core_db::enquiries::set_talent(tx, id, &talent).await?;
    let view = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "enquiry.created",
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

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    patch: EnquiryPatch,
    if_match: Option<i64>,
) -> ApiResult<EnquiryView> {
    ctx.authorize(Permission::EnquiryWrite)?;
    let mut tx = begin(state, ctx).await?;
    let view = update_in_tx(&mut tx, ctx, id, patch, if_match).await?;
    tx.commit().await?;
    Ok(view)
}

/// The update, inside the caller's transaction (rules and quotes use this).
pub(crate) async fn update_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    id: Uuid,
    p: EnquiryPatch,
    if_match: Option<i64>,
) -> ApiResult<EnquiryView> {
    let version = core_db::enquiries::lock(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let current = core_db::enquiries::get(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let before = load(tx, id).await?.ok_or(ApiError::NotFound)?;

    let mut check = Check::default();
    let title = match &p.title {
        Some(t) => check.required("title", t, 200),
        None => current.title.clone(),
    };
    let talent = match &p.talent_ids {
        Some(ids) => Some(links::check_talent(tx, &mut check, ids).await?),
        None => None,
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
    let starts_on = p.starts_on.unwrap_or(current.starts_on);
    let ends_on = p.ends_on.unwrap_or(current.ends_on);
    check_dates(&mut check, starts_on, ends_on);
    let budget = match &p.budget {
        Some(b) => check.money("budget", b.as_ref()),
        None => before.budget.clone(),
    };
    let owner_id = p.owner_id.unwrap_or(current.owner_id);
    if let Some(Some(owner)) = p.owner_id {
        check_owner(tx, &mut check, Some(owner)).await?;
    }
    let lost_reason = match &p.lost_reason {
        Some(v) => check.optional("lost_reason", v.as_deref(), 500),
        None => current.lost_reason.clone(),
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

    let mut stage_id = current.stage_id;
    let mut stage_entered_at = current.stage_entered_at;
    let mut stage_changed = false;
    if let Some(key) = &p.stage {
        let pipeline = current
            .pipeline_id
            .ok_or_else(|| ApiError::invalid("stage", "this enquiry is not in a pipeline"))?;
        let stages = core_db::pipelines::stages(tx, &[pipeline]).await?;
        let to = find_stage(&stages, key)?;
        if Some(to.id) != current.stage_id {
            let from = stages.iter().find(|s| Some(s.id) == current.stage_id);
            check_move(from, to, &custom)?;
            stage_id = Some(to.id);
            stage_entered_at = Some(Utc::now());
            stage_changed = true;
        }
    }

    let custom = Value::Object(custom);
    let budget_minor = budget.as_ref().map(|m| m.minor);
    let budget_currency = budget.as_ref().map(|m| m.currency.clone());
    let talent_changed = talent
        .as_ref()
        .is_some_and(|t| *t != before.talent.iter().map(|n| n.id).collect::<Vec<_>>());
    let unchanged = title == current.title
        && !talent_changed
        && organisation_id == current.organisation_id
        && contact_id == current.contact_id
        && venue_id == current.venue_id
        && venue_text == current.venue_text
        && starts_on == current.starts_on
        && ends_on == current.ends_on
        && budget_minor == current.budget_minor
        && budget_currency.as_deref() == current.budget_currency.as_deref().map(str::trim)
        && owner_id == current.owner_id
        && lost_reason == current.lost_reason
        && notes == current.notes
        && tags == current.tags
        && custom == current.custom
        && !stage_changed;
    if unchanged {
        return Ok(before);
    }

    core_db::enquiries::update(
        tx,
        id,
        core_db::enquiries::Values {
            title: &title,
            source: &current.source,
            organisation_id,
            contact_id,
            venue_id,
            venue_text: venue_text.as_deref(),
            starts_on,
            ends_on,
            budget_minor,
            budget_currency: budget_currency.as_deref(),
            pipeline_id: current.pipeline_id,
            stage_id,
            stage_entered_at,
            owner_id,
            lost_reason: lost_reason.as_deref(),
            notes: notes.as_deref(),
            tags: &tags,
            custom: &custom,
        },
        ctx.user_id(),
    )
    .await?;
    if let Some(t) = &talent
        && talent_changed
    {
        core_db::enquiries::set_talent(tx, id, t).await?;
    }
    let after = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "enquiry.updated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    if stage_changed {
        record(
            tx,
            ctx,
            Change {
                topic: "enquiry.stage_changed",
                resource: ENTITY,
                object_id: Some(id),
                before: Some(json!({ "stage": before.stage })),
                after: Some(json!({ "stage": after.stage })),
                event_extra: None,
            },
        )
        .await?;
    }
    Ok(after)
}

/// Where a quote's progress moves its enquiry.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Milestone {
    /// The stage keyed `quoted`, if there is one.
    Quoted,
    /// The first won stage.
    Won,
}

/// Moves an enquiry forward when a quote goes out or is accepted. Only
/// forward, only from an open stage, and only where the pipeline has such a
/// stage and allows the move; otherwise it stays where it is.
pub(crate) async fn reach(tx: &mut Tx, ctx: &Ctx, id: Uuid, to: Milestone) -> ApiResult<()> {
    let Some(e) = core_db::enquiries::get(tx, id).await? else {
        return Ok(());
    };
    let Some(pipeline) = e.pipeline_id else {
        return Ok(());
    };
    let stages = core_db::pipelines::stages(tx, &[pipeline]).await?;
    let current = stages.iter().find(|s| Some(s.id) == e.stage_id);
    if current.is_some_and(|s| s.outcome != Outcome::Open.as_str()) {
        return Ok(());
    }
    let live = stages.iter().filter(|s| s.archived_at.is_none());
    let target = match to {
        Milestone::Quoted => live.clone().find(|s| s.key == "quoted"),
        Milestone::Won => live.clone().find(|s| s.outcome == Outcome::Won.as_str()),
    };
    let Some(target) = target else {
        return Ok(());
    };
    if current.is_some_and(|c| c.position >= target.position) {
        return Ok(());
    }
    let custom = e.custom.as_object().cloned().unwrap_or_default();
    if check_move(current, target, &custom).is_err() {
        return Ok(());
    }
    update_in_tx(
        tx,
        ctx,
        id,
        EnquiryPatch {
            stage: Some(target.key.clone()),
            ..Default::default()
        },
        None,
    )
    .await
    .map(|_| ())
}

/// Deleting an enquiry keeps its bookings, still linked.
pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::EnquiryWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    core_db::enquiries::soft_delete(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "enquiry.deleted",
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
