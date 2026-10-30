//! Pipelines: the ordered stages an entity moves through.

use std::collections::HashSet;

use core_db::Tx;
use core_db::pipelines::{Pipeline, Stage};
use core_domain::{Ctx, Permission};
use core_engines::pipeline::{self as engine, Outcome};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Change, begin, check_version, record, required_text};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// Entities whose objects move through a pipeline.
pub const ENTITIES: &[&str] = &["enquiry", "thing"];

#[derive(Debug, Clone)]
pub struct WithStages {
    pub pipeline: Pipeline,
    /// Active stages in order, then archived ones.
    pub stages: Vec<Stage>,
}

#[derive(Debug, Clone)]
pub struct StageInput {
    pub key: String,
    pub name: String,
    pub colour: Option<String>,
    pub sla_days: Option<i32>,
    pub outcome: Outcome,
    pub required_fields: Vec<String>,
    pub allowed_next: Option<Vec<String>>,
}

pub fn to_engine(s: &Stage) -> engine::Stage {
    engine::Stage {
        key: s.key.clone(),
        name: s.name.clone(),
        outcome: Outcome::parse(&s.outcome).unwrap_or_default(),
        required_fields: s.required_fields.clone(),
        allowed_next: s.allowed_next.clone(),
        sla_days: s.sla_days,
        archived: s.archived_at.is_some(),
    }
}

fn check_entity(entity: &str) -> ApiResult<()> {
    if ENTITIES.contains(&entity) {
        Ok(())
    } else {
        Err(ApiError::invalid(
            "entity",
            format!(
                "`{entity}` does not use pipelines; use one of: {}",
                ENTITIES.join(", ")
            ),
        ))
    }
}

async fn check_stage_inputs(tx: &mut Tx, entity: &str, stages: &[StageInput]) -> ApiResult<()> {
    for (i, s) in stages.iter().enumerate() {
        if let Some(c) = &s.colour {
            let ok =
                c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit());
            if !ok {
                return Err(ApiError::invalid(
                    format!("stages[{i}].colour"),
                    "use a hex colour like #3366ff",
                ));
            }
        }
    }
    let fields = core_db::fields::list(tx, Some(entity), false).await?;
    let keys: HashSet<&str> = fields.iter().map(|f| f.key.as_str()).collect();
    let as_engine: Vec<engine::Stage> = stages
        .iter()
        .map(|s| engine::Stage {
            key: s.key.clone(),
            name: s.name.clone(),
            outcome: s.outcome,
            required_fields: s.required_fields.clone(),
            allowed_next: s.allowed_next.clone(),
            sla_days: s.sla_days,
            archived: false,
        })
        .collect();
    engine::check_stages(&as_engine, &keys)?;
    Ok(())
}

fn stage_values(s: &StageInput, position: i32) -> core_db::pipelines::StageValues<'_> {
    core_db::pipelines::StageValues {
        name: s.name.trim(),
        position,
        colour: s.colour.as_deref(),
        sla_days: s.sla_days,
        outcome: s.outcome.as_str(),
        required_fields: &s.required_fields,
        allowed_next: s.allowed_next.as_deref(),
    }
}

pub(crate) async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<WithStages>> {
    let Some(pipeline) = core_db::pipelines::get(tx, id).await? else {
        return Ok(None);
    };
    let stages = core_db::pipelines::stages(tx, &[id]).await?;
    Ok(Some(WithStages { pipeline, stages }))
}

fn view_json(p: &WithStages) -> Value {
    json!({
        "entity": p.pipeline.entity,
        "name": p.pipeline.name,
        "is_default": p.pipeline.is_default,
        "stages": p.stages.iter().filter(|s| s.archived_at.is_none()).map(|s| json!({
            "key": s.key, "name": s.name, "outcome": s.outcome, "colour": s.colour, "sla_days": s.sla_days,
            "required_fields": s.required_fields, "allowed_next": s.allowed_next,
        })).collect::<Vec<_>>(),
    })
}

pub async fn list(state: &AppState, ctx: &Ctx, entity: Option<&str>) -> ApiResult<Vec<WithStages>> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let pipelines = core_db::pipelines::list(&mut tx, entity).await?;
    let ids: Vec<Uuid> = pipelines.iter().map(|p| p.id).collect();
    let mut stages = core_db::pipelines::stages(&mut tx, &ids).await?;
    tx.commit().await?;
    Ok(pipelines
        .into_iter()
        .map(|pipeline| {
            let (mine, rest): (Vec<Stage>, Vec<Stage>) =
                stages.drain(..).partition(|s| s.pipeline_id == pipeline.id);
            stages = rest;
            WithStages {
                pipeline,
                stages: mine,
            }
        })
        .collect())
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<WithStages> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let p = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(p)
}

pub async fn create(
    state: &AppState,
    ctx: &Ctx,
    entity: &str,
    name: &str,
    is_default: bool,
    stages: Vec<StageInput>,
) -> ApiResult<WithStages> {
    ctx.authorize(Permission::ConfigWrite)?;
    check_entity(entity)?;
    let name = required_text("name", name, 200)?;
    let mut tx = begin(state, ctx).await?;
    check_stage_inputs(&mut tx, entity, &stages).await?;
    let id = core_domain::ids::new_id();
    core_db::pipelines::insert(&mut tx, id, entity, &name, ctx.user_id()).await?;
    for (i, s) in (0..).zip(&stages) {
        core_db::pipelines::insert_stage(
            &mut tx,
            core_domain::ids::new_id(),
            id,
            &s.key,
            stage_values(s, i),
        )
        .await?;
    }
    // The first pipeline of an entity is its default.
    let first = core_db::pipelines::default_for(&mut tx, entity)
        .await?
        .is_none();
    if is_default || first {
        core_db::pipelines::set_default(&mut tx, id, entity).await?;
    }
    let created = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "pipeline.created",
            resource: "pipeline",
            object_id: Some(id),
            before: None,
            after: Some(view_json(&created)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(created)
}

#[derive(Debug, Clone, Default)]
pub struct Patch {
    pub name: Option<String>,
    /// `true` makes this the entity's default pipeline.
    pub make_default: bool,
    /// The full list of stages, in order. Stages are matched by key: new keys
    /// are added, missing ones archived (objects in them stay until moved).
    pub stages: Option<Vec<StageInput>>,
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    patch: Patch,
    if_match: Option<i64>,
) -> ApiResult<WithStages> {
    ctx.authorize(Permission::ConfigWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    check_version(before.pipeline.version, if_match)?;
    let entity = before.pipeline.entity.clone();

    if let Some(name) = &patch.name {
        let name = required_text("name", name, 200)?;
        core_db::pipelines::update(&mut tx, id, &name, None, ctx.user_id()).await?;
    }
    if let Some(stages) = &patch.stages {
        check_stage_inputs(&mut tx, &entity, stages).await?;
        for (i, input) in (0..).zip(stages) {
            match before.stages.iter().find(|s| s.key == input.key) {
                Some(existing) => {
                    core_db::pipelines::update_stage(&mut tx, existing.id, stage_values(input, i))
                        .await?;
                }
                None => {
                    core_db::pipelines::insert_stage(
                        &mut tx,
                        core_domain::ids::new_id(),
                        id,
                        &input.key,
                        stage_values(input, i),
                    )
                    .await?;
                }
            }
        }
        for old in &before.stages {
            if !stages.iter().any(|s| s.key == old.key) {
                core_db::pipelines::archive_stage(&mut tx, old.id).await?;
            }
        }
        core_db::pipelines::touch(&mut tx, id, ctx.user_id()).await?;
    }
    if patch.make_default && !before.pipeline.is_default {
        core_db::pipelines::set_default(&mut tx, id, &entity).await?;
    }
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "pipeline.updated",
            resource: "pipeline",
            object_id: Some(id),
            before: Some(view_json(&before)),
            after: Some(view_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// Deletes an unused pipeline.
pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::ConfigWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let in_use = core_db::things::count_in_pipeline(&mut tx, id).await?
        + core_db::enquiries::count_in_pipeline(&mut tx, id).await?;
    if in_use > 0 {
        return Err(ApiError::conflict(
            "pipeline_in_use",
            format!("{in_use} records are still in this pipeline; move them first"),
        ));
    }
    core_db::pipelines::soft_delete(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "pipeline.deleted",
            resource: "pipeline",
            object_id: Some(id),
            before: Some(view_json(&before)),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
