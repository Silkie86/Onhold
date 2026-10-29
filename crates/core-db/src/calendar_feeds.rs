//! Calendar feeds: secret iCal links to the workspace's bookings, or one
//! talent's.

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Feed {
    pub id: Uuid,
    pub talent_id: Option<Uuid>,
    pub talent_name: Option<String>,
    pub name: String,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Feeds still working, newest first.
pub async fn list(conn: &mut PgConnection) -> Result<Vec<Feed>> {
    Ok(sqlx::query_as!(
        Feed,
        r#"SELECT f.id, f.talent_id, t.name AS "talent_name?", f.name, f.created_by, f.created_at, f.last_used_at
           FROM calendar_feeds f LEFT JOIN talent t ON t.id = f.talent_id
           WHERE f.workspace_id = app_workspace_id() AND f.revoked_at IS NULL
           ORDER BY f.created_at DESC"#
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Feed>> {
    Ok(sqlx::query_as!(
        Feed,
        r#"SELECT f.id, f.talent_id, t.name AS "talent_name?", f.name, f.created_by, f.created_at, f.last_used_at
           FROM calendar_feeds f LEFT JOIN talent t ON t.id = f.talent_id
           WHERE f.workspace_id = app_workspace_id() AND f.id = $1 AND f.revoked_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    talent_id: Option<Uuid>,
    name: &str,
    token_hash: &[u8],
    by: UserId,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO calendar_feeds (id, workspace_id, talent_id, name, token_hash, created_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5)"#,
        id,
        talent_id,
        name,
        token_hash,
        by.0
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn revoke(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE calendar_feeds SET revoked_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1 AND revoked_at IS NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Stops every feed someone made: a member who leaves takes theirs away.
pub async fn revoke_created_by(conn: &mut PgConnection, created_by: Uuid) -> Result<u64> {
    let done = sqlx::query!(
        r#"UPDATE calendar_feeds SET revoked_at = now()
           WHERE workspace_id = app_workspace_id() AND created_by = $1 AND revoked_at IS NULL"#,
        created_by
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}

/// The feed behind a link, in any workspace, if it still works. Notes the use.
#[derive(Debug, Clone)]
pub struct Found {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub talent_id: Option<Uuid>,
    pub created_by: UserId,
    pub name: String,
}

pub async fn find(conn: &mut PgConnection, token_hash: &[u8]) -> Result<Option<Found>> {
    Ok(sqlx::query_as!(
        Found,
        r#"SELECT id AS "id!", workspace_id AS "workspace_id!: WorkspaceId", talent_id,
                  created_by AS "created_by!: UserId", name AS "name!"
           FROM calendar_feed_find($1)"#,
        token_hash
    )
    .fetch_optional(conn)
    .await?)
}
