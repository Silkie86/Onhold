//! Automation rules, their execution log and the once-per-object guard.

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Rule {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub dry_run: bool,
    pub once_per_object: bool,
    pub trigger: Value,
    pub conditions: Value,
    pub actions: Value,
    pub next_run_at: Option<DateTime<Utc>>,
    pub scanned_until: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn list(conn: &mut PgConnection) -> Result<Vec<Rule>> {
    Ok(sqlx::query_as!(
        Rule,
        r#"SELECT id, name, enabled, dry_run, once_per_object, trigger, conditions, actions, next_run_at,
                  scanned_until, version, created_at, updated_at
           FROM rules WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
           ORDER BY created_at"#
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Rule>> {
    Ok(sqlx::query_as!(
        Rule,
        r#"SELECT id, name, enabled, dry_run, once_per_object, trigger, conditions, actions, next_run_at,
                  scanned_until, version, created_at, updated_at
           FROM rules WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Enabled rules triggered by `topic`.
pub async fn for_event(conn: &mut PgConnection, topic: &str) -> Result<Vec<Rule>> {
    Ok(sqlx::query_as!(
        Rule,
        r#"SELECT id, name, enabled, dry_run, once_per_object, trigger, conditions, actions, next_run_at,
                  scanned_until, version, created_at, updated_at
           FROM rules
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL AND enabled
             AND trigger_kind = 'event' AND trigger_event = $1
           ORDER BY created_at"#,
        topic
    )
    .fetch_all(conn)
    .await?)
}

pub struct Values<'a> {
    pub name: &'a str,
    pub enabled: bool,
    pub dry_run: bool,
    pub once_per_object: bool,
    pub trigger: &'a Value,
    pub conditions: &'a Value,
    pub actions: &'a Value,
    pub trigger_kind: &'a str,
    pub trigger_event: Option<&'a str>,
    /// For time-based rules: when the scheduler first looks.
    pub next_run_at: Option<DateTime<Utc>>,
    pub scanned_until: Option<DateTime<Utc>>,
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO rules (id, workspace_id, name, enabled, dry_run, once_per_object, trigger, conditions,
                              actions, trigger_kind, trigger_event, next_run_at, scanned_until,
                              created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13)"#,
        id,
        v.name,
        v.enabled,
        v.dry_run,
        v.once_per_object,
        v.trigger,
        v.conditions,
        v.actions,
        v.trigger_kind,
        v.trigger_event,
        v.next_run_at,
        v.scanned_until,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    expected_version: Option<i64>,
    by: Option<UserId>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE rules SET name = $2, enabled = $3, dry_run = $4, once_per_object = $5, trigger = $6,
                  conditions = $7, actions = $8, trigger_kind = $9, trigger_event = $10,
                  next_run_at = $11, scanned_until = $12, updated_by = $13
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
             AND ($14::bigint IS NULL OR version = $14)"#,
        id,
        v.name,
        v.enabled,
        v.dry_run,
        v.once_per_object,
        v.trigger,
        v.conditions,
        v.actions,
        v.trigger_kind,
        v.trigger_event,
        v.next_run_at,
        v.scanned_until,
        by.map(|u| u.0),
        expected_version
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE rules SET deleted_at = now(), enabled = false, updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Records how far a date-offset rule has scanned. App role, in the
/// rule's workspace; scheduler bookkeeping does not bump the version.
pub async fn set_scanned_until(
    conn: &mut PgConnection,
    id: Uuid,
    until: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        "UPDATE rules SET scanned_until = $2 WHERE workspace_id = app_workspace_id() AND id = $1",
        id,
        until
    )
    .execute(conn)
    .await?;
    Ok(())
}

// Once per object -----------------------------------------------------------------

/// Claims the rule's single run for `object`. False if it already ran.
pub async fn claim_fire(conn: &mut PgConnection, rule_id: Uuid, object_id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"INSERT INTO rule_fires (workspace_id, rule_id, object_id) VALUES (app_workspace_id(), $1, $2)
           ON CONFLICT DO NOTHING"#,
        rule_id,
        object_id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

// Execution log --------------------------------------------------------------------

pub struct NewRun<'a> {
    pub rule_id: Uuid,
    pub event_id: Option<Uuid>,
    pub object_id: Option<Uuid>,
    pub outcome: &'a str,
    pub reason: Option<&'a str>,
    pub actions: &'a Value,
}

pub async fn record_run(conn: &mut PgConnection, run: NewRun<'_>) -> Result<Uuid> {
    let id = core_domain::ids::new_id();
    sqlx::query!(
        r#"INSERT INTO rule_runs (id, workspace_id, rule_id, event_id, object_id, outcome, reason, actions)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7)"#,
        id,
        run.rule_id,
        run.event_id,
        run.object_id,
        run.outcome,
        run.reason,
        run.actions
    )
    .execute(conn)
    .await?;
    Ok(id)
}

#[derive(Debug, Clone)]
pub struct Run {
    pub id: Uuid,
    pub event_id: Option<Uuid>,
    pub object_id: Option<Uuid>,
    pub outcome: String,
    pub reason: Option<String>,
    pub actions: Value,
    pub created_at: DateTime<Utc>,
}

pub async fn runs(
    conn: &mut PgConnection,
    rule_id: Uuid,
    before: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Run>> {
    Ok(sqlx::query_as!(
        Run,
        r#"SELECT id, event_id, object_id, outcome, reason, actions, created_at
           FROM rule_runs
           WHERE workspace_id = app_workspace_id() AND rule_id = $1 AND ($2::uuid IS NULL OR id < $2)
           ORDER BY id DESC
           LIMIT $3"#,
        rule_id,
        before,
        limit
    )
    .fetch_all(conn)
    .await?)
}

// The scheduler (worker role) -------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Due {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub trigger: Value,
    pub next_run_at: DateTime<Utc>,
    pub default_timezone: String,
}

/// Enabled time-based rules due by `now`, in workspaces that are running
/// and have automations on.
pub async fn due(conn: &mut PgConnection, now: DateTime<Utc>, limit: i64) -> Result<Vec<Due>> {
    Ok(sqlx::query_as!(
        Due,
        r#"SELECT r.id, r.workspace_id AS "workspace_id: WorkspaceId", r.trigger,
                  r.next_run_at AS "next_run_at!", w.default_timezone
           FROM rules r JOIN workspaces w ON w.id = r.workspace_id
           WHERE r.deleted_at IS NULL AND r.enabled AND r.next_run_at <= $1
             AND w.status IN ('trial', 'active') AND NOT w.automations_paused
           ORDER BY r.next_run_at
           LIMIT $2
           FOR UPDATE OF r SKIP LOCKED"#,
        now,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Moves a time-based rule's next look to `next` (none: stop).
pub async fn advance(conn: &mut PgConnection, id: Uuid, next: Option<DateTime<Utc>>) -> Result<()> {
    sqlx::query!("UPDATE rules SET next_run_at = $2 WHERE id = $1", id, next)
        .execute(conn)
        .await?;
    Ok(())
}

/// Marks a rule as installed from a ready-made follow-up.
pub async fn set_preset(conn: &mut PgConnection, id: Uuid, preset: &str) -> Result<()> {
    sqlx::query!(
        "UPDATE rules SET preset = $2 WHERE workspace_id = app_workspace_id() AND id = $1",
        id,
        preset
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The live rules installed from ready-made follow-ups: `(preset, id, enabled)`.
pub async fn presets(conn: &mut PgConnection) -> Result<Vec<(String, Uuid, bool)>> {
    Ok(sqlx::query!(
        r#"SELECT preset AS "preset!", id, enabled FROM rules
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL AND preset IS NOT NULL
           ORDER BY preset"#
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| (r.preset, r.id, r.enabled))
    .collect())
}

/// Holds the workspace's ready-made follow-ups until the transaction ends.
pub async fn lock_presets(conn: &mut PgConnection) -> Result<()> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended('rule-presets:' || app_workspace_id()::text, 0))"
    )
    .execute(conn)
    .await?;
    Ok(())
}
