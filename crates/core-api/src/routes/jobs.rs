//! The workspace's background work: queued and finished jobs, retrying dead
//! ones, and the log of messages sent.

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Utc};
use core_db::jobs::JobStatus;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{IdempotencyHeader, PageQuery, WorkspaceHeader, next_before};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::WsCtx;
use crate::services::{jobs as svc, mail};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_jobs))
        .routes(routes!(get_job))
        .routes(routes!(retry_job))
        .routes(routes!(list_messages))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobView {
    pub id: Uuid,
    /// e.g. `mail.send`, `webhook.deliver`, `event.dispatch`.
    pub kind: String,
    #[schema(value_type = Object)]
    pub payload: Value,
    pub status: JobStatus,
    pub priority: i16,
    /// When it runs next (or ran last).
    pub run_at: DateTime<Utc>,
    pub attempts: i32,
    pub max_attempts: i32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl From<core_db::jobs::Job> for JobView {
    fn from(j: core_db::jobs::Job) -> Self {
        Self {
            id: j.id,
            kind: j.kind,
            payload: j.payload,
            status: j.status,
            priority: j.priority,
            run_at: j.run_at,
            attempts: j.attempts,
            max_attempts: j.max_attempts,
            last_error: j.last_error,
            created_at: j.created_at,
            finished_at: j.finished_at,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct JobListQuery {
    pub status: Option<JobStatus>,
    pub kind: Option<String>,
    pub before: Option<Uuid>,
    /// 1 to 200, default 50.
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobPage {
    pub jobs: Vec<JobView>,
    pub next_before: Option<Uuid>,
}

/// Background jobs, newest first.
#[utoipa::path(get, path = "/jobs", tag = "jobs", params(WorkspaceHeader, JobListQuery),
    responses((status = 200, body = JobPage), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_jobs(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<JobListQuery>,
) -> ApiResult<Json<JobPage>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let rows = svc::list(&state, &ctx, q.status, q.kind.as_deref(), q.before, limit).await?;
    let next_before = next_before(&rows, limit, |j| j.id);
    Ok(Json(JobPage {
        jobs: rows.into_iter().map(Into::into).collect(),
        next_before,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobRunView {
    pub attempt: i32,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub succeeded: bool,
    pub error: Option<String>,
    /// `[{"at", "message"}]` lines the handler wrote.
    #[schema(value_type = Vec<Object>)]
    pub log: Value,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobDetail {
    pub job: JobView,
    /// Every attempt, oldest first.
    pub runs: Vec<JobRunView>,
}

#[utoipa::path(get, path = "/jobs/{id}", tag = "jobs", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = JobDetail), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_job(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<JobDetail>> {
    let (job, runs) = svc::get(&state, &ctx, id).await?;
    Ok(Json(JobDetail {
        job: job.into(),
        runs: runs
            .into_iter()
            .map(|r| JobRunView {
                attempt: r.attempt,
                started_at: r.started_at,
                finished_at: r.finished_at,
                succeeded: r.succeeded,
                error: r.error,
                log: r.log,
            })
            .collect(),
    }))
}

/// Queue a dead job again with a fresh set of attempts.
#[utoipa::path(post, path = "/jobs/{id}/retry", tag = "jobs", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = JobView), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn retry_job(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<JobView>> {
    Ok(Json(svc::retry(&state, &ctx, id).await?.into()))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MessageView {
    pub id: Uuid,
    /// `email` for now.
    pub channel: String,
    pub to: String,
    pub subject: Option<String>,
    /// Sign-in and invitation links are replaced with `[link removed]`.
    pub body_html: Option<String>,
    pub body_text: String,
    pub template_key: Option<String>,
    pub rule_id: Option<Uuid>,
    pub job_id: Option<Uuid>,
    /// `sent` or `failed`.
    pub status: String,
    pub provider_message_id: Option<String>,
    pub error: Option<String>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MessagePage {
    pub messages: Vec<MessageView>,
    pub next_before: Option<Uuid>,
}

/// Messages the workspace sent, newest first.
#[utoipa::path(get, path = "/messages", tag = "messages", params(WorkspaceHeader, PageQuery),
    responses((status = 200, body = MessagePage), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_messages(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<MessagePage>> {
    let limit = q.limit();
    let rows = mail::list(&state, &ctx, q.before, limit).await?;
    let next_before = next_before(&rows, limit, |m| m.id);
    Ok(Json(MessagePage {
        messages: rows
            .into_iter()
            .map(|m| MessageView {
                id: m.id,
                channel: m.channel,
                to: m.to_address,
                subject: m.subject,
                body_html: m.body_html,
                body_text: m.body_text,
                template_key: m.template_key,
                rule_id: m.rule_id,
                job_id: m.job_id,
                status: m.status,
                provider_message_id: m.provider_message_id,
                error: m.error,
                at: m.created_at,
            })
            .collect(),
        next_before,
    }))
}
