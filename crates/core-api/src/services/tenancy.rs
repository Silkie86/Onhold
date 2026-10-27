//! Workspaces, members, invitations, API keys and the audit log.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use core_db::Tx;
use core_domain::crypto::{hash_token, new_token};
use core_domain::workspace::{self as ws, Perspective, WorkspaceStatus};
use core_domain::{Actor, Ctx, Permission, Role, UserId, WorkspaceId};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::extract::format_api_key;
use crate::services::{Change, record, record_with_secret};

async fn begin(state: &AppState, ctx: &Ctx) -> ApiResult<Tx> {
    Ok(core_db::begin(&state.db, Some(ctx.workspace_id), ctx.user_id()).await?)
}

/// Whether the actor counts as an owner for owner-only rules: an owner
/// member, an impersonating superadmin, or the operator CLI.
fn acts_as_owner(ctx: &Ctx) -> bool {
    ctx.role == Some(Role::Owner)
        || ctx.impersonator().is_some()
        || matches!(ctx.actor, Actor::System)
}

// Workspaces -------------------------------------------------------------------------

pub struct NewWorkspace {
    pub slug: String,
    pub name: String,
    pub country: String,
    pub default_currency: String,
    pub default_timezone: String,
    pub locale: Option<String>,
    pub plan: Option<String>,
    pub perspective: Option<Perspective>,
    pub plugin_slug: Option<String>,
    pub owner_email: String,
}

pub struct CreatedWorkspace {
    pub workspace: core_db::workspaces::Workspace,
    pub owner_invitation_id: Uuid,
    /// The link the owner uses to join. Email delivery arrives in M2; until
    /// then the operator passes it on.
    pub owner_invitation_url: String,
}

/// Creates a workspace with its role permissions and an owner invitation.
/// `created_by` is the superadmin, or `None` for the operator CLI.
pub async fn create_workspace(
    state: &AppState,
    created_by: Option<UserId>,
    meta: core_domain::authz::RequestMeta,
    input: NewWorkspace,
) -> ApiResult<CreatedWorkspace> {
    ws::validate_slug("slug", &input.slug)?;
    let name = ws::validate_name("name", &input.name)?;
    ws::validate_country(&input.country)?;
    ws::validate_currency(&input.default_currency)?;
    ws::validate_timezone(&input.default_timezone)?;
    let locale = input
        .locale
        .unwrap_or_else(|| format!("en-{}", input.country));
    ws::validate_locale(&locale)?;
    let plan = input.plan.unwrap_or_else(|| "standard".into());
    if plan.is_empty()
        || plan.len() > 40
        || !plan
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err(ApiError::BadRequest {
            code: "invalid_input",
            message: "use lowercase letters, digits, `-` or `_`".into(),
            field: Some("plan"),
        });
    }
    if let Some(p) = &input.plugin_slug {
        ws::validate_slug("plugin_slug", p)?;
    }
    let owner_email = validate_email(&input.owner_email)?;

    let id = WorkspaceId::new();
    let modules: Vec<String> = ws::MODULES.iter().map(|m| (*m).to_owned()).collect();
    let mut tx = core_db::begin(&state.db, Some(id), created_by).await?;
    let inserted = core_db::workspaces::insert(
        &mut tx,
        core_db::workspaces::NewWorkspace {
            id,
            slug: &input.slug,
            name: &name,
            plan: &plan,
            perspective: input.perspective.unwrap_or(Perspective::Sells),
            country: &input.country,
            default_currency: &input.default_currency,
            default_timezone: &input.default_timezone,
            locale: &locale,
            enabled_modules: &modules,
            plugin_slug: input.plugin_slug.as_deref(),
            created_by,
        },
    )
    .await;
    if let Err(err) = inserted {
        return Err(if err.is_unique_violation(Some("workspaces_slug_key")) {
            ApiError::conflict(
                "slug_taken",
                format!("the slug `{}` is already in use", input.slug),
            )
        } else {
            err.into()
        });
    }

    let ctx = Ctx {
        workspace_id: id,
        actor: match created_by {
            Some(user_id) => Actor::User {
                user_id,
                impersonator: Some(user_id),
            },
            None => Actor::System,
        },
        role: None,
        permissions: Permission::ALL.iter().copied().collect(),
        meta,
    };
    let workspace = core_db::workspaces::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        &ctx,
        Change {
            topic: "workspace.created",
            resource: "workspace",
            object_id: Some(id.0),
            before: None,
            after: Some(workspace_audit_view(&workspace)),
            event_extra: None,
        },
    )
    .await?;
    let (owner_invitation_id, owner_invitation_url) = invite_in_tx(
        state,
        &mut tx,
        &ctx,
        &owner_email,
        Role::Owner,
        &workspace.name,
    )
    .await?;
    tx.commit().await?;
    Ok(CreatedWorkspace {
        workspace,
        owner_invitation_id,
        owner_invitation_url,
    })
}

fn workspace_audit_view(w: &core_db::workspaces::Workspace) -> Value {
    json!({
        "slug": w.slug, "name": w.name, "plan": w.plan, "status": w.status, "perspective": w.perspective,
        "country": w.country, "default_currency": w.default_currency, "default_timezone": w.default_timezone,
        "locale": w.locale, "enabled_modules": w.enabled_modules, "branding": w.branding,
        "billing_contact": w.billing_contact, "plugin_slug": w.plugin_slug,
        "require_staff_2fa": w.require_staff_2fa, "hold_days": w.hold_days,
    })
}

pub async fn get_workspace(
    state: &AppState,
    ctx: &Ctx,
) -> ApiResult<core_db::workspaces::Workspace> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let workspace = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(workspace)
}

#[derive(Debug, Default)]
pub struct WorkspacePatch {
    pub name: Option<String>,
    pub perspective: Option<Perspective>,
    pub country: Option<String>,
    pub default_currency: Option<String>,
    pub default_timezone: Option<String>,
    pub locale: Option<String>,
    pub enabled_modules: Option<Vec<String>>,
    pub branding: Option<Value>,
    pub billing_contact: Option<Value>,
    pub require_staff_2fa: Option<bool>,
    pub hold_days: Option<i32>,
}

pub async fn update_workspace(
    state: &AppState,
    ctx: &Ctx,
    patch: WorkspacePatch,
    if_match: Option<i64>,
) -> ApiResult<core_db::workspaces::Workspace> {
    ctx.authorize(Permission::WorkspaceUpdate)?;
    if patch.billing_contact.is_some() {
        ctx.authorize(Permission::BillingWrite)?;
    }
    let mut tx = begin(state, ctx).await?;
    let before = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let expected = if_match.unwrap_or(before.version);
    if expected != before.version {
        return Err(ApiError::version_conflict());
    }

    let name = match &patch.name {
        Some(n) => ws::validate_name("name", n)?,
        None => before.name.clone(),
    };
    let country = patch
        .country
        .clone()
        .unwrap_or_else(|| before.country.clone());
    ws::validate_country(&country)?;
    let currency = patch
        .default_currency
        .clone()
        .unwrap_or_else(|| before.default_currency.clone());
    ws::validate_currency(&currency)?;
    let timezone = patch
        .default_timezone
        .clone()
        .unwrap_or_else(|| before.default_timezone.clone());
    ws::validate_timezone(&timezone)?;
    let locale = patch
        .locale
        .clone()
        .unwrap_or_else(|| before.locale.clone());
    ws::validate_locale(&locale)?;
    let modules = patch
        .enabled_modules
        .clone()
        .unwrap_or_else(|| before.enabled_modules.clone());
    ws::validate_modules(&modules)?;
    let branding = patch
        .branding
        .clone()
        .unwrap_or_else(|| before.branding.clone());
    let billing = patch
        .billing_contact
        .clone()
        .unwrap_or_else(|| before.billing_contact.clone());
    for (field, value) in [("branding", &branding), ("billing_contact", &billing)] {
        if !value.is_object() {
            return Err(ApiError::BadRequest {
                code: "invalid_input",
                message: "must be an object".into(),
                field: Some(field),
            });
        }
        if value.to_string().len() > 16 * 1024 {
            return Err(ApiError::BadRequest {
                code: "invalid_input",
                message: "must be under 16 KiB".into(),
                field: Some(field),
            });
        }
    }

    let hold_days = patch.hold_days.unwrap_or(before.hold_days);
    if !(0..=365).contains(&hold_days) {
        return Err(ApiError::invalid(
            "hold_days",
            "must be 0 (never) to 365 days",
        ));
    }

    let saved = core_db::workspaces::update_settings(
        &mut tx,
        ctx.workspace_id,
        core_db::workspaces::Settings {
            name: &name,
            perspective: patch.perspective.unwrap_or(before.perspective),
            country: &country,
            default_currency: &currency,
            default_timezone: &timezone,
            locale: &locale,
            enabled_modules: &modules,
            branding: &branding,
            billing_contact: &billing,
            require_staff_2fa: patch.require_staff_2fa.unwrap_or(before.require_staff_2fa),
            hold_days,
        },
        expected,
        ctx.user_id(),
    )
    .await?;
    if !saved {
        return Err(ApiError::version_conflict());
    }
    let after = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "workspace.updated",
            resource: "workspace",
            object_id: Some(ctx.workspace_id.0),
            before: Some(workspace_audit_view(&before)),
            after: Some(workspace_audit_view(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// Closes the workspace. Data is kept (nothing is hard-deleted); members lose access.
pub async fn close_workspace(state: &AppState, ctx: &Ctx, confirm_slug: &str) -> ApiResult<()> {
    ctx.authorize(Permission::WorkspaceClose)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if before.slug != confirm_slug {
        return Err(ApiError::BadRequest {
            code: "confirmation_mismatch",
            message: "type the workspace's slug to confirm".into(),
            field: Some("confirm_slug"),
        });
    }
    core_db::workspaces::set_status(
        &mut tx,
        ctx.workspace_id,
        WorkspaceStatus::Closed,
        ctx.user_id(),
    )
    .await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "workspace.closed",
            resource: "workspace",
            object_id: Some(ctx.workspace_id.0),
            before: Some(json!({ "status": before.status })),
            after: Some(json!({ "status": WorkspaceStatus::Closed })),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn role_permissions(
    state: &AppState,
    ctx: &Ctx,
) -> ApiResult<BTreeMap<Role, Vec<Permission>>> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let mut map = BTreeMap::new();
    for role in Role::ALL {
        let mut perms = core_db::workspaces::role_permissions(&mut tx, role).await?;
        perms.sort();
        map.insert(role, perms);
    }
    tx.commit().await?;
    Ok(map)
}

// Members ---------------------------------------------------------------------------------

pub async fn list_members(state: &AppState, ctx: &Ctx) -> ApiResult<Vec<core_db::members::Member>> {
    ctx.authorize(Permission::MemberRead)?;
    let mut tx = begin(state, ctx).await?;
    let members = core_db::members::list(&mut tx).await?;
    tx.commit().await?;
    Ok(members)
}

pub async fn get_member(
    state: &AppState,
    ctx: &Ctx,
    user: UserId,
) -> ApiResult<core_db::members::Member> {
    ctx.authorize(Permission::MemberRead)?;
    let mut tx = begin(state, ctx).await?;
    let member = core_db::members::get(&mut tx, user)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(member)
}

/// Roles that can be given through invitations or role changes in M1.
/// Talent memberships arrive with the talent portal (M7), tied to a talent record.
fn assignable(role: Role) -> ApiResult<()> {
    if role == Role::Talent {
        return Err(ApiError::BadRequest {
            code: "role_not_assignable",
            message: "talent access is granted from a talent record, which arrives with the talent portal".into(),
            field: Some("role"),
        });
    }
    Ok(())
}

pub async fn change_role(
    state: &AppState,
    ctx: &Ctx,
    user: UserId,
    role: Role,
    if_match: Option<i64>,
) -> ApiResult<core_db::members::Member> {
    ctx.authorize(Permission::MemberUpdate)?;
    assignable(role)?;
    let mut tx = begin(state, ctx).await?;
    let owners = core_db::members::lock_owner_count(&mut tx).await?;
    let before = core_db::members::get(&mut tx, user)
        .await?
        .ok_or(ApiError::NotFound)?;
    if (before.role == Role::Owner || role == Role::Owner) && !acts_as_owner(ctx) {
        return Err(ApiError::forbidden(
            "owner_required",
            "only an owner can give or take away the owner role",
        ));
    }
    if before.role == Role::Owner && role != Role::Owner && owners <= 1 {
        return Err(ApiError::conflict(
            "last_owner",
            "a workspace must keep at least one owner",
        ));
    }
    if before.role == role {
        tx.commit().await?;
        return Ok(before);
    }
    if !core_db::members::set_role(
        &mut tx,
        user,
        role,
        if_match.unwrap_or(before.version),
        ctx.user_id(),
    )
    .await?
    {
        return Err(ApiError::version_conflict());
    }
    let after = core_db::members::get(&mut tx, user)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "member.role_changed",
            resource: "member",
            object_id: Some(user.0),
            before: Some(json!({ "role": before.role })),
            after: Some(json!({ "role": after.role })),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

pub async fn remove_member(state: &AppState, ctx: &Ctx, user: UserId) -> ApiResult<()> {
    ctx.authorize(Permission::MemberRemove)?;
    let mut tx = begin(state, ctx).await?;
    let owners = core_db::members::lock_owner_count(&mut tx).await?;
    let before = core_db::members::get(&mut tx, user)
        .await?
        .ok_or(ApiError::NotFound)?;
    if before.role == Role::Owner {
        if !acts_as_owner(ctx) {
            return Err(ApiError::forbidden(
                "owner_required",
                "only an owner can remove an owner",
            ));
        }
        if owners <= 1 {
            return Err(ApiError::conflict(
                "last_owner",
                "a workspace must keep at least one owner",
            ));
        }
    }
    core_db::members::soft_delete(&mut tx, user, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "member.removed",
            resource: "member",
            object_id: Some(user.0),
            before: Some(json!({ "role": before.role, "email": before.email })),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// Invitations --------------------------------------------------------------------------

fn validate_email(email: &str) -> ApiResult<String> {
    let email = email.trim();
    let valid = (3..=320).contains(&email.len())
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        })
        && !email.chars().any(char::is_whitespace);
    if valid {
        Ok(email.to_owned())
    } else {
        Err(ApiError::BadRequest {
            code: "invalid_input",
            message: "enter a valid email address".into(),
            field: Some("email"),
        })
    }
}

async fn invite_in_tx(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    email: &str,
    role: Role,
    workspace_name: &str,
) -> ApiResult<(Uuid, String)> {
    let token = new_token();
    let expires_at = Utc::now()
        + chrono::Duration::from_std(state.config.invitation_ttl)
            .unwrap_or(chrono::Duration::days(7));
    let id = core_db::invitations::insert(
        tx,
        core_db::invitations::NewInvitation {
            workspace: ctx.workspace_id,
            email,
            role,
            token_hash: &hash_token(&token),
            expires_at,
            invited_by: ctx.user_id(),
        },
    )
    .await?;
    let url = state
        .config
        .app_url("/invitations/accept", &[("token", &token)]);
    record_with_secret(
        tx,
        ctx,
        Change {
            topic: "invitation.created",
            resource: "invitation",
            object_id: Some(id),
            before: None,
            after: Some(json!({ "email": email, "role": role, "expires_at": expires_at })),
            event_extra: Some(json!({ "workspace_name": workspace_name })),
        },
        &state.config.keyring,
        // Only the mailer may see the link: not the audit log, the event stream or webhooks.
        &json!({ "url": url }),
    )
    .await?;
    Ok((id, url))
}

pub async fn invite(
    state: &AppState,
    ctx: &Ctx,
    email: &str,
    role: Role,
) -> ApiResult<core_db::invitations::Invitation> {
    ctx.authorize(Permission::MemberInvite)?;
    assignable(role)?;
    if role == Role::Owner && !acts_as_owner(ctx) {
        return Err(ApiError::forbidden(
            "owner_required",
            "only an owner can invite another owner",
        ));
    }
    let email = validate_email(email)?;
    let mut tx = begin(state, ctx).await?;
    let already = core_db::members::list(&mut tx)
        .await?
        .into_iter()
        .any(|m| m.email.eq_ignore_ascii_case(&email));
    if already {
        return Err(ApiError::conflict(
            "already_member",
            format!("{email} is already a member"),
        ));
    }
    let workspace = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let (id, _url) = invite_in_tx(state, &mut tx, ctx, &email, role, &workspace.name).await?;
    let invitation = core_db::invitations::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(invitation)
}

pub async fn list_invitations(
    state: &AppState,
    ctx: &Ctx,
) -> ApiResult<Vec<core_db::invitations::Invitation>> {
    ctx.authorize(Permission::MemberRead)?;
    let mut tx = begin(state, ctx).await?;
    let list = core_db::invitations::list(&mut tx).await?;
    tx.commit().await?;
    Ok(list)
}

pub async fn revoke_invitation(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::MemberInvite)?;
    let mut tx = begin(state, ctx).await?;
    let inv = core_db::invitations::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !core_db::invitations::revoke(&mut tx, id).await? {
        return Err(ApiError::conflict(
            "invitation_closed",
            "that invitation was already accepted or revoked",
        ));
    }
    record(
        &mut tx,
        ctx,
        Change {
            topic: "invitation.revoked",
            resource: "invitation",
            object_id: Some(id),
            before: Some(json!({ "email": inv.email, "role": inv.role })),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// API keys ------------------------------------------------------------------------------

pub struct CreatedApiKey {
    pub key: core_db::api_keys::ApiKey,
    /// The full key, shown once.
    pub secret: String,
}

pub async fn create_api_key(
    state: &AppState,
    ctx: &Ctx,
    name: &str,
    permissions: &[Permission],
    expires_at: Option<DateTime<Utc>>,
) -> ApiResult<CreatedApiKey> {
    ctx.authorize(Permission::ApiKeyWrite)?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ApiError::BadRequest {
            code: "invalid_input",
            message: "must be 1 to 100 characters".into(),
            field: Some("name"),
        });
    }
    if permissions.is_empty() {
        return Err(ApiError::BadRequest {
            code: "invalid_input",
            message: "grant at least one permission".into(),
            field: Some("permissions"),
        });
    }
    // A key can never do more than the person who made it.
    if let Some(p) = permissions.iter().find(|p| !ctx.permissions.contains(p)) {
        return Err(ApiError::forbidden(
            "permission_denied",
            format!("you cannot grant `{p}`, which you don't have"),
        ));
    }
    if expires_at.is_some_and(|e| e <= Utc::now()) {
        return Err(ApiError::BadRequest {
            code: "invalid_input",
            message: "must be in the future".into(),
            field: Some("expires_at"),
        });
    }
    let mut perms: Vec<String> = permissions.iter().map(|p| p.as_str().to_owned()).collect();
    perms.sort();
    perms.dedup();

    let id = core_domain::ids::new_id();
    let secret = new_token();
    let mut tx = begin(state, ctx).await?;
    core_db::api_keys::insert(
        &mut tx,
        core_db::api_keys::NewApiKey {
            id,
            workspace: ctx.workspace_id,
            name,
            secret_hash: &hash_token(&secret),
            permissions: &perms,
            created_by: ctx.user_id(),
            expires_at,
        },
    )
    .await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "api_key.created",
            resource: "api_key",
            object_id: Some(id),
            before: None,
            after: Some(json!({ "name": name, "permissions": perms, "expires_at": expires_at })),
            event_extra: None,
        },
    )
    .await?;
    let key = core_db::api_keys::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(CreatedApiKey {
        key,
        secret: format_api_key(id, &secret),
    })
}

pub async fn list_api_keys(
    state: &AppState,
    ctx: &Ctx,
) -> ApiResult<Vec<core_db::api_keys::ApiKey>> {
    ctx.authorize(Permission::ApiKeyRead)?;
    let mut tx = begin(state, ctx).await?;
    let keys = core_db::api_keys::list(&mut tx).await?;
    tx.commit().await?;
    Ok(keys)
}

pub async fn revoke_api_key(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::ApiKeyWrite)?;
    let mut tx = begin(state, ctx).await?;
    let key = core_db::api_keys::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !core_db::api_keys::revoke(&mut tx, id).await? {
        return Err(ApiError::conflict(
            "already_revoked",
            "that key was already revoked",
        ));
    }
    record(
        &mut tx,
        ctx,
        Change {
            topic: "api_key.revoked",
            resource: "api_key",
            object_id: Some(id),
            before: Some(json!({ "name": key.name })),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// Audit log ---------------------------------------------------------------------------

pub async fn audit_page(
    state: &AppState,
    ctx: &Ctx,
    before: Option<Uuid>,
    limit: i64,
) -> ApiResult<Vec<core_db::audit::Row>> {
    ctx.authorize(Permission::AuditRead)?;
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::audit::list(&mut tx, before, limit.clamp(1, 500)).await?;
    tx.commit().await?;
    Ok(rows)
}

/// The audit log as CSV, newest first, up to `max_rows`.
pub async fn audit_csv(state: &AppState, ctx: &Ctx, max_rows: usize) -> ApiResult<Vec<u8>> {
    ctx.authorize(Permission::AuditRead)?;
    let mut tx = begin(state, ctx).await?;
    let mut out = csv::Writer::from_writer(Vec::new());
    out.write_record([
        "at",
        "id",
        "action",
        "resource",
        "object_id",
        "actor_user_id",
        "actor_api_key_id",
        "impersonator_id",
        "actor_rule_id",
        "ip",
        "user_agent",
        "request_id",
        "before",
        "after",
    ])
    .map_err(ApiError::internal)?;
    let mut cursor = None;
    let mut written = 0;
    while written < max_rows {
        let page = core_db::audit::list(&mut tx, cursor, 1000).await?;
        let Some(last) = page.last() else { break };
        cursor = Some(last.id);
        for row in &page {
            let opt = |v: Option<String>| csv_safe(v.unwrap_or_default());
            out.write_record([
                row.at.to_rfc3339(),
                row.id.to_string(),
                csv_safe(row.action.clone()),
                csv_safe(row.resource.clone()),
                opt(row.object_id.map(|v| v.to_string())),
                opt(row.actor_user_id.map(|v| v.to_string())),
                opt(row.actor_api_key_id.map(|v| v.to_string())),
                opt(row.impersonator_id.map(|v| v.to_string())),
                opt(row.actor_rule_id.map(|v| v.to_string())),
                opt(row.ip.map(|v| v.to_string())),
                opt(row.user_agent.clone()),
                opt(row.request_id.clone()),
                opt(row.before.as_ref().map(Value::to_string)),
                opt(row.after.as_ref().map(Value::to_string)),
            ])
            .map_err(ApiError::internal)?;
            written += 1;
            if written >= max_rows {
                break;
            }
        }
    }
    tx.commit().await?;
    out.into_inner().map_err(ApiError::internal)
}

/// Neutralises spreadsheet formula injection: a cell starting with `=`, `+`,
/// `-`, `@`, tab or carriage return is prefixed with `'` (OWASP guidance).
fn csv_safe(value: String) -> String {
    if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_cells_cannot_start_formulas() {
        assert_eq!(csv_safe("=HYPERLINK(\"x\")".into()), "'=HYPERLINK(\"x\")");
        assert_eq!(csv_safe("@SUM(A1)".into()), "'@SUM(A1)");
        assert_eq!(csv_safe("Mozilla/5.0".into()), "Mozilla/5.0");
    }

    #[test]
    fn emails() {
        for ok in ["a@b.co", "  first.last+tag@sub.example.com "] {
            assert!(validate_email(ok).is_ok(), "{ok}");
        }
        for bad in ["", "a", "a@b", "@b.co", "a@.co", "a b@c.co", "a@b.co."] {
            assert!(validate_email(bad).is_err(), "{bad}");
        }
    }
}
