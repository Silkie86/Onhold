//! Per-workspace API keys. Only the SHA-256 of the secret is stored.

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct ApiKey {
    pub id: Uuid,
    pub name: String,
    pub permissions: Vec<String>,
    pub created_by: Option<UserId>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

pub struct NewApiKey<'a> {
    pub id: Uuid,
    pub workspace: WorkspaceId,
    pub name: &'a str,
    pub secret_hash: &'a [u8],
    pub permissions: &'a [String],
    pub created_by: Option<UserId>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub async fn insert(conn: &mut PgConnection, new: NewApiKey<'_>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO api_keys (id, workspace_id, name, secret_hash, permissions, created_by, expires_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        new.id,
        new.workspace.0,
        new.name,
        new.secret_hash,
        new.permissions,
        new.created_by.map(|u| u.0),
        new.expires_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn list(conn: &mut PgConnection) -> Result<Vec<ApiKey>> {
    Ok(sqlx::query_as!(
        ApiKey,
        r#"SELECT id, name, permissions, created_by AS "created_by: UserId", created_at, expires_at,
                  last_used_at, revoked_at
           FROM api_keys WHERE workspace_id = app_workspace_id()
           ORDER BY created_at DESC"#
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<ApiKey>> {
    Ok(sqlx::query_as!(
        ApiKey,
        r#"SELECT id, name, permissions, created_by AS "created_by: UserId", created_at, expires_at,
                  last_used_at, revoked_at
           FROM api_keys WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn revoke(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        "UPDATE api_keys SET revoked_at = now() WHERE workspace_id = app_workspace_id() AND id = $1 AND revoked_at IS NULL",
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Updates `last_used_at`, at most once a minute per key.
pub async fn touch(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE api_keys SET last_used_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1
             AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}
