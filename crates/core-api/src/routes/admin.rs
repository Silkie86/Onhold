//! Operator routes (superadmin with two-factor authentication only).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use core_db::jobs::JobStatus;
use core_domain::WorkspaceId;
use core_domain::workspace::{Perspective, WorkspaceStatus};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::auth::ImpersonationView;
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::CurrentSession;
use crate::services::{admin as svc, setup, tenancy};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_workspaces, create_workspace))
        .routes(routes!(start_impersonation, end_impersonation))
        .routes(routes!(job_summary))
        .routes(routes!(list_setup_requests))
        .routes(routes!(resolve_setup_request))
        .routes(routes!(get_price_list, set_price_list))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminWorkspaceView {
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
    /// The setup form's answers; absent until the client sends it.
    #[schema(value_type = Option<Object>)]
    pub setup: Option<serde_json::Value>,
    pub setup_at: Option<DateTime<Utc>>,
    /// Comments from the setup form not yet marked done.
    pub open_requests: i64,
}

/// Every workspace on the platform.
#[utoipa::path(get, path = "/admin/workspaces", tag = "admin",
    responses((status = 200, body = Vec<AdminWorkspaceView>), (status = 403, body = ErrorBody)), security(("session" = [])))]
async fn list_workspaces(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
) -> ApiResult<Json<Vec<AdminWorkspaceView>>> {
    let list = svc::list_workspaces(&state, &session).await?;
    Ok(Json(
        list.into_iter()
            .map(|w| AdminWorkspaceView {
                id: w.id,
                slug: w.slug,
                name: w.name,
                plan: w.plan,
                status: w.status,
                created_at: w.created_at,
                member_count: w.member_count,
                presets: w.presets,
                enabled_modules: w.enabled_modules,
                extras: w.extras,
                setup: w.setup,
                setup_at: w.setup_at,
                open_requests: w.open_requests,
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateWorkspaceRequest {
    /// Lowercase letters, digits and inner hyphens; permanent.
    pub slug: String,
    pub name: String,
    /// ISO 3166-1 alpha-2, e.g. `GB`.
    pub country: String,
    /// ISO 4217, e.g. `GBP`.
    pub default_currency: String,
    /// IANA, e.g. `Europe/London`.
    pub default_timezone: String,
    /// BCP 47; defaults to `en-<country>`.
    pub locale: Option<String>,
    pub plan: Option<String>,
    pub perspective: Option<Perspective>,
    pub plugin_slug: Option<String>,
    /// Who receives the owner invitation.
    pub owner_email: String,
    /// Presets to combine, e.g. `["solo", "promoter"]`.
    #[serde(default)]
    pub presets: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedWorkspaceResponse {
    pub workspace_id: WorkspaceId,
    pub slug: String,
    pub owner_invitation_id: Uuid,
    /// The owner's join link. It is also emailed; it is shown here in case it needs passing on.
    pub owner_invitation_url: String,
}

/// Create a workspace and invite its owner.
#[utoipa::path(post, path = "/admin/workspaces", tag = "admin", request_body = CreateWorkspaceRequest,
    responses((status = 201, body = CreatedWorkspaceResponse), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = [])))]
async fn create_workspace(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
    Json(body): Json<CreateWorkspaceRequest>,
) -> ApiResult<(StatusCode, Json<CreatedWorkspaceResponse>)> {
    svc::require_superadmin(&state, &session).await?;
    let created = tenancy::create_workspace(
        &state,
        Some(session.user_id),
        session.meta.clone(),
        tenancy::NewWorkspace {
            slug: body.slug,
            name: body.name,
            country: body.country,
            default_currency: body.default_currency,
            default_timezone: body.default_timezone,
            locale: body.locale,
            plan: body.plan,
            perspective: body.perspective,
            plugin_slug: body.plugin_slug,
            owner_email: body.owner_email,
            presets: body.presets,
        },
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedWorkspaceResponse {
            workspace_id: created.workspace.id,
            slug: created.workspace.slug,
            owner_invitation_id: created.owner_invitation_id,
            owner_invitation_url: created.owner_invitation_url,
        }),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct StartImpersonationRequest {
    pub workspace_id: WorkspaceId,
    /// Recorded in the customer's audit log.
    pub reason: String,
}

/// Act inside a customer's workspace for support, for up to an hour. Every
/// change made meanwhile is flagged in their audit log.
#[utoipa::path(post, path = "/admin/impersonation", tag = "admin", request_body = StartImpersonationRequest,
    responses((status = 200, body = ImpersonationView), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = [])))]
async fn start_impersonation(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
    Json(body): Json<StartImpersonationRequest>,
) -> ApiResult<Json<ImpersonationView>> {
    let expires_at =
        svc::start_impersonation(&state, &session, body.workspace_id, &body.reason).await?;
    Ok(Json(ImpersonationView {
        workspace_id: body.workspace_id,
        expires_at,
    }))
}

/// Stop acting inside a customer's workspace.
#[utoipa::path(delete, path = "/admin/impersonation", tag = "admin", responses((status = 204)), security(("session" = [])))]
async fn end_impersonation(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
) -> ApiResult<StatusCode> {
    svc::end_impersonation(&state, &session).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobSummaryRow {
    /// Absent for platform jobs (such as sign-in emails).
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_slug: Option<String>,
    pub kind: String,
    pub status: JobStatus,
    pub jobs: i64,
    /// When the oldest of these was created.
    pub oldest: Option<DateTime<Utc>>,
}

/// Unfinished and dead jobs across the platform, by workspace, kind and status.
#[utoipa::path(get, path = "/admin/jobs", tag = "admin",
    responses((status = 200, body = Vec<JobSummaryRow>), (status = 403, body = ErrorBody)), security(("session" = [])))]
async fn job_summary(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
) -> ApiResult<Json<Vec<JobSummaryRow>>> {
    let rows = svc::job_summary(&state, &session).await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| JobSummaryRow {
                workspace_id: r.workspace_id,
                workspace_slug: r.workspace_slug,
                kind: r.kind,
                status: r.status,
                jobs: r.jobs,
                oldest: r.oldest,
            })
            .collect(),
    ))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminSetupRequestView {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub workspace_name: String,
    pub body: String,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// Requests and questions clients left in the setup form, open ones first.
#[utoipa::path(get, path = "/admin/setup-requests", tag = "admin",
    responses((status = 200, body = Vec<AdminSetupRequestView>), (status = 403, body = ErrorBody)), security(("session" = [])))]
async fn list_setup_requests(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
) -> ApiResult<Json<Vec<AdminSetupRequestView>>> {
    let rows = setup::admin_requests(&state, &session).await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| AdminSetupRequestView {
                id: r.id,
                workspace_id: r.workspace_id,
                workspace_name: r.workspace_name,
                body: r.body,
                author_name: r.author_name,
                author_email: r.author_email,
                created_at: r.created_at,
                resolved_at: r.resolved_at,
            })
            .collect(),
    ))
}

/// Mark a client's request dealt with.
#[utoipa::path(post, path = "/admin/setup-requests/{id}/resolve", tag = "admin", params(("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("session" = [])))]
async fn resolve_setup_request(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    setup::admin_resolve(&state, &session, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The price list the setup form's estimate uses (minor units a month).
#[utoipa::path(get, path = "/admin/price-list", tag = "admin",
    responses((status = 200, body = Object), (status = 403, body = ErrorBody)), security(("session" = [])))]
async fn get_price_list(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(setup::admin_price_list(&state, &session).await?))
}

/// Replace the price list: `currency`, `base`, `features` and `team` amounts in minor units a month.
#[utoipa::path(put, path = "/admin/price-list", tag = "admin", request_body = Object,
    responses((status = 204), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)), security(("session" = [])))]
async fn set_price_list(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<StatusCode> {
    setup::admin_set_price_list(&state, &session, &body).await?;
    Ok(StatusCode::NO_CONTENT)
}
