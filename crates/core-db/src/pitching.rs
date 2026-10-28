//! A workspace's pitching settings: its regions, statuses and what it pitches.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use serde_json::Value;
use sqlx::PgConnection;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Row {
    pub settings: Value,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Option<UserId>,
}

pub async fn settings(conn: &mut PgConnection) -> Result<Option<Row>> {
    Ok(sqlx::query_as!(
        Row,
        r#"SELECT settings, version, updated_at, updated_by AS "updated_by: UserId"
           FROM pitching_settings WHERE workspace_id = app_workspace_id()"#
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn save(conn: &mut PgConnection, settings: &Value, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO pitching_settings (workspace_id, settings, updated_by)
           VALUES (app_workspace_id(), $1, $2)
           ON CONFLICT (workspace_id) DO UPDATE SET settings = EXCLUDED.settings, updated_by = EXCLUDED.updated_by"#,
        settings,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}
