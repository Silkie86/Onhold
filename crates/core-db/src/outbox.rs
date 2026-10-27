//! The event outbox. Every meaningful change appends a row in the same
//! transaction. The worker publishes rows in batches: it gives each a `seq`
//! (in commit order, under an advisory lock) and fans it out to jobs.
//! Published rows are the workspace's event stream, read by `seq`.

use chrono::{DateTime, Utc};
use core_domain::WorkspaceId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

/// Appends an event. `workspace` must match the transaction's workspace
/// (`None` for user-level events such as a magic link request). `secret`
/// is encrypted data for the one consumer that needs it, never published.
pub async fn emit(
    conn: &mut PgConnection,
    workspace: Option<WorkspaceId>,
    topic: &str,
    payload: &Value,
) -> Result<Uuid> {
    emit_with_secret(
        conn,
        core_domain::ids::new_id(),
        workspace,
        topic,
        payload,
        None,
    )
    .await
}

pub async fn emit_with_secret(
    conn: &mut PgConnection,
    id: Uuid,
    workspace: Option<WorkspaceId>,
    topic: &str,
    payload: &Value,
    secret: Option<&[u8]>,
) -> Result<Uuid> {
    sqlx::query!(
        "INSERT INTO outbox (id, workspace_id, topic, payload, secret) VALUES ($1, $2, $3, $4, $5)",
        id,
        workspace.map(|w| w.0),
        topic,
        payload,
        secret
    )
    .execute(conn)
    .await?;
    Ok(id)
}

// Publishing (worker role) ---------------------------------------------------------

/// Serialises publishers so `seq` follows commit order.
const PUBLISH_LOCK: i64 = 0x6f6e_686f_6c64_0001;

#[derive(Debug, Clone)]
pub struct Pending {
    pub id: Uuid,
    pub workspace_id: Option<WorkspaceId>,
    pub topic: String,
    pub payload: Value,
    pub secret: Option<Vec<u8>>,
    pub created_at: DateTime<Utc>,
}

/// Takes the publish lock and the next unpublished events, oldest first.
/// Call inside a transaction and publish each with [`mark_published`].
pub async fn take_unpublished(conn: &mut PgConnection, limit: i64) -> Result<Vec<Pending>> {
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", PUBLISH_LOCK)
        .execute(&mut *conn)
        .await?;
    Ok(sqlx::query_as!(
        Pending,
        r#"SELECT id, workspace_id AS "workspace_id: WorkspaceId", topic, payload, secret, created_at
           FROM outbox WHERE processed_at IS NULL
           ORDER BY created_at, id
           LIMIT $1
           FOR UPDATE"#,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Gives an event its stream position and drops its secret (the job that
/// needs it carries its own copy).
pub async fn mark_published(conn: &mut PgConnection, id: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE outbox SET seq = nextval('outbox_seq'), processed_at = now(), secret = NULL
           WHERE id = $1 RETURNING seq AS "seq!""#,
        id
    )
    .fetch_one(conn)
    .await?)
}

/// Deletes events published before `before`, up to a stream position, and
/// records that position so clients with an older cursor know to resync.
pub async fn prune_published_before(conn: &mut PgConnection, before: DateTime<Utc>) -> Result<u64> {
    let Some(through) = sqlx::query_scalar!(
        "SELECT max(seq) FROM outbox WHERE processed_at IS NOT NULL AND processed_at < $1",
        before
    )
    .fetch_one(&mut *conn)
    .await?
    else {
        return Ok(0);
    };
    sqlx::query!(
        r#"INSERT INTO outbox_pruned (singleton, through_seq) VALUES (true, $1)
           ON CONFLICT (singleton) DO UPDATE SET through_seq = greatest(outbox_pruned.through_seq, $1)"#,
        through
    )
    .execute(&mut *conn)
    .await?;
    let done = sqlx::query!("DELETE FROM outbox WHERE seq <= $1", through)
        .execute(conn)
        .await?;
    Ok(done.rows_affected())
}

/// Events at or below this position may have been deleted.
pub async fn pruned_through(conn: &mut PgConnection) -> Result<i64> {
    Ok(
        sqlx::query_scalar!("SELECT through_seq FROM outbox_pruned WHERE singleton")
            .fetch_optional(conn)
            .await?
            .unwrap_or(0),
    )
}

// Reading the stream (app role) ----------------------------------------------------

#[derive(Debug, Clone)]
pub struct Event {
    pub id: Uuid,
    pub seq: i64,
    pub topic: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

/// Published events after `after` (a `seq`), oldest first. `topics` may
/// hold exact topics or `prefix.*` patterns.
pub async fn list(
    conn: &mut PgConnection,
    after: i64,
    topics: &[String],
    limit: i64,
) -> Result<Vec<Event>> {
    let exact: Vec<&str> = topics
        .iter()
        .filter(|t| !t.ends_with('*'))
        .map(String::as_str)
        .collect();
    let prefixes: Vec<String> = topics
        .iter()
        .filter_map(|t| t.strip_suffix('*'))
        .map(|p| {
            format!(
                "{}%",
                p.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        })
        .collect();
    let filtered = !topics.is_empty();
    Ok(sqlx::query_as!(
        Event,
        r#"SELECT id, seq AS "seq!", topic, payload, created_at
           FROM outbox
           WHERE workspace_id = app_workspace_id() AND seq > $1
             AND (NOT $2 OR topic = ANY($3) OR topic LIKE ANY($4))
           ORDER BY seq
           LIMIT $5"#,
        after,
        filtered,
        &exact as &[&str],
        &prefixes,
        limit
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Event>> {
    Ok(sqlx::query_as!(
        Event,
        r#"SELECT id, seq AS "seq!", topic, payload, created_at
           FROM outbox WHERE workspace_id = app_workspace_id() AND id = $1 AND seq IS NOT NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// The oldest and newest positions still in the workspace's stream.
pub async fn bounds(conn: &mut PgConnection) -> Result<(Option<i64>, Option<i64>)> {
    let row = sqlx::query!(
        r#"SELECT min(seq) AS "min", max(seq) AS "max" FROM outbox
           WHERE workspace_id = app_workspace_id() AND seq IS NOT NULL"#
    )
    .fetch_one(conn)
    .await?;
    Ok((row.min, row.max))
}
