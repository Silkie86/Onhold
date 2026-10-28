//! Requests and questions left in the setup form's comments box.

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone, serde::Serialize)]
pub struct SetupRequest {
    pub id: Uuid,
    pub body: String,
    pub created_by: Option<UserId>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// Saves a comment in the workspace in context.
pub async fn insert(
    conn: &mut PgConnection,
    body: &str,
    by: Option<UserId>,
) -> Result<SetupRequest> {
    Ok(sqlx::query_as!(
        SetupRequest,
        r#"INSERT INTO setup_requests (id, workspace_id, body, created_by)
           VALUES ($1, app_workspace_id(), $2, $3)
           RETURNING id, body, created_by AS "created_by: UserId", created_at, resolved_at"#,
        Uuid::now_v7(),
        body,
        by.map(|u| u.0)
    )
    .fetch_one(conn)
    .await?)
}

/// The workspace's latest comments, newest first.
pub async fn recent(conn: &mut PgConnection, limit: i64) -> Result<Vec<SetupRequest>> {
    Ok(sqlx::query_as!(
        SetupRequest,
        r#"SELECT id, body, created_by AS "created_by: UserId", created_at, resolved_at
           FROM setup_requests WHERE workspace_id = app_workspace_id()
           ORDER BY created_at DESC LIMIT $1"#,
        limit
    )
    .fetch_all(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct AdminSetupRequest {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub workspace_name: String,
    pub body: String,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// Every workspace's comments, open ones first. Raises unless the user in
/// context is a superadmin.
pub async fn admin_list(conn: &mut PgConnection, limit: i32) -> Result<Vec<AdminSetupRequest>> {
    Ok(sqlx::query_as!(
        AdminSetupRequest,
        r#"SELECT id AS "id!", workspace_id AS "workspace_id!: WorkspaceId",
                  workspace_name AS "workspace_name!", body AS "body!", author_name, author_email,
                  created_at AS "created_at!", resolved_at
           FROM admin_setup_requests($1)"#,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Marks a comment dealt with; false if it was not open.
pub async fn admin_resolve(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    Ok(
        sqlx::query_scalar!(r#"SELECT admin_resolve_setup_request($1) AS "done!""#, id)
            .fetch_one(conn)
            .await?,
    )
}
