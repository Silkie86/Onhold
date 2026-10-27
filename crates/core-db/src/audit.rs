//! The append-only audit log. `onhold_app` can insert and read rows in the
//! workspace in context, and has no UPDATE or DELETE grant.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use core_domain::{Ctx, UserId};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

pub struct Entry<'a> {
    pub action: &'a str,
    pub resource: &'a str,
    pub object_id: Option<Uuid>,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

pub async fn record(conn: &mut PgConnection, ctx: &Ctx, entry: Entry<'_>) -> Result<Uuid> {
    let id = core_domain::ids::new_id();
    sqlx::query!(
        r#"INSERT INTO audit_log (id, workspace_id, actor_user_id, actor_api_key_id, impersonator_id,
                                  action, resource, object_id, before, after, ip, user_agent, request_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
        id,
        ctx.workspace_id.0,
        ctx.user_id().map(|u| u.0),
        ctx.api_key_id(),
        ctx.impersonator().map(|u| u.0),
        entry.action,
        entry.resource,
        entry.object_id,
        entry.before,
        entry.after,
        ctx.meta.ip as Option<IpAddr>,
        ctx.meta.user_agent.as_deref(),
        ctx.meta.request_id.as_deref()
    )
    .execute(conn)
    .await?;
    Ok(id)
}

#[derive(Debug, Clone)]
pub struct Row {
    pub id: Uuid,
    pub actor_user_id: Option<UserId>,
    pub actor_api_key_id: Option<Uuid>,
    pub impersonator_id: Option<UserId>,
    pub action: String,
    pub resource: String,
    pub object_id: Option<Uuid>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub request_id: Option<String>,
    pub at: DateTime<Utc>,
}

/// Newest first. `before` is a keyset cursor: rows strictly older than that id.
pub async fn list(conn: &mut PgConnection, before: Option<Uuid>, limit: i64) -> Result<Vec<Row>> {
    Ok(sqlx::query_as!(
        Row,
        r#"SELECT id, actor_user_id AS "actor_user_id: UserId", actor_api_key_id,
                  impersonator_id AS "impersonator_id: UserId", action, resource, object_id,
                  before, after, ip AS "ip: IpAddr", user_agent, request_id, at
           FROM audit_log
           WHERE $1::uuid IS NULL OR id < $1
           ORDER BY id DESC
           LIMIT $2"#,
        before,
        limit
    )
    .fetch_all(conn)
    .await?)
}
