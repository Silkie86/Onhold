//! OAuth 2 authorization-code flows for the providers people connect their
//! own accounts from: Google and Microsoft for mail and calendars (Xero and
//! QuickBooks follow the same shape). OnHold keeps the refresh token,
//! encrypted, and the worker refreshes access tokens as they run out.

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use url::Url;

use crate::provider::{self, ProviderError};

pub const GOOGLE_AUTHORIZE: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_TOKEN: &str = "https://oauth2.googleapis.com/token";
pub const GOOGLE_REVOKE: &str = "https://oauth2.googleapis.com/revoke";
pub const MICROSOFT_LOGIN: &str = "https://login.microsoftonline.com";

/// Reading and sending a person's Gmail.
pub const GOOGLE_MAIL_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.send",
];
/// Reading and sending a person's Outlook mail. `offline_access` is what
/// makes Microsoft hand out a refresh token.
pub const MICROSOFT_MAIL_SCOPES: &[&str] =
    &["offline_access", "User.Read", "Mail.Read", "Mail.Send"];
/// A shared mailbox the person has access to, read and sent from as itself.
pub const MICROSOFT_SHARED_MAIL_SCOPES: &[&str] = &["Mail.Read.Shared", "Mail.Send.Shared"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Google,
    Microsoft,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Microsoft => "microsoft",
        }
    }
}

/// One OAuth client registered with a provider.
#[derive(Debug, Clone)]
pub struct OAuthApp {
    pub provider: Provider,
    pub client_id: String,
    pub client_secret: String,
    pub authorize_url: Url,
    pub token_url: Url,
    /// Where a token is revoked on disconnect, if the provider has one.
    pub revoke_url: Option<Url>,
}

impl OAuthApp {
    pub fn google(client_id: String, client_secret: String) -> Self {
        #[allow(clippy::expect_used)]
        Self {
            provider: Provider::Google,
            client_id,
            client_secret,
            authorize_url: Url::parse(GOOGLE_AUTHORIZE).expect("static URL"),
            token_url: Url::parse(GOOGLE_TOKEN).expect("static URL"),
            revoke_url: Some(Url::parse(GOOGLE_REVOKE).expect("static URL")),
        }
    }

    /// `tenant` is `common`, `organizations` or a tenant id, as for sign-in.
    pub fn microsoft(client_id: String, client_secret: String, tenant: &str) -> Self {
        Self::microsoft_at(client_id, client_secret, tenant, MICROSOFT_LOGIN)
    }

    /// Like [`Self::microsoft`], against another login host (a stand-in in tests).
    pub fn microsoft_at(
        client_id: String,
        client_secret: String,
        tenant: &str,
        login: &str,
    ) -> Self {
        let base = login.trim_end_matches('/');
        let tenant = provider::segment(tenant);
        #[allow(clippy::expect_used)]
        let url =
            |p: &str| Url::parse(&format!("{base}/{tenant}/oauth2/v2.0/{p}")).expect("a base URL");
        Self {
            provider: Provider::Microsoft,
            client_id,
            client_secret,
            authorize_url: url("authorize"),
            token_url: url("token"),
            // Microsoft has no token revocation for delegated grants; the
            // person removes the app from their account page.
            revoke_url: None,
        }
    }

    /// Points every endpoint at `base` (a stand-in server in tests),
    /// keeping their paths.
    pub fn at(mut self, base: &Url) -> Self {
        let rebase = |u: &Url| provider::endpoint(base, u.path());
        self.authorize_url = rebase(&self.authorize_url);
        self.token_url = rebase(&self.token_url);
        self.revoke_url = self.revoke_url.as_ref().map(rebase);
        self
    }

    /// Where to send the person to grant `scopes`.
    pub fn authorize(
        &self,
        state: &str,
        redirect_uri: &str,
        scopes: &[&str],
        login_hint: Option<&str>,
    ) -> Url {
        let mut url = self.authorize_url.clone();
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("response_type", "code")
                .append_pair("client_id", &self.client_id)
                .append_pair("redirect_uri", redirect_uri)
                .append_pair("scope", &scopes.join(" "))
                .append_pair("state", state);
            match self.provider {
                // A refresh token only comes with offline access, and only
                // on a consent screen (not a silent re-grant).
                Provider::Google => {
                    q.append_pair("access_type", "offline")
                        .append_pair("prompt", "consent")
                        .append_pair("include_granted_scopes", "true");
                }
                Provider::Microsoft => {
                    q.append_pair("response_mode", "query")
                        .append_pair("prompt", "select_account");
                }
            }
            if let Some(hint) = login_hint {
                q.append_pair("login_hint", hint);
            }
        }
        url
    }

    /// Like [`Self::authorize`], but the grant carries only these scopes,
    /// never others the person gave this app before.
    pub fn authorize_exact(&self, state: &str, redirect_uri: &str, scopes: &[&str]) -> Url {
        let mut url = self.authorize(state, redirect_uri, scopes, None);
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| k != "include_granted_scopes")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.query_pairs_mut().clear().extend_pairs(kept);
        url
    }

    /// Exchanges the code the provider sent back.
    pub async fn exchange(
        &self,
        http: &reqwest::Client,
        code: &str,
        redirect_uri: &str,
    ) -> Result<Tokens, ProviderError> {
        self.token(
            http,
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
            ],
        )
        .await
    }

    /// A new access token. Some providers also return a new refresh token,
    /// which then replaces the old one.
    pub async fn refresh(
        &self,
        http: &reqwest::Client,
        refresh_token: &str,
    ) -> Result<Tokens, ProviderError> {
        self.token(
            http,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
        )
        .await
    }

    /// Withdraws OnHold's access, where the provider allows it. A token
    /// the provider no longer knows is fine.
    pub async fn revoke(&self, http: &reqwest::Client, token: &str) -> Result<(), ProviderError> {
        let Some(url) = &self.revoke_url else {
            return Ok(());
        };
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("token", token)
            .finish();
        let req = http
            .post(url.clone())
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body);
        match provider::send_raw(req).await {
            Ok(_) | Err(ProviderError::Refused { status: 400, .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn token(
        &self,
        http: &reqwest::Client,
        form: &[(&str, &str)],
    ) -> Result<Tokens, ProviderError> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .append_pair("client_id", &self.client_id)
            .append_pair("client_secret", &self.client_secret)
            .finish();
        let req = http
            .post(self.token_url.clone())
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept", "application/json")
            .body(body);
        let raw: RawTokens = match provider::send_json(req).await {
            // A token endpoint's 401 is a bad client or grant, not a stale
            // access token: refreshing again cannot help.
            Err(ProviderError::Unauthorized) => {
                return Err(ProviderError::Revoked(
                    "the provider refused OnHold's credentials".into(),
                ));
            }
            other => other?,
        };
        Ok(raw.into_tokens(Utc::now()))
    }
}

#[derive(Debug, Deserialize)]
struct RawTokens {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

impl RawTokens {
    fn into_tokens(self, now: DateTime<Utc>) -> Tokens {
        Tokens {
            access_token: self.access_token,
            refresh_token: self.refresh_token.filter(|t| !t.is_empty()),
            // An hour is what both providers give when they do not say.
            expires_at: now + Duration::seconds(self.expires_in.unwrap_or(3600).clamp(60, 86_400)),
            scopes: self
                .scope
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
        }
    }
}

/// What a token endpoint returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
    /// What the person actually granted, which may be less than asked.
    pub scopes: Vec<String>,
}

impl Tokens {
    /// Whether the access token runs out within the next minute.
    pub fn expiring(&self, now: DateTime<Utc>) -> bool {
        self.expires_at - Duration::seconds(60) <= now
    }
}

/// Whether every scope in `wanted` was granted. Microsoft lists the scopes
/// it granted without `offline_access` and sometimes with a resource
/// prefix, and in any case, so compare loosely.
pub fn granted(scopes: &[String], wanted: &[&str]) -> bool {
    let norm = |s: &str| s.rsplit('/').next().unwrap_or(s).to_ascii_lowercase();
    let have: Vec<String> = scopes.iter().map(|s| norm(s)).collect();
    wanted
        .iter()
        .filter(|w| !matches!(**w, "offline_access" | "openid" | "email" | "profile"))
        .all(|w| {
            // Google lists full URLs, so compare those whole.
            if w.starts_with("https://") {
                scopes.iter().any(|s| s == w)
            } else {
                have.contains(&norm(w))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_urls_ask_for_offline_access() {
        let g = OAuthApp::google("cid".into(), "secret".into());
        let url = g.authorize(
            "st",
            "https://app.test/cb",
            GOOGLE_MAIL_SCOPES,
            Some("ana@x.test"),
        );
        let q: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("access_type"), Some("offline"));
        assert_eq!(get("prompt"), Some("consent"));
        assert_eq!(get("state"), Some("st"));
        assert_eq!(get("login_hint"), Some("ana@x.test"));
        assert!(get("scope").is_some_and(|s| s.contains("gmail.send")));
        assert!(!url.as_str().contains("secret"));
        assert_eq!(get("include_granted_scopes"), Some("true"));
        let exact = g.authorize_exact("st", "https://app.test/cb", &["openid"]);
        assert!(!exact.as_str().contains("include_granted_scopes"));
        assert!(exact.as_str().contains("access_type=offline"));

        let m = OAuthApp::microsoft("cid".into(), "s".into(), "common");
        assert_eq!(
            m.token_url.as_str(),
            "https://login.microsoftonline.com/common/oauth2/v2.0/token"
        );
        let base = Url::parse("http://127.0.0.1:1/ms/").expect("static URL");
        assert_eq!(
            m.at(&base).token_url.as_str(),
            "http://127.0.0.1:1/ms/common/oauth2/v2.0/token"
        );
    }

    #[test]
    fn tokens_expire_a_minute_early() {
        let now = Utc::now();
        let t = RawTokens {
            access_token: "a".into(),
            refresh_token: Some(String::new()),
            expires_in: Some(3599),
            scope: Some("Mail.Read Mail.Send".into()),
        }
        .into_tokens(now);
        assert_eq!(t.refresh_token, None, "an empty refresh token is none");
        assert!(!t.expiring(now));
        assert!(t.expiring(now + Duration::seconds(3550)));
    }

    #[test]
    fn granted_scopes_compare_loosely() {
        let ms = vec![
            "https://graph.microsoft.com/Mail.Read".to_owned(),
            "https://graph.microsoft.com/mail.send".to_owned(),
            "User.Read".to_owned(),
        ];
        assert!(granted(&ms, MICROSOFT_MAIL_SCOPES));
        assert!(!granted(&ms, MICROSOFT_SHARED_MAIL_SCOPES));
        let g = vec![GOOGLE_MAIL_SCOPES[0].to_owned()];
        assert!(!granted(&g, GOOGLE_MAIL_SCOPES), "send was not granted");
    }
}
