//! Invitations to join a workspace. Only the token's SHA-256 is stored.

use chrono::{DateTime, Utc};
use core_domain::{Role, UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Invitation {
    pub id: Uuid,
    pub email: String,
    pub role: Role,
    pub invited_by: Option<UserId>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

pub struct NewInvitation<'a> {
    pub workspace: WorkspaceId,
    pub email: &'a str,
    pub role: Role,
    pub token_hash: &'a [u8],
    pub expires_at: DateTime<Utc>,
    pub invited_by: Option<UserId>,
}

pub async fn insert(conn: &mut PgConnection, new: NewInvitation<'_>) -> Result<Uuid> {
    let id = core_domain::ids::new_id();
    sqlx::query!(
        r#"INSERT INTO invitations (id, workspace_id, email, role, token_hash, expires_at, invited_by)
           VALUES ($1, $2, $3::text::citext, $4, $5, $6, $7)"#,
        id,
        new.workspace.0,
        new.email,
        new.role as Role,
        new.token_hash,
        new.expires_at,
        new.invited_by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(id)
}

/// Invitations in the workspace in context, newest first.
pub async fn list(conn: &mut PgConnection) -> Result<Vec<Invitation>> {
    Ok(sqlx::query_as!(
        Invitation,
        r#"SELECT id, email::text AS "email!", role AS "role: Role", invited_by AS "invited_by: UserId",
                  created_at, expires_at, accepted_at, revoked_at
           FROM invitations WHERE workspace_id = app_workspace_id()
           ORDER BY created_at DESC
           LIMIT 500"#
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Invitation>> {
    Ok(sqlx::query_as!(
        Invitation,
        r#"SELECT id, email::text AS "email!", role AS "role: Role", invited_by AS "invited_by: UserId",
                  created_at, expires_at, accepted_at, revoked_at
           FROM invitations WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn revoke(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE invitations SET revoked_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1 AND revoked_at IS NULL AND accepted_at IS NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Marks an invitation accepted. Returns false if someone else got there first.
pub async fn mark_accepted(conn: &mut PgConnection, id: Uuid, by: UserId) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE invitations SET accepted_at = now(), accepted_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1
             AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > now()"#,
        id,
        by.0
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}
