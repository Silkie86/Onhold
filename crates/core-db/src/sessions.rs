//! Sessions of browsers (cookie) and of the desktop and mobile apps (bearer
//! token). Only the SHA-256 of the token is stored. Rows are visible to their
//! own user only.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

pub struct NewSession<'a> {
    pub user_id: UserId,
    pub token_hash: &'a [u8],
    pub csrf_token: &'a str,
    pub auth_method: &'a str,
    pub mfa_verified: bool,
    pub ip: Option<IpAddr>,
    pub user_agent: Option<&'a str>,
    pub expires_at: DateTime<Utc>,
    /// `browser` or `app`.
    pub client: &'a str,
    pub device_name: Option<&'a str>,
}

pub async fn insert(conn: &mut PgConnection, new: NewSession<'_>) -> Result<Uuid> {
    let id = core_domain::ids::new_id();
    sqlx::query!(
        r#"INSERT INTO sessions (id, user_id, token_hash, csrf_token, auth_method, mfa_verified,
                                 ip, user_agent, expires_at, client, device_name)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
        id,
        new.user_id.0,
        new.token_hash,
        new.csrf_token,
        new.auth_method,
        new.mfa_verified,
        new.ip as Option<IpAddr>,
        new.user_agent,
        new.expires_at,
        new.client,
        new.device_name
    )
    .execute(conn)
    .await?;
    Ok(id)
}

/// Updates `last_seen_at`, at most once a minute per session to keep writes cheap.
pub async fn touch(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        "UPDATE sessions SET last_seen_at = now() WHERE id = $1 AND last_seen_at < now() - interval '1 minute'",
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Completes sign-in for a session that was waiting on its second factor,
/// extending it from the short pending lifetime to the full one.
pub async fn mark_mfa_verified(
    conn: &mut PgConnection,
    id: Uuid,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        "UPDATE sessions SET mfa_verified = true, mfa_failed_attempts = 0, expires_at = $2 WHERE id = $1",
        id,
        expires_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Counts a wrong second-factor code; revokes the session once `max` is reached.
/// Returns true if the session is still usable.
pub async fn record_mfa_failure(conn: &mut PgConnection, id: Uuid, max: i32) -> Result<bool> {
    let attempts = sqlx::query_scalar!(
        r#"UPDATE sessions
           SET mfa_failed_attempts = mfa_failed_attempts + 1,
               revoked_at = CASE WHEN mfa_failed_attempts + 1 >= $2 THEN now() ELSE revoked_at END
           WHERE id = $1
           RETURNING mfa_failed_attempts"#,
        id,
        max
    )
    .fetch_one(conn)
    .await?;
    Ok(attempts < max)
}

pub async fn revoke(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        "UPDATE sessions SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Revokes every session of the user in context except `keep`.
pub async fn revoke_all_except(
    conn: &mut PgConnection,
    user: UserId,
    keep: Option<Uuid>,
) -> Result<u64> {
    let done = sqlx::query!(
        "UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL AND id IS DISTINCT FROM $2",
        user.0,
        keep
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: Uuid,
    pub auth_method: String,
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub client: String,
    pub device_name: Option<String>,
}

/// Active sessions of the user in context, newest first.
pub async fn list_active(conn: &mut PgConnection, user: UserId) -> Result<Vec<SessionInfo>> {
    Ok(sqlx::query_as!(
        SessionInfo,
        r#"SELECT id, auth_method, ip AS "ip: IpAddr", user_agent, created_at, last_seen_at, expires_at,
                  client, device_name
           FROM sessions
           WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now()
           ORDER BY last_seen_at DESC"#,
        user.0
    )
    .fetch_all(conn)
    .await?)
}

pub async fn start_impersonation(
    conn: &mut PgConnection,
    id: Uuid,
    workspace: WorkspaceId,
    until: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        "UPDATE sessions SET impersonated_workspace_id = $2, impersonation_expires_at = $3 WHERE id = $1",
        id,
        workspace.0,
        until
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn end_impersonation(conn: &mut PgConnection, id: Uuid) -> Result<Option<WorkspaceId>> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE sessions s SET impersonated_workspace_id = NULL, impersonation_expires_at = NULL
           FROM (SELECT impersonated_workspace_id AS old FROM sessions WHERE id = $1 FOR UPDATE) prev
           WHERE s.id = $1
           RETURNING prev.old AS "old: WorkspaceId""#,
        id
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}
