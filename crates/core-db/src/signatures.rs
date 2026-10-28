//! Email signatures: a member's own, and the workspace's.

use core_domain::UserId;
use sqlx::PgConnection;

use crate::Result;

/// The member's own signature in this workspace.
pub async fn member(conn: &mut PgConnection, user: UserId) -> Result<Option<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT email_signature FROM memberships
           WHERE workspace_id = app_workspace_id() AND user_id = $1 AND deleted_at IS NULL"#,
        user.0
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

/// The workspace's signature.
pub async fn workspace(conn: &mut PgConnection) -> Result<Option<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT email_signature FROM workspaces WHERE id = app_workspace_id()"#
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

/// What goes under a message from `user`: theirs, else the workspace's.
pub async fn for_sender(conn: &mut PgConnection, user: Option<UserId>) -> Result<Option<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT coalesce(
               (SELECT m.email_signature FROM memberships m
                WHERE m.workspace_id = w.id AND m.user_id = $1 AND m.deleted_at IS NULL),
               w.email_signature)
           FROM workspaces w WHERE w.id = app_workspace_id()"#,
        user.map(|u| u.0)
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

/// Sets the member's own; false when they are not a member here.
pub async fn set_member(
    conn: &mut PgConnection,
    user: UserId,
    signature: Option<&str>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE memberships SET email_signature = $2
           WHERE workspace_id = app_workspace_id() AND user_id = $1 AND deleted_at IS NULL"#,
        user.0,
        signature
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn set_workspace(conn: &mut PgConnection, signature: Option<&str>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE workspaces SET email_signature = $1 WHERE id = app_workspace_id()"#,
        signature
    )
    .execute(conn)
    .await?;
    Ok(())
}
