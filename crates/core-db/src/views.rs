//! Saved list views: a list's columns, sort and filters under a name.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct View {
    pub id: Uuid,
    pub entity: String,
    pub name: String,
    /// `None` when shared with the workspace.
    pub owner_id: Option<UserId>,
    pub config: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The views the caller can see for an entity: shared ones and their own.
pub async fn list(conn: &mut PgConnection, entity: &str) -> Result<Vec<View>> {
    Ok(sqlx::query_as!(
        View,
        r#"SELECT id, entity, name, owner_id AS "owner_id: UserId", config, version, created_at, updated_at
           FROM saved_views
           WHERE workspace_id = app_workspace_id() AND entity = $1 AND deleted_at IS NULL
           ORDER BY owner_id NULLS FIRST, lower(name), id"#,
        entity
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get_for_update(conn: &mut PgConnection, id: Uuid) -> Result<Option<View>> {
    Ok(sqlx::query_as!(
        View,
        r#"SELECT id, entity, name, owner_id AS "owner_id: UserId", config, version, created_at, updated_at
           FROM saved_views
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    entity: &str,
    name: &str,
    owner: Option<UserId>,
    config: &Value,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO saved_views (id, workspace_id, entity, name, owner_id, config, created_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6)"#,
        id,
        entity,
        name,
        owner.map(|u| u.0),
        config,
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
    owner: Option<UserId>,
    config: &Value,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE saved_views SET name = $2, owner_id = $3, config = $4
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        name,
        owner.map(|u| u.0),
        config
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE saved_views SET deleted_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}
