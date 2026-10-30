//! Google and Microsoft sign-in (OpenID Connect, authorization code + PKCE).
//!
//! The flow's state (CSRF token, nonce, PKCE verifier, and what the user was
//! doing) travels in a short-lived cookie encrypted with the server keyring,
//! so no table is needed.
//!
//! Account resolution, in order:
//! 1. a known identity (provider + subject) signs in its user;
//! 2. a signed-in user who started the flow links the identity to themselves;
//! 3. an invitation token links (or creates) the account for the invited
//!    address, because holding the token proves the inbox is theirs;
//! 4. Google only: a verified email matching an existing account links to it.
//!
//! Microsoft's `email` claim is not verified (anyone controlling an Entra
//! tenant can set it), so it is never used to match accounts.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use core_domain::crypto::{Keyring, hash_token};
use core_domain::{UserId, authz::RequestMeta};
use data_encoding::BASE64URL_NOPAD;
use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreJsonWebKeySet, CoreProviderMetadata,
};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, reqwest,
};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::services::auth::{IssuedSession, issue_session, join_from_invitation};

pub const STATE_COOKIE: &str = "onhold_oidc";
const STATE_TTL_SECS: i64 = 600;
const STATE_AAD: &[u8] = b"oidc_state";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// A standard provider. `trust_verified_email` allows rule 4 above.
    Standard { trust_verified_email: bool },
    /// Microsoft's `common` / `organizations` endpoints, whose discovery
    /// document advertises a per-tenant issuer template.
    MicrosoftMultiTenant,
}

#[derive(Debug, Clone)]
pub struct OidcProvider {
    /// Path segment and stored provider name: `google`, `microsoft`.
    pub name: String,
    pub display_name: String,
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub kind: ProviderKind,
}

impl OidcProvider {
    pub fn google(client_id: String, client_secret: String) -> Self {
        Self {
            name: "google".into(),
            display_name: "Google".into(),
            issuer: "https://accounts.google.com".into(),
            client_id,
            client_secret,
            kind: ProviderKind::Standard {
                trust_verified_email: true,
            },
        }
    }

    /// `tenant` is `common`, `organizations`, or a specific tenant id.
    pub fn microsoft(client_id: String, client_secret: String, tenant: &str) -> Self {
        let multi = matches!(tenant, "common" | "organizations" | "consumers");
        Self {
            name: "microsoft".into(),
            display_name: "Microsoft".into(),
            issuer: format!("https://login.microsoftonline.com/{tenant}/v2.0"),
            client_id,
            client_secret,
            kind: if multi {
                ProviderKind::MicrosoftMultiTenant
            } else {
                ProviderKind::Standard {
                    trust_verified_email: false,
                }
            },
        }
    }
}

/// Configured providers with their discovery documents, fetched once.
#[derive(Default)]
pub struct Registry {
    providers: HashMap<String, (OidcProvider, OnceCell<CoreProviderMetadata>)>,
    http: Option<reqwest::Client>,
}

impl Registry {
    pub fn new(providers: Vec<OidcProvider>) -> ApiResult<Self> {
        let http = reqwest::ClientBuilder::new()
            // Following redirects from an identity provider would open SSRF paths.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(ApiError::internal)?;
        Ok(Self {
            providers: providers
                .into_iter()
                .map(|p| (p.name.clone(), (p, OnceCell::new())))
                .collect(),
            http: Some(http),
        })
    }

    pub fn names(&self) -> Vec<(String, String)> {
        let mut v: Vec<_> = self
            .providers
            .values()
            .map(|(p, _)| (p.name.clone(), p.display_name.clone()))
            .collect();
        v.sort();
        v
    }

    fn http(&self) -> ApiResult<&reqwest::Client> {
        self.http
            .as_ref()
            .ok_or_else(|| ApiError::internal("OIDC HTTP client missing"))
    }

    fn provider(&self, name: &str) -> ApiResult<&(OidcProvider, OnceCell<CoreProviderMetadata>)> {
        self.providers.get(name).ok_or(ApiError::NotFound)
    }

    async fn metadata(&self, name: &str) -> ApiResult<(OidcProvider, CoreProviderMetadata)> {
        let (provider, cell) = self.provider(name)?;
        let http = self.http()?;
        let meta = cell
            .get_or_try_init(|| async {
                match provider.kind {
                    ProviderKind::Standard { .. } => {
                        let issuer =
                            IssuerUrl::new(provider.issuer.clone()).map_err(ApiError::internal)?;
                        CoreProviderMetadata::discover_async(issuer, http)
                            .await
                            .map_err(ApiError::internal)
                    }
                    ProviderKind::MicrosoftMultiTenant => {
                        let url = format!("{}/.well-known/openid-configuration", provider.issuer);
                        let body = http
                            .get(url)
                            .send()
                            .await
                            .map_err(ApiError::internal)?
                            .bytes()
                            .await
                            .map_err(ApiError::internal)?;
                        let meta: CoreProviderMetadata =
                            serde_json::from_slice(&body).map_err(ApiError::internal)?;
                        let jwks = CoreJsonWebKeySet::fetch_async(meta.jwks_uri(), http)
                            .await
                            .map_err(ApiError::internal)?;
                        Ok(meta.set_jwks(jwks))
                    }
                }
            })
            .await?;
        Ok((provider.clone(), meta.clone()))
    }
}

/// What the browser carries between `start` and `callback`.
#[derive(Debug, Serialize, Deserialize)]
struct FlowState {
    provider: String,
    csrf: String,
    nonce: String,
    pkce_verifier: String,
    invitation: Option<String>,
    link_user: Option<UserId>,
    redirect: String,
    exp: i64,
}

fn seal(keyring: &Keyring, state: &FlowState) -> ApiResult<String> {
    let json = serde_json::to_vec(state).map_err(ApiError::internal)?;
    Ok(BASE64URL_NOPAD.encode(&keyring.encrypt(&json, STATE_AAD)))
}

fn open(keyring: &Keyring, cookie: &str) -> ApiResult<FlowState> {
    let bad = || {
        ApiError::bad_request(
            "oidc_state_invalid",
            "the sign-in attempt expired; start again",
        )
    };
    let raw = BASE64URL_NOPAD
        .decode(cookie.as_bytes())
        .map_err(|_| bad())?;
    let json = keyring.decrypt(&raw, STATE_AAD).map_err(|_| bad())?;
    let state: FlowState = serde_json::from_slice(&json).map_err(|_| bad())?;
    if state.exp < Utc::now().timestamp() {
        return Err(bad());
    }
    Ok(state)
}

/// Only same-site relative paths, so the callback cannot be used as an open redirect.
pub fn safe_redirect(path: Option<&str>) -> String {
    match path {
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && !p.contains('\\')
                && !p.contains("://") =>
        {
            p.to_owned()
        }
        _ => "/".to_owned(),
    }
}

fn redirect_uri(state: &AppState, provider: &str) -> ApiResult<RedirectUrl> {
    RedirectUrl::new(
        state
            .config
            .app_url(&format!("/api/v1/auth/oidc/{provider}/callback"), &[]),
    )
    .map_err(ApiError::internal)
}

pub struct Started {
    pub authorize_url: String,
    pub state_cookie: String,
}

pub async fn start(
    state: &AppState,
    provider_name: &str,
    invitation: Option<String>,
    link_user: Option<UserId>,
    redirect: Option<&str>,
) -> ApiResult<Started> {
    let (provider, meta) = state.oidc.metadata(provider_name).await?;
    let client = CoreClient::from_provider_metadata(
        meta,
        ClientId::new(provider.client_id.clone()),
        Some(ClientSecret::new(provider.client_secret)),
    )
    .set_redirect_uri(redirect_uri(state, provider_name)?);
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, csrf, nonce) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("email".into()))
        .add_scope(Scope::new("profile".into()))
        .set_pkce_challenge(challenge)
        .url();
    let flow = FlowState {
        provider: provider_name.to_owned(),
        csrf: csrf.secret().clone(),
        nonce: nonce.secret().clone(),
        pkce_verifier: verifier.secret().clone(),
        invitation,
        link_user,
        redirect: safe_redirect(redirect),
        exp: Utc::now().timestamp() + STATE_TTL_SECS,
    };
    Ok(Started {
        authorize_url: url.to_string(),
        state_cookie: seal(&state.config.keyring, &flow)?,
    })
}

/// Claims we use from the ID token, after verification.
#[derive(Debug, Clone)]
pub struct VerifiedIdentity {
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
}

pub struct Completed {
    pub session: Option<IssuedSession>,
    pub redirect: String,
}

/// Handles the provider's redirect back. Returns a new session (or `None`
/// when a signed-in user linked an identity) and where to send the browser.
pub async fn callback(
    state: &AppState,
    provider_name: &str,
    code: &str,
    returned_state: &str,
    cookie: Option<&str>,
    meta: &RequestMeta,
) -> ApiResult<Completed> {
    let flow = open(
        &state.config.keyring,
        cookie.ok_or_else(|| {
            ApiError::bad_request(
                "oidc_state_invalid",
                "the sign-in attempt expired; start again",
            )
        })?,
    )?;
    if flow.provider != provider_name
        || !core_domain::crypto::constant_time_eq(flow.csrf.as_bytes(), returned_state.as_bytes())
    {
        return Err(ApiError::bad_request(
            "oidc_state_invalid",
            "the sign-in attempt did not match; start again",
        ));
    }
    let (provider, meta_doc) = state.oidc.metadata(provider_name).await?;
    let client = CoreClient::from_provider_metadata(
        meta_doc,
        ClientId::new(provider.client_id.clone()),
        Some(ClientSecret::new(provider.client_secret.clone())),
    )
    .set_redirect_uri(redirect_uri(state, provider_name)?);
    let token = client
        .exchange_code(AuthorizationCode::new(code.to_owned()))
        .map_err(ApiError::internal)?
        .set_pkce_verifier(PkceCodeVerifier::new(flow.pkce_verifier.clone()))
        .request_async(state.oidc.http()?)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, provider = provider_name, "OIDC code exchange failed");
            ApiError::unauthorized(
                "oidc_failed",
                "the sign-in provider rejected the request; try again",
            )
        })?;
    let id_token = token.id_token().ok_or_else(|| {
        ApiError::unauthorized("oidc_failed", "the sign-in provider sent no identity")
    })?;
    let mut verifier = client.id_token_verifier();
    if provider.kind == ProviderKind::MicrosoftMultiTenant {
        verifier = verifier.require_issuer_match(false);
    }
    let claims = id_token
        .claims(&verifier, &Nonce::new(flow.nonce.clone()))
        .map_err(|e| {
            tracing::warn!(error = %e, provider = provider_name, "ID token failed verification");
            ApiError::unauthorized("oidc_failed", "the sign-in could not be verified")
        })?;
    if provider.kind == ProviderKind::MicrosoftMultiTenant
        && !is_microsoft_tenant_issuer(claims.issuer().as_str())
    {
        return Err(ApiError::unauthorized(
            "oidc_failed",
            "the sign-in could not be verified",
        ));
    }
    let identity = VerifiedIdentity {
        subject: claims.subject().as_str().to_owned(),
        email: claims.email().map(|e| e.as_str().to_owned()),
        email_verified: claims.email_verified().unwrap_or(false),
        name: claims
            .name()
            .and_then(|n| n.get(None))
            .map(|n| n.as_str().to_owned()),
    };
    let session = resolve(state, &provider, &identity, &flow, meta).await?;
    Ok(Completed {
        session,
        redirect: flow.redirect,
    })
}

/// `https://login.microsoftonline.com/<tenant guid>/v2.0`.
pub fn is_microsoft_tenant_issuer(iss: &str) -> bool {
    iss.strip_prefix("https://login.microsoftonline.com/")
        .and_then(|rest| rest.strip_suffix("/v2.0"))
        .is_some_and(|tenant| uuid::Uuid::try_parse(tenant).is_ok())
}

async fn resolve(
    state: &AppState,
    provider: &OidcProvider,
    identity: &VerifiedIdentity,
    flow: &FlowState,
    meta: &RequestMeta,
) -> ApiResult<Option<IssuedSession>> {
    let method = format!("oidc:{}", provider.name);
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let known =
        core_db::auth::find_oidc_identity(&mut tx, &provider.name, &identity.subject).await?;

    // 2. Linking to the signed-in user.
    if let Some(link_user) = flow.link_user {
        if known.is_some_and(|u| u != link_user) {
            return Err(ApiError::conflict(
                "identity_in_use",
                "that account is already linked to another OnHold user",
            ));
        }
        core_db::set_context(&mut tx, None, Some(link_user)).await?;
        if known.is_none() {
            core_db::users::link_oidc_identity(
                &mut tx,
                link_user,
                &provider.name,
                &identity.subject,
                identity.email.as_deref(),
            )
            .await?;
        }
        tx.commit().await?;
        return Ok(None);
    }

    let invitation = match &flow.invitation {
        Some(token) => core_db::auth::find_invitation(&mut tx, &hash_token(token)).await?,
        None => None,
    };

    let user = match known {
        // 1. Known identity.
        Some(user) => {
            core_db::set_context(&mut tx, None, Some(user)).await?;
            core_db::users::touch_oidc_identity(&mut tx, user, &provider.name, &identity.subject)
                .await?;
            user
        }
        None => {
            // 3. Invitation, then 4. trusted verified email.
            let email = match (&invitation, provider.kind) {
                (Some(inv), _) => Some(inv.email.clone()),
                (
                    None,
                    ProviderKind::Standard {
                        trust_verified_email: true,
                    },
                ) if identity.email_verified => identity.email.clone(),
                _ => None,
            };
            let Some(email) = email else {
                return Err(ApiError::forbidden(
                    "no_account",
                    "there is no OnHold account for that sign-in; ask your workspace for an invitation",
                ));
            };
            let user = match core_db::auth::find_user_by_email(&mut tx, &email).await? {
                // An invitation only creates accounts. Holding one never
                // attaches a new identity to someone's existing account,
                // unless the provider vouches for that very address (rule 4).
                Some(_)
                    if invitation.is_some()
                        && !(provider.kind
                            == ProviderKind::Standard {
                                trust_verified_email: true,
                            }
                            && identity.email_verified
                            && identity
                                .email
                                .as_deref()
                                .is_some_and(|e| e.eq_ignore_ascii_case(&email))) =>
                {
                    return Err(ApiError::conflict(
                        "account_exists",
                        "an account with this email already exists; sign in, then accept the invitation",
                    ));
                }
                Some(found) => {
                    core_db::set_context(&mut tx, None, Some(found.user_id)).await?;
                    core_db::users::mark_email_verified(&mut tx, found.user_id).await?;
                    found.user_id
                }
                None if invitation.is_some() => {
                    let id = UserId::new();
                    core_db::set_context(&mut tx, None, Some(id)).await?;
                    let fallback = email.split('@').next().unwrap_or("New user").to_owned();
                    let name = identity
                        .name
                        .clone()
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or(fallback);
                    core_db::users::insert(
                        &mut tx,
                        core_db::users::NewUser {
                            id,
                            email: &email,
                            name: name.trim(),
                            password_hash: None,
                            email_verified: true,
                        },
                    )
                    .await?;
                    id
                }
                None => {
                    return Err(ApiError::forbidden(
                        "no_account",
                        "there is no OnHold account for that sign-in; ask your workspace for an invitation",
                    ));
                }
            };
            core_db::users::link_oidc_identity(
                &mut tx,
                user,
                &provider.name,
                &identity.subject,
                identity.email.as_deref(),
            )
            .await?;
            user
        }
    };

    if let Some(inv) = &invitation {
        let email_matches = core_db::users::get(&mut tx, user)
            .await?
            .is_some_and(|u| u.email.eq_ignore_ascii_case(&inv.email));
        if email_matches {
            join_from_invitation(&mut tx, inv, user, meta).await?;
            core_db::set_context(&mut tx, None, Some(user)).await?;
        }
    }
    let issued = issue_session(state, &mut tx, user, &method, meta).await?;
    crate::services::auth::reset_failures_if_signed_in(&mut tx, &issued).await?;
    tx.commit().await?;
    tracing::info!(user_id = %user, provider = %provider.name, "signed in with OIDC");
    Ok(Some(issued))
}

pub type SharedRegistry = Arc<Registry>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_stay_on_site() {
        assert_eq!(safe_redirect(Some("/bookings?x=1")), "/bookings?x=1");
        for bad in [
            None,
            Some(""),
            Some("https://evil.test"),
            Some("//evil.test"),
            Some("/\\evil.test"),
            Some("javascript:alert(1)"),
        ] {
            assert_eq!(safe_redirect(bad), "/", "{bad:?}");
        }
    }

    #[test]
    fn microsoft_issuers() {
        assert!(is_microsoft_tenant_issuer(
            "https://login.microsoftonline.com/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0"
        ));
        assert!(!is_microsoft_tenant_issuer(
            "https://login.microsoftonline.com/common/v2.0"
        ));
        assert!(!is_microsoft_tenant_issuer(
            "https://evil.test/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0"
        ));
    }

    #[test]
    fn flow_state_is_sealed_and_expires() {
        let ring = Keyring::ephemeral();
        let mut flow = FlowState {
            provider: "google".into(),
            csrf: "c".into(),
            nonce: "n".into(),
            pkce_verifier: "v".into(),
            invitation: None,
            link_user: None,
            redirect: "/".into(),
            exp: Utc::now().timestamp() + 60,
        };
        let sealed = seal(&ring, &flow).expect("seals");
        assert_eq!(open(&ring, &sealed).expect("opens").csrf, "c");
        assert!(
            open(&Keyring::ephemeral(), &sealed).is_err(),
            "another key cannot open it"
        );
        flow.exp = Utc::now().timestamp() - 1;
        assert!(
            open(&ring, &seal(&ring, &flow).expect("seals")).is_err(),
            "expired"
        );
    }
}
