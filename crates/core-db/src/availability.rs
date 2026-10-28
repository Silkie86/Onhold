//! Availability: stretches of time talent cannot work, might not, or works
//! only within.

use chrono::{DateTime, Utc};
use core_domain::{AvailabilityKind, UserId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Availability {
    pub id: Uuid,
    pub talent_id: Uuid,
    pub talent_name: String,
    pub kind: AvailabilityKind,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    pub source: String,
    pub note: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Live entries of these talent (all talent when empty) overlapping
/// `[from, to)`, of any of `kinds` (all kinds when empty), with the busy
/// times read from their own calendars (`source` "external", which cannot
/// be changed here). Each kind is capped on its own, so busy times can
/// never push out what was marked here.
pub async fn list(
    conn: &mut PgConnection,
    talent: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    kinds: &[AvailabilityKind],
) -> Result<Vec<Availability>> {
    Ok(sqlx::query_as!(
        Availability,
        r#"(SELECT a.id AS "id!", a.talent_id AS "talent_id!", t.name AS "talent_name!",
                  a.kind AS "kind!: AvailabilityKind", a.starts_at AS "starts_at!", a.ends_at AS "ends_at!",
                  a.all_day AS "all_day!", a.source AS "source!", a.note, a.version AS "version!",
                  a.created_at AS "created_at!", a.updated_at AS "updated_at!"
           FROM availability a JOIN talent t ON t.id = a.talent_id
           WHERE a.workspace_id = app_workspace_id() AND a.deleted_at IS NULL
             AND (cardinality($1::uuid[]) = 0 OR a.talent_id = ANY($1))
             AND tstzrange(a.starts_at, a.ends_at, '[)') && tstzrange($2, $3, '[)')
             AND (cardinality($4::availability_kind[]) = 0 OR a.kind = ANY($4))
           ORDER BY a.starts_at, t.name, a.id
           LIMIT 5000)
           UNION ALL
           -- Busy times from the talent's own calendars, named by whose
           -- calendar they come from.
           (SELECT b.id, b.talent_id, t.name, CASE WHEN b.tentative THEN 'tentative' ELSE 'unavailable' END::availability_kind,
                  b.starts_at, b.ends_at, false, 'external', 'Busy in their own calendar (' || coalesce(c.person, c.account) || ')', 1::bigint,
                  c.connected_at, coalesce(c.last_synced_at, c.connected_at)
           FROM calendar_busy b JOIN talent t ON t.id = b.talent_id
                JOIN talent_calendars c ON c.id = b.calendar_id
           WHERE b.workspace_id = app_workspace_id()
             AND (cardinality($1::uuid[]) = 0 OR b.talent_id = ANY($1))
             AND tstzrange(b.starts_at, b.ends_at, '[)') && tstzrange($2, $3, '[)')
             AND (cardinality($4::availability_kind[]) = 0
                  OR (CASE WHEN b.tentative THEN 'tentative' ELSE 'unavailable' END)::availability_kind = ANY($4))
           ORDER BY b.starts_at, t.name, b.id
           LIMIT 5000)
           ORDER BY 5, 3, 1"#,
        talent,
        from,
        to,
        kinds as &[AvailabilityKind]
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Availability>> {
    Ok(sqlx::query_as!(
        Availability,
        r#"SELECT a.id, a.talent_id, t.name AS talent_name, a.kind AS "kind: AvailabilityKind",
                  a.starts_at, a.ends_at, a.all_day, a.source, a.note, a.version, a.created_at, a.updated_at
           FROM availability a JOIN talent t ON t.id = a.talent_id
           WHERE a.workspace_id = app_workspace_id() AND a.id = $1 AND a.deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Whether the id is busy time read from a talent's own calendar.
pub async fn is_external(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM calendar_busy WHERE workspace_id = app_workspace_id() AND id = $1) AS "x!""#,
        id
    )
    .fetch_one(conn)
    .await?)
}

/// Locks the row for an update in this transaction; returns its version.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM availability
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct Values<'a> {
    pub talent_id: Uuid,
    pub kind: AvailabilityKind,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    pub note: Option<&'a str>,
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    source: &str,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO availability (id, workspace_id, talent_id, kind, starts_at, ends_at, all_day, source,
                                     note, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $9)"#,
        id,
        v.talent_id,
        v.kind as AvailabilityKind,
        v.starts_at,
        v.ends_at,
        v.all_day,
        source,
        v.note,
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
        r#"UPDATE availability SET kind = $2, starts_at = $3, ends_at = $4, all_day = $5, note = $6,
                  updated_by = $7
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.kind as AvailabilityKind,
        v.starts_at,
        v.ends_at,
        v.all_day,
        v.note,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE availability SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}
