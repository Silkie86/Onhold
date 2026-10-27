//! The single OnHold binary: `onhold api`, `onhold worker` and `onhold migrate`.

mod telemetry;

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Parser)]
#[command(name = "onhold", version, about = "OnHold platform core")]
struct Cli {
    #[command(flatten)]
    db: DbArgs,

    #[command(flatten)]
    telemetry: telemetry::TelemetryArgs,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Args)]
struct DbArgs {
    /// Postgres connection string.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true, global = true)]
    database_url: Option<String>,

    /// Maximum connections in the pool.
    #[arg(
        long,
        env = "DATABASE_MAX_CONNECTIONS",
        default_value_t = 10,
        global = true
    )]
    database_max_connections: u32,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the HTTP API.
    Api {
        #[arg(long, env = "BIND_ADDR", default_value = "0.0.0.0:8080")]
        bind: SocketAddr,
    },
    /// Run the background worker, with health probes on their own port.
    Worker {
        #[arg(long, env = "WORKER_HEALTH_ADDR", default_value = "0.0.0.0:8081")]
        health_bind: SocketAddr,
    },
    /// Apply pending database migrations and exit.
    Migrate,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    telemetry::init(&cli.telemetry)?;

    let pool_config = core_db::PoolConfig {
        url: cli.db.database_url.context("DATABASE_URL is not set")?,
        max_connections: cli.db.database_max_connections,
        acquire_timeout: Duration::from_secs(5),
    };
    let db = core_db::connect_lazy(&pool_config)?;

    let shutdown = CancellationToken::new();
    tokio::spawn(cancel_on_signal(shutdown.clone()));

    match cli.command {
        Command::Api { bind } => {
            let app = core_api::router(core_api::AppState { db });
            core_api::serve(bind, app, shutdown).await?;
        }
        Command::Worker { health_bind } => {
            let probes = core_api::health::standalone(db.clone());
            let probes = core_api::serve(health_bind, probes, shutdown.clone());
            let worker = core_worker::run(db, core_worker::HEARTBEAT_INTERVAL, shutdown.clone());
            let (probes, ()) = tokio::join!(probes, worker);
            probes?;
        }
        Command::Migrate => core_db::migrate(&db).await?,
    }
    Ok(())
}

/// Cancels `token` on Ctrl-C or SIGTERM, which is how containers are stopped.
async fn cancel_on_signal(token: CancellationToken) {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %err, "failed to listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown requested");
    token.cancel();
}
