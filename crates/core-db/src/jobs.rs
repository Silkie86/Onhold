//! The job queue. Producers insert rows (in the same transaction as the work
//! that caused them); the worker claims ready rows with `FOR UPDATE SKIP
//! LOCKED`, holds a lease while it runs them, and records every attempt.

use std::time::Duration;

use chrono::{DateTime, Utc};
use core_domain::WorkspaceId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    sqlx::Type,
    utoipa::ToSchema,
)]
#[sqlx(type_name = "job_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    /// Out of attempts, or failed in a way retrying cannot fix.
    Dead,
    Cancelled,
}

pub struct NewJob<'a> {
    pub workspace: Option<WorkspaceId>,
    pub kind: &'a str,
    pub payload: &'a Value,
    /// Encrypted data only the handler may read (never shown in the API).
    pub secret: Option<&'a [u8]>,
    /// A second job with the same workspace, kind and key is not created.
    pub dedupe_key: Option<&'a str>,
    pub priority: i16,
    pub run_at: Option<DateTime<Utc>>,
    pub max_attempts: i32,
}

impl<'a> NewJob<'a> {
    pub fn new(workspace: Option<WorkspaceId>, kind: &'a str, payload: &'a Value) -> Self {
        Self {
            workspace,
            kind,
            payload,
            secret: None,
            dedupe_key: None,
            priority: 0,
            run_at: None,
            max_attempts: 10,
        }
    }

    pub fn dedupe(mut self, key: &'a str) -> Self {
        self.dedupe_key = Some(key);
        self
    }
}

/// Queues a job. Returns `None` when an identical job (same dedupe key) exists.
pub async fn enqueue(conn: &mut PgConnection, job: NewJob<'_>) -> Result<Option<Uuid>> {
    let id = core_domain::ids::new_id();
    let inserted = sqlx::query_scalar!(
        r#"INSERT INTO jobs (id, workspace_id, kind, payload, secret, dedupe_key, priority, run_at, max_attempts)
           VALUES ($1, $2, $3, $4, $5, $6, $7, coalesce($8, now()), $9)
           ON CONFLICT (workspace_id, kind, dedupe_key) WHERE dedupe_key IS NOT NULL DO NOTHING
           RETURNING id"#,
        id,
        job.workspace.map(|w| w.0),
        job.kind,
        job.payload,
        job.secret,
        job.dedupe_key,
        job.priority,
        job.run_at,
        job.max_attempts
    )
    .fetch_optional(conn)
    .await?;
    Ok(inserted)
}

/// A job the worker has leased.
#[derive(Debug, Clone)]
pub struct Claimed {
    pub id: Uuid,
    pub workspace_id: Option<WorkspaceId>,
    pub kind: String,
    pub payload: Value,
    pub secret: Option<Vec<u8>>,
    /// This attempt's number, from 1.
    pub attempt: i32,
    pub max_attempts: i32,
}

/// Leases up to `limit` ready jobs for `lease`. Worker role.
///
/// Workspaces take turns: within a priority, each workspace's next job
/// comes before anyone's second, counting the jobs it already has running,
/// so a big import or a first mailbox read in one workspace cannot hold up
/// timed work in the others.
pub async fn claim(conn: &mut PgConnection, limit: i64, lease: Duration) -> Result<Vec<Claimed>> {
    let lease_secs = lease.as_secs_f64();
    Ok(sqlx::query_as!(
        Claimed,
        r#"WITH running AS (
               SELECT coalesce(workspace_id, '00000000-0000-0000-0000-000000000000') AS ws, count(*) AS n
               FROM jobs WHERE status = 'running' GROUP BY 1
           ),
           ready AS (
               SELECT q.id, q.priority, q.run_at,
                      row_number() OVER (PARTITION BY q.workspace_id ORDER BY q.priority DESC, q.run_at, q.id)
                          + coalesce(r.n, 0) AS turn
               FROM jobs q
               LEFT JOIN running r ON r.ws = coalesce(q.workspace_id, '00000000-0000-0000-0000-000000000000')
               WHERE q.status = 'queued' AND q.run_at <= now()
           )
           UPDATE jobs SET status = 'running', attempts = attempts + 1,
                           locked_until = now() + make_interval(secs => $2)
           WHERE id IN (SELECT j.id FROM ready t JOIN jobs j ON j.id = t.id
                        WHERE j.status = 'queued'
                        ORDER BY t.priority DESC, t.turn, t.run_at, t.id
                        LIMIT $1
                        FOR UPDATE OF j SKIP LOCKED)
           RETURNING id, workspace_id AS "workspace_id: WorkspaceId", kind, payload, secret,
                     attempts AS attempt, max_attempts"#,
        limit,
        lease_secs
    )
    .fetch_all(conn)
    .await?)
}

/// Marks a leased job done. Worker role.
pub async fn succeed(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE jobs SET status = 'succeeded', locked_until = NULL, finished_at = now(),
                           last_error = NULL, secret = NULL
           WHERE id = $1 AND status = 'running'"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Records a failed attempt: back in the queue at `retry_at`, or dead when
/// `retry_at` is `None`. Worker role.
pub async fn fail(
    conn: &mut PgConnection,
    id: Uuid,
    error: &str,
    retry_at: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE jobs SET
               status = CASE WHEN $3::timestamptz IS NULL THEN 'dead'::job_status ELSE 'queued' END,
               run_at = coalesce($3, run_at),
               locked_until = NULL,
               last_error = $2,
               finished_at = CASE WHEN $3::timestamptz IS NULL THEN now() END
           WHERE id = $1 AND status = 'running'"#,
        id,
        error,
        retry_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// One line of a job's log.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogLine {
    pub at: DateTime<Utc>,
    pub message: String,
}

pub struct RunRecord<'a> {
    pub job_id: Uuid,
    pub workspace: Option<WorkspaceId>,
    pub attempt: i32,
    pub started_at: DateTime<Utc>,
    pub succeeded: bool,
    pub error: Option<&'a str>,
    pub log: &'a [LogLine],
}

/// Appends an attempt to the job's run log. Worker role.
pub async fn record_run(conn: &mut PgConnection, run: RunRecord<'_>) -> Result<()> {
    let log = serde_json::to_value(run.log).unwrap_or_default();
    sqlx::query!(
        r#"INSERT INTO job_runs (id, job_id, workspace_id, attempt, started_at, succeeded, error, log)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
        core_domain::ids::new_id(),
        run.job_id,
        run.workspace.map(|w| w.0),
        run.attempt,
        run.started_at,
        run.succeeded,
        run.error,
        log
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The error recorded on a job whose worker stopped mid-run.
pub const WORKER_STOPPED: &str = "the worker stopped while running this job";

/// A job [`reclaim_expired`] returned to the queue, or buried (`dead`).
#[derive(Debug, Clone)]
pub struct Reclaimed {
    pub workspace_id: Option<WorkspaceId>,
    pub kind: String,
    pub payload: Value,
    pub dead: bool,
}

/// Returns jobs whose worker died mid-run (lease expired) to the queue, or
/// buries them if that was their last attempt. Worker role.
pub async fn reclaim_expired(conn: &mut PgConnection) -> Result<Vec<Reclaimed>> {
    Ok(sqlx::query_as!(
        Reclaimed,
        r#"UPDATE jobs SET
               status = CASE WHEN attempts >= max_attempts THEN 'dead'::job_status ELSE 'queued' END,
               finished_at = CASE WHEN attempts >= max_attempts THEN now() END,
               locked_until = NULL,
               run_at = now(),
               last_error = $1
           WHERE status = 'running' AND locked_until < now()
           RETURNING workspace_id AS "workspace_id: WorkspaceId", kind, payload,
                     status = 'dead' AS "dead!""#,
        WORKER_STOPPED
    )
    .fetch_all(conn)
    .await?)
}

/// Deletes jobs that finished well, and their run logs, after a while. Worker role.
pub async fn delete_succeeded_before(
    conn: &mut PgConnection,
    before: DateTime<Utc>,
) -> Result<u64> {
    let done = sqlx::query!(
        "DELETE FROM jobs WHERE status = 'succeeded' AND finished_at < $1",
        before
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}

/// Deletes jobs that ran out of attempts or were cancelled, and their run
/// logs, after a while. Worker role.
pub async fn delete_finished_before(conn: &mut PgConnection, before: DateTime<Utc>) -> Result<u64> {
    let done = sqlx::query!(
        r#"DELETE FROM jobs WHERE status IN ('dead', 'cancelled')
             AND coalesce(finished_at, updated_at) < $1"#,
        before
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}

// The workspace's view (app role) ----------------------------------------------

#[derive(Debug, Clone)]
pub struct Job {
    pub id: Uuid,
    pub kind: String,
    pub payload: Value,
    pub dedupe_key: Option<String>,
    pub priority: i16,
    pub status: JobStatus,
    pub run_at: DateTime<Utc>,
    pub attempts: i32,
    pub max_attempts: i32,
    pub locked_until: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Newest first; `before` is a keyset cursor (a job id).
pub async fn list(
    conn: &mut PgConnection,
    status: Option<JobStatus>,
    kind: Option<&str>,
    before: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Job>> {
    Ok(sqlx::query_as!(
        Job,
        r#"SELECT id, kind, payload, dedupe_key, priority, status AS "status: JobStatus", run_at,
                  attempts, max_attempts, locked_until, last_error, created_at, updated_at, finished_at
           FROM jobs
           WHERE workspace_id = app_workspace_id()
             AND ($1::job_status IS NULL OR status = $1)
             AND ($2::text IS NULL OR kind = $2)
             AND ($3::uuid IS NULL OR id < $3)
           ORDER BY id DESC
           LIMIT $4"#,
        status as Option<JobStatus>,
        kind,
        before,
        limit
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Job>> {
    Ok(sqlx::query_as!(
        Job,
        r#"SELECT id, kind, payload, dedupe_key, priority, status AS "status: JobStatus", run_at,
                  attempts, max_attempts, locked_until, last_error, created_at, updated_at, finished_at
           FROM jobs WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct Run {
    pub attempt: i32,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub succeeded: bool,
    pub error: Option<String>,
    pub log: Value,
}

pub async fn runs(conn: &mut PgConnection, job_id: Uuid) -> Result<Vec<Run>> {
    Ok(sqlx::query_as!(
        Run,
        r#"SELECT attempt, started_at, finished_at, succeeded, error, log
           FROM job_runs WHERE workspace_id = app_workspace_id() AND job_id = $1
           ORDER BY finished_at, attempt"#,
        job_id
    )
    .fetch_all(conn)
    .await?)
}

/// Puts a dead or cancelled job back in the queue with fresh attempts.
pub async fn retry(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE jobs SET status = 'queued', run_at = now(), attempts = 0, finished_at = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND status IN ('dead', 'cancelled')"#,
        id
    )
    .execute(&mut *conn)
    .await?;
    if done.rows_affected() == 1 {
        sqlx::query!("SELECT pg_notify('onhold_jobs', '')")
            .execute(conn)
            .await?;
    }
    Ok(done.rows_affected() == 1)
}

/// Queue health across all workspaces, for operators (superadmin only; the
/// function checks).
#[derive(Debug, Clone)]
pub struct Summary {
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_slug: Option<String>,
    pub kind: String,
    pub status: JobStatus,
    pub jobs: i64,
    pub oldest: Option<DateTime<Utc>>,
}

pub async fn admin_summary(conn: &mut PgConnection) -> Result<Vec<Summary>> {
    Ok(sqlx::query_as!(
        Summary,
        r#"SELECT workspace_id AS "workspace_id: WorkspaceId", workspace_slug, kind AS "kind!",
                  status AS "status!: JobStatus", jobs AS "jobs!", oldest
           FROM admin_job_summary()"#
    )
    .fetch_all(conn)
    .await?)
}
