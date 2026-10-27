//! Pipelines and their stages.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub id: Uuid,
    pub entity: String,
    pub name: String,
    pub is_default: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Stage {
    pub id: Uuid,
    pub pipeline_id: Uuid,
    pub key: String,
    pub name: String,
    pub position: i32,
    pub colour: Option<String>,
    pub sla_days: Option<i32>,
    pub outcome: String,
    pub required_fields: Vec<String>,
    pub allowed_next: Option<Vec<String>>,
    pub archived_at: Option<DateTime<Utc>>,
}

pub async fn list(conn: &mut PgConnection, entity: Option<&str>) -> Result<Vec<Pipeline>> {
    Ok(sqlx::query_as!(
        Pipeline,
        r#"SELECT id, entity, name, is_default, version, created_at, updated_at
           FROM pipelines
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
             AND ($1::text IS NULL OR entity = $1)
           ORDER BY entity, is_default DESC, created_at"#,
        entity
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Pipeline>> {
    Ok(sqlx::query_as!(
        Pipeline,
        r#"SELECT id, entity, name, is_default, version, created_at, updated_at
           FROM pipelines WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn default_for(conn: &mut PgConnection, entity: &str) -> Result<Option<Pipeline>> {
    Ok(sqlx::query_as!(
        Pipeline,
        r#"SELECT id, entity, name, is_default, version, created_at, updated_at
           FROM pipelines
           WHERE workspace_id = app_workspace_id() AND entity = $1 AND is_default AND deleted_at IS NULL"#,
        entity
    )
    .fetch_optional(conn)
    .await?)
}

/// Stages of the given pipelines (archived included), in order.
pub async fn stages(conn: &mut PgConnection, pipeline_ids: &[Uuid]) -> Result<Vec<Stage>> {
    Ok(sqlx::query_as!(
        Stage,
        r#"SELECT id, pipeline_id, key, name, position, colour, sla_days, outcome, required_fields,
                  allowed_next, archived_at
           FROM pipeline_stages
           WHERE workspace_id = app_workspace_id() AND pipeline_id = ANY($1)
           ORDER BY pipeline_id, archived_at NULLS FIRST, position"#,
        pipeline_ids
    )
    .fetch_all(conn)
    .await?)
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    entity: &str,
    name: &str,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO pipelines (id, workspace_id, entity, name, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $4)"#,
        id,
        entity,
        name,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    name: &str,
    expected_version: Option<i64>,
    by: Option<UserId>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE pipelines SET name = $2, updated_by = $3
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
             AND ($4::bigint IS NULL OR version = $4)"#,
        id,
        name,
        by.map(|u| u.0),
        expected_version
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Makes `id` its entity's default pipeline (and no other).
pub async fn set_default(conn: &mut PgConnection, id: Uuid, entity: &str) -> Result<()> {
    sqlx::query!(
        r#"UPDATE pipelines SET is_default = false
           WHERE workspace_id = app_workspace_id() AND entity = $1 AND is_default AND id <> $2"#,
        entity,
        id
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "UPDATE pipelines SET is_default = true WHERE workspace_id = app_workspace_id() AND id = $1",
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE pipelines SET deleted_at = now(), is_default = false, updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub struct StageValues<'a> {
    pub name: &'a str,
    pub position: i32,
    pub colour: Option<&'a str>,
    pub sla_days: Option<i32>,
    pub outcome: &'a str,
    pub required_fields: &'a [String],
    pub allowed_next: Option<&'a [String]>,
}

pub async fn insert_stage(
    conn: &mut PgConnection,
    id: Uuid,
    pipeline_id: Uuid,
    key: &str,
    v: StageValues<'_>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO pipeline_stages (id, workspace_id, pipeline_id, key, name, position, colour,
                                        sla_days, outcome, required_fields, allowed_next)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
        id,
        pipeline_id,
        key,
        v.name,
        v.position,
        v.colour,
        v.sla_days,
        v.outcome,
        v.required_fields,
        v.allowed_next
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Updates a stage and brings it back if it was archived.
pub async fn update_stage(conn: &mut PgConnection, id: Uuid, v: StageValues<'_>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE pipeline_stages SET name = $2, position = $3, colour = $4, sla_days = $5, outcome = $6,
                  required_fields = $7, allowed_next = $8, archived_at = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.name,
        v.position,
        v.colour,
        v.sla_days,
        v.outcome,
        v.required_fields,
        v.allowed_next
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn archive_stage(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE pipeline_stages SET archived_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1 AND archived_at IS NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Bumps the pipeline's version after its stages change, so `If-Match`
/// covers the whole pipeline.
pub async fn touch(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        "UPDATE pipelines SET updated_by = $2 WHERE workspace_id = app_workspace_id() AND id = $1",
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}
