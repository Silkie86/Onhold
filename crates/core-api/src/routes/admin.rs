//! Operator routes (superadmin with two-factor authentication only).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
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
use crate::services::{admin as svc, tenancy};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_workspaces, create_workspace))
        .routes(routes!(start_impersonation, end_impersonation))
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
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedWorkspaceResponse {
    pub workspace_id: WorkspaceId,
    pub slug: String,
    pub owner_invitation_id: Uuid,
    /// The owner's join link. Until email delivery lands (M2), pass it on yourself.
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
