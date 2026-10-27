//! Every outbound message as sent, for the audit trail.

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

pub struct NewMessage<'a> {
    /// `None` for platform mail to a user (a sign-in link); the transaction's
    /// user must then be `user`.
    pub workspace: Option<WorkspaceId>,
    pub user: Option<UserId>,
    pub to_address: &'a str,
    pub subject: Option<&'a str>,
    pub body_html: Option<&'a str>,
    pub body_text: &'a str,
    pub template_key: Option<&'a str>,
    pub rule_id: Option<Uuid>,
    pub job_id: Option<Uuid>,
    pub status: &'a str,
    pub provider_message_id: Option<&'a str>,
    pub error: Option<&'a str>,
}

pub async fn insert(conn: &mut PgConnection, m: NewMessage<'_>) -> Result<Uuid> {
    let id = core_domain::ids::new_id();
    sqlx::query!(
        r#"INSERT INTO outbound_messages (id, workspace_id, user_id, channel, to_address, subject, body_html,
                                          body_text, template_key, rule_id, job_id, status,
                                          provider_message_id, error)
           VALUES ($1, $2, $3, 'email', $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
        id,
        m.workspace.map(|w| w.0),
        m.user.map(|u| u.0),
        m.to_address,
        m.subject,
        m.body_html,
        m.body_text,
        m.template_key,
        m.rule_id,
        m.job_id,
        m.status,
        m.provider_message_id,
        m.error
    )
    .execute(conn)
    .await?;
    Ok(id)
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: Uuid,
    pub channel: String,
    pub to_address: String,
    pub subject: Option<String>,
    pub body_html: Option<String>,
    pub body_text: String,
    pub template_key: Option<String>,
    pub rule_id: Option<Uuid>,
    pub job_id: Option<Uuid>,
    pub status: String,
    pub provider_message_id: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// The workspace's sent messages, newest first.
pub async fn list(
    conn: &mut PgConnection,
    before: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Message>> {
    Ok(sqlx::query_as!(
        Message,
        r#"SELECT id, channel, to_address, subject, body_html, body_text, template_key, rule_id, job_id,
                  status, provider_message_id, error, created_at
           FROM outbound_messages
           WHERE workspace_id = app_workspace_id() AND ($1::uuid IS NULL OR id < $1)
           ORDER BY id DESC
           LIMIT $2"#,
        before,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Messages sent to any of these addresses (lower-cased), newest first.
pub async fn to_addresses(
    conn: &mut PgConnection,
    addresses: &[String],
    before: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Message>> {
    Ok(sqlx::query_as!(
        Message,
        r#"SELECT id, channel, to_address, subject, body_html, body_text, template_key, rule_id, job_id,
                  status, provider_message_id, error, created_at
           FROM outbound_messages
           WHERE workspace_id = app_workspace_id() AND lower(to_address) = ANY($1)
             AND ($2::uuid IS NULL OR id < $2)
           ORDER BY id DESC
           LIMIT $3"#,
        addresses,
        before,
        limit
    )
    .fetch_all(conn)
    .await?)
}
