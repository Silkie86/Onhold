//! Structured logging. OpenTelemetry export is added when there is a
//! collector to send to (see MILESTONE-0.md).

use clap::{Args, ValueEnum};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LogFormat {
    /// One JSON object per line, for production log shipping.
    Json,
    /// Human-readable, for local development.
    Pretty,
}

#[derive(Debug, Args)]
pub struct TelemetryArgs {
    #[arg(long, env = "LOG_FORMAT", value_enum, default_value_t = LogFormat::Json, global = true)]
    log_format: LogFormat,
}

/// Installs the global subscriber. Levels come from `RUST_LOG`, defaulting to info.
pub fn init(args: &TelemetryArgs) -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match args.log_format {
        LogFormat::Json => builder
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(false)
            .try_init(),
        LogFormat::Pretty => builder.try_init(),
    }
    .map_err(|err| anyhow::anyhow!("failed to install tracing subscriber: {err}"))
}
