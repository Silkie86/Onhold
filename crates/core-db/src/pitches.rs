//! The pitch log: what was done to win an organisation over, and the
//! answers it gave.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: Uuid,
    pub organisation_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub contact_name: Option<String>,
    pub kind: String,
    pub type_key: String,
    pub channel: Option<String>,
    pub talent_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub item_name: Option<String>,
    pub note: Option<String>,
    pub happened_at: DateTime<Utc>,
    pub source: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

/// An organisation's log, latest first.
pub async fn list(
    conn: &mut PgConnection,
    organisation: Uuid,
    offset: i64,
    limit: i64,
) -> Result<(Vec<Entry>, i64)> {
    let rows = sqlx::query!(
        r#"SELECT p.id, p.organisation_id, p.contact_id, c.name AS "contact_name?", p.kind, p.type_key,
                  p.channel, p.talent_id, p.project_id, coalesce(t.name, pr.name) AS item_name, p.note,
                  p.happened_at, p.source, p.version, p.created_at, p.updated_at,
                  p.created_by AS "created_by: UserId", p.updated_by AS "updated_by: UserId",
                  count(*) OVER () AS "total!"
           FROM pitch_log p
           LEFT JOIN contacts c ON c.workspace_id = p.workspace_id AND c.id = p.contact_id AND c.deleted_at IS NULL
           LEFT JOIN talent t ON t.workspace_id = p.workspace_id AND t.id = p.talent_id AND t.deleted_at IS NULL
           LEFT JOIN projects pr ON pr.workspace_id = p.workspace_id AND pr.id = p.project_id
                                AND pr.deleted_at IS NULL
           WHERE p.workspace_id = app_workspace_id() AND p.organisation_id = $1 AND p.deleted_at IS NULL
           ORDER BY p.happened_at DESC, p.id DESC
           OFFSET $2 LIMIT $3"#,
        organisation,
        offset,
        limit
    )
    .fetch_all(conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    let items = rows
        .into_iter()
        .map(|r| Entry {
            id: r.id,
            organisation_id: r.organisation_id,
            contact_id: r.contact_id,
            contact_name: r.contact_name,
            kind: r.kind,
            type_key: r.type_key,
            channel: r.channel,
            talent_id: r.talent_id,
            project_id: r.project_id,
            item_name: r.item_name,
            note: r.note,
            happened_at: r.happened_at,
            source: r.source,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
            created_by: r.created_by,
            updated_by: r.updated_by,
        })
        .collect();
    Ok((items, total))
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Entry>> {
    Ok(sqlx::query_as!(
        Entry,
        r#"SELECT p.id, p.organisation_id, p.contact_id, c.name AS "contact_name?", p.kind, p.type_key,
                  p.channel, p.talent_id, p.project_id, coalesce(t.name, pr.name) AS item_name, p.note,
                  p.happened_at, p.source, p.version, p.created_at, p.updated_at,
                  p.created_by AS "created_by: UserId", p.updated_by AS "updated_by: UserId"
           FROM pitch_log p
           LEFT JOIN contacts c ON c.workspace_id = p.workspace_id AND c.id = p.contact_id AND c.deleted_at IS NULL
           LEFT JOIN talent t ON t.workspace_id = p.workspace_id AND t.id = p.talent_id AND t.deleted_at IS NULL
           LEFT JOIN projects pr ON pr.workspace_id = p.workspace_id AND pr.id = p.project_id
                                AND pr.deleted_at IS NULL
           WHERE p.workspace_id = app_workspace_id() AND p.id = $1 AND p.deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Locks an entry for an update in this transaction; returns its version.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM pitch_log
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct Values<'a> {
    pub organisation_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub kind: &'a str,
    pub type_key: &'a str,
    pub channel: Option<&'a str>,
    pub talent_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub note: Option<&'a str>,
    pub happened_at: DateTime<Utc>,
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO pitch_log (id, workspace_id, organisation_id, contact_id, kind, type_key, channel,
                                  talent_id, project_id, note, happened_at, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)"#,
        id,
        v.organisation_id,
        v.contact_id,
        v.kind,
        v.type_key,
        v.channel,
        v.talent_id,
        v.project_id,
        v.note,
        v.happened_at,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Everything but the organisation and the kind, which an entry keeps.
pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE pitch_log SET contact_id = $2, type_key = $3, channel = $4, talent_id = $5, project_id = $6,
                  note = $7, happened_at = $8, updated_by = $9
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.contact_id,
        v.type_key,
        v.channel,
        v.talent_id,
        v.project_id,
        v.note,
        v.happened_at,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE pitch_log SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}
