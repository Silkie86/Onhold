//! API settings from flags and the environment.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Args;
use core_api::Config;
use core_api::config::ProxyHeaders;
use core_api::config::{MailboxConfig, PdfConfig, StorageConfig};
use core_api::oidc::OidcProvider;
use core_domain::crypto::Keyring;
use core_integrations::mail::{LogMailer, Mailer, SmtpMailer};
use core_integrations::oauth::OAuthApp;
use core_integrations::storage::S3Config;
use core_integrations::stripe::{StripeClient, StripeConfig};
use url::Url;

#[derive(Debug, Args)]
pub struct ApiArgs {
    /// Where the web app is served. Links in emails and OIDC redirects are built from it.
    #[arg(
        long,
        env = "PUBLIC_BASE_URL",
        default_value = "http://localhost:5173",
        global = true
    )]
    public_base_url: Url,

    /// Mark cookies `Secure`. Only turn off for plain-HTTP local development.
    #[arg(long, env = "COOKIE_SECURE", default_value_t = true, action = clap::ArgAction::Set, global = true)]
    cookie_secure: bool,

    /// Where the client IP comes from: `false` (the connection), `true` (the
    /// last `X-Forwarded-For` entry, behind a proxy that appends to it) or
    /// `cloudflare` (`CF-Connecting-IP`, when every request comes through Cloudflare).
    #[arg(long, env = "TRUST_PROXY_HEADERS", default_value = "false", value_parser = proxy_headers, global = true)]
    trust_proxy_headers: ProxyHeaders,

    /// Encryption keys for secrets at rest, `id:base64key[,id:base64key…]`.
    /// The first encrypts; all decrypt, so rotate by prepending a new key.
    /// Generate one with `onhold admin generate-key`.
    #[arg(long, env = "ENCRYPTION_KEYS", hide_env_values = true, global = true)]
    encryption_keys: Option<String>,

    /// Session lifetime, in days.
    #[arg(long, env = "SESSION_TTL_DAYS", default_value_t = 30, global = true)]
    session_ttl_days: u64,

    /// Lifetime of the desktop and mobile apps' sessions, in days.
    #[arg(
        long,
        env = "APP_SESSION_TTL_DAYS",
        default_value_t = 90,
        global = true
    )]
    app_session_ttl_days: u64,

    /// Plans whose workspaces may use the desktop and mobile apps,
    /// comma-separated. Unset: every plan. The website is always available.
    #[arg(long, env = "APP_PLANS", value_delimiter = ',', global = true)]
    app_plans: Option<Vec<String>>,

    /// More origins allowed to call the API with bearer tokens, comma-separated,
    /// beside the apps' own (`tauri://localhost`, `http(s)://tauri.localhost`).
    /// For example a development server of the app on another port.
    #[arg(long, env = "CORS_ORIGINS", value_delimiter = ',', global = true)]
    cors_origins: Vec<String>,

    #[arg(long, env = "OIDC_GOOGLE_CLIENT_ID", global = true)]
    google_client_id: Option<String>,
    #[arg(
        long,
        env = "OIDC_GOOGLE_CLIENT_SECRET",
        hide_env_values = true,
        global = true
    )]
    google_client_secret: Option<String>,

    #[arg(long, env = "OIDC_MICROSOFT_CLIENT_ID", global = true)]
    microsoft_client_id: Option<String>,
    #[arg(
        long,
        env = "OIDC_MICROSOFT_CLIENT_SECRET",
        hide_env_values = true,
        global = true
    )]
    microsoft_client_secret: Option<String>,
    /// `organizations` (any work or school account), `common`, or one tenant id.
    #[arg(
        long,
        env = "OIDC_MICROSOFT_TENANT",
        default_value = "organizations",
        global = true
    )]
    microsoft_tenant: String,

    /// Let webhooks reach private and internal addresses. Local development only.
    #[arg(long, env = "WEBHOOKS_ALLOW_PRIVATE", default_value_t = false, action = clap::ArgAction::Set, global = true)]
    webhooks_allow_private: bool,

    /// Where uploaded files and printed PDFs are kept: `database` (in
    /// Postgres), `directory` (under STORAGE_DIR) or `s3` (an S3-compatible
    /// bucket such as Cloudflare R2, set with the S3_* variables).
    #[arg(long, env = "STORAGE", default_value = "database", global = true)]
    storage: String,
    #[arg(long, env = "STORAGE_DIR", global = true)]
    storage_dir: Option<std::path::PathBuf>,
    /// `https://<account>.r2.cloudflarestorage.com`, `https://s3.<region>.amazonaws.com`…
    #[arg(long, env = "S3_ENDPOINT", global = true)]
    s3_endpoint: Option<Url>,
    #[arg(long, env = "S3_BUCKET", global = true)]
    s3_bucket: Option<String>,
    /// `auto` for R2.
    #[arg(long, env = "S3_REGION", default_value = "auto", global = true)]
    s3_region: String,
    #[arg(long, env = "S3_ACCESS_KEY_ID", global = true)]
    s3_access_key_id: Option<String>,
    #[arg(
        long,
        env = "S3_SECRET_ACCESS_KEY",
        hide_env_values = true,
        global = true
    )]
    s3_secret_access_key: Option<String>,

    /// How documents become PDFs: `auto` (Chromium if installed, else a
    /// plain layout), `chromium` (CHROMIUM_PATH, required) or `plain`.
    #[arg(long, env = "PDF_RENDERER", default_value = "auto", global = true)]
    pdf_renderer: String,
    #[arg(long, env = "CHROMIUM_PATH", global = true)]
    chromium_path: Option<std::path::PathBuf>,
    /// Run Chromium without its sandbox. Only where the container cannot
    /// give it one (it still has no network and no file access).
    #[arg(long, env = "CHROMIUM_NO_SANDBOX", default_value_t = false, action = clap::ArgAction::Set, global = true)]
    chromium_no_sandbox: bool,

    /// The largest file one upload may be, in megabytes.
    #[arg(long, env = "MAX_UPLOAD_MB", default_value_t = 25, global = true)]
    max_upload_mb: usize,

    /// The platform's Stripe secret key, for card payments through Stripe
    /// Connect. Set it with STRIPE_CLIENT_ID and STRIPE_WEBHOOK_SECRET, or none.
    #[arg(long, env = "STRIPE_SECRET_KEY", hide_env_values = true, global = true)]
    stripe_secret_key: Option<String>,
    /// The platform's Connect client id (`ca_…`).
    #[arg(long, env = "STRIPE_CLIENT_ID", global = true)]
    stripe_client_id: Option<String>,
    /// The signing secret of the Connect webhook endpoint
    /// (`<api>/api/v1/webhooks/stripe`, listening to connected accounts).
    #[arg(
        long,
        env = "STRIPE_WEBHOOK_SECRET",
        hide_env_values = true,
        global = true
    )]
    stripe_webhook_secret: Option<String>,
    /// OnHold's fee on each card payment, in hundredths of a percent (150 = 1.5%).
    #[arg(
        long,
        env = "STRIPE_APPLICATION_FEE_BPS",
        default_value_t = 0,
        global = true
    )]
    stripe_application_fee_bps: u32,
    /// Stripe's API, for a mock in tests.
    #[arg(long, env = "STRIPE_API_BASE", default_value = core_integrations::stripe::API_BASE, global = true, hide = true)]
    stripe_api_base: Url,
    #[arg(long, env = "STRIPE_CONNECT_BASE", default_value = core_integrations::stripe::CONNECT_BASE, global = true, hide = true)]
    stripe_connect_base: Url,

    /// How often each connected mailbox is read, in seconds.
    #[arg(long, env = "MAIL_POLL_SECONDS", default_value_t = 120, global = true)]
    mail_poll_seconds: u64,
    /// How far back the first read of a newly connected mailbox goes, in days.
    #[arg(
        long,
        env = "MAIL_FIRST_READ_DAYS",
        default_value_t = 30,
        global = true
    )]
    mail_first_read_days: i64,
    /// Google's and Microsoft's endpoints, for stand-ins in tests.
    #[arg(long, env = "GOOGLE_OAUTH_BASE", global = true, hide = true)]
    google_oauth_base: Option<Url>,
    #[arg(long, env = "GMAIL_API_BASE", default_value = core_integrations::gmail::API_BASE, global = true, hide = true)]
    gmail_api_base: Url,
    #[arg(long, env = "MICROSOFT_LOGIN_BASE", default_value = core_integrations::oauth::MICROSOFT_LOGIN, global = true, hide = true)]
    microsoft_login_base: Url,
    #[arg(long, env = "GRAPH_API_BASE", default_value = core_integrations::graph::API_BASE, global = true, hide = true)]
    graph_api_base: Url,
}

fn proxy_headers(value: &str) -> Result<ProxyHeaders, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "false" | "" => Ok(ProxyHeaders::Off),
        "true" => Ok(ProxyHeaders::Forwarded),
        "cloudflare" => Ok(ProxyHeaders::Cloudflare),
        _ => Err("use false, true or cloudflare".to_owned()),
    }
}

/// Worker settings.
#[derive(Debug, Args)]
pub struct WorkerArgs {
    /// Outgoing mail server, e.g. `smtp://user:pass@smtp.example.com:587?tls=required`
    /// or `smtp://localhost:1025` for Mailpit. Unset: emails are only logged,
    /// links removed, and only allowed while `PUBLIC_BASE_URL` is plain HTTP.
    #[arg(long, env = "SMTP_URL", hide_env_values = true)]
    smtp_url: Option<String>,

    /// The sender of every email, e.g. `OnHold <no-reply@example.com>`.
    #[arg(long, env = "MAIL_FROM", default_value = "OnHold <no-reply@localhost>")]
    mail_from: String,

    /// Jobs this process runs at the same time.
    #[arg(long, env = "WORKER_CONCURRENCY", default_value_t = 8)]
    concurrency: usize,

    /// Fetch the European Central Bank's daily reference exchange rates.
    /// Off: invoices in another currency need a rate typed in.
    #[arg(long, env = "FX_RATES", default_value_t = true, action = clap::ArgAction::Set)]
    fx_rates: bool,
}

impl WorkerArgs {
    /// The mail server. A deployment on https must have one: without it,
    /// sign-in and invitation emails would never arrive.
    pub fn mailer(&self, public_base_url: &Url) -> anyhow::Result<Arc<dyn Mailer>> {
        Ok(match &self.smtp_url {
            Some(url) => Arc::new(
                SmtpMailer::from_url(url, &self.mail_from).map_err(|e| anyhow::anyhow!("{e}"))?,
            ),
            None if public_base_url.scheme() == "https" => anyhow::bail!(
                "SMTP_URL is not set while PUBLIC_BASE_URL is https; set the mail server to send sign-in and invitation emails"
            ),
            None => {
                tracing::warn!("SMTP_URL is not set; emails will be written to the log, not sent");
                Arc::new(LogMailer)
            }
        })
    }

    pub fn settings(&self) -> anyhow::Result<core_worker::Settings> {
        anyhow::ensure!(
            (1..=256).contains(&self.concurrency),
            "WORKER_CONCURRENCY must be 1 to 256"
        );
        Ok(core_worker::Settings {
            concurrency: self.concurrency,
            fx_rates: self.fx_rates.then(core_worker::FxFeeds::default),
            ..Default::default()
        })
    }
}

impl ApiArgs {
    /// Builds the API configuration. Without `ENCRYPTION_KEYS`, `require_keys`
    /// decides between failing (serving the API) and an ephemeral key (tools
    /// that never read stored secrets).
    pub fn build(self, require_keys: bool) -> anyhow::Result<Config> {
        let keyring = match self.encryption_keys.as_deref() {
            Some(spec) => Keyring::parse(spec).context("ENCRYPTION_KEYS")?,
            None if require_keys => {
                anyhow::bail!(
                    "ENCRYPTION_KEYS is not set; generate one with `onhold admin generate-key`"
                )
            }
            None => Keyring::ephemeral(),
        };

        // Mailboxes are connected through the same OAuth clients as sign-in.
        let mut mailboxes = MailboxConfig {
            gmail_api: self.gmail_api_base,
            graph_api: self.graph_api_base,
            poll_every: Duration::from_secs(self.mail_poll_seconds.clamp(30, 3600)),
            first_read_days: self.mail_first_read_days.clamp(0, 365),
            ..MailboxConfig::default()
        };
        if let (Some(id), Some(secret)) = (&self.google_client_id, &self.google_client_secret) {
            let app = OAuthApp::google(id.clone(), secret.clone());
            mailboxes.google = Some(match &self.google_oauth_base {
                Some(base) => app.at(base),
                None => app,
            });
        }
        if let (Some(id), Some(secret)) = (&self.microsoft_client_id, &self.microsoft_client_secret)
        {
            mailboxes.microsoft = Some(OAuthApp::microsoft_at(
                id.clone(),
                secret.clone(),
                &self.microsoft_tenant,
                self.microsoft_login_base.as_str(),
            ));
        }

        let mut oidc = Vec::new();
        match (self.google_client_id, self.google_client_secret) {
            (Some(id), Some(secret)) => oidc.push(OidcProvider::google(id, secret)),
            (None, None) => {}
            _ => anyhow::bail!(
                "set both OIDC_GOOGLE_CLIENT_ID and OIDC_GOOGLE_CLIENT_SECRET, or neither"
            ),
        }
        match (self.microsoft_client_id, self.microsoft_client_secret) {
            (Some(id), Some(secret)) => {
                oidc.push(OidcProvider::microsoft(id, secret, &self.microsoft_tenant))
            }
            (None, None) => {}
            _ => anyhow::bail!(
                "set both OIDC_MICROSOFT_CLIENT_ID and OIDC_MICROSOFT_CLIENT_SECRET, or neither"
            ),
        }

        if !self.cookie_secure && self.public_base_url.scheme() == "https" {
            tracing::warn!(
                "COOKIE_SECURE is off while PUBLIC_BASE_URL is https; session cookies will travel without the Secure flag"
            );
        }

        let mut cors_origins: Vec<String> = core_api::config::APP_ORIGINS
            .iter()
            .map(|o| (*o).to_owned())
            .collect();
        for origin in self
            .cors_origins
            .iter()
            .map(|o| o.trim())
            .filter(|o| !o.is_empty())
        {
            let parsed = Url::parse(origin).with_context(|| format!("CORS_ORIGINS: {origin}"))?;
            anyhow::ensure!(
                parsed.origin().ascii_serialization() == origin.trim_end_matches('/'),
                "CORS_ORIGINS: {origin} is not an origin (scheme://host[:port], no path)"
            );
            cors_origins.push(origin.trim_end_matches('/').to_owned());
        }
        let app_plans = self.app_plans.map(|plans| {
            plans
                .into_iter()
                .map(|p| p.trim().to_owned())
                .filter(|p| !p.is_empty())
                .collect()
        });

        let storage = match self.storage.as_str() {
            "database" => StorageConfig::Database,
            "directory" => StorageConfig::Directory(
                self.storage_dir
                    .context("STORAGE=directory needs STORAGE_DIR")?,
            ),
            "s3" => StorageConfig::S3(S3Config {
                endpoint: self.s3_endpoint.context("STORAGE=s3 needs S3_ENDPOINT")?,
                bucket: self.s3_bucket.context("STORAGE=s3 needs S3_BUCKET")?,
                region: self.s3_region,
                access_key_id: self
                    .s3_access_key_id
                    .context("STORAGE=s3 needs S3_ACCESS_KEY_ID")?,
                secret_access_key: self
                    .s3_secret_access_key
                    .context("STORAGE=s3 needs S3_SECRET_ACCESS_KEY")?,
            }),
            other => anyhow::bail!("STORAGE must be database, directory or s3, not `{other}`"),
        };
        let no_sandbox = self.chromium_no_sandbox;
        let pdf = match self.pdf_renderer.as_str() {
            "auto" => PdfConfig::Auto { no_sandbox },
            "chromium" => PdfConfig::Chromium {
                path: self
                    .chromium_path
                    .context("PDF_RENDERER=chromium needs CHROMIUM_PATH")?,
                no_sandbox,
            },
            "plain" => PdfConfig::Plain,
            other => anyhow::bail!("PDF_RENDERER must be auto, chromium or plain, not `{other}`"),
        };
        anyhow::ensure!(
            (1..=100).contains(&self.max_upload_mb),
            "MAX_UPLOAD_MB must be 1 to 100"
        );
        let stripe = match (
            self.stripe_secret_key,
            self.stripe_client_id,
            self.stripe_webhook_secret,
        ) {
            (Some(key), Some(client_id), Some(webhook_secret)) => {
                anyhow::ensure!(
                    self.stripe_application_fee_bps <= 2000,
                    "STRIPE_APPLICATION_FEE_BPS must be 0 to 2000"
                );
                let config = StripeConfig {
                    api_base: self.stripe_api_base,
                    connect_base: self.stripe_connect_base,
                    application_fee_bps: self.stripe_application_fee_bps,
                    ..StripeConfig::new(key, client_id, webhook_secret)
                };
                Some(StripeClient::new(config).map_err(|e| anyhow::anyhow!("{e}"))?)
            }
            (None, None, None) => None,
            _ => anyhow::bail!(
                "set all of STRIPE_SECRET_KEY, STRIPE_CLIENT_ID and STRIPE_WEBHOOK_SECRET, or none"
            ),
        };

        Ok(Config {
            storage,
            pdf,
            max_upload_bytes: self.max_upload_mb * 1024 * 1024,
            cookie_secure: self.cookie_secure,
            trust_proxy_headers: self.trust_proxy_headers,
            session_ttl: Duration::from_secs(self.session_ttl_days * 24 * 3600),
            app_session_ttl: Duration::from_secs(self.app_session_ttl_days * 24 * 3600),
            app_plans,
            cors_origins,
            oidc,
            webhooks_allow_private: self.webhooks_allow_private,
            stripe,
            mailboxes,
            ..Config::new(self.public_base_url, keyring)
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        api: ApiArgs,
    }

    fn args(extra: &[&str]) -> ApiArgs {
        let mut argv = vec!["onhold"];
        argv.extend_from_slice(extra);
        Cli::try_parse_from(argv).expect("parses").api
    }

    const KEY: &str = "--encryption-keys=k1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    #[test]
    fn the_api_needs_encryption_keys() {
        assert!(args(&[]).build(true).is_err());
        assert!(args(&[]).build(false).is_ok());
        assert!(args(&[KEY]).build(true).is_ok());
        assert!(args(&["--encryption-keys=k1:short"]).build(true).is_err());
    }

    #[test]
    fn proxy_headers_are_trusted_only_when_named() {
        let mode = |extra: &[&str]| {
            let mut all = vec![KEY];
            all.extend_from_slice(extra);
            args(&all).build(true).expect("config").trust_proxy_headers
        };
        assert_eq!(mode(&[]), ProxyHeaders::Off);
        assert_eq!(mode(&["--trust-proxy-headers=false"]), ProxyHeaders::Off);
        assert_eq!(
            mode(&["--trust-proxy-headers=true"]),
            ProxyHeaders::Forwarded
        );
        assert_eq!(
            mode(&["--trust-proxy-headers=cloudflare"]),
            ProxyHeaders::Cloudflare
        );
        assert!(Cli::try_parse_from(["onhold", KEY, "--trust-proxy-headers=yes"]).is_err());
    }

    // The SMTP transport's connection pool needs a Tokio runtime, even to drop.
    #[tokio::test]
    async fn a_deployment_on_https_needs_a_mail_server() {
        #[derive(Parser)]
        struct WorkerCli {
            #[command(flatten)]
            worker: WorkerArgs,
        }
        let worker = |extra: &[&str]| {
            let mut argv = vec!["onhold"];
            argv.extend_from_slice(extra);
            WorkerCli::try_parse_from(argv).expect("parses").worker
        };
        let https = Url::parse("https://app.example.com").expect("url");
        let http = Url::parse("http://localhost:5173").expect("url");
        assert!(worker(&[]).mailer(&https).is_err());
        assert!(worker(&[]).mailer(&http).is_ok());
        assert!(
            worker(&["--smtp-url=smtp://localhost:1025"])
                .mailer(&https)
                .is_ok()
        );
    }

    #[test]
    fn oidc_providers_need_both_halves() {
        assert!(args(&[KEY, "--google-client-id=x"]).build(true).is_err());
        let config = args(&[
            KEY,
            "--google-client-id=x",
            "--google-client-secret=y",
            "--microsoft-client-id=a",
            "--microsoft-client-secret=b",
        ])
        .build(true)
        .expect("config");
        let names: Vec<&str> = config.oidc.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["google", "microsoft"]);
    }

    #[test]
    fn cookies_are_secure_unless_turned_off() {
        assert!(args(&[KEY]).build(true).expect("config").cookie_secure);
        assert!(
            !args(&[KEY, "--cookie-secure=false"])
                .build(true)
                .expect("config")
                .cookie_secure
        );
    }

    #[test]
    fn the_apps_origins_are_always_allowed_and_more_can_be_added() {
        let config = args(&[KEY]).build(true).expect("config");
        assert_eq!(config.cors_origins, core_api::config::APP_ORIGINS);
        assert!(config.apps_allowed("anything"));

        let config = args(&[
            KEY,
            "--cors-origins=http://localhost:5174/",
            "--app-plans=pro, team",
        ])
        .build(true)
        .expect("config");
        assert_eq!(
            config.cors_origins.last().map(String::as_str),
            Some("http://localhost:5174")
        );
        assert!(config.apps_allowed("team"));
        assert!(!config.apps_allowed("standard"));

        assert!(
            args(&[KEY, "--cors-origins=http://localhost:5174/app"])
                .build(true)
                .is_err()
        );
        assert!(
            args(&[KEY, "--cors-origins=not a url"])
                .build(true)
                .is_err()
        );
    }
}
