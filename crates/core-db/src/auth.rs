//! Pre-authentication lookups. Each wraps one SECURITY DEFINER function that
//! returns at most one row for one exact key; nothing here can list rows.

use chrono::{DateTime, Utc};
use core_domain::{Role, UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct SessionLookup {
    pub session_id: Uuid,
    pub user_id: UserId,
    pub csrf_token: String,
    pub mfa_verified: bool,
    pub expires_at: DateTime<Utc>,
    pub impersonated_workspace_id: Option<WorkspaceId>,
    pub impersonation_expires_at: Option<DateTime<Utc>>,
}

pub async fn find_session(
    conn: &mut PgConnection,
    token_hash: &[u8],
) -> Result<Option<SessionLookup>> {
    Ok(sqlx::query_as!(
        SessionLookup,
        r#"SELECT session_id AS "session_id!", user_id AS "user_id!: UserId", csrf_token AS "csrf_token!",
                  mfa_verified AS "mfa_verified!", expires_at AS "expires_at!",
                  impersonated_workspace_id AS "impersonated_workspace_id: WorkspaceId",
                  impersonation_expires_at
           FROM auth_find_session($1)"#,
        token_hash
    )
    .fetch_optional(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct LoginLookup {
    pub user_id: UserId,
    pub password_hash: Option<String>,
    pub locked_until: Option<DateTime<Utc>>,
}

pub async fn find_user_by_email(
    conn: &mut PgConnection,
    email: &str,
) -> Result<Option<LoginLookup>> {
    Ok(sqlx::query_as!(
        LoginLookup,
        r#"SELECT user_id AS "user_id!: UserId", password_hash, locked_until
           FROM auth_find_user_by_email($1::text::citext)"#,
        email
    )
    .fetch_optional(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct ApiKeyLookup {
    pub workspace_id: WorkspaceId,
    pub secret_hash: Vec<u8>,
    pub permissions: Vec<String>,
    pub created_by: Option<UserId>,
}

pub async fn find_api_key(conn: &mut PgConnection, key_id: Uuid) -> Result<Option<ApiKeyLookup>> {
    Ok(sqlx::query_as!(
        ApiKeyLookup,
        r#"SELECT workspace_id AS "workspace_id!: WorkspaceId", secret_hash AS "secret_hash!",
                  permissions AS "permissions!", created_by AS "created_by: UserId"
           FROM auth_find_api_key($1)"#,
        key_id
    )
    .fetch_optional(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct InvitationLookup {
    pub invitation_id: Uuid,
    pub workspace_id: WorkspaceId,
    pub email: String,
    pub role: Role,
    pub workspace_name: String,
}

pub async fn find_invitation(
    conn: &mut PgConnection,
    token_hash: &[u8],
) -> Result<Option<InvitationLookup>> {
    Ok(sqlx::query_as!(
        InvitationLookup,
        r#"SELECT invitation_id AS "invitation_id!", workspace_id AS "workspace_id!: WorkspaceId",
                  email::text AS "email!", role AS "role!: Role", workspace_name AS "workspace_name!"
           FROM auth_find_invitation($1)"#,
        token_hash
    )
    .fetch_optional(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct MagicLinkLookup {
    pub magic_link_id: Uuid,
    pub user_id: UserId,
}

pub async fn find_magic_link(
    conn: &mut PgConnection,
    token_hash: &[u8],
) -> Result<Option<MagicLinkLookup>> {
    Ok(sqlx::query_as!(
        MagicLinkLookup,
        r#"SELECT magic_link_id AS "magic_link_id!", user_id AS "user_id!: UserId"
           FROM auth_find_magic_link($1)"#,
        token_hash
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn find_oidc_identity(
    conn: &mut PgConnection,
    provider: &str,
    subject: &str,
) -> Result<Option<UserId>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT user_id AS "user_id!: UserId" FROM auth_find_oidc_identity($1, $2)"#,
        provider,
        subject
    )
    .fetch_optional(conn)
    .await?)
}
