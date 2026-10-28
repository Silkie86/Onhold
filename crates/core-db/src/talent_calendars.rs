//! Talent's own calendars: the links they connect them through, the
//! connections, and when each calendar is busy (times only).

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

// Links ---------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Link {
    pub id: Uuid,
    pub talent_id: Uuid,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

pub async fn insert_link(
    conn: &mut PgConnection,
    id: Uuid,
    talent_id: Uuid,
    token_hash: &[u8],
    by: UserId,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO calendar_connect_links (id, workspace_id, talent_id, token_hash, created_by, expires_at)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5)"#,
        id,
        talent_id,
        token_hash,
        by.0,
        expires_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A talent's links that still work, newest first.
pub async fn links(conn: &mut PgConnection, talent_id: Uuid) -> Result<Vec<Link>> {
    Ok(sqlx::query_as!(
        Link,
        r#"SELECT id, talent_id, created_by, created_at, expires_at, last_used_at
           FROM calendar_connect_links
           WHERE workspace_id = app_workspace_id() AND talent_id = $1
             AND revoked_at IS NULL AND expires_at > now()
           ORDER BY created_at DESC"#,
        talent_id
    )
    .fetch_all(conn)
    .await?)
}

/// Stops a link working; `false` when there was no such working link.
pub async fn revoke_link(conn: &mut PgConnection, talent_id: Uuid, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE calendar_connect_links SET revoked_at = now()
           WHERE workspace_id = app_workspace_id() AND talent_id = $1 AND id = $2
             AND revoked_at IS NULL"#,
        talent_id,
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Stops every link of a talent, or of the workspace when `None`.
pub async fn revoke_links(conn: &mut PgConnection, talent_id: Option<Uuid>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE calendar_connect_links SET revoked_at = now()
           WHERE workspace_id = app_workspace_id() AND revoked_at IS NULL
             AND ($1::uuid IS NULL OR talent_id = $1)"#,
        talent_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct FoundLink {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub talent_id: Uuid,
}

/// The working link behind a token, from any workspace (no context needed).
pub async fn find_link(conn: &mut PgConnection, token_hash: &[u8]) -> Result<Option<FoundLink>> {
    Ok(sqlx::query_as!(
        FoundLink,
        r#"SELECT id AS "id!", workspace_id AS "workspace_id!: WorkspaceId", talent_id AS "talent_id!"
           FROM calendar_connect_link_find($1)"#,
        token_hash
    )
    .fetch_optional(conn)
    .await?)
}

/// Whether a link still works, inside its workspace.
pub async fn link_works(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM calendar_connect_links
                          WHERE workspace_id = app_workspace_id() AND id = $1
                            AND revoked_at IS NULL AND expires_at > now()) AS "works!""#,
        id
    )
    .fetch_one(conn)
    .await?)
}

// Calendars -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Calendar {
    pub id: Uuid,
    pub talent_id: Uuid,
    pub link_id: Uuid,
    pub provider: String,
    pub account: String,
    pub person: Option<String>,
    pub secret: Vec<u8>,
    pub status: String,
    pub failures: i32,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub connected_at: DateTime<Utc>,
    pub disconnected_at: Option<DateTime<Utc>>,
    pub version: i64,
}

macro_rules! select_calendar {
    ($tail:literal $(, $arg:expr)*) => {
        sqlx::query_as!(
            Calendar,
            r#"SELECT id, talent_id, link_id, provider, account, person, secret, status, failures,
                      last_error, last_error_at, last_synced_at, connected_at, disconnected_at, version
               FROM talent_calendars WHERE workspace_id = app_workspace_id() "# + $tail
            $(, $arg)*
        )
    };
}

/// A talent's connected calendars, or every one in the workspace.
pub async fn list(conn: &mut PgConnection, talent_id: Option<Uuid>) -> Result<Vec<Calendar>> {
    Ok(select_calendar!(
        "AND disconnected_at IS NULL AND ($1::uuid IS NULL OR talent_id = $1)
         ORDER BY connected_at, id",
        talent_id
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Calendar>> {
    Ok(select_calendar!("AND id = $1", id)
        .fetch_optional(conn)
        .await?)
}

pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<Calendar>> {
    Ok(select_calendar!("AND id = $1 FOR UPDATE", id)
        .fetch_optional(conn)
        .await?)
}

/// The live connection to the same account for this talent, if any.
pub async fn live_for(
    conn: &mut PgConnection,
    talent_id: Uuid,
    provider: &str,
    account: &str,
) -> Result<Option<Calendar>> {
    Ok(select_calendar!(
        "AND talent_id = $1 AND provider = $2 AND account = $3 AND disconnected_at IS NULL
         FOR UPDATE",
        talent_id,
        provider,
        account
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn count_live(conn: &mut PgConnection, talent_id: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM talent_calendars
           WHERE workspace_id = app_workspace_id() AND talent_id = $1 AND disconnected_at IS NULL"#,
        talent_id
    )
    .fetch_one(conn)
    .await?)
}

pub struct NewCalendar<'a> {
    pub id: Uuid,
    pub talent_id: Uuid,
    pub link_id: Uuid,
    pub provider: &'a str,
    pub account: &'a str,
    pub person: Option<&'a str>,
    pub secret: &'a [u8],
}

pub async fn insert(conn: &mut PgConnection, c: NewCalendar<'_>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO talent_calendars (id, workspace_id, talent_id, link_id, provider, account, person,
                                         secret, next_poll_at)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, now())"#,
        c.id,
        c.talent_id,
        c.link_id,
        c.provider,
        c.account,
        c.person,
        c.secret
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A new grant (connected again, or a refreshed token).
pub async fn set_secret(conn: &mut PgConnection, id: Uuid, secret: &[u8]) -> Result<()> {
    sqlx::query!(
        r#"UPDATE talent_calendars SET secret = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id,
        secret
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Connected again: healthy, and read straight away.
pub async fn revive(
    conn: &mut PgConnection,
    id: Uuid,
    link_id: Uuid,
    person: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE talent_calendars
           SET status = 'active', failures = 0, last_error = NULL, last_error_at = NULL,
               link_id = $2, person = coalesce($3, person), next_poll_at = now(), poll_lease_until = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id,
        link_id,
        person
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Takes a calendar for one read: `false` when it is disconnected, needs
/// connecting again, or another read holds it.
pub async fn claim_poll(conn: &mut PgConnection, id: Uuid, lease_secs: f64) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE talent_calendars SET poll_lease_until = now() + make_interval(secs => $2),
               next_poll_at = now() + make_interval(secs => $2)
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL
             AND status <> 'reconnect' AND (poll_lease_until IS NULL OR poll_lease_until < now())"#,
        id,
        lease_secs
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// A good read: healthy again, read next at `next`.
pub async fn synced(conn: &mut PgConnection, id: Uuid, next: DateTime<Utc>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE talent_calendars
           SET last_synced_at = now(), status = 'active', failures = 0, last_error = NULL,
               last_error_at = NULL, next_poll_at = $2, poll_lease_until = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id,
        next
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A failed read, and when to try again (`None`: not until connected again).
pub async fn failed(
    conn: &mut PgConnection,
    id: Uuid,
    status: &str,
    error: &str,
    next: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE talent_calendars
           SET status = $2, last_error = left($3, 1000), last_error_at = now(), failures = failures + 1,
               next_poll_at = $4, poll_lease_until = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id,
        status,
        error,
        next
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Asks for a read as soon as the worker can.
pub async fn poll_now(conn: &mut PgConnection, id: Uuid) -> Result<Option<DateTime<Utc>>> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE talent_calendars SET next_poll_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL
             AND status <> 'reconnect'
           RETURNING next_poll_at AS "at!""#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Stops reading a calendar and forgets when it was busy.
pub async fn disconnect(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE talent_calendars
           SET disconnected_at = now(), next_poll_at = NULL, poll_lease_until = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "DELETE FROM calendar_busy WHERE workspace_id = app_workspace_id() AND calendar_id = $1",
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Disconnects every calendar of a talent, or of the workspace when `None`;
/// returns their ids.
pub async fn disconnect_all(conn: &mut PgConnection, talent_id: Option<Uuid>) -> Result<Vec<Uuid>> {
    let ids = sqlx::query_scalar!(
        r#"SELECT id FROM talent_calendars
           WHERE workspace_id = app_workspace_id() AND disconnected_at IS NULL
             AND ($1::uuid IS NULL OR talent_id = $1)"#,
        talent_id
    )
    .fetch_all(&mut *conn)
    .await?;
    for id in &ids {
        disconnect(conn, *id).await?;
    }
    Ok(ids)
}

#[derive(Debug, Clone)]
pub struct Due {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub at: DateTime<Utc>,
}

/// Calendars due a read, in workspaces still working (the worker's sweep).
pub async fn due(conn: &mut PgConnection, now: DateTime<Utc>, limit: i64) -> Result<Vec<Due>> {
    Ok(sqlx::query_as!(
        Due,
        r#"SELECT c.id, c.workspace_id AS "workspace_id: WorkspaceId", c.next_poll_at AS "at!"
           FROM talent_calendars c JOIN workspaces w ON w.id = c.workspace_id
           WHERE c.next_poll_at <= $1
             AND (c.poll_lease_until IS NULL OR c.poll_lease_until < $1)
             AND w.status IN ('trial', 'active')
           ORDER BY c.next_poll_at LIMIT $2"#,
        now,
        limit
    )
    .fetch_all(conn)
    .await?)
}

// Busy times ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusyRow {
    pub id: Uuid,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub tentative: bool,
}

pub async fn busy(conn: &mut PgConnection, calendar_id: Uuid) -> Result<Vec<BusyRow>> {
    Ok(sqlx::query_as!(
        BusyRow,
        r#"SELECT id, starts_at, ends_at, tentative FROM calendar_busy
           WHERE workspace_id = app_workspace_id() AND calendar_id = $1
           ORDER BY starts_at"#,
        calendar_id
    )
    .fetch_all(conn)
    .await?)
}

/// Removes these stretches.
pub async fn remove_busy(conn: &mut PgConnection, ids: &[Uuid]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query!(
        "DELETE FROM calendar_busy WHERE workspace_id = app_workspace_id() AND id = ANY($1)",
        ids
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Adds stretches to a calendar: `(id, starts, ends, tentative)` each.
pub async fn add_busy(
    conn: &mut PgConnection,
    calendar_id: Uuid,
    talent_id: Uuid,
    rows: &[(Uuid, DateTime<Utc>, DateTime<Utc>, bool)],
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let starts: Vec<DateTime<Utc>> = rows.iter().map(|r| r.1).collect();
    let ends: Vec<DateTime<Utc>> = rows.iter().map(|r| r.2).collect();
    let tentative: Vec<bool> = rows.iter().map(|r| r.3).collect();
    sqlx::query!(
        r#"INSERT INTO calendar_busy (id, workspace_id, calendar_id, talent_id, starts_at, ends_at, tentative)
           SELECT u.id, app_workspace_id(), $1, $2, u.starts_at, u.ends_at, u.tentative
           FROM unnest($3::uuid[], $4::timestamptz[], $5::timestamptz[], $6::bool[])
                AS u (id, starts_at, ends_at, tentative)"#,
        calendar_id,
        talent_id,
        &ids,
        &starts,
        &ends,
        &tentative
    )
    .execute(conn)
    .await?;
    Ok(())
}
