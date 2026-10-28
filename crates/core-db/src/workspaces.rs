//! Workspaces and their per-role permission sets.

use chrono::{DateTime, Utc};
use core_domain::workspace::{Perspective, WorkspaceStatus};
use core_domain::{Permission, Role, UserId, WorkspaceId};
use serde_json::Value;
use sqlx::PgConnection;

use crate::Result;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub slug: String,
    pub name: String,
    pub plan: String,
    pub status: WorkspaceStatus,
    pub perspective: Perspective,
    pub country: String,
    pub default_currency: String,
    pub default_timezone: String,
    pub locale: String,
    pub enabled_modules: Vec<String>,
    pub presets: Vec<String>,
    pub extras: Vec<String>,
    pub vocabulary: Value,
    /// The setup form's answers; `None` until it is first sent.
    pub setup: Option<Value>,
    pub setup_at: Option<DateTime<Utc>>,
    pub branding: Value,
    pub billing_contact: Value,
    pub plugin_slug: Option<String>,
    pub require_staff_2fa: bool,
    /// How many days a new hold lasts; 0 means holds never lapse.
    pub hold_days: i32,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn get(conn: &mut PgConnection, id: WorkspaceId) -> Result<Option<Workspace>> {
    Ok(sqlx::query_as!(
        Workspace,
        r#"SELECT id AS "id: WorkspaceId", slug, name, plan, status AS "status: WorkspaceStatus",
                  perspective AS "perspective: Perspective", country, default_currency, default_timezone,
                  locale, enabled_modules, presets, extras, vocabulary, setup, setup_at,
                  branding, billing_contact, plugin_slug, require_staff_2fa,
                  hold_days, version, created_at, updated_at
           FROM workspaces WHERE id = $1"#,
        id.0
    )
    .fetch_optional(conn)
    .await?)
}

pub struct NewWorkspace<'a> {
    pub id: WorkspaceId,
    pub slug: &'a str,
    pub name: &'a str,
    pub plan: &'a str,
    pub perspective: Perspective,
    pub country: &'a str,
    pub default_currency: &'a str,
    pub default_timezone: &'a str,
    pub locale: &'a str,
    pub enabled_modules: &'a [String],
    pub presets: &'a [String],
    pub extras: &'a [String],
    pub vocabulary: &'a Value,
    pub plugin_slug: Option<&'a str>,
    pub created_by: Option<UserId>,
}

/// Inserts a workspace and seeds its role permissions. The transaction's
/// workspace must be `new.id` (row-level security).
pub async fn insert(conn: &mut PgConnection, new: NewWorkspace<'_>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO workspaces (id, slug, name, plan, perspective, country, default_currency,
                                   default_timezone, locale, enabled_modules, plugin_slug, created_by, updated_by,
                                   presets, extras, vocabulary)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12, $13, $14, $15)"#,
        new.id.0,
        new.slug,
        new.name,
        new.plan,
        new.perspective as Perspective,
        new.country,
        new.default_currency,
        new.default_timezone,
        new.locale,
        new.enabled_modules,
        new.plugin_slug,
        new.created_by.map(|u| u.0),
        new.presets,
        new.extras,
        new.vocabulary
    )
    .execute(&mut *conn)
    .await?;
    for role in Role::ALL {
        for permission in role.default_permissions() {
            sqlx::query!(
                "INSERT INTO role_permissions (workspace_id, role, permission) VALUES ($1, $2, $3)",
                new.id.0,
                role as Role,
                permission.as_str()
            )
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}

/// A full replacement of the editable settings; the service merges the patch first.
pub struct Settings<'a> {
    pub name: &'a str,
    pub perspective: Perspective,
    pub country: &'a str,
    pub default_currency: &'a str,
    pub default_timezone: &'a str,
    pub locale: &'a str,
    pub enabled_modules: &'a [String],
    pub branding: &'a Value,
    pub billing_contact: &'a Value,
    pub require_staff_2fa: bool,
    pub hold_days: i32,
}

/// Returns false when `expected_version` no longer matches (someone else saved first).
pub async fn update_settings(
    conn: &mut PgConnection,
    id: WorkspaceId,
    s: Settings<'_>,
    expected_version: i64,
    by: Option<UserId>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE workspaces
           SET name = $2, perspective = $3, country = $4, default_currency = $5, default_timezone = $6,
               locale = $7, enabled_modules = $8, branding = $9, billing_contact = $10,
               require_staff_2fa = $11, updated_by = $12, hold_days = $14
           WHERE id = $1 AND version = $13"#,
        id.0,
        s.name,
        s.perspective as Perspective,
        s.country,
        s.default_currency,
        s.default_timezone,
        s.locale,
        s.enabled_modules,
        s.branding,
        s.billing_contact,
        s.require_staff_2fa,
        by.map(|u| u.0),
        expected_version,
        s.hold_days
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// What the setup form set up, with its answers.
pub struct Setup<'a> {
    pub perspective: Perspective,
    pub presets: &'a [String],
    pub enabled_modules: &'a [String],
    pub extras: &'a [String],
    pub vocabulary: &'a Value,
    pub answers: &'a Value,
}

/// Returns false when `expected_version` no longer matches.
pub async fn apply_setup(
    conn: &mut PgConnection,
    id: WorkspaceId,
    s: Setup<'_>,
    expected_version: i64,
    by: Option<UserId>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE workspaces
           SET perspective = $2, presets = $3, enabled_modules = $4, extras = $5, vocabulary = $6,
               setup = $7, setup_at = now(), updated_by = $8
           WHERE id = $1 AND version = $9"#,
        id.0,
        s.perspective as Perspective,
        s.presets,
        s.enabled_modules,
        s.extras,
        s.vocabulary,
        s.answers,
        by.map(|u| u.0),
        expected_version
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn set_status(
    conn: &mut PgConnection,
    id: WorkspaceId,
    status: WorkspaceStatus,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        "UPDATE workspaces SET status = $2, updated_by = $3 WHERE id = $1",
        id.0,
        status as WorkspaceStatus,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The permissions a role holds in the workspace in context. Unknown strings
/// (from a newer release during a rolling deploy) are ignored.
pub async fn role_permissions(conn: &mut PgConnection, role: Role) -> Result<Vec<Permission>> {
    let rows = sqlx::query_scalar!(
        "SELECT permission FROM role_permissions WHERE workspace_id = app_workspace_id() AND role = $1",
        role as Role
    )
    .fetch_all(conn)
    .await?;
    Ok(rows.iter().filter_map(|p| p.parse().ok()).collect())
}

#[derive(Debug, Clone)]
pub struct AdminListing {
    pub id: WorkspaceId,
    pub slug: String,
    pub name: String,
    pub plan: String,
    pub status: WorkspaceStatus,
    pub created_at: DateTime<Utc>,
    pub member_count: i64,
    pub presets: Vec<String>,
    pub enabled_modules: Vec<String>,
    pub extras: Vec<String>,
    pub setup: Option<Value>,
    pub setup_at: Option<DateTime<Utc>>,
    /// Comments from the setup form not yet marked done.
    pub open_requests: i64,
}

/// Every workspace. The database raises unless the user in context is a superadmin.
pub async fn admin_list(conn: &mut PgConnection) -> Result<Vec<AdminListing>> {
    Ok(sqlx::query_as!(
        AdminListing,
        r#"SELECT id AS "id!: WorkspaceId", slug AS "slug!", name AS "name!", plan AS "plan!",
                  status AS "status!: WorkspaceStatus", created_at AS "created_at!",
                  member_count AS "member_count!", presets AS "presets!",
                  enabled_modules AS "enabled_modules!", extras AS "extras!", setup, setup_at,
                  open_requests AS "open_requests!"
           FROM admin_list_workspaces()"#
    )
    .fetch_all(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct MyWorkspace {
    pub id: WorkspaceId,
    pub slug: String,
    pub name: String,
    pub status: WorkspaceStatus,
    pub role: Role,
    pub plan: String,
}

/// The workspaces the user in context belongs to.
pub async fn for_user(conn: &mut PgConnection, user: UserId) -> Result<Vec<MyWorkspace>> {
    Ok(sqlx::query_as!(
        MyWorkspace,
        r#"SELECT w.id AS "id: WorkspaceId", w.slug, w.name, w.status AS "status: WorkspaceStatus",
                  m.role AS "role: Role", w.plan
           FROM memberships m JOIN workspaces w ON w.id = m.workspace_id
           WHERE m.user_id = $1 AND m.deleted_at IS NULL AND w.status <> 'closed'
           ORDER BY w.name"#,
        user.0
    )
    .fetch_all(conn)
    .await?)
}

/// The automation kill switch for the workspace in context.
pub async fn automations_paused(conn: &mut PgConnection) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        "SELECT automations_paused FROM workspaces WHERE id = app_workspace_id()"
    )
    .fetch_optional(conn)
    .await?
    .unwrap_or(true))
}

pub async fn set_automations_paused(conn: &mut PgConnection, paused: bool) -> Result<()> {
    sqlx::query!(
        "UPDATE workspaces SET automations_paused = $1 WHERE id = app_workspace_id()",
        paused
    )
    .execute(conn)
    .await?;
    Ok(())
}
