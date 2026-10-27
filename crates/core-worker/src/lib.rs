//! The background worker: job runner, schedulers and pollers. It runs from the
//! same binary as the API in worker mode.
//!
//! In M0 it only proves the process lifecycle (start, heartbeat, clean
//! shutdown). The outbox drain and job queue arrive in M2.

use std::time::Duration;

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

/// How often the idle worker logs that it is alive.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// Runs the worker loop until `shutdown` is cancelled.
pub async fn run(_db: PgPool, heartbeat: Duration, shutdown: CancellationToken) {
    tracing::info!("worker started");
    let mut ticker = tokio::time::interval(heartbeat);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            _ = ticker.tick() => tracing::debug!("worker heartbeat"),
        }
    }
    tracing::info!("worker stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    #[tokio::test]
    async fn stops_promptly_when_shutdown_is_requested() {
        let db = PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .expect("lazy pool");
        let shutdown = CancellationToken::new();
        let handle = tokio::spawn(run(db, Duration::from_millis(10), shutdown.clone()));
        tokio::time::sleep(Duration::from_millis(30)).await;
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("worker stopped within a second")
            .expect("worker task did not panic");
    }
}
