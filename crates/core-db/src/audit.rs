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
                                  action, resource, object_id, before, after, ip, user_agent, request_id,
                                  actor_rule_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"#,
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
        ctx.meta.request_id.as_deref(),
        ctx.rule_id()
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
    pub actor_rule_id: Option<Uuid>,
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
                  impersonator_id AS "impersonator_id: UserId", actor_rule_id, action, resource, object_id,
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

/// An audit entry about a record, with the acting member's name.
#[derive(Debug, Clone)]
pub struct ObjectEntry {
    pub id: Uuid,
    pub actor_user_id: Option<UserId>,
    /// `None` when the actor is no longer a member of the workspace.
    pub actor_name: Option<String>,
    pub actor_api_key_id: Option<Uuid>,
    pub impersonator_id: Option<UserId>,
    pub actor_rule_id: Option<Uuid>,
    pub action: String,
    pub resource: String,
    pub object_id: Option<Uuid>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub at: DateTime<Utc>,
}

/// The audit entries about these records, newest first; `before` is a
/// keyset cursor on the entry id.
pub async fn for_objects(
    conn: &mut PgConnection,
    ids: &[Uuid],
    before: Option<Uuid>,
    limit: i64,
) -> Result<Vec<ObjectEntry>> {
    Ok(sqlx::query_as!(
        ObjectEntry,
        r#"SELECT a.id, a.actor_user_id AS "actor_user_id: UserId", u.name AS "actor_name?",
                  a.actor_api_key_id, a.impersonator_id AS "impersonator_id: UserId", a.actor_rule_id,
                  a.action, a.resource, a.object_id, a.before, a.after, a.at
           FROM audit_log a LEFT JOIN users u ON u.id = a.actor_user_id
           WHERE a.workspace_id = app_workspace_id() AND a.object_id = ANY($1)
             AND ($2::uuid IS NULL OR a.id < $2)
           ORDER BY a.id DESC
           LIMIT $3"#,
        ids,
        before,
        limit
    )
    .fetch_all(conn)
    .await?)
}
