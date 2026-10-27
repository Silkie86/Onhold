//! The single OnHold binary: `onhold api`, `onhold worker`, both at once with
//! `onhold serve`, `onhold migrate`, `onhold openapi` and the operator tools
//! under `onhold admin`.

mod config;
mod telemetry;

use std::io::Write;
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

    #[command(flatten)]
    api: config::ApiArgs,

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
    /// Serve the HTTP API and run the background worker in one process.
    ///
    /// One small machine for a small deployment; split into `api` and
    /// `worker` to scale them separately.
    Serve {
        #[arg(long, env = "BIND_ADDR", default_value = "0.0.0.0:8080")]
        bind: SocketAddr,
        #[command(flatten)]
        worker: config::WorkerArgs,
    },
    /// Run the background worker, with health probes on their own port.
    Worker {
        #[arg(long, env = "WORKER_HEALTH_ADDR", default_value = "0.0.0.0:8081")]
        health_bind: SocketAddr,
        #[command(flatten)]
        worker: config::WorkerArgs,
    },
    /// Apply pending database migrations and exit.
    Migrate,
    /// Print the OpenAPI document as JSON. Needs no database.
    Openapi,
    /// Operator tools.
    #[command(subcommand)]
    Admin(AdminCommand),
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Print a new random encryption key, ready for `ENCRYPTION_KEYS`.
    GenerateKey {
        /// Key id; use a new one for each rotation.
        #[arg(long, default_value = "k1")]
        id: String,
    },
    /// Workspaces.
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Platform operator rights. The user must have signed in at least once.
    #[command(subcommand)]
    Superadmin(SuperadminCommand),
}

#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    /// Create a workspace and print its owner's invitation link.
    Create {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        name: String,
        /// ISO 3166-1 alpha-2, e.g. GB.
        #[arg(long)]
        country: String,
        /// ISO 4217, e.g. GBP.
        #[arg(long)]
        currency: String,
        /// IANA time zone, e.g. Europe/London.
        #[arg(long)]
        timezone: String,
        #[arg(long)]
        owner_email: String,
        #[arg(long)]
        locale: Option<String>,
        #[arg(long)]
        plan: Option<String>,
        #[arg(long)]
        plugin_slug: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum SuperadminCommand {
    Grant {
        #[arg(long)]
        email: String,
    },
    Revoke {
        #[arg(long)]
        email: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // A `.env` file, if present, fills in settings the environment lacks (local development).
    let dotenv = dotenvy::dotenv();
    let cli = Cli::parse();
    telemetry::init(&cli.telemetry)?;
    if let Err(err) = dotenv
        && !err.not_found()
    {
        tracing::warn!(error = %err, "could not read .env");
    }

    // Commands that touch no database.
    match &cli.command {
        Command::Openapi => {
            let json = core_api::openapi().to_pretty_json()?;
            // A closed pipe (`onhold openapi | head`) is not an error.
            let mut out = std::io::stdout().lock();
            match writeln!(out, "{json}") {
                Err(err) if err.kind() != std::io::ErrorKind::BrokenPipe => return Err(err.into()),
                _ => return Ok(()),
            }
        }
        Command::Admin(AdminCommand::GenerateKey { id }) => {
            let key = core_domain::crypto::random_bytes::<32>();
            let spec = format!("{id}:{}", data_encoding::BASE64.encode(&key));
            core_domain::crypto::Keyring::parse(&spec)?;
            println!("{spec}");
            return Ok(());
        }
        _ => {}
    }

    let pool_config = core_db::PoolConfig {
        url: cli.db.database_url.context("DATABASE_URL is not set")?,
        max_connections: cli.db.database_max_connections,
        acquire_timeout: Duration::from_secs(5),
    };

    let shutdown = CancellationToken::new();
    tokio::spawn(cancel_on_signal(shutdown.clone()));

    match cli.command {
        Command::Api { bind } => {
            let state =
                core_api::AppState::new(core_db::connect_app(&pool_config)?, cli.api.build(true)?)?;
            core_api::serve(bind, core_api::router(state), shutdown).await?;
        }
        Command::Serve { bind, worker } => {
            let config = cli.api.build(true)?;
            let mailer = worker.mailer(&config.public_base_url)?;
            let webhooks =
                core_integrations::http::WebhookClient::new(config.webhooks_allow_private)
                    .map_err(|e| anyhow::anyhow!("webhook client: {e}"))?;
            let state = core_api::AppState::new(core_db::connect_app(&pool_config)?, config)?;
            let worker = core_worker::Worker {
                app: state.clone(),
                queue: core_db::connect_worker(&pool_config)?,
                mailer,
                webhooks,
                settings: worker.settings()?,
            };
            let api = core_api::serve(bind, core_api::router(state), shutdown.clone());
            let (api, ()) = tokio::join!(api, core_worker::run(worker, shutdown.clone()));
            api?;
        }
        Command::Worker {
            health_bind,
            worker,
        } => {
            let config = cli.api.build(true)?;
            let mailer = worker.mailer(&config.public_base_url)?;
            let webhooks =
                core_integrations::http::WebhookClient::new(config.webhooks_allow_private)
                    .map_err(|e| anyhow::anyhow!("webhook client: {e}"))?;
            let db = core_db::connect_app(&pool_config)?;
            let worker = core_worker::Worker {
                app: core_api::AppState::new(db.clone(), config)?,
                queue: core_db::connect_worker(&pool_config)?,
                mailer,
                webhooks,
                settings: worker.settings()?,
            };
            let probes = core_api::health::standalone(db);
            let probes = core_api::serve(health_bind, probes, shutdown.clone());
            let (probes, ()) = tokio::join!(probes, core_worker::run(worker, shutdown.clone()));
            probes?;
        }
        Command::Migrate => core_db::migrate(&core_db::connect_owner(&pool_config)?).await?,
        Command::Admin(AdminCommand::Workspace(WorkspaceCommand::Create {
            slug,
            name,
            country,
            currency,
            timezone,
            owner_email,
            locale,
            plan,
            plugin_slug,
        })) => {
            let state = core_api::AppState::new(
                core_db::connect_app(&pool_config)?,
                cli.api.build(false)?,
            )?;
            let created = core_api::services::tenancy::create_workspace(
                &state,
                None,
                Default::default(),
                core_api::services::tenancy::NewWorkspace {
                    slug,
                    name,
                    country,
                    default_currency: currency,
                    default_timezone: timezone,
                    locale,
                    plan,
                    perspective: None,
                    plugin_slug,
                    owner_email,
                },
            )
            .await
            .map_err(|err| match err {
                core_api::error::ApiError::Internal(detail) => {
                    anyhow::anyhow!("internal error: {detail}")
                }
                err => err.into(),
            })?;
            println!(
                "workspace {} ({})",
                created.workspace.slug, created.workspace.id.0
            );
            println!("owner invitation: {}", created.owner_invitation_url);
        }
        Command::Admin(AdminCommand::Superadmin(cmd)) => {
            let (email, on) = match cmd {
                SuperadminCommand::Grant { email } => (email, true),
                SuperadminCommand::Revoke { email } => (email, false),
            };
            let owner = core_db::connect_owner(&pool_config)?;
            if !core_db::users::set_superadmin(&owner, &email, on).await? {
                anyhow::bail!("no user with email {email}; they must accept an invitation first");
            }
            tracing::warn!(%email, superadmin = on, "platform operator rights changed");
            println!(
                "{email}: superadmin {}",
                if on { "granted" } else { "revoked" }
            );
        }
        Command::Openapi | Command::Admin(AdminCommand::GenerateKey { .. }) => {
            unreachable!("handled above")
        }
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
