//! Custom field definitions.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub id: Uuid,
    pub entity: String,
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub required: bool,
    pub default_value: Option<Value>,
    pub validation: Value,
    pub visibility: String,
    pub searchable: bool,
    pub field_group: Option<String>,
    pub position: i32,
    pub help_text: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

/// One entity's fields (all entities when `None`), in form order.
pub async fn list(
    conn: &mut PgConnection,
    entity: Option<&str>,
    include_archived: bool,
) -> Result<Vec<FieldDef>> {
    Ok(sqlx::query_as!(
        FieldDef,
        r#"SELECT id, entity, key, label, field_type, required, default_value, validation, visibility,
                  searchable, field_group, position, help_text, version, created_at, updated_at, archived_at
           FROM custom_field_defs
           WHERE workspace_id = app_workspace_id()
             AND ($1::text IS NULL OR entity = $1)
             AND ($2 OR archived_at IS NULL)
           ORDER BY entity, position, created_at"#,
        entity,
        include_archived
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<FieldDef>> {
    Ok(sqlx::query_as!(
        FieldDef,
        r#"SELECT id, entity, key, label, field_type, required, default_value, validation, visibility,
                  searchable, field_group, position, help_text, version, created_at, updated_at, archived_at
           FROM custom_field_defs WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Every column a definition stores, for inserts and full updates.
pub struct Values<'a> {
    pub label: &'a str,
    pub required: bool,
    pub default_value: Option<&'a Value>,
    pub validation: &'a Value,
    pub visibility: &'a str,
    pub searchable: bool,
    pub field_group: Option<&'a str>,
    pub position: i32,
    pub help_text: Option<&'a str>,
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    entity: &str,
    key: &str,
    field_type: &str,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO custom_field_defs (id, workspace_id, entity, key, label, field_type, required,
                                          default_value, validation, visibility, searchable, field_group,
                                          position, help_text, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $14)"#,
        id,
        entity,
        key,
        v.label,
        field_type,
        v.required,
        v.default_value,
        v.validation,
        v.visibility,
        v.searchable,
        v.field_group,
        v.position,
        v.help_text,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Saves new values if the row is still at `expected_version` (any version
/// when `None`). Returns false when the version moved on.
pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    expected_version: Option<i64>,
    by: Option<UserId>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE custom_field_defs SET label = $2, required = $3, default_value = $4, validation = $5,
                  visibility = $6, searchable = $7, field_group = $8, position = $9, help_text = $10,
                  updated_by = $11
           WHERE workspace_id = app_workspace_id() AND id = $1 AND archived_at IS NULL
             AND ($12::bigint IS NULL OR version = $12)"#,
        id,
        v.label,
        v.required,
        v.default_value,
        v.validation,
        v.visibility,
        v.searchable,
        v.field_group,
        v.position,
        v.help_text,
        by.map(|u| u.0),
        expected_version
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn archive(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE custom_field_defs SET archived_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND archived_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}
