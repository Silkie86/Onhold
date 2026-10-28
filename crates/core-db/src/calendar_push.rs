//! Members' own calendars that bookings are written into, and which event
//! there stands for which booking.

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Push {
    pub id: Uuid,
    pub user_id: UserId,
    pub talent_id: Option<Uuid>,
    pub talent_name: Option<String>,
    pub provider: String,
    pub account: String,
    pub secret: Vec<u8>,
    pub remote_calendar: Option<String>,
    pub status: String,
    pub failures: i32,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub connected_at: DateTime<Utc>,
    pub disconnected_at: Option<DateTime<Utc>>,
    pub version: i64,
}

macro_rules! select_push {
    ($tail:literal $(, $arg:expr)*) => {
        sqlx::query_as!(
            Push,
            r#"SELECT p.id, p.user_id AS "user_id: UserId", p.talent_id, t.name AS "talent_name?",
                      p.provider, p.account, p.secret, p.remote_calendar, p.status, p.failures,
                      p.last_error, p.last_error_at, p.last_synced_at, p.connected_at,
                      p.disconnected_at, p.version
               FROM calendar_pushes p LEFT JOIN talent t ON t.id = p.talent_id
               WHERE p.workspace_id = app_workspace_id() "# + $tail
            $(, $arg)*
        )
    };
}

/// Live connections: one member's, or everyone's when `None`.
pub async fn list(conn: &mut PgConnection, user: Option<UserId>) -> Result<Vec<Push>> {
    Ok(select_push!(
        "AND p.disconnected_at IS NULL AND ($1::uuid IS NULL OR p.user_id = $1)
         ORDER BY p.connected_at, p.id",
        user.map(|u| u.0)
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Push>> {
    Ok(select_push!("AND p.id = $1", id)
        .fetch_optional(conn)
        .await?)
}

pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<Push>> {
    Ok(select_push!("AND p.id = $1 FOR UPDATE OF p", id)
        .fetch_optional(conn)
        .await?)
}

/// The member's live connection to the same account and bookings.
pub async fn live_for(
    conn: &mut PgConnection,
    user: UserId,
    provider: &str,
    account: &str,
    talent_id: Option<Uuid>,
) -> Result<Option<Push>> {
    Ok(select_push!(
        "AND p.user_id = $1 AND p.provider = $2 AND p.account = $3
         AND p.talent_id IS NOT DISTINCT FROM $4 AND p.disconnected_at IS NULL
         FOR UPDATE OF p",
        user.0,
        provider,
        account,
        talent_id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn count_live(conn: &mut PgConnection, user: UserId) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM calendar_pushes
           WHERE workspace_id = app_workspace_id() AND user_id = $1 AND disconnected_at IS NULL"#,
        user.0
    )
    .fetch_one(conn)
    .await?)
}

pub struct NewPush<'a> {
    pub id: Uuid,
    pub user_id: UserId,
    pub talent_id: Option<Uuid>,
    pub provider: &'a str,
    pub account: &'a str,
    pub secret: &'a [u8],
}

/// Adds a connection, due a full write straight away.
pub async fn insert(conn: &mut PgConnection, p: NewPush<'_>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO calendar_pushes (id, workspace_id, user_id, talent_id, provider, account, secret, next_sync_at)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, now())"#,
        p.id,
        p.user_id.0,
        p.talent_id,
        p.provider,
        p.account,
        p.secret
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A new grant (connected again, or a refreshed token).
pub async fn set_secret(conn: &mut PgConnection, id: Uuid, secret: &[u8]) -> Result<()> {
    sqlx::query!(
        r#"UPDATE calendar_pushes SET secret = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id,
        secret
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The calendar OnHold made in the account (`None`: make it again), by
/// the write holding `token`: `false` when that write no longer holds it.
pub async fn set_remote_calendar(
    conn: &mut PgConnection,
    id: Uuid,
    token: Uuid,
    calendar: Option<&str>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE calendar_pushes SET remote_calendar = $3
           WHERE workspace_id = app_workspace_id() AND id = $1 AND sync_token = $2
             AND disconnected_at IS NULL"#,
        id,
        token,
        calendar
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Connected again: healthy, and written in full as soon as any write
/// under way is done.
pub async fn revive(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE calendar_pushes
           SET status = 'active', failures = 0, last_error = NULL, last_error_at = NULL,
               next_sync_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Takes a connection for one write, returning the token its steps
/// carry: `None` when it is disconnected, needs connecting again, or
/// another write holds it.
pub async fn claim(conn: &mut PgConnection, id: Uuid, lease_secs: f64) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE calendar_pushes
           SET sync_lease_until = now() + make_interval(secs => $3), sync_token = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL
             AND status <> 'reconnect' AND (sync_lease_until IS NULL OR sync_lease_until < now())
           RETURNING sync_token AS "token!""#,
        id,
        core_domain::ids::new_id(),
        lease_secs
    )
    .fetch_optional(conn)
    .await?)
}

/// Keeps the write holding `token` going for another `lease_secs`:
/// `false` when it no longer holds the connection.
pub async fn renew(
    conn: &mut PgConnection,
    id: Uuid,
    token: Uuid,
    lease_secs: f64,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE calendar_pushes SET sync_lease_until = now() + make_interval(secs => $3)
           WHERE workspace_id = app_workspace_id() AND id = $1 AND sync_token = $2
             AND disconnected_at IS NULL"#,
        id,
        token,
        lease_secs
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Lets go of a connection after a write of one booking. Its health is
/// left to the full writes.
pub async fn release(conn: &mut PgConnection, id: Uuid, token: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE calendar_pushes SET sync_lease_until = NULL, sync_token = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND sync_token = $2"#,
        id,
        token
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A good full write: healthy again, compared in full next at `next`,
/// unless another write was asked for while this one ran (its lease began
/// `lease_secs` before it ends), which then follows straight away.
pub async fn synced(
    conn: &mut PgConnection,
    id: Uuid,
    token: Uuid,
    next: DateTime<Utc>,
    lease_secs: f64,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE calendar_pushes
           SET last_synced_at = now(), status = 'active', failures = 0, last_error = NULL,
               last_error_at = NULL, sync_lease_until = NULL, sync_token = NULL,
               next_sync_at = CASE
                   WHEN next_sync_at > sync_lease_until - make_interval(secs => $4)
                   THEN least(next_sync_at, $3) ELSE $3 END
           WHERE workspace_id = app_workspace_id() AND id = $1 AND sync_token = $2
             AND disconnected_at IS NULL"#,
        id,
        token,
        next,
        lease_secs
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A failed write, and when to try in full again (`None`: not until
/// connected again).
pub async fn failed(
    conn: &mut PgConnection,
    id: Uuid,
    token: Uuid,
    status: &str,
    error: &str,
    next: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE calendar_pushes
           SET status = $3, last_error = left($4, 1000), last_error_at = now(), failures = failures + 1,
               next_sync_at = $5, sync_lease_until = NULL, sync_token = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND sync_token = $2
             AND disconnected_at IS NULL"#,
        id,
        token,
        status,
        error,
        next
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// When the connection is next due a full write.
pub async fn next_sync_at(conn: &mut PgConnection, id: Uuid) -> Result<Option<DateTime<Utc>>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT next_sync_at FROM calendar_pushes WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

/// Asks for a full write as soon as the worker can.
pub async fn sync_now(conn: &mut PgConnection, id: Uuid) -> Result<Option<DateTime<Utc>>> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE calendar_pushes SET next_sync_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL
             AND status <> 'reconnect'
           RETURNING next_sync_at AS "at!""#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Stops writing and forgets which event was which booking. The grant is
/// kept for the worker, which removes OnHold's calendar, revokes, then
/// forgets it.
pub async fn disconnect(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE calendar_pushes
           SET disconnected_at = now(), next_sync_at = NULL, sync_lease_until = NULL,
               sync_token = NULL, revoke_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id
    )
    .execute(&mut *conn)
    .await?;
    clear_events(conn, id).await
}

/// Disconnects every connection of a member leaving, or of a closing
/// workspace (`None`), and returns their ids. The grants are kept for the
/// worker, which removes OnHold's calendars, revokes, then forgets them.
pub async fn disconnect_all(conn: &mut PgConnection, user: Option<UserId>) -> Result<Vec<Uuid>> {
    let ids = sqlx::query_scalar!(
        r#"UPDATE calendar_pushes
           SET disconnected_at = now(), next_sync_at = NULL, sync_lease_until = NULL,
               sync_token = NULL, revoke_at = now()
           WHERE workspace_id = app_workspace_id() AND disconnected_at IS NULL
             AND ($1::uuid IS NULL OR user_id = $1)
           RETURNING id"#,
        user.map(|u| u.0)
    )
    .fetch_all(&mut *conn)
    .await?;
    if !ids.is_empty() {
        sqlx::query!(
            "DELETE FROM calendar_push_events WHERE workspace_id = app_workspace_id() AND push_id = ANY($1)",
            &ids
        )
        .execute(conn)
        .await?;
    }
    Ok(ids)
}

/// A disconnected connection whose grant is still to be revoked.
pub async fn to_revoke(conn: &mut PgConnection, id: Uuid) -> Result<Option<Push>> {
    Ok(
        select_push!("AND p.id = $1 AND p.revoke_at IS NOT NULL", id)
            .fetch_optional(conn)
            .await?,
    )
}

/// Locks this workspace's connections to an account still waiting to be
/// revoked, in one order, so that of several revoked together the last
/// sees the others forgotten (and revokes the account's grant).
pub async fn lock_revokes(conn: &mut PgConnection, provider: &str, account: &str) -> Result<()> {
    sqlx::query!(
        r#"SELECT id FROM calendar_pushes
           WHERE workspace_id = app_workspace_id() AND provider = $1 AND account = $2
             AND revoke_at IS NOT NULL
           ORDER BY id FOR UPDATE"#,
        provider,
        account
    )
    .fetch_all(conn)
    .await?;
    Ok(())
}

/// Removing OnHold's calendar failed for now: try again at `at`.
pub async fn revoke_later(conn: &mut PgConnection, id: Uuid, at: DateTime<Utc>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE calendar_pushes SET revoke_at = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND revoke_at IS NOT NULL"#,
        id,
        at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The grant has been revoked (or need not be): forget it.
pub async fn forget_grant(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE calendar_pushes SET secret = '\x'::bytea, revoke_at = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NOT NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Live connections a booking change is written to, in this workspace.
pub async fn writable(conn: &mut PgConnection) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM calendar_pushes
           WHERE workspace_id = app_workspace_id() AND disconnected_at IS NULL
             AND status <> 'reconnect'"#
    )
    .fetch_all(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct Due {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub at: DateTime<Utc>,
}

/// Connections due a full write, in workspaces still working (the worker's
/// sweep).
pub async fn due(conn: &mut PgConnection, now: DateTime<Utc>, limit: i64) -> Result<Vec<Due>> {
    Ok(sqlx::query_as!(
        Due,
        r#"SELECT p.id, p.workspace_id AS "workspace_id: WorkspaceId", p.next_sync_at AS "at!"
           FROM calendar_pushes p JOIN workspaces w ON w.id = p.workspace_id
           WHERE p.next_sync_at <= $1
             AND (p.sync_lease_until IS NULL OR p.sync_lease_until < $1)
             AND w.status IN ('trial', 'active')
           ORDER BY p.next_sync_at LIMIT $2"#,
        now,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Disconnected connections still to be cleaned up, closed workspaces
/// included (the worker's sweep).
pub async fn revokes_due(conn: &mut PgConnection, limit: i64) -> Result<Vec<Due>> {
    Ok(sqlx::query_as!(
        Due,
        r#"SELECT id, workspace_id AS "workspace_id: WorkspaceId", revoke_at AS "at!"
           FROM calendar_pushes WHERE revoke_at <= now()
           ORDER BY revoke_at LIMIT $1"#,
        limit
    )
    .fetch_all(conn)
    .await?)
}

// Events ---------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub booking_id: Uuid,
    pub remote_id: String,
    pub content_hash: Vec<u8>,
}

/// What has been written for a connection: every booking, or one.
pub async fn written(
    conn: &mut PgConnection,
    push_id: Uuid,
    booking_id: Option<Uuid>,
) -> Result<Vec<Written>> {
    Ok(sqlx::query_as!(
        Written,
        r#"SELECT booking_id, remote_id, content_hash FROM calendar_push_events
           WHERE workspace_id = app_workspace_id() AND push_id = $1
             AND ($2::uuid IS NULL OR booking_id = $2)"#,
        push_id,
        booking_id
    )
    .fetch_all(conn)
    .await?)
}

/// Records that `booking_id`'s event is `remote_id`, as written.
pub async fn record_written(
    conn: &mut PgConnection,
    push_id: Uuid,
    booking_id: Uuid,
    remote_id: &str,
    content_hash: &[u8],
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO calendar_push_events (id, workspace_id, push_id, booking_id, remote_id, content_hash)
           SELECT $1, app_workspace_id(), $2, $3, $4, $5
           WHERE EXISTS (SELECT 1 FROM calendar_pushes
                         WHERE workspace_id = app_workspace_id() AND id = $2 AND disconnected_at IS NULL)
           ON CONFLICT (push_id, booking_id)
           DO UPDATE SET remote_id = EXCLUDED.remote_id, content_hash = EXCLUDED.content_hash,
                         written_at = now()"#,
        core_domain::ids::new_id(),
        push_id,
        booking_id,
        remote_id,
        content_hash
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The booking's event was removed.
pub async fn forget_written(
    conn: &mut PgConnection,
    push_id: Uuid,
    booking_id: Uuid,
) -> Result<()> {
    sqlx::query!(
        "DELETE FROM calendar_push_events WHERE workspace_id = app_workspace_id() AND push_id = $1 AND booking_id = $2",
        push_id,
        booking_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Forgets every event of a connection (its calendar is gone).
pub async fn clear_events(conn: &mut PgConnection, push_id: Uuid) -> Result<()> {
    sqlx::query!(
        "DELETE FROM calendar_push_events WHERE workspace_id = app_workspace_id() AND push_id = $1",
        push_id
    )
    .execute(conn)
    .await?;
    Ok(())
}
