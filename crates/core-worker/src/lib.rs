//! The background worker. It runs from the same binary as the API
//! (`onhold worker`) and does three things:
//!
//! - **Publish** ([`Worker::publish_once`]): gives each new outbox event its
//!   stream position, queues the jobs it causes, and tells live event
//!   streams there is something new.
//! - **Run jobs** ([`Worker::run_jobs_once`]): leases ready jobs, runs each
//!   with a time limit, and records the attempt; failures are retried with
//!   exponential backoff until the job's attempts run out.
//! - **Schedule** ([`Worker::schedule_once`]): queues runs of time-based
//!   rules, returns jobs whose worker died, and deletes old data.
//!
//! Each step is a plain function so tests can drive the worker one step at
//! a time; [`run`] loops them, woken by database notifications with polling
//! as the fallback. Any number of workers may run at once: claims use
//! `SKIP LOCKED`, publishing and scheduling take advisory locks.

mod handlers;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use core_api::AppState;
use core_api::services::automation::{JOB_DISPATCH, JOB_SYSTEM_MAIL};
use core_api::services::bookings::{JOB_COMPLETE, JOB_EXPIRE_HOLD};
use core_api::services::events::CHANNEL as EVENTS_CHANNEL;
use core_api::services::invoices::JOB_OVERDUE;
use core_api::services::mail::SYSTEM_TOPICS;
use core_api::services::quotes::JOB_EXPIRE as JOB_EXPIRE_QUOTE;
use core_db::DbError;
use core_db::jobs::{Claimed, LogLine, NewJob, RunRecord};
use core_integrations::http::WebhookClient;
use core_integrations::mail::Mailer;
use serde_json::json;
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub use handlers::JobError;

/// Channels the database notifies when there is work.
const OUTBOX_CHANNEL: &str = "onhold_outbox";
const JOBS_CHANNEL: &str = "onhold_jobs";
/// Only one worker schedules at a time.
const SCHEDULER_LOCK: i64 = 0x6f6e_686f_6c64_0002;
const PUBLISH_BATCH: i64 = 200;
const DUE_BATCH: i64 = 200;
/// Sign-in links and invitations go before everything else.
const SYSTEM_MAIL_PRIORITY: i16 = 10;

#[derive(Debug, Clone)]
pub struct Settings {
    /// Jobs run at the same time by this process.
    pub concurrency: usize,
    /// How long a claimed job is reserved; another worker takes it over after.
    pub lease: Duration,
    /// A job running longer than this fails (and is retried).
    pub job_timeout: Duration,
    /// How long one import job works before handing the rest of the file
    /// to a fresh job. Well under `job_timeout`.
    pub import_slice: Duration,
    /// How often to look for work when no notification arrives.
    pub poll: Duration,
    /// How often to queue due rules and reclaim expired leases.
    pub schedule_every: Duration,
    /// How often to delete old data.
    pub cleanup_every: Duration,
    /// Published events are kept this long for clients syncing from the feed.
    pub event_retention: chrono::Duration,
    /// Finished jobs and their run logs are kept this long.
    pub job_retention: chrono::Duration,
    /// Uploaded spreadsheets never run are kept this long.
    pub upload_retention: chrono::Duration,
    /// Delay before retry number `attempt` (from 1).
    pub backoff: fn(i32) -> Duration,
    /// Where to fetch reference exchange rates (the ECB feeds); `None`
    /// turns fetching off, leaving manual rates only.
    pub fx_rates: Option<FxFeeds>,
    /// How often to look for new exchange rates (fetched once a day at most).
    pub fx_every: Duration,
}

/// The exchange rate feeds: the day's rates, and a longer history for the
/// first fetch or after a gap.
#[derive(Debug, Clone)]
pub struct FxFeeds {
    pub daily: String,
    pub history: String,
}

impl Default for FxFeeds {
    fn default() -> Self {
        Self {
            daily: core_integrations::fx::ECB_DAILY.to_owned(),
            history: core_integrations::fx::ECB_90_DAYS.to_owned(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            concurrency: 8,
            lease: Duration::from_secs(5 * 60),
            job_timeout: Duration::from_secs(2 * 60),
            import_slice: Duration::from_secs(60),
            poll: Duration::from_secs(2),
            schedule_every: Duration::from_secs(15),
            cleanup_every: Duration::from_secs(60 * 60),
            event_retention: chrono::Duration::days(30),
            job_retention: chrono::Duration::days(7),
            upload_retention: chrono::Duration::days(7),
            backoff: core_engines::schedule::retry_delay,
            fx_rates: Some(FxFeeds::default()),
            fx_every: Duration::from_secs(60 * 60),
        }
    }
}

/// Everything the worker needs. Cheap to clone.
#[derive(Clone)]
pub struct Worker {
    /// The API's state: the app pool (tenant work runs through the same
    /// services, under row-level security), configuration and keys.
    pub app: AppState,
    /// Connections as `onhold_worker`, which sees only the queue tables.
    pub queue: PgPool,
    pub mailer: Arc<dyn Mailer>,
    pub webhooks: WebhookClient,
    pub settings: Settings,
}

impl Worker {
    /// Publishes the next batch of outbox events. Returns how many.
    pub async fn publish_once(&self) -> Result<usize, DbError> {
        let mut tx = self.queue.begin().await?;
        let pending = core_db::outbox::take_unpublished(&mut tx, PUBLISH_BATCH).await?;
        let mut workspaces = BTreeSet::new();
        for event in &pending {
            core_db::outbox::mark_published(&mut tx, event.id).await?;
            let id = event.id.to_string();
            if SYSTEM_TOPICS.contains(&event.topic.as_str()) {
                let payload =
                    json!({ "event_id": event.id, "topic": event.topic, "event": event.payload });
                let mut job =
                    NewJob::new(event.workspace_id, JOB_SYSTEM_MAIL, &payload).dedupe(&id);
                job.secret = event.secret.as_deref();
                job.priority = SYSTEM_MAIL_PRIORITY;
                core_db::jobs::enqueue(&mut tx, job).await?;
            }
            if let Some(ws) = event.workspace_id {
                let payload = json!({ "event_id": event.id });
                core_db::jobs::enqueue(
                    &mut tx,
                    NewJob::new(Some(ws), JOB_DISPATCH, &payload).dedupe(&id),
                )
                .await?;
                workspaces.insert(ws);
            }
        }
        // Delivered when the transaction commits.
        for ws in workspaces {
            sqlx::query("SELECT pg_notify($1, $2)")
                .bind(EVENTS_CHANNEL)
                .bind(ws.0.to_string())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(pending.len())
    }

    /// Claims up to `limit` ready jobs.
    pub async fn claim(&self, limit: usize) -> Result<Vec<Claimed>, DbError> {
        let mut conn = self.queue.acquire().await?;
        core_db::jobs::claim(&mut conn, limit as i64, self.settings.lease).await
    }

    /// Claims ready jobs and runs them to completion, up to `concurrency`
    /// at once. Returns how many ran. For tests and one-shot use; [`run`]
    /// keeps slots busy instead.
    pub async fn run_jobs_once(&self) -> Result<usize, DbError> {
        let jobs = self.claim(self.settings.concurrency).await?;
        let n = jobs.len();
        futures_util::future::join_all(jobs.into_iter().map(|job| self.execute(job))).await;
        Ok(n)
    }

    /// Runs one claimed job and records the outcome.
    pub async fn execute(&self, job: Claimed) {
        let started_at = Utc::now();
        let mut log = Vec::new();
        let result = match tokio::time::timeout(
            self.settings.job_timeout,
            self.handle(&job, &mut log),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => {
                let message = format!(
                    "timed out after {} seconds",
                    self.settings.job_timeout.as_secs()
                );
                // The handler was cut off, so it could not clean up itself.
                if job.attempt >= job.max_attempts {
                    self.gave_up(job.workspace_id, &job.kind, &job.payload, &message)
                        .await;
                }
                Err(JobError::retry(message))
            }
        };
        if let Err(err) = self.finish(&job, started_at, &log, result).await {
            // The lease runs out and the job is retried.
            tracing::error!(job_id = %job.id, kind = %job.kind, error = %err, "could not record a job's outcome");
        }
    }

    async fn finish(
        &self,
        job: &Claimed,
        started_at: chrono::DateTime<Utc>,
        log: &[LogLine],
        result: Result<(), JobError>,
    ) -> Result<(), DbError> {
        let mut tx = self.queue.begin().await?;
        let error = match &result {
            Ok(()) => {
                core_db::jobs::succeed(&mut tx, job.id).await?;
                None
            }
            Err(err) => {
                let retry_at = (!err.permanent && job.attempt < job.max_attempts).then(|| {
                    let delay = (self.settings.backoff)(job.attempt);
                    Utc::now()
                        + chrono::Duration::from_std(delay).unwrap_or(chrono::Duration::hours(6))
                });
                match retry_at {
                    Some(_) => {
                        tracing::warn!(job_id = %job.id, kind = %job.kind, attempt = job.attempt, error = %err.message, "job failed; will retry")
                    }
                    None => {
                        tracing::error!(job_id = %job.id, kind = %job.kind, attempt = job.attempt, error = %err.message, "job failed for good")
                    }
                }
                core_db::jobs::fail(&mut tx, job.id, &err.message, retry_at).await?;
                Some(err.message.as_str())
            }
        };
        core_db::jobs::record_run(
            &mut tx,
            RunRecord {
                job_id: job.id,
                workspace: job.workspace_id,
                attempt: job.attempt,
                started_at,
                succeeded: result.is_ok(),
                error,
                log,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Queues runs of due time-based rules and returns jobs whose worker
    /// stopped mid-run. Returns how many rules were queued, or `None` when
    /// another worker is scheduling.
    pub async fn schedule_once(&self) -> Result<Option<usize>, DbError> {
        let mut tx = self.queue.begin().await?;
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(SCHEDULER_LOCK)
            .fetch_one(&mut *tx)
            .await?;
        if !locked {
            return Ok(None);
        }
        let reclaimed = core_db::jobs::reclaim_expired(&mut tx).await?;
        if !reclaimed.is_empty() {
            tracing::warn!(
                reclaimed = reclaimed.len(),
                "returned jobs whose worker stopped to the queue"
            );
        }
        let now = Utc::now();
        let due = core_db::rules::due(&mut tx, now, DUE_BATCH).await?;
        for rule in &due {
            let (planned, next) = core_api::services::automation::plan_due(rule, now);
            if let Some(p) = planned {
                core_db::jobs::enqueue(
                    &mut tx,
                    NewJob::new(Some(rule.workspace_id), p.kind, &p.payload).dedupe(&p.dedupe_key),
                )
                .await?;
            }
            core_db::rules::advance(&mut tx, rule.id, next).await?;
        }
        let swept = self.sweep(&mut tx, now).await?;
        tx.commit().await?;
        for job in reclaimed.iter().filter(|j| j.dead) {
            self.gave_up(
                job.workspace_id,
                &job.kind,
                &job.payload,
                core_db::jobs::WORKER_STOPPED,
            )
            .await;
        }
        Ok(Some(due.len() + swept))
    }

    /// Queues the bookings module's timed work: holds past their expiry,
    /// confirmed bookings that have ended, and quotes past their last day.
    /// Each job is keyed by the time that made it due, so a hold placed
    /// again later is swept again.
    async fn sweep(
        &self,
        tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
        now: chrono::DateTime<Utc>,
    ) -> Result<usize, DbError> {
        let mut queued = 0;
        for b in core_db::bookings::holds_expired(tx, now, DUE_BATCH).await? {
            let key = format!("{}@{}", b.id, b.at.to_rfc3339());
            let payload = json!({ "booking_id": b.id });
            core_db::jobs::enqueue(
                tx,
                NewJob::new(Some(b.workspace_id), JOB_EXPIRE_HOLD, &payload).dedupe(&key),
            )
            .await?;
            queued += 1;
        }
        for b in core_db::bookings::ended(tx, now, DUE_BATCH).await? {
            let key = format!("{}@{}", b.id, b.at.to_rfc3339());
            let payload = json!({ "booking_id": b.id });
            core_db::jobs::enqueue(
                tx,
                NewJob::new(Some(b.workspace_id), JOB_COMPLETE, &payload).dedupe(&key),
            )
            .await?;
            queued += 1;
        }
        for i in core_db::invoices::overdue(tx, now, DUE_BATCH).await? {
            let key = format!("{}@{}", i.id, i.due_date);
            let payload = json!({ "invoice_id": i.id });
            core_db::jobs::enqueue(
                tx,
                NewJob::new(Some(i.workspace_id), JOB_OVERDUE, &payload).dedupe(&key),
            )
            .await?;
            queued += 1;
        }
        for q in core_db::quotes::lapsed(tx, now, DUE_BATCH).await? {
            let key = format!("{}@{}", q.id, q.valid_until);
            let payload = json!({ "quote_id": q.id });
            core_db::jobs::enqueue(
                tx,
                NewJob::new(Some(q.workspace_id), JOB_EXPIRE_QUOTE, &payload).dedupe(&key),
            )
            .await?;
            queued += 1;
        }
        Ok(queued)
    }

    /// Deletes old events, finished jobs, stored idempotent responses and
    /// uploads never run.
    pub async fn cleanup_once(&self) -> Result<(), DbError> {
        let now = Utc::now();
        let mut tx = self.queue.begin().await?;
        let events =
            core_db::outbox::prune_published_before(&mut tx, now - self.settings.event_retention)
                .await?;
        let jobs =
            core_db::jobs::delete_succeeded_before(&mut tx, now - self.settings.job_retention)
                .await?;
        let keys =
            core_db::idempotency::delete_before(&mut tx, now - chrono::Duration::hours(24)).await?;
        let uploads =
            core_db::imports::delete_stale_uploads(&mut tx, now - self.settings.upload_retention)
                .await?;
        tx.commit().await?;
        // Files uploaded but never attached to anything.
        let mut conn = self.queue.acquire().await?;
        let files = match core_api::services::files::sweep_unattached(&self.app, &mut conn).await {
            Ok(n) => n,
            Err(err) => {
                tracing::warn!(error = %err, "removing unattached files failed");
                0
            }
        };
        tracing::info!(
            events,
            jobs,
            idempotency_keys = keys,
            uploads,
            files,
            "deleted old data"
        );
        Ok(())
    }

    /// Fetches reference exchange rates when today's are not stored yet
    /// (the whole recent history after a gap). Returns how many days were
    /// stored. Rates are published on working days, so most checks after
    /// the first of the day find nothing new and cost one small request.
    pub async fn fetch_rates_once(&self) -> Result<usize, DbError> {
        let Some(feeds) = &self.settings.fx_rates else {
            return Ok(0);
        };
        let mut conn = self.queue.acquire().await?;
        let today = Utc::now().date_naive();
        let latest = core_db::finance::latest_rate_day(&mut conn).await?;
        if latest == Some(today) {
            return Ok(0);
        }
        // The daily feed has one day: anything older than yesterday's
        // means days may be missing, which the 90-day feed fills.
        let gap = latest.is_none_or(|d| (today - d).num_days() > 1);
        let url = if gap { &feeds.history } else { &feeds.daily };
        let days = match core_integrations::fx::fetch_ecb(url).await {
            Ok(days) => days,
            Err(err) => {
                tracing::warn!(error = %err, "fetching exchange rates failed");
                return Ok(0);
            }
        };
        // Every day in the feed, all or none, so a failure cannot leave
        // older days missing behind newer ones.
        let mut tx = sqlx::Connection::begin(&mut *conn).await?;
        let mut stored = 0;
        for day in &days {
            if core_db::finance::store_rates(&mut tx, day.date, &day.rates, "ecb").await? > 0 {
                stored += 1;
            }
        }
        tx.commit().await?;
        if stored > 0 {
            tracing::info!(days = stored, "stored exchange rates");
        }
        Ok(stored)
    }
}

/// Runs the worker until `shutdown` is cancelled, then waits (up to the
/// job timeout) for running jobs to finish.
pub async fn run(worker: Worker, shutdown: CancellationToken) {
    tracing::info!(concurrency = worker.settings.concurrency, "worker started");
    let outbox = Arc::new(Notify::new());
    let jobs = Arc::new(Notify::new());
    let listener = tokio::spawn(listen(
        worker.queue.clone(),
        outbox.clone(),
        jobs.clone(),
        shutdown.clone(),
    ));

    tokio::join!(
        publisher(worker.clone(), outbox, shutdown.clone()),
        runner(worker.clone(), jobs, shutdown.clone()),
        scheduler(worker, shutdown),
    );
    listener.abort();
    tracing::info!("worker stopped");
}

async fn wait(notify: &Notify, poll: Duration, shutdown: &CancellationToken) {
    tokio::select! {
        () = notify.notified() => {}
        () = tokio::time::sleep(poll) => {}
        () = shutdown.cancelled() => {}
    }
}

async fn publisher(worker: Worker, notify: Arc<Notify>, shutdown: CancellationToken) {
    while !shutdown.is_cancelled() {
        match worker.publish_once().await {
            Ok(n) if n as i64 == PUBLISH_BATCH => continue,
            Ok(_) => {}
            Err(err) => tracing::error!(error = %err, "publishing events failed"),
        }
        wait(&notify, worker.settings.poll, &shutdown).await;
    }
}

async fn runner(worker: Worker, notify: Arc<Notify>, shutdown: CancellationToken) {
    let slots = Arc::new(Semaphore::new(worker.settings.concurrency));
    let finished = Arc::new(Notify::new());
    let mut running = JoinSet::new();
    while !shutdown.is_cancelled() {
        while running.try_join_next().is_some() {}
        let free = slots.available_permits();
        let claimed = if free == 0 {
            Vec::new()
        } else {
            worker.claim(free).await.unwrap_or_else(|err| {
                tracing::error!(error = %err, "claiming jobs failed");
                Vec::new()
            })
        };
        let got = claimed.len();
        for job in claimed {
            let Ok(permit) = slots.clone().acquire_owned().await else {
                break;
            };
            let worker = worker.clone();
            let finished = finished.clone();
            running.spawn(async move {
                worker.execute(job).await;
                drop(permit);
                finished.notify_one();
            });
        }
        // Full or empty: wait for a slot or for new work. Otherwise look again.
        if got == 0 || got == free {
            tokio::select! {
                () = notify.notified() => {}
                () = finished.notified() => {}
                () = tokio::time::sleep(worker.settings.poll) => {}
                () = shutdown.cancelled() => {}
            }
        }
    }
    let grace = worker.settings.job_timeout;
    if tokio::time::timeout(grace, async {
        while running.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tracing::warn!(
            "stopping with jobs still running; their leases will expire and they will be retried"
        );
    }
}

async fn scheduler(worker: Worker, shutdown: CancellationToken) {
    let mut schedule = tokio::time::interval(worker.settings.schedule_every);
    let mut cleanup = tokio::time::interval(worker.settings.cleanup_every);
    let mut rates = tokio::time::interval(worker.settings.fx_every);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            _ = schedule.tick() => {
                if let Err(err) = worker.schedule_once().await {
                    tracing::error!(error = %err, "scheduling failed");
                }
            }
            _ = cleanup.tick() => {
                if let Err(err) = worker.cleanup_once().await {
                    tracing::error!(error = %err, "cleanup failed");
                }
            }
            _ = rates.tick() => {
                if let Err(err) = worker.fetch_rates_once().await {
                    tracing::error!(error = %err, "storing exchange rates failed");
                }
            }
        }
    }
}

/// Wakes the publisher and runner when the database says there is work.
/// Polling covers the gaps if the connection drops.
async fn listen(pool: PgPool, outbox: Arc<Notify>, jobs: Arc<Notify>, shutdown: CancellationToken) {
    while !shutdown.is_cancelled() {
        let result: Result<(), sqlx::Error> = async {
            let mut listener = PgListener::connect_with(&pool).await?;
            listener.listen_all([OUTBOX_CHANNEL, JOBS_CHANNEL]).await?;
            loop {
                let n = listener.recv().await?;
                match n.channel() {
                    OUTBOX_CHANNEL => outbox.notify_one(),
                    JOBS_CHANNEL => jobs.notify_one(),
                    _ => {}
                }
            }
        }
        .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "lost the notification connection; reconnecting");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
