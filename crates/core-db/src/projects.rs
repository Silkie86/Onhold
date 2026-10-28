//! Projects (tours, seasons, circuits, campaigns): their dates, which are
//! bookings, and their crew.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Project {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub status: String,
    pub currency: String,
    pub owner_id: Option<Uuid>,
    pub budget: Value,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
    /// The first and last date's start, and how many dates.
    pub first_at: Option<DateTime<Utc>>,
    pub last_at: Option<DateTime<Utc>>,
    pub dates: i64,
}

/// Projects with dates still to come first, soonest first; then the rest,
/// latest first.
pub async fn list(
    conn: &mut PgConnection,
    status: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<(Vec<Project>, i64)> {
    let rows = sqlx::query!(
        r#"SELECT p.id, p.kind, p.name, p.status, p.currency::text AS "currency!", p.owner_id, p.budget,
                  p.notes, p.tags, p.version, p.created_at, p.updated_at,
                  p.created_by AS "created_by: UserId", p.updated_by AS "updated_by: UserId",
                  d.first_at, d.last_at, coalesce(d.dates, 0) AS "dates!", count(*) OVER () AS "total!"
           FROM projects p
           LEFT JOIN LATERAL (
               SELECT min(b.starts_at) AS first_at, max(b.starts_at) AS last_at, count(*) AS dates
               FROM project_bookings pb JOIN bookings b ON b.id = pb.booking_id
               WHERE pb.project_id = p.id AND b.deleted_at IS NULL
           ) d ON true
           WHERE p.workspace_id = app_workspace_id() AND p.deleted_at IS NULL
             AND ($1::text IS NULL OR p.status = $1)
           ORDER BY coalesce(d.last_at < now(), false),
                    CASE WHEN d.last_at IS NULL OR d.last_at >= now() THEN d.first_at END ASC NULLS FIRST,
                    d.last_at DESC NULLS LAST, p.created_at DESC, p.id
           OFFSET $2 LIMIT $3"#,
        status,
        offset,
        limit
    )
    .fetch_all(conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    Ok((
        rows.into_iter()
            .map(|r| Project {
                id: r.id,
                kind: r.kind,
                name: r.name,
                status: r.status,
                currency: r.currency,
                owner_id: r.owner_id,
                budget: r.budget,
                notes: r.notes,
                tags: r.tags,
                version: r.version,
                created_at: r.created_at,
                updated_at: r.updated_at,
                created_by: r.created_by,
                updated_by: r.updated_by,
                first_at: r.first_at,
                last_at: r.last_at,
                dates: r.dates,
            })
            .collect(),
        total,
    ))
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Project>> {
    Ok(sqlx::query_as!(
        Project,
        r#"SELECT p.id, p.kind, p.name, p.status, p.currency::text AS "currency!", p.owner_id, p.budget,
                  p.notes, p.tags, p.version, p.created_at, p.updated_at,
                  p.created_by AS "created_by: UserId", p.updated_by AS "updated_by: UserId",
                  d.first_at, d.last_at, coalesce(d.dates, 0) AS "dates!"
           FROM projects p
           LEFT JOIN LATERAL (
               SELECT min(b.starts_at) AS first_at, max(b.starts_at) AS last_at, count(*) AS dates
               FROM project_bookings pb JOIN bookings b ON b.id = pb.booking_id
               WHERE pb.project_id = p.id AND b.deleted_at IS NULL
           ) d ON true
           WHERE p.workspace_id = app_workspace_id() AND p.id = $1 AND p.deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM projects
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct Values<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub status: &'a str,
    pub currency: &'a str,
    pub owner_id: Option<Uuid>,
    pub budget: &'a Value,
    pub notes: Option<&'a str>,
    pub tags: &'a [String],
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO projects (id, workspace_id, kind, name, status, currency, owner_id, budget, notes, tags,
                                 created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)"#,
        id,
        v.kind,
        v.name,
        v.status,
        v.currency,
        v.owner_id,
        v.budget,
        v.notes,
        v.tags,
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
        r#"UPDATE projects SET kind = $2, name = $3, status = $4, currency = $5, owner_id = $6, budget = $7,
                  notes = $8, tags = $9, updated_by = $10
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.kind,
        v.name,
        v.status,
        v.currency,
        v.owner_id,
        v.budget,
        v.notes,
        v.tags,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Deletes the project with its own travel, costs and crew, and lets go of
/// its dates (which stay bookings).
pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let by = by.map(|u| u.0);
    sqlx::query!(
        r#"UPDATE travel_items SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND project_id = $1 AND deleted_at IS NULL"#,
        id,
        by
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        r#"UPDATE money_lines SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND project_id = $1 AND deleted_at IS NULL"#,
        id,
        by
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        r#"UPDATE project_crew SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND project_id = $1 AND deleted_at IS NULL"#,
        id,
        by
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        r#"DELETE FROM project_bookings WHERE workspace_id = app_workspace_id() AND project_id = $1"#,
        id
    )
    .execute(&mut *conn)
    .await?;
    let done = sqlx::query!(
        r#"UPDATE projects SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

// Dates -----------------------------------------------------------------------------

/// The bookings of a project, in time order (deleted ones left out).
pub async fn booking_ids(conn: &mut PgConnection, project_id: Uuid) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT b.id FROM project_bookings pb JOIN bookings b ON b.id = pb.booking_id
           WHERE pb.workspace_id = app_workspace_id() AND pb.project_id = $1 AND b.deleted_at IS NULL
           ORDER BY b.starts_at, b.id"#,
        project_id
    )
    .fetch_all(conn)
    .await?)
}

/// The project a booking belongs to, if any.
pub async fn of_booking(conn: &mut PgConnection, booking_id: Uuid) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT pb.project_id FROM project_bookings pb JOIN projects p ON p.id = pb.project_id
           WHERE pb.workspace_id = app_workspace_id() AND pb.booking_id = $1 AND p.deleted_at IS NULL"#,
        booking_id
    )
    .fetch_optional(conn)
    .await?)
}

/// Puts a booking in a project, moving it out of any other.
pub async fn attach(conn: &mut PgConnection, project_id: Uuid, booking_id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO project_bookings (workspace_id, project_id, booking_id)
           VALUES (app_workspace_id(), $1, $2)
           ON CONFLICT (booking_id) DO UPDATE SET project_id = excluded.project_id"#,
        project_id,
        booking_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn detach(conn: &mut PgConnection, project_id: Uuid, booking_id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"DELETE FROM project_bookings
           WHERE workspace_id = app_workspace_id() AND project_id = $1 AND booking_id = $2"#,
        project_id,
        booking_id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

// Crew ------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CrewMember {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub role: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub notes: Option<String>,
    pub position: i32,
    pub version: i64,
}

pub async fn crew(conn: &mut PgConnection, project_id: Uuid) -> Result<Vec<CrewMember>> {
    Ok(sqlx::query_as!(
        CrewMember,
        r#"SELECT id, project_id, name, role, phone, email, notes, position, version FROM project_crew
           WHERE workspace_id = app_workspace_id() AND project_id = $1 AND deleted_at IS NULL
           ORDER BY position, created_at, id"#,
        project_id
    )
    .fetch_all(conn)
    .await?)
}

pub async fn crew_member(conn: &mut PgConnection, id: Uuid) -> Result<Option<CrewMember>> {
    Ok(sqlx::query_as!(
        CrewMember,
        r#"SELECT id, project_id, name, role, phone, email, notes, position, version FROM project_crew
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct CrewValues<'a> {
    pub name: &'a str,
    pub role: Option<&'a str>,
    pub phone: Option<&'a str>,
    pub email: Option<&'a str>,
    pub notes: Option<&'a str>,
}

pub async fn insert_crew(
    conn: &mut PgConnection,
    id: Uuid,
    project_id: Uuid,
    v: CrewValues<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO project_crew (id, workspace_id, project_id, name, role, phone, email, notes, position,
                                     created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7,
                   (SELECT coalesce(max(position) + 1, 0) FROM project_crew
                    WHERE workspace_id = app_workspace_id() AND project_id = $2),
                   $8, $8)"#,
        id,
        project_id,
        v.name,
        v.role,
        v.phone,
        v.email,
        v.notes,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn update_crew(
    conn: &mut PgConnection,
    id: Uuid,
    v: CrewValues<'_>,
    by: Option<UserId>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE project_crew SET name = $2, role = $3, phone = $4, email = $5, notes = $6, updated_by = $7
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        v.name,
        v.role,
        v.phone,
        v.email,
        v.notes,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn delete_crew(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE project_crew SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}
