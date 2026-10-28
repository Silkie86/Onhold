//! API configuration, built by the binary from the environment.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use core_domain::crypto::Keyring;
use core_integrations::oauth::OAuthApp;
use core_integrations::storage::S3Config;
use core_integrations::stripe::StripeClient;
use url::Url;

use crate::oidc::OidcProvider;

/// Where Tauri serves the app from: `tauri://localhost` on macOS, iOS and
/// Linux, `http(s)://tauri.localhost` on Windows and Android.
pub const APP_ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

/// Where files' bytes are kept.
#[derive(Debug, Clone)]
pub enum StorageConfig {
    /// In Postgres, next to everything else: nothing more to run or back up.
    Database,
    /// Under a local directory (a mounted volume).
    Directory(PathBuf),
    /// An S3-compatible bucket: Cloudflare R2, AWS S3, MinIO.
    S3(S3Config),
}

/// How documents become PDFs.
#[derive(Debug, Clone)]
pub enum PdfConfig {
    /// Chromium if one can be found, plain text otherwise.
    Auto {
        no_sandbox: bool,
    },
    Chromium {
        path: PathBuf,
        no_sandbox: bool,
    },
    /// The plain-text layout only (tests, and machines without a browser).
    Plain,
}

/// Which proxy header, if any, names the client's address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyHeaders {
    /// None: the address is the connection's peer.
    #[default]
    Off,
    /// The last `X-Forwarded-For` entry, the one our proxy appended. Entries
    /// before it are whatever the client sent.
    Forwarded,
    /// `CF-Connecting-IP`, which Cloudflare overwrites. Any other proxy
    /// passes on whatever the client sent in it.
    Cloudflare,
}

/// Connecting people's own mailboxes, and talent's own calendars through
/// the same OAuth clients.
#[derive(Debug, Clone)]
pub struct MailboxConfig {
    /// Gmail through Google's OAuth; `None` when Google is not set up.
    pub google: Option<OAuthApp>,
    /// Outlook and Microsoft 365 through Microsoft's OAuth.
    pub microsoft: Option<OAuthApp>,
    pub gmail_api: Url,
    pub graph_api: Url,
    /// Google's own APIs: the free/busy query and the account's address.
    pub google_api: Url,
    /// How often each mailbox is read.
    pub poll_every: Duration,
    /// How often each talent's connected calendar is read.
    pub calendar_every: Duration,
    /// How far back the first read of a new mailbox goes.
    pub first_read_days: i64,
    pub http: reqwest::Client,
}

impl Default for MailboxConfig {
    fn default() -> Self {
        #[allow(clippy::expect_used)]
        Self {
            google: None,
            microsoft: None,
            gmail_api: Url::parse(core_integrations::gmail::API_BASE).expect("static URL"),
            graph_api: Url::parse(core_integrations::graph::API_BASE).expect("static URL"),
            google_api: Url::parse(core_integrations::calendars::GOOGLE_API).expect("static URL"),
            poll_every: Duration::from_secs(120),
            calendar_every: Duration::from_secs(15 * 60),
            first_read_days: 30,
            http: core_integrations::provider::http_client().unwrap_or_default(),
        }
    }
}

/// Connecting a workspace's books: Xero and QuickBooks Online.
#[derive(Debug, Clone)]
pub struct AccountingConfig {
    /// `None` when Xero is not set up on this server.
    pub xero: Option<OAuthApp>,
    pub quickbooks: Option<OAuthApp>,
    pub xero_api: Url,
    /// Intuit's production API, or its sandbox while an app is being built.
    pub quickbooks_api: Url,
    pub http: reqwest::Client,
}

impl Default for AccountingConfig {
    fn default() -> Self {
        #[allow(clippy::expect_used)]
        Self {
            xero: None,
            quickbooks: None,
            xero_api: Url::parse(core_integrations::accounting::XERO_API).expect("static URL"),
            quickbooks_api: Url::parse(core_integrations::accounting::QUICKBOOKS_API)
                .expect("static URL"),
            http: core_integrations::provider::http_client().unwrap_or_default(),
        }
    }
}

/// Workspaces' own sender domains: the provider that verifies them, the
/// DNS lookups, and Domain Connect when OnHold's template is published.
#[derive(Debug, Clone)]
pub struct SenderDomainsConfig {
    pub resend: core_integrations::domains::Resend,
    pub doh: core_integrations::dns::Doh,
    pub domain_connect: Option<Arc<core_integrations::dns::DomainConnect>>,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Where the web app is served; links in emails and OIDC redirects are built from it.
    pub public_base_url: Url,
    /// Set the `Secure` flag on cookies. Only ever false for plain-HTTP local development.
    pub cookie_secure: bool,
    /// Where the client IP comes from behind a proxy.
    pub trust_proxy_headers: ProxyHeaders,
    pub session_ttl: Duration,
    /// Sessions of the desktop and mobile apps, which may be offline for days.
    pub app_session_ttl: Duration,
    /// How long an app's sign-in code waits for approval in the browser.
    pub app_sign_in_ttl: Duration,
    /// Origins allowed to call the API from another origin: the desktop and
    /// mobile apps' web views. They authenticate with bearer tokens, never cookies.
    pub cors_origins: Vec<String>,
    /// Plans whose workspaces may use the desktop and mobile apps; `None`
    /// allows every plan. The website is always available.
    pub app_plans: Option<Vec<String>>,
    /// How long a session may wait for its second factor before it is useless.
    pub mfa_pending_ttl: Duration,
    pub invitation_ttl: Duration,
    pub magic_link_ttl: Duration,
    pub impersonation_ttl: Duration,
    /// Failed sign-ins before an account is locked, and for how long.
    pub max_login_failures: i32,
    pub lockout: Duration,
    /// Wrong second-factor codes before a pending session is revoked.
    pub max_mfa_failures: i32,
    pub keyring: Keyring,
    pub oidc: Vec<OidcProvider>,
    /// Let webhooks reach private and local addresses. Development and
    /// tests only: in production it would let tenants probe the network.
    pub webhooks_allow_private: bool,
    pub storage: StorageConfig,
    pub pdf: PdfConfig,
    /// The largest file one upload may be.
    pub max_upload_bytes: usize,
    /// Stripe Connect, for paying invoices by card. `None`: invoices are
    /// paid by bank transfer only.
    pub stripe: Option<StripeClient>,
    pub mailboxes: MailboxConfig,
    pub accounting: AccountingConfig,
    /// Sending from workspaces' own domains; `None`: everything is sent
    /// from the platform's address.
    pub sender_domains: Option<SenderDomainsConfig>,
}

impl Config {
    /// Production defaults for everything but the base URL and keys.
    pub fn new(public_base_url: Url, keyring: Keyring) -> Self {
        Self {
            public_base_url,
            cookie_secure: true,
            trust_proxy_headers: ProxyHeaders::Off,
            session_ttl: Duration::from_secs(30 * 24 * 3600),
            app_session_ttl: Duration::from_secs(90 * 24 * 3600),
            app_sign_in_ttl: Duration::from_secs(10 * 60),
            cors_origins: APP_ORIGINS.iter().map(|o| (*o).to_owned()).collect(),
            app_plans: None,
            mfa_pending_ttl: Duration::from_secs(10 * 60),
            invitation_ttl: Duration::from_secs(7 * 24 * 3600),
            magic_link_ttl: Duration::from_secs(15 * 60),
            impersonation_ttl: Duration::from_secs(60 * 60),
            max_login_failures: 10,
            lockout: Duration::from_secs(15 * 60),
            max_mfa_failures: 5,
            keyring,
            oidc: Vec::new(),
            webhooks_allow_private: false,
            storage: StorageConfig::Database,
            pdf: PdfConfig::Auto { no_sandbox: false },
            max_upload_bytes: 25 * 1024 * 1024,
            stripe: None,
            mailboxes: MailboxConfig::default(),
            accounting: AccountingConfig::default(),
            sender_domains: None,
        }
    }

    /// Settings for tests and local development: localhost over plain HTTP,
    /// an ephemeral key, no OIDC.
    pub fn for_development() -> Self {
        #[allow(clippy::expect_used)]
        let url = Url::parse("http://localhost:5173").expect("static URL");
        Self {
            cookie_secure: false,
            webhooks_allow_private: true,
            pdf: PdfConfig::Plain,
            ..Self::new(url, Keyring::ephemeral())
        }
    }

    /// Whether a workspace on `plan` may be used from the desktop and mobile apps.
    pub fn apps_allowed(&self, plan: &str) -> bool {
        self.app_plans
            .as_ref()
            .is_none_or(|plans| plans.iter().any(|p| p == plan))
    }

    /// An absolute link for a same-site `path_and_query` that was already
    /// checked with [`crate::oidc::safe_redirect`].
    pub fn app_link(&self, path_and_query: &str) -> String {
        self.public_base_url
            .join(path_and_query)
            .map_or_else(|_| self.public_base_url.to_string(), |u| u.to_string())
    }

    /// An absolute link into the web app.
    pub fn app_url(&self, path: &str, query: &[(&str, &str)]) -> String {
        let mut url = self.public_base_url.clone();
        url.set_path(path);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        url.to_string()
    }
}
