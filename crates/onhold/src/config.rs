//! API settings from flags and the environment.

use std::time::Duration;

use anyhow::Context;
use clap::Args;
use core_api::Config;
use core_api::oidc::OidcProvider;
use core_domain::crypto::Keyring;
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

    /// Take the client IP from `X-Forwarded-For`. Only behind a proxy that sets it.
    #[arg(long, env = "TRUST_PROXY_HEADERS", default_value_t = false, action = clap::ArgAction::Set, global = true)]
    trust_proxy_headers: bool,

    /// Encryption keys for secrets at rest, `id:base64key[,id:base64key…]`.
    /// The first encrypts; all decrypt, so rotate by prepending a new key.
    /// Generate one with `onhold admin generate-key`.
    #[arg(long, env = "ENCRYPTION_KEYS", hide_env_values = true, global = true)]
    encryption_keys: Option<String>,

    /// Session lifetime, in days.
    #[arg(long, env = "SESSION_TTL_DAYS", default_value_t = 30, global = true)]
    session_ttl_days: u64,

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

        Ok(Config {
            cookie_secure: self.cookie_secure,
            trust_proxy_headers: self.trust_proxy_headers,
            session_ttl: Duration::from_secs(self.session_ttl_days * 24 * 3600),
            oidc,
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
}
