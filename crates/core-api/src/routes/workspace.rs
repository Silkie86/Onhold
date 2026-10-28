//! Routes that act inside one workspace: settings, members, invitations, API
//! keys and the audit log. Every handler takes the workspace context and
//! passes it to the service, which authorizes.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use core_domain::workspace::{Perspective, WorkspaceStatus};
use core_domain::{Permission, Role, UserId, WorkspaceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{IdempotencyHeader, IfMatchHeader, WorkspaceHeader, with_etag};
use crate::AppState;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::extract::{IfMatch, WsCtx};
use crate::services::signatures::{self, SignatureBody, Signatures};
use crate::services::tenancy as svc;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_workspace, update_workspace))
        .routes(routes!(close_workspace))
        .routes(routes!(get_signatures))
        .routes(routes!(set_my_signature))
        .routes(routes!(set_workspace_signature))
        .routes(routes!(roles))
        .routes(routes!(list_members))
        .routes(routes!(get_member, update_member, remove_member))
        .routes(routes!(list_invitations, create_invitation))
        .routes(routes!(revoke_invitation))
        .routes(routes!(list_api_keys, create_api_key))
        .routes(routes!(revoke_api_key))
        .routes(routes!(audit))
        .routes(routes!(audit_export))
        .routes(routes!(public_logo))
}

/// A workspace's logo for emails; see [`svc::public_logo`].
#[utoipa::path(get, path = "/public/workspaces/{id}/logo", tag = "public", params(("id" = Uuid, Path)),
    responses((status = 200, content_type = "image/*", body = Vec<u8>), (status = 404, body = ErrorBody)))]
async fn public_logo(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Response> {
    let (content_type, bytes) = svc::public_logo(&state, WorkspaceId(id)).await?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            // The address carries the logo's version, so it can be kept a long time.
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=2592000, immutable"),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("default-src 'none'; sandbox"),
            ),
        ],
        bytes,
    )
        .into_response())
}

// Workspace ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct WorkspaceView {
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
    #[schema(value_type = Object)]
    pub branding: Value,
    /// Only shown to roles with `billing:read`.
    #[schema(value_type = Option<Object>)]
    pub billing_contact: Option<Value>,
    pub plugin_slug: Option<String>,
    pub require_staff_2fa: bool,
    /// How many days a new hold lasts before it lapses; 0 means never.
    pub hold_days: i32,
    /// Send back as `If-Match` when saving to avoid overwriting someone else's change.
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn workspace_view(w: core_db::workspaces::Workspace, show_billing: bool) -> WorkspaceView {
    WorkspaceView {
        id: w.id,
        slug: w.slug,
        name: w.name,
        plan: w.plan,
        status: w.status,
        perspective: w.perspective,
        country: w.country.trim().to_owned(),
        default_currency: w.default_currency.trim().to_owned(),
        default_timezone: w.default_timezone,
        locale: w.locale,
        enabled_modules: w.enabled_modules,
        branding: w.branding,
        billing_contact: show_billing.then_some(w.billing_contact),
        plugin_slug: w.plugin_slug,
        require_staff_2fa: w.require_staff_2fa,
        hold_days: w.hold_days,
        version: w.version,
        created_at: w.created_at,
        updated_at: w.updated_at,
    }
}

/// The current workspace's settings.
#[utoipa::path(get, path = "/workspace", tag = "workspace", params(WorkspaceHeader),
    responses((status = 200, body = WorkspaceView), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_workspace(State(state): State<AppState>, WsCtx(ctx): WsCtx) -> ApiResult<Response> {
    let w = svc::get_workspace(&state, &ctx).await?;
    let show = ctx.authorize(Permission::BillingRead).is_ok();
    Ok(with_etag(w.version, workspace_view(w, show)))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct WorkspacePatchRequest {
    pub name: Option<String>,
    pub perspective: Option<Perspective>,
    pub country: Option<String>,
    pub default_currency: Option<String>,
    pub default_timezone: Option<String>,
    pub locale: Option<String>,
    pub enabled_modules: Option<Vec<String>>,
    #[schema(value_type = Option<Object>)]
    pub branding: Option<Value>,
    /// Needs `billing:write`.
    #[schema(value_type = Option<Object>)]
    pub billing_contact: Option<Value>,
    pub require_staff_2fa: Option<bool>,
    /// 0 (holds never lapse) to 365.
    pub hold_days: Option<i32>,
}

/// Your email signature and the workspace's.
#[utoipa::path(get, path = "/signatures", tag = "workspace", params(WorkspaceHeader),
    responses((status = 200, body = Signatures), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_signatures(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Signatures>> {
    Ok(Json(signatures::get(&state, &ctx).await?))
}

/// Set your email signature in this workspace: under what you send from
/// the inbox, and the quotes, invoices and signing requests you send.
#[utoipa::path(put, path = "/signatures/mine", tag = "workspace", params(WorkspaceHeader),
    request_body = SignatureBody,
    responses((status = 200, body = Signatures), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn set_my_signature(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<SignatureBody>,
) -> ApiResult<Json<Signatures>> {
    Ok(Json(signatures::set_mine(&state, &ctx, b).await?))
}

/// Set the workspace's email signature: used when a member has none, and
/// under emails rules send.
#[utoipa::path(put, path = "/signatures/workspace", tag = "workspace", params(WorkspaceHeader),
    request_body = SignatureBody,
    responses((status = 200, body = Signatures), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn set_workspace_signature(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<SignatureBody>,
) -> ApiResult<Json<Signatures>> {
    Ok(Json(signatures::set_workspace(&state, &ctx, b).await?))
}

/// Change workspace settings. Only the fields sent are changed.
#[utoipa::path(patch, path = "/workspace", tag = "workspace", params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader),
    request_body = WorkspacePatchRequest,
    responses((status = 200, body = WorkspaceView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_workspace(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    IfMatch(if_match): IfMatch,
    Json(body): Json<WorkspacePatchRequest>,
) -> ApiResult<Response> {
    let patch = svc::WorkspacePatch {
        name: body.name,
        perspective: body.perspective,
        country: body.country,
        default_currency: body.default_currency,
        default_timezone: body.default_timezone,
        locale: body.locale,
        enabled_modules: body.enabled_modules,
        branding: body.branding,
        billing_contact: body.billing_contact,
        require_staff_2fa: body.require_staff_2fa,
        hold_days: body.hold_days,
    };
    let w = svc::update_workspace(&state, &ctx, patch, if_match).await?;
    let show = ctx.authorize(Permission::BillingRead).is_ok();
    Ok(with_etag(w.version, workspace_view(w, show)))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CloseWorkspaceRequest {
    /// The workspace's slug, typed again to confirm.
    pub confirm_slug: String,
}

/// Close the workspace. Data is kept; every member loses access.
#[utoipa::path(post, path = "/workspace/close", tag = "workspace", params(WorkspaceHeader), request_body = CloseWorkspaceRequest,
    responses((status = 204), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = [])))]
async fn close_workspace(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(body): Json<CloseWorkspaceRequest>,
) -> ApiResult<StatusCode> {
    svc::close_workspace(&state, &ctx, &body.confirm_slug).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RolesResponse {
    /// Permissions per role in this workspace.
    pub roles: BTreeMap<Role, Vec<Permission>>,
    /// The caller's own effective permissions.
    pub mine: Vec<Permission>,
}

/// Roles, what each may do, and what the caller may do (for showing or hiding UI).
#[utoipa::path(get, path = "/workspace/roles", tag = "workspace", params(WorkspaceHeader),
    responses((status = 200, body = RolesResponse)), security(("session" = []), ("api_key" = [])))]
async fn roles(State(state): State<AppState>, WsCtx(ctx): WsCtx) -> ApiResult<Json<RolesResponse>> {
    let roles = svc::role_permissions(&state, &ctx).await?;
    Ok(Json(RolesResponse {
        roles,
        mine: ctx.permissions.iter().copied().collect(),
    }))
}

// Members --------------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct MemberView {
    pub user_id: UserId,
    pub email: String,
    pub name: String,
    pub role: Role,
    pub has_2fa: bool,
    pub version: i64,
    pub joined_at: DateTime<Utc>,
}

impl From<core_db::members::Member> for MemberView {
    fn from(m: core_db::members::Member) -> Self {
        Self {
            user_id: m.user_id,
            email: m.email,
            name: m.name,
            role: m.role,
            has_2fa: m.has_2fa,
            version: m.version,
            joined_at: m.joined_at,
        }
    }
}

/// Everyone with access to this workspace.
#[utoipa::path(get, path = "/members", tag = "members", params(WorkspaceHeader),
    responses((status = 200, body = Vec<MemberView>), (status = 403, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn list_members(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Vec<MemberView>>> {
    Ok(Json(
        svc::list_members(&state, &ctx)
            .await?
            .into_iter()
            .map(MemberView::from)
            .collect(),
    ))
}

/// One member.
#[utoipa::path(get, path = "/members/{user_id}", tag = "members", params(WorkspaceHeader, ("user_id" = Uuid, Path)),
    responses((status = 200, body = MemberView), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn get_member(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(user_id): Path<Uuid>,
) -> ApiResult<Response> {
    let m = svc::get_member(&state, &ctx, UserId(user_id)).await?;
    Ok(with_etag(m.version, MemberView::from(m)))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMemberRequest {
    pub role: Role,
}

/// Change a member's role. Only owners can give or take away `owner`, and
/// the last owner cannot be demoted.
#[utoipa::path(patch, path = "/members/{user_id}", tag = "members",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("user_id" = Uuid, Path)), request_body = UpdateMemberRequest,
    responses((status = 200, body = MemberView), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_member(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    IfMatch(if_match): IfMatch,
    Path(user_id): Path<Uuid>,
    Json(body): Json<UpdateMemberRequest>,
) -> ApiResult<Response> {
    let m = svc::change_role(&state, &ctx, UserId(user_id), body.role, if_match).await?;
    Ok(with_etag(m.version, MemberView::from(m)))
}

/// Remove a member's access. Their past work stays attributed to them.
#[utoipa::path(delete, path = "/members/{user_id}", tag = "members", params(WorkspaceHeader, IdempotencyHeader, ("user_id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn remove_member(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(user_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    svc::remove_member(&state, &ctx, UserId(user_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Invitations ----------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct InvitationView {
    pub id: Uuid,
    pub email: String,
    pub role: Role,
    pub invited_by: Option<UserId>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl From<core_db::invitations::Invitation> for InvitationView {
    fn from(i: core_db::invitations::Invitation) -> Self {
        Self {
            id: i.id,
            email: i.email,
            role: i.role,
            invited_by: i.invited_by,
            created_at: i.created_at,
            expires_at: i.expires_at,
            accepted_at: i.accepted_at,
            revoked_at: i.revoked_at,
        }
    }
}

/// Invitations sent from this workspace, newest first.
#[utoipa::path(get, path = "/invitations", tag = "invitations", params(WorkspaceHeader),
    responses((status = 200, body = Vec<InvitationView>), (status = 403, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn list_invitations(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Vec<InvitationView>>> {
    Ok(Json(
        svc::list_invitations(&state, &ctx)
            .await?
            .into_iter()
            .map(InvitationView::from)
            .collect(),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateInvitationRequest {
    pub email: String,
    pub role: Role,
}

/// Invite someone by email. The link is emailed to them (delivery starts in M2).
#[utoipa::path(post, path = "/invitations", tag = "invitations", params(WorkspaceHeader, IdempotencyHeader), request_body = CreateInvitationRequest,
    responses((status = 201, body = InvitationView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_invitation(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(body): Json<CreateInvitationRequest>,
) -> ApiResult<(StatusCode, Json<InvitationView>)> {
    let inv = svc::invite(&state, &ctx, &body.email, body.role).await?;
    Ok((StatusCode::CREATED, Json(InvitationView::from(inv))))
}

/// Withdraw an invitation that hasn't been accepted.
#[utoipa::path(delete, path = "/invitations/{id}", tag = "invitations", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn revoke_invitation(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    svc::revoke_invitation(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// API keys ------------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyView {
    pub id: Uuid,
    pub name: String,
    pub permissions: Vec<String>,
    pub created_by: Option<UserId>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl From<core_db::api_keys::ApiKey> for ApiKeyView {
    fn from(k: core_db::api_keys::ApiKey) -> Self {
        Self {
            id: k.id,
            name: k.name,
            permissions: k.permissions,
            created_by: k.created_by,
            created_at: k.created_at,
            expires_at: k.expires_at,
            last_used_at: k.last_used_at,
            revoked_at: k.revoked_at,
        }
    }
}

/// This workspace's API keys (never their secrets).
#[utoipa::path(get, path = "/api-keys", tag = "api-keys", params(WorkspaceHeader),
    responses((status = 200, body = Vec<ApiKeyView>), (status = 403, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn list_api_keys(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Vec<ApiKeyView>>> {
    Ok(Json(
        svc::list_api_keys(&state, &ctx)
            .await?
            .into_iter()
            .map(ApiKeyView::from)
            .collect(),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateApiKeyRequest {
    pub name: String,
    /// What the key may do; at most what you can do yourself.
    pub permissions: Vec<Permission>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedApiKeyResponse {
    pub key: ApiKeyView,
    /// The key itself. Shown once; store it somewhere safe.
    pub secret: String,
}

/// Create an API key.
#[utoipa::path(post, path = "/api-keys", tag = "api-keys", params(WorkspaceHeader, IdempotencyHeader), request_body = CreateApiKeyRequest,
    responses((status = 201, body = CreatedApiKeyResponse), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_api_key(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(body): Json<CreateApiKeyRequest>,
) -> ApiResult<(StatusCode, Json<CreatedApiKeyResponse>)> {
    let created =
        svc::create_api_key(&state, &ctx, &body.name, &body.permissions, body.expires_at).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedApiKeyResponse {
            key: ApiKeyView::from(created.key),
            secret: created.secret,
        }),
    ))
}

/// Revoke an API key. Requests using it fail immediately.
#[utoipa::path(delete, path = "/api-keys/{id}", tag = "api-keys", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn revoke_api_key(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    svc::revoke_api_key(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Audit log ----------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct AuditQuery {
    /// Return entries older than this entry id (the previous page's `next_before`).
    pub before: Option<Uuid>,
    /// 1 to 500, default 100.
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditEntryView {
    pub id: Uuid,
    pub at: DateTime<Utc>,
    pub action: String,
    pub resource: String,
    pub object_id: Option<Uuid>,
    pub actor_user_id: Option<UserId>,
    pub actor_api_key_id: Option<Uuid>,
    /// Set when an OnHold operator made the change while supporting this workspace.
    pub impersonator_id: Option<UserId>,
    /// Set when an automation rule made the change.
    pub actor_rule_id: Option<Uuid>,
    #[schema(value_type = Option<Object>)]
    pub before: Option<Value>,
    #[schema(value_type = Option<Object>)]
    pub after: Option<Value>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub request_id: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditPage {
    pub entries: Vec<AuditEntryView>,
    /// Pass as `before` for the next page; absent on the last page.
    pub next_before: Option<Uuid>,
}

/// Who changed what, newest first.
#[utoipa::path(get, path = "/audit", tag = "audit", params(WorkspaceHeader, AuditQuery),
    responses((status = 200, body = AuditPage), (status = 403, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn audit(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<AuditPage>> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let rows = svc::audit_page(&state, &ctx, q.before, limit).await?;
    let next_before = (rows.len() as i64 == limit)
        .then(|| rows.last().map(|r| r.id))
        .flatten();
    Ok(Json(AuditPage {
        entries: rows
            .into_iter()
            .map(|r| AuditEntryView {
                id: r.id,
                at: r.at,
                action: r.action,
                resource: r.resource,
                object_id: r.object_id,
                actor_user_id: r.actor_user_id,
                actor_api_key_id: r.actor_api_key_id,
                impersonator_id: r.impersonator_id,
                actor_rule_id: r.actor_rule_id,
                before: r.before,
                after: r.after,
                ip: r.ip.map(|ip| ip.to_string()),
                user_agent: r.user_agent,
                request_id: r.request_id,
            })
            .collect(),
        next_before,
    }))
}

const AUDIT_EXPORT_MAX_ROWS: usize = 100_000;

/// The audit log as CSV (newest first, up to 100,000 rows).
#[utoipa::path(get, path = "/audit/export", tag = "audit", params(WorkspaceHeader),
    responses((status = 200, content_type = "text/csv", body = String), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn audit_export(State(state): State<AppState>, WsCtx(ctx): WsCtx) -> ApiResult<Response> {
    let csv = svc::audit_csv(&state, &ctx, AUDIT_EXPORT_MAX_ROWS).await?;
    let filename = format!(
        "attachment; filename=\"audit-{}.csv\"",
        Utc::now().format("%Y%m%d-%H%M%S")
    );
    let disposition = HeaderValue::from_str(&filename).map_err(ApiError::internal)?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/csv; charset=utf-8"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        csv,
    )
        .into_response())
}
