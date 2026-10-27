//! Tasks: to-dos with a due date, an assignee and, optionally, the record
//! they are about.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Task {
    pub id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub assignee_id: Option<Uuid>,
    pub related_entity: Option<String>,
    pub related_id: Option<Uuid>,
    pub status: String,
    pub done_at: Option<DateTime<Utc>>,
    pub done_by: Option<Uuid>,
    pub origin: String,
    pub rule_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

pub struct Filter {
    /// `open` or `done`; both when `None`.
    pub status: Option<String>,
    pub assignee_id: Option<Uuid>,
    pub related_id: Option<Uuid>,
    /// Open tasks due before this.
    pub due_before: Option<DateTime<Utc>>,
    pub offset: i64,
    pub limit: i64,
}

/// Open tasks first, soonest due first; then done ones, latest first.
pub async fn list(conn: &mut PgConnection, f: Filter) -> Result<(Vec<Task>, i64)> {
    let rows = sqlx::query!(
        r#"SELECT id, title, notes, due_at, assignee_id, related_entity, related_id, status, done_at, done_by,
                  origin, rule_id, version, created_at, updated_at, created_by AS "created_by: UserId",
                  updated_by AS "updated_by: UserId", count(*) OVER () AS "total!"
           FROM tasks
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
             AND ($1::text IS NULL OR status = $1)
             AND ($2::uuid IS NULL OR assignee_id = $2)
             AND ($3::uuid IS NULL OR related_id = $3)
             AND ($4::timestamptz IS NULL OR (status = 'open' AND due_at < $4))
           ORDER BY status = 'done', CASE WHEN status = 'open' THEN due_at END ASC NULLS LAST,
                    done_at DESC, created_at DESC, id
           OFFSET $5 LIMIT $6"#,
        f.status,
        f.assignee_id,
        f.related_id,
        f.due_before,
        f.offset,
        f.limit
    )
    .fetch_all(conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    let items = rows
        .into_iter()
        .map(|r| Task {
            id: r.id,
            title: r.title,
            notes: r.notes,
            due_at: r.due_at,
            assignee_id: r.assignee_id,
            related_entity: r.related_entity,
            related_id: r.related_id,
            status: r.status,
            done_at: r.done_at,
            done_by: r.done_by,
            origin: r.origin,
            rule_id: r.rule_id,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
            created_by: r.created_by,
            updated_by: r.updated_by,
        })
        .collect();
    Ok((items, total))
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Task>> {
    Ok(sqlx::query_as!(
        Task,
        r#"SELECT id, title, notes, due_at, assignee_id, related_entity, related_id, status, done_at, done_by,
                  origin, rule_id, version, created_at, updated_at, created_by AS "created_by: UserId",
                  updated_by AS "updated_by: UserId"
           FROM tasks WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Locks the row for an update in this transaction; returns its version.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM tasks
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct Values<'a> {
    pub title: &'a str,
    pub notes: Option<&'a str>,
    pub due_at: Option<DateTime<Utc>>,
    pub assignee_id: Option<Uuid>,
    pub related_entity: Option<&'a str>,
    pub related_id: Option<Uuid>,
    pub status: &'a str,
    pub done_at: Option<DateTime<Utc>>,
    pub done_by: Option<Uuid>,
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    rule_id: Option<Uuid>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO tasks (id, workspace_id, title, notes, due_at, assignee_id, related_entity, related_id,
                              status, done_at, done_by, origin, rule_id, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10,
                   CASE WHEN $11::uuid IS NULL THEN 'manual' ELSE 'rule' END, $11, $12, $12)"#,
        id,
        v.title,
        v.notes,
        v.due_at,
        v.assignee_id,
        v.related_entity,
        v.related_id,
        v.status,
        v.done_at,
        v.done_by,
        rule_id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE tasks SET title = $2, notes = $3, due_at = $4, assignee_id = $5, related_entity = $6,
                  related_id = $7, status = $8, done_at = $9, done_by = $10, updated_by = $11
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.title,
        v.notes,
        v.due_at,
        v.assignee_id,
        v.related_entity,
        v.related_id,
        v.status,
        v.done_at,
        v.done_by,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE tasks SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}
