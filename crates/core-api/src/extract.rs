//! Request extractors: where the request came from, who is calling, and the
//! workspace context every service call needs.
//!
//! Credentials are the session cookie (browsers), `Authorization: Bearer
//! ohs_…` (sessions of the desktop and mobile apps) or `Authorization: Bearer
//! ohk_…` (API keys). Cookie-authenticated writes must echo the session's CSRF
//! token in `X-CSRF-Token`; bearer tokens are never sent automatically, so
//! they need no such check. Workspace routes name
//! their workspace in `X-Workspace-Id`; an API key implies its own.

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, header};
use axum_extra::extract::CookieJar;
use chrono::{DateTime, Utc};
use core_domain::authz::RequestMeta;
use core_domain::crypto::{constant_time_eq, hash_token};
use core_domain::workspace::WorkspaceStatus;
use core_domain::{Actor, Ctx, Permission, UserId, WorkspaceId};
use uuid::Uuid;

use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const SESSION_COOKIE: &str = "onhold_session";
pub const CSRF_HEADER: &str = "x-csrf-token";
pub const WORKSPACE_HEADER: &str = "x-workspace-id";
pub const API_KEY_PREFIX: &str = "ohk_";
pub const APP_SESSION_PREFIX: &str = "ohs_";

/// Client IP, user agent and request id, for audit rows and sessions.
#[derive(Debug, Clone, Default)]
pub struct ClientMeta(pub RequestMeta);

impl FromRequestParts<AppState> for ClientMeta {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        Ok(ClientMeta(request_meta(
            parts,
            state.config.trust_proxy_headers,
        )))
    }
}

pub fn request_meta(
    parts: &Parts,
    trust_proxy_headers: crate::config::ProxyHeaders,
) -> RequestMeta {
    use crate::config::ProxyHeaders;
    let header = |name: &str| parts.headers.get(name).and_then(|v| v.to_str().ok());
    // Only what our own proxy wrote: anything else the client could have sent.
    let forwarded = match trust_proxy_headers {
        ProxyHeaders::Off => None,
        ProxyHeaders::Forwarded => parts
            .headers
            .get_all("x-forwarded-for")
            .iter()
            .next_back()
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next()),
        ProxyHeaders::Cloudflare => header("cf-connecting-ip"),
    }
    .and_then(|v| v.trim().parse::<IpAddr>().ok());
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    RequestMeta {
        ip: forwarded.or(peer),
        user_agent: header(header::USER_AGENT.as_str()).map(|ua| ua.chars().take(512).collect()),
        request_id: header("x-request-id").map(str::to_owned),
    }
}

/// The credential a request carries, before it is checked.
#[derive(Debug, Clone)]
pub enum Credential {
    /// `bearer` is true for an app's session token, false for a browser cookie.
    Session {
        token: String,
        bearer: bool,
    },
    ApiKey {
        key_id: Uuid,
        secret: String,
    },
}

/// Reads the bearer token or session cookie. A malformed bearer is an error;
/// a missing credential is `None`.
pub fn read_credential(headers: &HeaderMap) -> ApiResult<Option<Credential>> {
    if let Some(auth) = headers.get(header::AUTHORIZATION) {
        let value = auth.to_str().map_err(|_| {
            ApiError::unauthorized("invalid_api_key", "malformed Authorization header")
        })?;
        let token = value.strip_prefix("Bearer ").ok_or_else(|| {
            ApiError::unauthorized("invalid_api_key", "use `Authorization: Bearer <api key>`")
        })?;
        if token.starts_with(APP_SESSION_PREFIX) {
            return Ok(Some(Credential::Session {
                token: token.to_owned(),
                bearer: true,
            }));
        }
        return parse_api_key(token).map(Some);
    }
    let jar = CookieJar::from_headers(headers);
    Ok(jar.get(SESSION_COOKIE).map(|c| Credential::Session {
        token: c.value().to_owned(),
        bearer: false,
    }))
}

/// `ohk_<key id as 32 hex>_<secret>`.
pub fn parse_api_key(token: &str) -> ApiResult<Credential> {
    let invalid = || ApiError::unauthorized("invalid_api_key", "that API key is not valid");
    let rest = token.strip_prefix(API_KEY_PREFIX).ok_or_else(invalid)?;
    let (id, secret) = rest.split_once('_').ok_or_else(invalid)?;
    let key_id = Uuid::try_parse(id).map_err(|_| invalid())?;
    if secret.is_empty() {
        return Err(invalid());
    }
    Ok(Credential::ApiKey {
        key_id,
        secret: secret.to_owned(),
    })
}

pub fn format_api_key(key_id: Uuid, secret: &str) -> String {
    format!("{API_KEY_PREFIX}{}_{secret}", key_id.simple())
}

/// A checked session, of a browser or an app.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: Uuid,
    pub user_id: UserId,
    pub csrf_token: String,
    pub mfa_verified: bool,
    pub expires_at: DateTime<Utc>,
    pub impersonating: Option<(WorkspaceId, DateTime<Utc>)>,
    pub meta: RequestMeta,
    /// A desktop or mobile app's session rather than a browser's.
    pub is_app: bool,
}

fn is_write(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

async fn load_session(
    parts: &Parts,
    state: &AppState,
    token: &str,
    bearer: bool,
) -> ApiResult<Session> {
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let found = core_db::auth::find_session(&mut tx, &hash_token(token)).await?;
    tx.commit().await?;
    // An app's token only works as a bearer token and a browser's only as a
    // cookie, so neither can be replayed through the other's checks.
    let expected_client = if bearer { "app" } else { "browser" };
    let found = found
        .filter(|f| f.client == expected_client)
        .ok_or_else(|| {
            ApiError::unauthorized("session_expired", "your session has ended; sign in again")
        })?;

    if !bearer && is_write(&parts.method) {
        let sent = parts
            .headers
            .get(CSRF_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if !constant_time_eq(sent.as_bytes(), found.csrf_token.as_bytes()) {
            return Err(ApiError::forbidden(
                "csrf_failed",
                "missing or wrong X-CSRF-Token header",
            ));
        }
    }

    let mut tx = core_db::begin(&state.db, None, Some(found.user_id)).await?;
    core_db::sessions::touch(&mut tx, found.session_id).await?;
    tx.commit().await?;

    let now = Utc::now();
    Ok(Session {
        id: found.session_id,
        user_id: found.user_id,
        csrf_token: found.csrf_token,
        is_app: bearer,
        mfa_verified: found.mfa_verified,
        expires_at: found.expires_at,
        impersonating: found
            .impersonated_workspace_id
            .zip(found.impersonation_expires_at)
            .filter(|(_, until)| *until > now),
        meta: request_meta(parts, state.config.trust_proxy_headers),
    })
}

async fn session_from_parts(parts: &Parts, state: &AppState) -> ApiResult<Option<Session>> {
    match read_credential(&parts.headers)? {
        Some(Credential::Session { token, bearer }) => {
            load_session(parts, state, &token, bearer).await.map(Some)
        }
        Some(Credential::ApiKey { .. }) => Err(ApiError::forbidden(
            "session_required",
            "this endpoint is for signed-in people, not API keys",
        )),
        None => Ok(None),
    }
}

/// A signed-in person whose second factor (if any) is done.
pub struct CurrentSession(pub Session);

impl FromRequestParts<AppState> for CurrentSession {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let session = session_from_parts(parts, state)
            .await?
            .ok_or(ApiError::Unauthenticated)?;
        if !session.mfa_verified {
            return Err(ApiError::unauthorized(
                "mfa_required",
                "enter the code from your authenticator app",
            ));
        }
        Ok(CurrentSession(session))
    }
}

/// A session that may still be waiting for its second factor. Only the
/// sign-in endpoints that finish or abandon sign-in accept this.
pub struct PendingSession(pub Session);

impl FromRequestParts<AppState> for PendingSession {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let session = session_from_parts(parts, state)
            .await?
            .ok_or(ApiError::Unauthenticated)?;
        Ok(PendingSession(session))
    }
}

/// A fully signed-in session if there is one.
pub struct OptionalSession(pub Option<Session>);

impl FromRequestParts<AppState> for OptionalSession {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let session = session_from_parts(parts, state).await?;
        Ok(OptionalSession(session.filter(|s| s.mfa_verified)))
    }
}

/// The workspace context: who is acting, where, with which permissions.
pub struct WsCtx(pub Ctx);

impl FromRequestParts<AppState> for WsCtx {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let requested = parts
            .headers
            .get(WORKSPACE_HEADER)
            .map(|v| {
                v.to_str()
                    .ok()
                    .and_then(|s| Uuid::try_parse(s.trim()).ok())
                    .map(WorkspaceId)
                    .ok_or_else(|| {
                        ApiError::bad_request(
                            "invalid_workspace_header",
                            "X-Workspace-Id must be a workspace id",
                        )
                    })
            })
            .transpose()?;

        let ctx = match read_credential(&parts.headers)? {
            Some(Credential::ApiKey { key_id, secret }) => {
                api_key_ctx(parts, state, key_id, &secret, requested).await?
            }
            Some(Credential::Session { token, bearer }) => {
                let session = load_session(parts, state, &token, bearer).await?;
                if !session.mfa_verified {
                    return Err(ApiError::unauthorized(
                        "mfa_required",
                        "enter the code from your authenticator app",
                    ));
                }
                let workspace = requested.ok_or_else(|| {
                    ApiError::bad_request("workspace_required", "send the X-Workspace-Id header")
                })?;
                session_ctx(state, session, workspace).await?
            }
            None => return Err(ApiError::Unauthenticated),
        };
        // Every log line for the rest of the request says where and who.
        let span = tracing::Span::current();
        span.record("workspace_id", tracing::field::display(ctx.workspace_id));
        match &ctx.actor {
            Actor::User { user_id, .. } => span.record("user_id", tracing::field::display(user_id)),
            Actor::ApiKey { key_id, .. } => {
                span.record("api_key_id", tracing::field::display(key_id))
            }
            Actor::Rule { .. } | Actor::System => &span,
        };
        Ok(WsCtx(ctx))
    }
}

async fn api_key_ctx(
    parts: &Parts,
    state: &AppState,
    key_id: Uuid,
    secret: &str,
    requested: Option<WorkspaceId>,
) -> ApiResult<Ctx> {
    let invalid = || ApiError::unauthorized("invalid_api_key", "that API key is not valid");
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let key = core_db::auth::find_api_key(&mut tx, key_id)
        .await?
        .ok_or_else(invalid)?;
    if !constant_time_eq(&hash_token(secret), &key.secret_hash) {
        return Err(invalid());
    }
    if requested.is_some_and(|w| w != key.workspace_id) {
        return Err(ApiError::forbidden(
            "workspace_mismatch",
            "this API key belongs to a different workspace",
        ));
    }
    core_db::set_context(&mut tx, Some(key.workspace_id), None).await?;
    let workspace = core_db::workspaces::get(&mut tx, key.workspace_id)
        .await?
        .ok_or_else(invalid)?;
    check_status(workspace.status, false)?;
    // A key never does more than the person who made it can do today: it
    // stops working when they leave and shrinks when their role does.
    let creator_gone = || {
        ApiError::unauthorized(
            "invalid_api_key",
            "the person who created this API key is no longer a member of the workspace",
        )
    };
    let creator = key.created_by.ok_or_else(creator_gone)?;
    let role = core_db::members::role_of(&mut tx, creator)
        .await?
        .ok_or_else(creator_gone)?;
    let allowed = core_db::workspaces::role_permissions(&mut tx, role).await?;
    core_db::api_keys::touch(&mut tx, key_id).await?;
    tx.commit().await?;

    Ok(Ctx {
        workspace_id: key.workspace_id,
        actor: Actor::ApiKey {
            key_id,
            created_by: key.created_by,
        },
        role: None,
        permissions: key
            .permissions
            .iter()
            .filter_map(|p| p.parse::<Permission>().ok())
            .filter(|p| allowed.contains(p))
            .collect(),
        meta: request_meta(parts, state.config.trust_proxy_headers),
    })
}

/// Resolves a session into a workspace context: an impersonating
/// superadmin, or an active member with their role's permissions.
pub async fn session_ctx(
    state: &AppState,
    session: Session,
    workspace_id: WorkspaceId,
) -> ApiResult<Ctx> {
    let not_found =
        || ApiError::forbidden("not_a_member", "you are not a member of that workspace");
    let mut tx = core_db::begin(&state.db, Some(workspace_id), Some(session.user_id)).await?;
    let workspace = core_db::workspaces::get(&mut tx, workspace_id)
        .await?
        .ok_or_else(not_found)?;

    if let Some((target, _)) = session.impersonating
        && target == workspace_id
    {
        let user = core_db::users::get(&mut tx, session.user_id)
            .await?
            .ok_or(ApiError::Unauthenticated)?;
        if user.is_superadmin && user.totp_enabled_at.is_some() {
            check_status(workspace.status, true)?;
            tx.commit().await?;
            return Ok(Ctx {
                workspace_id,
                actor: Actor::User {
                    user_id: session.user_id,
                    impersonator: Some(session.user_id),
                },
                role: None,
                permissions: Permission::ALL.iter().copied().collect(),
                meta: session.meta,
            });
        }
    }

    let role = core_db::members::role_of(&mut tx, session.user_id)
        .await?
        .ok_or_else(not_found)?;
    check_status(workspace.status, false)?;
    if session.is_app && !state.config.apps_allowed(&workspace.plan) {
        return Err(ApiError::forbidden(
            "apps_not_in_plan",
            "the desktop and mobile apps are not part of this workspace's plan; use the website instead",
        ));
    }
    if workspace.require_staff_2fa && role.is_staff() {
        let user = core_db::users::get(&mut tx, session.user_id)
            .await?
            .ok_or(ApiError::Unauthenticated)?;
        if user.totp_enabled_at.is_none() {
            return Err(ApiError::forbidden(
                "mfa_enrollment_required",
                "this workspace requires two-factor authentication; set it up under your account",
            ));
        }
    }
    let permissions: BTreeSet<Permission> = core_db::workspaces::role_permissions(&mut tx, role)
        .await?
        .into_iter()
        .collect();
    tx.commit().await?;

    Ok(Ctx {
        workspace_id,
        actor: Actor::User {
            user_id: session.user_id,
            impersonator: None,
        },
        role: Some(role),
        permissions,
        meta: session.meta,
    })
}

fn check_status(status: WorkspaceStatus, operator: bool) -> ApiResult<()> {
    match status {
        WorkspaceStatus::Closed => Err(ApiError::forbidden(
            "workspace_closed",
            "this workspace has been closed",
        )),
        WorkspaceStatus::Suspended if !operator => Err(ApiError::forbidden(
            "workspace_suspended",
            "this workspace is suspended; contact support",
        )),
        WorkspaceStatus::Trial | WorkspaceStatus::Active | WorkspaceStatus::Suspended => Ok(()),
    }
}

/// The version a write expects, from `If-Match: "<version>"`. Absent means
/// "overwrite whatever is there", which interactive clients may choose.
pub struct IfMatch(pub Option<i64>);

impl<S: Send + Sync> FromRequestParts<S> for IfMatch {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> ApiResult<Self> {
        let Some(value) = parts.headers.get(header::IF_MATCH) else {
            return Ok(IfMatch(None));
        };
        let parsed = value
            .to_str()
            .ok()
            .map(|v| v.trim().trim_start_matches("W/").trim_matches('"'))
            .and_then(|v| v.parse::<i64>().ok())
            .ok_or_else(|| {
                ApiError::bad_request(
                    "invalid_if_match",
                    "If-Match must be a quoted version number, e.g. \"3\"",
                )
            })?;
        Ok(IfMatch(Some(parsed)))
    }
}

/// An `ETag` header value for a version.
pub fn etag(version: i64) -> String {
    format!("\"{version}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_ip_comes_only_from_what_our_proxy_wrote() {
        use crate::config::ProxyHeaders;
        let (mut parts, ()) = axum::http::Request::builder()
            .header("cf-connecting-ip", "203.0.113.9")
            .header("x-forwarded-for", "203.0.113.9, 198.51.100.1")
            .header("x-forwarded-for", "192.0.2.7")
            .body(())
            .expect("request")
            .into_parts();
        let peer: SocketAddr = "10.0.0.2:4000".parse().expect("addr");
        parts.extensions.insert(ConnectInfo(peer));
        let ip = |parts: &Parts, mode| request_meta(parts, mode).ip.map(|ip| ip.to_string());

        assert_eq!(ip(&parts, ProxyHeaders::Off).as_deref(), Some("10.0.0.2"));
        // The proxy appends the address it saw; the client wrote the rest.
        assert_eq!(
            ip(&parts, ProxyHeaders::Forwarded).as_deref(),
            Some("192.0.2.7")
        );
        assert_eq!(
            ip(&parts, ProxyHeaders::Cloudflare).as_deref(),
            Some("203.0.113.9")
        );

        parts.headers.remove("x-forwarded-for");
        parts.headers.remove("cf-connecting-ip");
        assert_eq!(
            ip(&parts, ProxyHeaders::Forwarded).as_deref(),
            Some("10.0.0.2")
        );
        assert_eq!(
            ip(&parts, ProxyHeaders::Cloudflare).as_deref(),
            Some("10.0.0.2")
        );
    }

    #[test]
    fn api_keys_round_trip() {
        let id = Uuid::now_v7();
        let formatted = format_api_key(id, "s3cret");
        match parse_api_key(&formatted).expect("parses") {
            Credential::ApiKey { key_id, secret } => {
                assert_eq!(key_id, id);
                assert_eq!(secret, "s3cret");
            }
            Credential::Session { .. } => panic!("expected an API key"),
        }
        assert!(matches!(
            read_credential(&HeaderMap::from_iter([(
                header::AUTHORIZATION,
                "Bearer ohs_abc".parse().expect("header")
            )])),
            Ok(Some(Credential::Session { bearer: true, .. }))
        ));
        for bad in [
            "",
            "ohk_",
            "ohk_nothex_x",
            "sk_live_abc",
            &format!("ohk_{}_", id.simple()),
        ] {
            assert!(parse_api_key(bad).is_err(), "{bad}");
        }
    }
}
