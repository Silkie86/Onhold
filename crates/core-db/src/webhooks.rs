//! Outbound webhook endpoints and their delivery log. Signing secrets are
//! stored encrypted.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub id: Uuid,
    pub url: String,
    pub description: Option<String>,
    pub events: Vec<String>,
    pub enabled: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn list(conn: &mut PgConnection) -> Result<Vec<Endpoint>> {
    Ok(sqlx::query_as!(
        Endpoint,
        r#"SELECT id, url, description, events, enabled, version, created_at, updated_at
           FROM webhook_endpoints WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
           ORDER BY created_at"#
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Endpoint>> {
    Ok(sqlx::query_as!(
        Endpoint,
        r#"SELECT id, url, description, events, enabled, version, created_at, updated_at
           FROM webhook_endpoints WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// The encrypted signing secret.
pub async fn secret(conn: &mut PgConnection, id: Uuid) -> Result<Option<Vec<u8>>> {
    Ok(sqlx::query_scalar!(
        "SELECT secret_enc FROM webhook_endpoints WHERE workspace_id = app_workspace_id() AND id = $1",
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    url: &str,
    description: Option<&str>,
    events: &[String],
    secret_enc: &[u8],
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO webhook_endpoints (id, workspace_id, url, description, events, secret_enc,
                                          created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $6)"#,
        id,
        url,
        description,
        events,
        secret_enc,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// An endpoint's editable settings.
pub struct Values<'a> {
    pub url: &'a str,
    pub description: Option<&'a str>,
    pub events: &'a [String],
    pub enabled: bool,
}

pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    expected_version: Option<i64>,
    by: Option<UserId>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE webhook_endpoints SET url = $2, description = $3, events = $4, enabled = $5, updated_by = $6
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
             AND ($7::bigint IS NULL OR version = $7)"#,
        id,
        v.url,
        v.description,
        v.events,
        v.enabled,
        by.map(|u| u.0),
        expected_version
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn set_secret(
    conn: &mut PgConnection,
    id: Uuid,
    secret_enc: &[u8],
    by: Option<UserId>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE webhook_endpoints SET secret_enc = $2, updated_by = $3
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        secret_enc,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE webhook_endpoints SET deleted_at = now(), enabled = false, updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

// Deliveries ------------------------------------------------------------------------

pub struct NewDelivery<'a> {
    pub endpoint_id: Uuid,
    pub event_id: Uuid,
    pub job_id: Option<Uuid>,
    pub attempt: i32,
    pub succeeded: bool,
    pub status_code: Option<i32>,
    pub error: Option<&'a str>,
    pub response_excerpt: Option<&'a str>,
    pub duration_ms: i32,
}

pub async fn record_delivery(conn: &mut PgConnection, d: NewDelivery<'_>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO webhook_deliveries (id, workspace_id, endpoint_id, event_id, job_id, attempt, succeeded,
                                           status_code, error, response_excerpt, duration_ms)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
        core_domain::ids::new_id(),
        d.endpoint_id,
        d.event_id,
        d.job_id,
        d.attempt,
        d.succeeded,
        d.status_code,
        d.error,
        d.response_excerpt,
        d.duration_ms
    )
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Delivery {
    pub id: Uuid,
    pub event_id: Uuid,
    pub topic: Option<String>,
    pub job_id: Option<Uuid>,
    pub attempt: i32,
    pub succeeded: bool,
    pub status_code: Option<i32>,
    pub error: Option<String>,
    pub response_excerpt: Option<String>,
    pub duration_ms: i32,
    pub created_at: DateTime<Utc>,
}

pub async fn deliveries(
    conn: &mut PgConnection,
    endpoint_id: Uuid,
    before: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Delivery>> {
    Ok(sqlx::query_as!(
        Delivery,
        r#"SELECT d.id, d.event_id, o.topic AS "topic?", d.job_id, d.attempt, d.succeeded, d.status_code,
                  d.error, d.response_excerpt, d.duration_ms, d.created_at
           FROM webhook_deliveries d
           LEFT JOIN outbox o ON o.id = d.event_id AND o.workspace_id = d.workspace_id AND o.seq IS NOT NULL
           WHERE d.workspace_id = app_workspace_id() AND d.endpoint_id = $1
             AND ($2::uuid IS NULL OR d.id < $2)
           ORDER BY d.id DESC
           LIMIT $3"#,
        endpoint_id,
        before,
        limit
    )
    .fetch_all(conn)
    .await?)
}
