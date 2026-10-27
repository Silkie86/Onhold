//! The workspace's view of its job queue: what ran, what failed, and
//! retrying dead jobs.

use core_db::jobs::{Job, JobStatus, Run};
use core_domain::{Ctx, Permission};
use uuid::Uuid;

use super::{Change, begin, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub async fn list(
    state: &AppState,
    ctx: &Ctx,
    status: Option<JobStatus>,
    kind: Option<&str>,
    before: Option<Uuid>,
    limit: i64,
) -> ApiResult<Vec<Job>> {
    ctx.authorize(Permission::JobRead)?;
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::jobs::list(&mut tx, status, kind, before, limit.clamp(1, 200)).await?;
    tx.commit().await?;
    Ok(rows)
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<(Job, Vec<Run>)> {
    ctx.authorize(Permission::JobRead)?;
    let mut tx = begin(state, ctx).await?;
    let job = core_db::jobs::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let runs = core_db::jobs::runs(&mut tx, id).await?;
    tx.commit().await?;
    Ok((job, runs))
}

/// Puts a dead job back in the queue with a fresh set of attempts.
pub async fn retry(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<Job> {
    ctx.authorize(Permission::JobWrite)?;
    let mut tx = begin(state, ctx).await?;
    let job = core_db::jobs::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !core_db::jobs::retry(&mut tx, id).await? {
        return Err(ApiError::conflict(
            "job_not_dead",
            format!(
                "only dead or cancelled jobs can be retried; this one is {:?}",
                job.status
            )
            .to_lowercase(),
        ));
    }
    record(
        &mut tx,
        ctx,
        Change {
            topic: "job.retried",
            resource: "job",
            object_id: Some(id),
            before: None,
            after: Some(serde_json::json!({ "kind": job.kind })),
            event_extra: None,
        },
    )
    .await?;
    let job = core_db::jobs::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(job)
}
