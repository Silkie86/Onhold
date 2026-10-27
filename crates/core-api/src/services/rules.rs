//! Automation rules: saving them (with every reference checked), the
//! execution log, the dry-run tester and the workspace kill switch.
//! Running them is [`super::automation`].

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use core_db::Tx;
use core_db::rules::Rule;
use core_domain::{Ctx, Permission};
use core_engines::fields::FieldType;
use core_engines::rules::{self as engine, Action, RuleDef, Trigger};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Change, begin, check_version, record, required_text};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// How often the scheduler looks for dates that a date-offset rule has reached.
pub const SCAN_INTERVAL: chrono::Duration = chrono::Duration::minutes(5);

/// What a workspace's rules may refer to.
pub(crate) struct Catalog {
    thing_dates: Vec<String>,
    email_templates: HashSet<String>,
}

impl engine::Catalog for Catalog {
    fn date_fields(&self, entity: &str) -> Option<Vec<String>> {
        (entity == super::things::ENTITY).then(|| self.thing_dates.clone())
    }

    fn template_exists(&self, key: &str) -> bool {
        self.email_templates.contains(key)
    }

    fn is_valid_timezone(&self, tz: &str) -> bool {
        tz.parse::<chrono_tz::Tz>().is_ok()
    }
}

pub(crate) async fn catalog(tx: &mut Tx) -> ApiResult<Catalog> {
    let mut thing_dates = vec!["due_at".to_owned()];
    for f in core_db::fields::list(tx, Some(super::things::ENTITY), false).await? {
        if matches!(
            FieldType::parse(&f.field_type),
            Some(FieldType::Date | FieldType::Datetime)
        ) {
            thing_dates.push(format!("custom.{}", f.key));
        }
    }
    let email_templates = core_db::templates::list(tx, Some("email"))
        .await?
        .into_iter()
        .map(|t| t.key)
        .collect();
    Ok(Catalog {
        thing_dates,
        email_templates,
    })
}

/// Parses a stored rule; a rule that no longer parses (say a template it
/// used was removed) is reported, not run.
pub(crate) fn parse_stored(rule: &Rule, catalog: &Catalog) -> Result<RuleDef, String> {
    engine::parse(&rule.trigger, &rule.conditions, &rule.actions, catalog)
        .map_err(|i| i.to_string())
}

/// Checks references the engine cannot: webhooks, custom fields and stages.
async fn check_references(tx: &mut Tx, def: &RuleDef) -> ApiResult<()> {
    for (i, action) in def.actions.iter().enumerate() {
        match action {
            Action::CallWebhook { webhook_id } => {
                if core_db::webhooks::get(tx, *webhook_id).await?.is_none() {
                    return Err(ApiError::invalid(
                        format!("actions[{i}].webhook_id"),
                        "there is no such webhook",
                    ));
                }
            }
            Action::SetField { field, value } => {
                if let Some(key) = field.strip_prefix("custom.") {
                    let defs = super::fields::engine_defs(tx, super::things::ENTITY).await?;
                    let def = defs
                        .iter()
                        .find(|d| d.key == key && !d.archived)
                        .ok_or_else(|| {
                            ApiError::invalid(
                                format!("actions[{i}].field"),
                                format!("there is no field `{key}`"),
                            )
                        })?;
                    if !value.is_null() {
                        core_engines::fields::normalize(def, value)
                            .map_err(|m| ApiError::invalid(format!("actions[{i}].value"), m))?;
                    }
                }
            }
            Action::MoveStage { stage } => {
                let pipelines = core_db::pipelines::list(tx, Some(super::things::ENTITY)).await?;
                let ids: Vec<Uuid> = pipelines.iter().map(|p| p.id).collect();
                let stages = core_db::pipelines::stages(tx, &ids).await?;
                if !stages
                    .iter()
                    .any(|s| &s.key == stage && s.archived_at.is_none())
                {
                    return Err(ApiError::invalid(
                        format!("actions[{i}].stage"),
                        format!("no pipeline has a stage `{stage}`"),
                    ));
                }
            }
            Action::SendEmail { .. } => {}
        }
    }
    Ok(())
}

async fn workspace_tz(tx: &mut Tx) -> ApiResult<chrono_tz::Tz> {
    let tz = sqlx::query_scalar!(
        "SELECT default_timezone FROM workspaces WHERE id = app_workspace_id()"
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(tz.parse().unwrap_or(chrono_tz::Tz::UTC))
}

/// The scheduler columns for a rule's trigger, as of `now`.
pub(crate) fn schedule(
    trigger: &Trigger,
    workspace_tz: chrono_tz::Tz,
    now: DateTime<Utc>,
) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    match trigger {
        Trigger::Event { .. } => (None, None),
        // Look soon, from now: dates already passed when the rule is saved do not fire.
        Trigger::DateOffset { .. } => (Some(now), Some(now)),
        Trigger::Schedule { cron, timezone } => {
            let tz = timezone
                .as_deref()
                .and_then(|t| t.parse().ok())
                .unwrap_or(workspace_tz);
            let next = core_engines::schedule::parse_cron(cron)
                .ok()
                .and_then(|c| core_engines::schedule::next_run(&c, tz, now));
            (next, None)
        }
    }
}

#[derive(Debug, Clone)]
pub struct Input {
    pub name: String,
    pub enabled: bool,
    pub dry_run: bool,
    pub once_per_object: bool,
    pub trigger: Value,
    pub conditions: Value,
    pub actions: Value,
}

fn view_json(r: &Rule) -> Value {
    json!({ "name": r.name, "enabled": r.enabled, "dry_run": r.dry_run, "once_per_object": r.once_per_object,
            "trigger": r.trigger, "conditions": r.conditions, "actions": r.actions })
}

async fn save(
    tx: &mut Tx,
    ctx: &Ctx,
    id: Uuid,
    input: &Input,
    existing: Option<&Rule>,
    if_match: Option<i64>,
) -> ApiResult<()> {
    let name = required_text("name", &input.name, 200)?;
    let catalog = catalog(tx).await?;
    let def = engine::parse(&input.trigger, &input.conditions, &input.actions, &catalog)?;
    check_references(tx, &def).await?;
    let (event, kind) = match &def.trigger {
        Trigger::Event { topic } => (Some(topic.as_str()), "event"),
        t => (None, t.kind()),
    };
    let now = Utc::now();
    let (mut next_run_at, mut scanned_until) = schedule(&def.trigger, workspace_tz(tx).await?, now);
    // An unchanged date-offset trigger keeps its place in the scan.
    if let Some(old) = existing
        && old.trigger == input.trigger
        && matches!(def.trigger, Trigger::DateOffset { .. })
    {
        next_run_at = old.next_run_at.or(next_run_at);
        scanned_until = old.scanned_until.or(scanned_until);
    }
    let values = core_db::rules::Values {
        name: &name,
        enabled: input.enabled,
        dry_run: input.dry_run,
        once_per_object: input.once_per_object,
        trigger: &input.trigger,
        conditions: &input.conditions,
        actions: &input.actions,
        trigger_kind: kind,
        trigger_event: event,
        next_run_at,
        scanned_until,
    };
    match existing {
        None => core_db::rules::insert(tx, id, values, ctx.user_id()).await?,
        Some(_) => {
            if !core_db::rules::update(tx, id, values, if_match, ctx.user_id()).await? {
                return Err(ApiError::version_conflict());
            }
        }
    }
    Ok(())
}

pub async fn list(state: &AppState, ctx: &Ctx) -> ApiResult<Vec<Rule>> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::rules::list(&mut tx).await?;
    tx.commit().await?;
    Ok(rows)
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<Rule> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let r = core_db::rules::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(r)
}

pub async fn create(state: &AppState, ctx: &Ctx, input: Input) -> ApiResult<Rule> {
    ctx.authorize(Permission::ConfigWrite)?;
    let mut tx = begin(state, ctx).await?;
    let id = core_domain::ids::new_id();
    save(&mut tx, ctx, id, &input, None, None).await?;
    let rule = core_db::rules::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "rule.created",
            resource: "rule",
            object_id: Some(id),
            before: None,
            after: Some(view_json(&rule)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(rule)
}

#[derive(Debug, Default)]
pub struct Patch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub dry_run: Option<bool>,
    pub once_per_object: Option<bool>,
    pub trigger: Option<Value>,
    pub conditions: Option<Value>,
    pub actions: Option<Value>,
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    patch: Patch,
    if_match: Option<i64>,
) -> ApiResult<Rule> {
    ctx.authorize(Permission::ConfigWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::rules::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(before.version, if_match)?;
    let input = Input {
        name: patch.name.unwrap_or_else(|| before.name.clone()),
        enabled: patch.enabled.unwrap_or(before.enabled),
        dry_run: patch.dry_run.unwrap_or(before.dry_run),
        once_per_object: patch.once_per_object.unwrap_or(before.once_per_object),
        trigger: patch.trigger.unwrap_or_else(|| before.trigger.clone()),
        conditions: patch
            .conditions
            .unwrap_or_else(|| before.conditions.clone()),
        actions: patch.actions.unwrap_or_else(|| before.actions.clone()),
    };
    save(&mut tx, ctx, id, &input, Some(&before), if_match).await?;
    let after = core_db::rules::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "rule.updated",
            resource: "rule",
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

pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::ConfigWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::rules::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    core_db::rules::soft_delete(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "rule.deleted",
            resource: "rule",
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

/// The rule's execution log, newest first.
pub async fn log(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    before: Option<Uuid>,
    limit: i64,
) -> ApiResult<Vec<core_db::rules::Run>> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    core_db::rules::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let runs = core_db::rules::runs(&mut tx, id, before, limit.clamp(1, 200)).await?;
    tx.commit().await?;
    Ok(runs)
}

pub async fn automations_paused(state: &AppState, ctx: &Ctx) -> ApiResult<bool> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let paused = core_db::workspaces::automations_paused(&mut tx).await?;
    tx.commit().await?;
    Ok(paused)
}

/// The kill switch: while paused, no rule in the workspace runs.
pub async fn set_automations_paused(state: &AppState, ctx: &Ctx, paused: bool) -> ApiResult<bool> {
    ctx.authorize(Permission::ConfigWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::workspaces::automations_paused(&mut tx).await?;
    if before != paused {
        core_db::workspaces::set_automations_paused(&mut tx, paused).await?;
        record(
            &mut tx,
            ctx,
            Change {
                topic: if paused {
                    "automations.paused"
                } else {
                    "automations.resumed"
                },
                resource: "workspace",
                object_id: Some(ctx.workspace_id.0),
                before: Some(json!({ "automations_paused": before })),
                after: Some(json!({ "automations_paused": paused })),
                event_extra: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(paused)
}
