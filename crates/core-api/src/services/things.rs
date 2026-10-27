//! Things: the placeholder entity that exercises custom fields, pipelines,
//! events and rules until the real modules (contacts, bookings…) arrive.

use chrono::{DateTime, Utc};
use core_db::Tx;
use core_db::pipelines::Stage;
use core_db::things::Thing;
use core_domain::{Ctx, Permission};
use core_engines::fields::Mode;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{Change, begin, check_version, record, required_text};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const ENTITY: &str = "thing";

/// A thing with its stage resolved to a key.
#[derive(Debug, Clone)]
pub struct View {
    pub thing: Thing,
    pub stage: Option<String>,
}

/// The JSON shape used in events, audit rows, rule conditions and
/// templates (`thing.custom.budget`, `thing.stage`).
pub fn to_json(v: &View) -> Value {
    let t = &v.thing;
    json!({
        "id": t.id,
        "name": t.name,
        "pipeline_id": t.pipeline_id,
        "stage": v.stage,
        "stage_entered_at": t.stage_entered_at,
        "due_at": t.due_at,
        "custom": t.custom,
        "version": t.version,
        "created_at": t.created_at,
        "updated_at": t.updated_at,
    })
}

async fn stage_key(tx: &mut Tx, t: &Thing) -> ApiResult<Option<String>> {
    let (Some(pipeline), Some(stage)) = (t.pipeline_id, t.stage_id) else {
        return Ok(None);
    };
    Ok(core_db::pipelines::stages(tx, &[pipeline])
        .await?
        .into_iter()
        .find(|s| s.id == stage)
        .map(|s| s.key))
}

pub(crate) async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<View>> {
    let Some(thing) = core_db::things::get(tx, id).await? else {
        return Ok(None);
    };
    let stage = stage_key(tx, &thing).await?;
    Ok(Some(View { thing, stage }))
}

pub async fn list(
    state: &AppState,
    ctx: &Ctx,
    pipeline_id: Option<Uuid>,
    stage: Option<&str>,
    q: Option<&str>,
    before: Option<Uuid>,
    limit: i64,
) -> ApiResult<Vec<View>> {
    ctx.authorize(Permission::ThingRead)?;
    let mut tx = begin(state, ctx).await?;
    let pipelines = core_db::pipelines::list(&mut tx, Some(ENTITY)).await?;
    let ids: Vec<Uuid> = pipelines.iter().map(|p| p.id).collect();
    let stages = core_db::pipelines::stages(&mut tx, &ids).await?;
    let stage_id = match stage {
        None => None,
        Some(key) => {
            let pipeline = match pipeline_id {
                Some(p) => Some(p),
                None => pipelines.iter().find(|p| p.is_default).map(|p| p.id),
            };
            let found = stages
                .iter()
                .find(|s| Some(s.pipeline_id) == pipeline && s.key == key)
                .ok_or_else(|| ApiError::invalid("stage", format!("there is no stage `{key}`")))?;
            Some(found.id)
        }
    };
    let rows = core_db::things::list(
        &mut tx,
        core_db::things::Filter {
            pipeline_id,
            stage_id,
            q: q.map(str::trim).filter(|q| !q.is_empty()),
            before,
            limit: limit.clamp(1, 200),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(|thing| {
            let stage = stages
                .iter()
                .find(|s| Some(s.id) == thing.stage_id)
                .map(|s| s.key.clone());
            View { thing, stage }
        })
        .collect())
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<View> {
    ctx.authorize(Permission::ThingRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Clone, Default)]
pub struct New {
    pub name: String,
    pub due_at: Option<DateTime<Utc>>,
    /// Defaults to the entity's default pipeline, if there is one.
    pub pipeline_id: Option<Uuid>,
    /// Stage key; defaults to the pipeline's first stage.
    pub stage: Option<String>,
    pub custom: Map<String, Value>,
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

pub async fn create(state: &AppState, ctx: &Ctx, new: New) -> ApiResult<View> {
    ctx.authorize(Permission::ThingWrite)?;
    let name = required_text("name", &new.name, 200)?;
    let mut tx = begin(state, ctx).await?;
    let defs = super::fields::engine_defs(&mut tx, ENTITY).await?;
    let custom = core_engines::fields::apply(&defs, &Map::new(), &new.custom, Mode::Create)
        .map_err(ApiError::Invalid)?;

    let pipeline = match new.pipeline_id {
        Some(id) => Some(
            core_db::pipelines::get(&mut tx, id)
                .await?
                .filter(|p| p.entity == ENTITY)
                .ok_or_else(|| ApiError::invalid("pipeline_id", "there is no such pipeline"))?,
        ),
        None => core_db::pipelines::default_for(&mut tx, ENTITY).await?,
    };
    let (pipeline_id, stage_id) = match &pipeline {
        None if new.stage.is_some() => {
            return Err(ApiError::invalid(
                "stage",
                "there is no pipeline to place it in",
            ));
        }
        None => (None, None),
        Some(p) => {
            let stages = core_db::pipelines::stages(&mut tx, &[p.id]).await?;
            let stage = match &new.stage {
                Some(key) => find_stage(&stages, key)?,
                None => stages
                    .iter()
                    .find(|s| s.archived_at.is_none())
                    .ok_or_else(|| {
                        ApiError::invalid("pipeline_id", "the pipeline has no stages")
                    })?,
            };
            check_move(None, stage, &custom)?;
            (Some(p.id), Some(stage.id))
        }
    };

    let id = core_domain::ids::new_id();
    let custom = Value::Object(custom);
    core_db::things::insert(
        &mut tx,
        id,
        core_db::things::Values {
            name: &name,
            pipeline_id,
            stage_id,
            stage_entered_at: stage_id.map(|_| Utc::now()),
            due_at: new.due_at,
            custom: &custom,
        },
        ctx.user_id(),
    )
    .await?;
    let view = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "thing.created",
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

#[derive(Debug, Clone, Default)]
pub struct Patch {
    pub name: Option<String>,
    /// `Some(None)` clears it.
    pub due_at: Option<Option<DateTime<Utc>>>,
    /// Move to this stage key (within the thing's pipeline).
    pub stage: Option<String>,
    /// Custom values to set; `null` clears one. Fields not named are kept.
    pub custom: Option<Map<String, Value>>,
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    patch: Patch,
    if_match: Option<i64>,
) -> ApiResult<View> {
    ctx.authorize(Permission::ThingWrite)?;
    let mut tx = begin(state, ctx).await?;
    let view = update_in_tx(&mut tx, ctx, id, patch, if_match).await?;
    tx.commit().await?;
    Ok(view)
}

/// The update, inside the caller's transaction (rule actions use this).
pub(crate) async fn update_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    id: Uuid,
    patch: Patch,
    if_match: Option<i64>,
) -> ApiResult<View> {
    let current = core_db::things::get_for_update(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(current.version, if_match)?;
    let before = View {
        stage: stage_key(tx, &current).await?,
        thing: current.clone(),
    };

    let name = match &patch.name {
        Some(n) => required_text("name", n, 200)?,
        None => current.name.clone(),
    };
    let current_custom = current.custom.as_object().cloned().unwrap_or_default();
    let custom = match &patch.custom {
        Some(changes) => {
            let defs = super::fields::engine_defs(tx, ENTITY).await?;
            core_engines::fields::apply(&defs, &current_custom, changes, Mode::Update)
                .map_err(ApiError::Invalid)?
        }
        None => current_custom,
    };

    let mut stage_id = current.stage_id;
    let mut stage_entered_at = current.stage_entered_at;
    let mut stage_changed = false;
    if let Some(key) = &patch.stage {
        let pipeline = current
            .pipeline_id
            .ok_or_else(|| ApiError::invalid("stage", "this record is not in a pipeline"))?;
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

    let due_at = patch.due_at.unwrap_or(current.due_at);
    let custom = Value::Object(custom);
    let unchanged = name == current.name
        && due_at == current.due_at
        && custom == current.custom
        && !stage_changed;
    if unchanged {
        return Ok(before);
    }

    core_db::things::update(
        tx,
        id,
        core_db::things::Values {
            name: &name,
            pipeline_id: current.pipeline_id,
            stage_id,
            stage_entered_at,
            due_at,
            custom: &custom,
        },
        ctx.user_id(),
    )
    .await?;
    let after = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "thing.updated",
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
                topic: "thing.stage_changed",
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

pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::ThingWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    core_db::things::soft_delete(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "thing.deleted",
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
