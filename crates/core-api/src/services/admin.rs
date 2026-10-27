//! Operator (superadmin) actions. Every one needs a superadmin with TOTP on,
//! signed in with it; impersonation is recorded in the target workspace's
//! audit log and flagged on every row written while it lasts.

use chrono::Utc;
use core_domain::{Actor, Ctx, Permission, WorkspaceId};
use serde_json::json;

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::extract::Session;
use crate::services::{Change, record};

/// Confirms the session belongs to a superadmin with two-factor authentication on.
pub async fn require_superadmin(state: &AppState, session: &Session) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let user = core_db::users::get(&mut tx, session.user_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    tx.commit().await?;
    if !user.is_superadmin {
        return Err(ApiError::forbidden(
            "superadmin_required",
            "operator access only",
        ));
    }
    if user.totp_enabled_at.is_none() || !session.mfa_verified {
        return Err(ApiError::forbidden(
            "superadmin_requires_2fa",
            "operators must have two-factor authentication on and use it to sign in",
        ));
    }
    Ok(())
}

pub async fn list_workspaces(
    state: &AppState,
    session: &Session,
) -> ApiResult<Vec<core_db::workspaces::AdminListing>> {
    require_superadmin(state, session).await?;
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let list = core_db::workspaces::admin_list(&mut tx).await?;
    tx.commit().await?;
    Ok(list)
}

fn operator_ctx(session: &Session, workspace_id: WorkspaceId) -> Ctx {
    Ctx {
        workspace_id,
        actor: Actor::User {
            user_id: session.user_id,
            impersonator: Some(session.user_id),
        },
        role: None,
        permissions: Permission::ALL.iter().copied().collect(),
        meta: session.meta.clone(),
    }
}

pub async fn start_impersonation(
    state: &AppState,
    session: &Session,
    workspace_id: WorkspaceId,
    reason: &str,
) -> ApiResult<chrono::DateTime<Utc>> {
    require_superadmin(state, session).await?;
    let reason = reason.trim();
    if reason.len() < 3 || reason.chars().count() > 500 {
        return Err(ApiError::BadRequest {
            code: "invalid_input",
            message:
                "say why you need access (3 to 500 characters); it goes in the customer's audit log"
                    .into(),
            field: Some("reason"),
        });
    }
    let mut tx = core_db::begin(&state.db, Some(workspace_id), Some(session.user_id)).await?;
    core_db::workspaces::get(&mut tx, workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let until = Utc::now()
        + chrono::Duration::from_std(state.config.impersonation_ttl)
            .unwrap_or(chrono::Duration::hours(1));
    core_db::sessions::start_impersonation(&mut tx, session.id, workspace_id, until).await?;
    record(
        &mut tx,
        &operator_ctx(session, workspace_id),
        Change {
            topic: "impersonation.started",
            resource: "workspace",
            object_id: Some(workspace_id.0),
            before: None,
            after: Some(json!({ "reason": reason, "until": until })),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    tracing::warn!(user_id = %session.user_id, workspace_id = %workspace_id, "impersonation started");
    Ok(until)
}

pub async fn end_impersonation(state: &AppState, session: &Session) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let ended = core_db::sessions::end_impersonation(&mut tx, session.id).await?;
    if let Some(workspace_id) = ended {
        core_db::set_context(&mut tx, Some(workspace_id), Some(session.user_id)).await?;
        record(
            &mut tx,
            &operator_ctx(session, workspace_id),
            Change {
                topic: "impersonation.ended",
                resource: "workspace",
                object_id: Some(workspace_id.0),
                before: None,
                after: None,
                event_extra: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
