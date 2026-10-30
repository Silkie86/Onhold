//! Things: the placeholder entity M2 uses to exercise every engine (custom
//! fields, a pipeline, rules, templates, events). Real entities in M3 and
//! later follow the same shape.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Thing {
    pub id: Uuid,
    pub name: String,
    pub pipeline_id: Option<Uuid>,
    pub stage_id: Option<Uuid>,
    pub stage_entered_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    pub custom: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

pub struct Filter<'a> {
    pub pipeline_id: Option<Uuid>,
    pub stage_id: Option<Uuid>,
    /// Matches names containing this text, ignoring case.
    pub q: Option<&'a str>,
    /// Keyset cursor: things created before this id.
    pub before: Option<Uuid>,
    pub limit: i64,
}

pub async fn list(conn: &mut PgConnection, f: Filter<'_>) -> Result<Vec<Thing>> {
    let pattern = f.q.map(|q| {
        format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    });
    Ok(sqlx::query_as!(
        Thing,
        r#"SELECT id, name, pipeline_id, stage_id, stage_entered_at, due_at, custom, version, created_at,
                  updated_at, created_by AS "created_by: UserId", updated_by AS "updated_by: UserId"
           FROM things
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
             AND ($1::uuid IS NULL OR pipeline_id = $1)
             AND ($2::uuid IS NULL OR stage_id = $2)
             AND ($3::text IS NULL OR name ILIKE $3)
             AND ($4::uuid IS NULL OR id < $4)
           ORDER BY id DESC
           LIMIT $5"#,
        f.pipeline_id,
        f.stage_id,
        pattern,
        f.before,
        f.limit
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Thing>> {
    Ok(sqlx::query_as!(
        Thing,
        r#"SELECT id, name, pipeline_id, stage_id, stage_entered_at, due_at, custom, version, created_at,
                  updated_at, created_by AS "created_by: UserId", updated_by AS "updated_by: UserId"
           FROM things WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// The same row, locked for an update in this transaction.
pub async fn get_for_update(conn: &mut PgConnection, id: Uuid) -> Result<Option<Thing>> {
    Ok(sqlx::query_as!(
        Thing,
        r#"SELECT id, name, pipeline_id, stage_id, stage_entered_at, due_at, custom, version, created_at,
                  updated_at, created_by AS "created_by: UserId", updated_by AS "updated_by: UserId"
           FROM things WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct Values<'a> {
    pub name: &'a str,
    pub pipeline_id: Option<Uuid>,
    pub stage_id: Option<Uuid>,
    pub stage_entered_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    pub custom: &'a Value,
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO things (id, workspace_id, name, pipeline_id, stage_id, stage_entered_at, due_at,
                               custom, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $8)"#,
        id,
        v.name,
        v.pipeline_id,
        v.stage_id,
        v.stage_entered_at,
        v.due_at,
        v.custom,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Replaces the row's values. The caller has locked the row and checked its
/// version.
pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE things SET name = $2, pipeline_id = $3, stage_id = $4, stage_entered_at = $5,
                  due_at = $6, custom = $7, updated_by = $8
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.name,
        v.pipeline_id,
        v.stage_id,
        v.stage_entered_at,
        v.due_at,
        v.custom,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE things SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// A thing whose date (plus a rule's offset) has arrived.
#[derive(Debug, Clone)]
pub struct Reached {
    pub id: Uuid,
    pub at: DateTime<Utc>,
}

/// Things whose `due_at + offset` falls in `(from, to]`, earliest first, for
/// date-offset rules.
pub async fn due_between(
    conn: &mut PgConnection,
    offset_minutes: i32,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Reached>> {
    Ok(sqlx::query_as!(
        Reached,
        r#"SELECT id, due_at + make_interval(mins => $1::int) AS "at!" FROM things
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL AND due_at IS NOT NULL
             AND due_at + make_interval(mins => $1::int) > $2
             AND due_at + make_interval(mins => $1::int) <= $3
           ORDER BY 2, id
           LIMIT $4"#,
        offset_minutes,
        from,
        to,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Things whose custom date or date-time field `key`, plus the offset,
/// falls in `(from, to]`. A bare date counts as midnight in `timezone`.
pub async fn custom_date_between(
    conn: &mut PgConnection,
    key: &str,
    timezone: &str,
    offset_minutes: i32,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Reached>> {
    Ok(sqlx::query_as!(
        Reached,
        r#"SELECT id AS "id!", at AS "at!" FROM (
               SELECT id,
                      CASE
                          WHEN custom->>$1 ~ '^\d{4}-\d{2}-\d{2}$'
                              THEN ((custom->>$1)::date::timestamp AT TIME ZONE $2)
                          WHEN custom->>$1 ~ '^\d{4}-\d{2}-\d{2}T'
                              THEN (custom->>$1)::timestamptz
                      END + make_interval(mins => $3::int) AS at
               FROM things
               WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL AND custom ? $1
           ) t
           WHERE at > $4 AND at <= $5
           ORDER BY at, id
           LIMIT $6"#,
        key,
        timezone,
        offset_minutes,
        from,
        to,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// How many live things sit in a pipeline.
pub async fn count_in_pipeline(conn: &mut PgConnection, pipeline_id: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM things
           WHERE workspace_id = app_workspace_id() AND pipeline_id = $1 AND deleted_at IS NULL"#,
        pipeline_id
    )
    .fetch_one(conn)
    .await?)
}
