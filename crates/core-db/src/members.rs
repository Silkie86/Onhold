//! Workspace memberships.

use chrono::{DateTime, Utc};
use core_domain::{Role, UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Member {
    pub user_id: UserId,
    pub email: String,
    pub name: String,
    pub role: Role,
    pub has_2fa: bool,
    pub version: i64,
    pub joined_at: DateTime<Utc>,
}

/// Active members of the workspace in context.
pub async fn list(conn: &mut PgConnection) -> Result<Vec<Member>> {
    Ok(sqlx::query_as!(
        Member,
        r#"SELECT u.id AS "user_id: UserId", u.email::text AS "email!", u.name, m.role AS "role: Role",
                  u.totp_enabled_at IS NOT NULL AS "has_2fa!", m.version, m.created_at AS joined_at
           FROM memberships m JOIN users u ON u.id = m.user_id
           WHERE m.workspace_id = app_workspace_id() AND m.deleted_at IS NULL AND u.deleted_at IS NULL
           ORDER BY u.name, u.email"#
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, user: UserId) -> Result<Option<Member>> {
    Ok(sqlx::query_as!(
        Member,
        r#"SELECT u.id AS "user_id: UserId", u.email::text AS "email!", u.name, m.role AS "role: Role",
                  u.totp_enabled_at IS NOT NULL AS "has_2fa!", m.version, m.created_at AS joined_at
           FROM memberships m JOIN users u ON u.id = m.user_id
           WHERE m.workspace_id = app_workspace_id() AND m.user_id = $1
             AND m.deleted_at IS NULL AND u.deleted_at IS NULL"#,
        user.0
    )
    .fetch_optional(conn)
    .await?)
}

/// The user's role in the workspace in context, if they are an active member.
pub async fn role_of(conn: &mut PgConnection, user: UserId) -> Result<Option<Role>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT role AS "role: Role" FROM memberships
           WHERE workspace_id = app_workspace_id() AND user_id = $1 AND deleted_at IS NULL"#,
        user.0
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn insert(
    conn: &mut PgConnection,
    workspace: WorkspaceId,
    user: UserId,
    role: Role,
    by: Option<UserId>,
) -> Result<Uuid> {
    let id = core_domain::ids::new_id();
    sqlx::query!(
        r#"INSERT INTO memberships (id, workspace_id, user_id, role, created_by, updated_by)
           VALUES ($1, $2, $3, $4, $5, $5)"#,
        id,
        workspace.0,
        user.0,
        role as Role,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(id)
}

/// Returns false when the member is gone or `expected_version` no longer matches.
pub async fn set_role(
    conn: &mut PgConnection,
    user: UserId,
    role: Role,
    expected_version: i64,
    by: Option<UserId>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE memberships SET role = $2, updated_by = $3
           WHERE workspace_id = app_workspace_id() AND user_id = $1 AND deleted_at IS NULL AND version = $4"#,
        user.0,
        role as Role,
        by.map(|u| u.0),
        expected_version
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn soft_delete(
    conn: &mut PgConnection,
    user: UserId,
    by: Option<UserId>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE memberships SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND user_id = $1 AND deleted_at IS NULL"#,
        user.0,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Active owners in the workspace in context, locked so concurrent demotions
/// cannot both see "another owner remains" and leave none.
pub async fn lock_owner_count(conn: &mut PgConnection) -> Result<i64> {
    let owners = sqlx::query_scalar!(
        r#"SELECT user_id FROM memberships
           WHERE workspace_id = app_workspace_id() AND role = 'owner' AND deleted_at IS NULL
           FOR UPDATE"#
    )
    .fetch_all(conn)
    .await?;
    Ok(owners.len() as i64)
}
