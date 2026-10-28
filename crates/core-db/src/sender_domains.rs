//! Workspaces' own sender domains: the provider's id, the DNS records the
//! owner adds, and whether the provider has found them.

use chrono::{DateTime, Utc};
use core_domain::WorkspaceId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct SenderDomain {
    pub id: Uuid,
    pub domain: String,
    pub provider_id: String,
    pub status: String,
    pub records: Value,
    pub local_part: String,
    pub in_use: bool,
    pub dmarc: bool,
    pub dns_provider: Option<Value>,
    pub checked_at: Option<DateTime<Utc>>,
    pub next_check_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

macro_rules! select_domain {
    ($tail:literal $(, $arg:expr)*) => {
        sqlx::query_as!(
            SenderDomain,
            r#"SELECT id, domain, provider_id, status, records, local_part, in_use, dmarc, dns_provider,
                      checked_at, next_check_at, verified_at, created_at, version
               FROM sender_domains WHERE workspace_id = app_workspace_id() "# + $tail
            $(, $arg)*
        )
    };
}

pub async fn list(conn: &mut PgConnection) -> Result<Vec<SenderDomain>> {
    Ok(select_domain!("ORDER BY created_at, id")
        .fetch_all(conn)
        .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<SenderDomain>> {
    Ok(select_domain!("AND id = $1", id)
        .fetch_optional(conn)
        .await?)
}

/// One domain, locked for an update in this transaction.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<SenderDomain>> {
    Ok(select_domain!("AND id = $1 FOR UPDATE", id)
        .fetch_optional(conn)
        .await?)
}

/// Whether any workspace has the domain.
pub async fn taken(conn: &mut PgConnection, domain: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar!(r#"SELECT sender_domain_taken($1) AS "taken!""#, domain)
            .fetch_one(conn)
            .await?,
    )
}

pub struct NewSenderDomain<'a> {
    pub id: Uuid,
    pub domain: &'a str,
    pub provider_id: &'a str,
    pub records: &'a Value,
    pub local_part: &'a str,
    pub next_check_at: Option<DateTime<Utc>>,
}

pub async fn insert(conn: &mut PgConnection, n: NewSenderDomain<'_>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO sender_domains (id, workspace_id, domain, provider_id, records, local_part, next_check_at)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6)"#,
        n.id,
        n.domain,
        n.provider_id,
        n.records,
        n.local_part,
        n.next_check_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// What a check found.
pub struct Checked<'a> {
    pub status: &'a str,
    pub records: &'a Value,
    pub dmarc: bool,
    pub dns_provider: Option<&'a Value>,
    pub next_check_at: Option<DateTime<Utc>>,
}

pub async fn set_checked(conn: &mut PgConnection, id: Uuid, c: Checked<'_>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE sender_domains
           SET status = $2, records = $3, dmarc = $4, dns_provider = $5, next_check_at = $6, checked_at = now(),
               verified_at = CASE WHEN $2 = 'verified' THEN coalesce(verified_at, now()) ELSE verified_at END
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        c.status,
        c.records,
        c.dmarc,
        c.dns_provider,
        c.next_check_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Brings the next check forward (or back).
pub async fn set_next_check(
    conn: &mut PgConnection,
    id: Uuid,
    at: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query!(
        "UPDATE sender_domains SET next_check_at = $2 WHERE workspace_id = app_workspace_id() AND id = $1",
        id,
        at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The sending address and whether it is used. Using one domain stops
/// using any other.
pub async fn set_preferences(
    conn: &mut PgConnection,
    id: Uuid,
    local_part: &str,
    in_use: bool,
) -> Result<()> {
    if in_use {
        sqlx::query!(
            "UPDATE sender_domains SET in_use = false WHERE workspace_id = app_workspace_id() AND in_use AND id <> $1",
            id
        )
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query!(
        r#"UPDATE sender_domains SET local_part = $2, in_use = $3
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        local_part,
        in_use
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Whether the workspace sends from any domain yet.
pub async fn any_in_use(conn: &mut PgConnection) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM sender_domains WHERE workspace_id = app_workspace_id() AND in_use) AS "any!""#
    )
    .fetch_one(conn)
    .await?)
}

pub async fn delete(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    Ok(sqlx::query!(
        "DELETE FROM sender_domains WHERE workspace_id = app_workspace_id() AND id = $1",
        id
    )
    .execute(conn)
    .await?
    .rows_affected()
        > 0)
}

/// Removes every domain of the workspace, returning the provider's ids.
pub async fn delete_all(conn: &mut PgConnection) -> Result<Vec<(Uuid, String)>> {
    Ok(sqlx::query!(
        "DELETE FROM sender_domains WHERE workspace_id = app_workspace_id() RETURNING id, provider_id"
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| (r.id, r.provider_id))
    .collect())
}

/// The address the workspace sends from: at its domain in use, while that
/// domain is verified.
pub async fn sending_address(conn: &mut PgConnection) -> Result<Option<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT local_part || '@' || domain AS "address!" FROM sender_domains
           WHERE workspace_id = app_workspace_id() AND in_use AND status = 'verified'"#
    )
    .fetch_optional(conn)
    .await?)
}

pub struct Due {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub at: DateTime<Utc>,
}

/// Domains due a check, in workspaces still open (the worker's sweep).
pub async fn due(conn: &mut PgConnection, now: DateTime<Utc>, limit: i64) -> Result<Vec<Due>> {
    Ok(sqlx::query_as!(
        Due,
        r#"SELECT d.id, d.workspace_id AS "workspace_id: WorkspaceId", d.next_check_at AS "at!"
           FROM sender_domains d JOIN workspaces w ON w.id = d.workspace_id
           WHERE d.next_check_at <= $1 AND w.status IN ('trial', 'active')
           ORDER BY d.next_check_at LIMIT $2"#,
        now,
        limit
    )
    .fetch_all(conn)
    .await?)
}
