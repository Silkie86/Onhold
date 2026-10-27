//! `/auth/*` and invitation acceptance: everything a person does before or
//! outside a particular workspace.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use chrono::{DateTime, Utc};
use core_domain::workspace::WorkspaceStatus;
use core_domain::{Role, UserId, WorkspaceId};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{SessionResponse, clear_session_cookie, session_cookie, session_cookie_until};
use crate::AppState;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::extract::{ClientMeta, CurrentSession, OptionalSession, PendingSession, SESSION_COOKIE};
use crate::oidc::{self, STATE_COOKIE};
use crate::services::app_sign_in as app_svc;
use crate::services::auth as svc;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(login))
        .routes(routes!(logout))
        .routes(routes!(me))
        .routes(routes!(verify_mfa))
        .routes(routes!(totp_setup))
        .routes(routes!(totp_enable))
        .routes(routes!(totp_disable))
        .routes(routes!(recovery_codes))
        .routes(routes!(change_password))
        .routes(routes!(list_sessions))
        .routes(routes!(revoke_session))
        .routes(routes!(request_magic_link))
        .routes(routes!(consume_magic_link))
        .routes(routes!(oidc_providers))
        .routes(routes!(oidc_start))
        .routes(routes!(oidc_callback))
        .routes(routes!(preview_invitation))
        .routes(routes!(accept_invitation))
        .routes(routes!(start_app_sign_in))
        .routes(routes!(app_sign_in_token))
        .routes(routes!(lookup_app_sign_in))
        .routes(routes!(decide_app_sign_in))
}

// Sign-in ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// Sign in with email and password.
///
/// Sets the session cookie. If the user has two-factor authentication on,
/// `mfa_required` is true and the session only becomes usable after
/// `POST /auth/mfa/verify`.
#[utoipa::path(post, path = "/auth/login", tag = "auth", request_body = LoginRequest,
    responses((status = 200, body = SessionResponse), (status = 401, body = ErrorBody)))]
async fn login(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    jar: CookieJar,
    Json(body): Json<LoginRequest>,
) -> ApiResult<(CookieJar, Json<SessionResponse>)> {
    let issued = svc::login(&state, &body.email, &body.password, &meta).await?;
    let response = SessionResponse::from(&issued);
    Ok((jar.add(session_cookie(&state, &issued)), Json(response)))
}

/// Sign out of this session.
#[utoipa::path(post, path = "/auth/logout", tag = "auth",
    responses((status = 204), (status = 401, body = ErrorBody)), security(("session" = [])))]
async fn logout(
    State(state): State<AppState>,
    PendingSession(session): PendingSession,
    jar: CookieJar,
) -> ApiResult<(CookieJar, StatusCode)> {
    svc::logout(&state, &session).await?;
    Ok((
        jar.add(clear_session_cookie(&state)),
        StatusCode::NO_CONTENT,
    ))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UserView {
    pub id: UserId,
    pub email: String,
    pub name: String,
    pub has_password: bool,
    pub email_verified: bool,
    pub totp_enabled: bool,
    pub is_superadmin: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MyWorkspaceView {
    pub id: WorkspaceId,
    pub slug: String,
    pub name: String,
    pub status: WorkspaceStatus,
    pub role: Role,
    /// False when the workspace's plan does not include the desktop and
    /// mobile apps; an app's requests to it are then refused.
    pub apps_allowed: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CurrentSessionView {
    pub id: Uuid,
    /// `browser` or `app`.
    pub client: String,
    pub csrf_token: String,
    pub mfa_verified: bool,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ImpersonationView {
    pub workspace_id: WorkspaceId,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MeResponse {
    pub user: UserView,
    /// Workspaces this user belongs to (empty while a second factor is pending).
    pub workspaces: Vec<MyWorkspaceView>,
    pub session: CurrentSessionView,
    /// Set while an operator is acting inside a customer's workspace; the UI must show it.
    pub impersonation: Option<ImpersonationView>,
}

/// The signed-in user, their workspaces and this session.
#[utoipa::path(get, path = "/auth/me", tag = "auth",
    responses((status = 200, body = MeResponse), (status = 401, body = ErrorBody)), security(("session" = [])))]
async fn me(
    State(state): State<AppState>,
    PendingSession(session): PendingSession,
) -> ApiResult<Json<MeResponse>> {
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let user = core_db::users::get(&mut tx, session.user_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    let workspaces = if session.mfa_verified {
        core_db::workspaces::for_user(&mut tx, session.user_id).await?
    } else {
        Vec::new()
    };
    tx.commit().await?;
    Ok(Json(MeResponse {
        user: UserView {
            id: user.id,
            email: user.email,
            name: user.name,
            has_password: user.has_password,
            email_verified: user.email_verified_at.is_some(),
            totp_enabled: user.totp_enabled_at.is_some(),
            is_superadmin: user.is_superadmin,
        },
        workspaces: workspaces
            .into_iter()
            .map(|w| MyWorkspaceView {
                id: w.id,
                slug: w.slug,
                name: w.name,
                status: w.status,
                role: w.role,
                apps_allowed: state.config.apps_allowed(&w.plan),
            })
            .collect(),
        session: CurrentSessionView {
            id: session.id,
            client: if session.is_app { "app" } else { "browser" }.to_owned(),
            csrf_token: session.csrf_token.clone(),
            mfa_verified: session.mfa_verified,
            expires_at: session.expires_at,
        },
        impersonation: session
            .impersonating
            .map(|(workspace_id, expires_at)| ImpersonationView {
                workspace_id,
                expires_at,
            }),
    }))
}

// Two-factor -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct MfaVerifyRequest {
    /// The 6-digit code from the authenticator app.
    pub code: Option<String>,
    /// Or one of the single-use recovery codes.
    pub recovery_code: Option<String>,
}

/// Finish signing in with a TOTP or recovery code.
#[utoipa::path(post, path = "/auth/mfa/verify", tag = "auth", request_body = MfaVerifyRequest,
    responses((status = 204), (status = 401, body = ErrorBody)), security(("session" = [])))]
async fn verify_mfa(
    State(state): State<AppState>,
    PendingSession(session): PendingSession,
    jar: CookieJar,
    Json(body): Json<MfaVerifyRequest>,
) -> ApiResult<(CookieJar, StatusCode)> {
    let expires_at = svc::verify_mfa(
        &state,
        &session,
        body.code.as_deref(),
        body.recovery_code.as_deref(),
    )
    .await?;
    // The cookie was set to last only as long as the pending session; give
    // it the full session's lifetime now.
    let token = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    let jar = match token {
        Some(token) if !session.is_app => jar.add(session_cookie_until(&state, token, expires_at)),
        _ => jar,
    };
    Ok((jar, StatusCode::NO_CONTENT))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TotpSetupResponse {
    /// Render as a QR code for the authenticator app.
    pub otpauth_url: String,
    /// The same secret in base32, for typing in by hand.
    pub secret: String,
}

/// Start turning on two-factor authentication.
#[utoipa::path(post, path = "/auth/totp/setup", tag = "auth",
    responses((status = 200, body = TotpSetupResponse), (status = 409, body = ErrorBody)), security(("session" = [])))]
async fn totp_setup(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
) -> ApiResult<Json<TotpSetupResponse>> {
    let setup = svc::totp_setup(&state, &session).await?;
    Ok(Json(TotpSetupResponse {
        otpauth_url: setup.otpauth_url,
        secret: setup.secret,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TotpCodeRequest {
    pub code: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecoveryCodesResponse {
    /// Shown once. Each works once if the authenticator app is lost.
    pub recovery_codes: Vec<String>,
}

/// Confirm setup with a code from the app; returns recovery codes.
#[utoipa::path(post, path = "/auth/totp/enable", tag = "auth", request_body = TotpCodeRequest,
    responses((status = 200, body = RecoveryCodesResponse), (status = 400, body = ErrorBody)), security(("session" = [])))]
async fn totp_enable(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
    Json(body): Json<TotpCodeRequest>,
) -> ApiResult<Json<RecoveryCodesResponse>> {
    let recovery_codes = svc::totp_enable(&state, &session, &body.code).await?;
    Ok(Json(RecoveryCodesResponse { recovery_codes }))
}

/// Turn off two-factor authentication.
#[utoipa::path(post, path = "/auth/totp/disable", tag = "auth", request_body = TotpCodeRequest,
    responses((status = 204), (status = 400, body = ErrorBody)), security(("session" = [])))]
async fn totp_disable(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
    Json(body): Json<TotpCodeRequest>,
) -> ApiResult<StatusCode> {
    svc::totp_disable(&state, &session, &body.code).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Replace all recovery codes with a new set.
#[utoipa::path(post, path = "/auth/totp/recovery-codes", tag = "auth", request_body = TotpCodeRequest,
    responses((status = 200, body = RecoveryCodesResponse), (status = 400, body = ErrorBody)), security(("session" = [])))]
async fn recovery_codes(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
    Json(body): Json<TotpCodeRequest>,
) -> ApiResult<Json<RecoveryCodesResponse>> {
    let recovery_codes = svc::regenerate_recovery_codes(&state, &session, &body.code).await?;
    Ok(Json(RecoveryCodesResponse { recovery_codes }))
}

// Password and sessions ---------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct PasswordChangeRequest {
    /// Required when the account already has a password.
    pub current_password: Option<String>,
    pub new_password: String,
}

/// Set or change the password. Signs out every other session.
#[utoipa::path(post, path = "/auth/password", tag = "auth", request_body = PasswordChangeRequest,
    responses((status = 204), (status = 400, body = ErrorBody)), security(("session" = [])))]
async fn change_password(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
    Json(body): Json<PasswordChangeRequest>,
) -> ApiResult<StatusCode> {
    svc::change_password(
        &state,
        &session,
        body.current_password.as_deref(),
        &body.new_password,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionInfoView {
    pub id: Uuid,
    pub auth_method: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub current: bool,
    /// `browser` or `app` (the desktop and mobile apps).
    pub client: String,
    /// The app's name for its device, such as "Sam's MacBook".
    pub device_name: Option<String>,
}

/// Every active session of the signed-in user.
#[utoipa::path(get, path = "/auth/sessions", tag = "auth",
    responses((status = 200, body = Vec<SessionInfoView>)), security(("session" = [])))]
async fn list_sessions(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
) -> ApiResult<Json<Vec<SessionInfoView>>> {
    let list = svc::sessions(&state, &session).await?;
    Ok(Json(
        list.into_iter()
            .map(|s| SessionInfoView {
                current: s.id == session.id,
                id: s.id,
                auth_method: s.auth_method,
                ip: s.ip.map(|ip| ip.to_string()),
                user_agent: s.user_agent,
                created_at: s.created_at,
                last_seen_at: s.last_seen_at,
                expires_at: s.expires_at,
                client: s.client,
                device_name: s.device_name,
            })
            .collect(),
    ))
}

/// Sign out one of your sessions (for example a lost phone).
#[utoipa::path(delete, path = "/auth/sessions/{id}", tag = "auth", params(("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = [])))]
async fn revoke_session(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    svc::revoke_session(&state, &session, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Magic links ----------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct MagicLinkRequest {
    pub email: String,
}

/// Email a one-time sign-in link. Always 202, whether or not the address has an account.
#[utoipa::path(post, path = "/auth/magic-link", tag = "auth", request_body = MagicLinkRequest, responses((status = 202)))]
async fn request_magic_link(
    State(state): State<AppState>,
    Json(body): Json<MagicLinkRequest>,
) -> ApiResult<StatusCode> {
    svc::request_magic_link(&state, &body.email).await?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TokenRequest {
    pub token: String,
}

/// Sign in with the token from a magic link.
#[utoipa::path(post, path = "/auth/magic-link/consume", tag = "auth", request_body = TokenRequest,
    responses((status = 200, body = SessionResponse), (status = 401, body = ErrorBody)))]
async fn consume_magic_link(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    jar: CookieJar,
    Json(body): Json<TokenRequest>,
) -> ApiResult<(CookieJar, Json<SessionResponse>)> {
    let issued = svc::consume_magic_link(&state, &body.token, &meta).await?;
    let response = SessionResponse::from(&issued);
    Ok((jar.add(session_cookie(&state, &issued)), Json(response)))
}

// OIDC ------------------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct OidcProviderView {
    pub name: String,
    pub display_name: String,
}

/// The single sign-on providers this deployment has configured.
#[utoipa::path(get, path = "/auth/oidc/providers", tag = "auth", responses((status = 200, body = Vec<OidcProviderView>)))]
async fn oidc_providers(State(state): State<AppState>) -> Json<Vec<OidcProviderView>> {
    Json(
        state
            .oidc
            .names()
            .into_iter()
            .map(|(name, display_name)| OidcProviderView { name, display_name })
            .collect(),
    )
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct OidcStartQuery {
    /// An invitation token to accept as part of signing in.
    pub invitation: Option<String>,
    /// A path in the web app to return to afterwards.
    pub redirect: Option<String>,
    /// Link this provider to the signed-in account instead of signing in.
    pub link: Option<bool>,
}

/// Redirect to the provider's sign-in page.
#[utoipa::path(get, path = "/auth/oidc/{provider}/start", tag = "auth",
    params(("provider" = String, Path), OidcStartQuery), responses((status = 303), (status = 404, body = ErrorBody)))]
async fn oidc_start(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<OidcStartQuery>,
    OptionalSession(session): OptionalSession,
    jar: CookieJar,
) -> ApiResult<(CookieJar, Redirect)> {
    let link_user = if q.link.unwrap_or(false) {
        Some(session.ok_or(ApiError::Unauthenticated)?.user_id)
    } else {
        None
    };
    let started = oidc::start(
        &state,
        &provider,
        q.invitation,
        link_user,
        q.redirect.as_deref(),
    )
    .await?;
    let cookie = Cookie::build((STATE_COOKIE, started.state_cookie))
        .path(format!("/api/v1/auth/oidc/{provider}"))
        .http_only(true)
        .secure(state.config.cookie_secure)
        // Lax, not Strict: the provider's redirect back is a top-level cross-site navigation.
        .same_site(SameSite::Lax)
        .max_age(time::Duration::minutes(10))
        .build();
    Ok((jar.add(cookie), Redirect::to(&started.authorize_url)))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct OidcCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

/// Where the provider sends the browser back. Redirects into the web app.
#[utoipa::path(get, path = "/auth/oidc/{provider}/callback", tag = "auth",
    params(("provider" = String, Path), OidcCallbackQuery), responses((status = 303)))]
async fn oidc_callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<OidcCallbackQuery>,
    ClientMeta(meta): ClientMeta,
    jar: CookieJar,
) -> Response {
    let state_cookie = jar.get(STATE_COOKIE).map(|c| c.value().to_owned());
    let jar = jar.remove(
        Cookie::build(STATE_COOKIE)
            .path(format!("/api/v1/auth/oidc/{provider}"))
            .build(),
    );
    let fail = |code: &str| {
        let url = state.config.app_url("/sign-in", &[("error", code)]);
        (jar.clone(), Redirect::to(&url)).into_response()
    };
    let (Some(code), Some(returned_state)) = (q.code.as_deref(), q.state.as_deref()) else {
        tracing::info!(provider, error = ?q.error, "OIDC sign-in cancelled or refused");
        return fail("oidc_cancelled");
    };
    match oidc::callback(
        &state,
        &provider,
        code,
        returned_state,
        state_cookie.as_deref(),
        &meta,
    )
    .await
    {
        Ok(done) => {
            let mut jar = jar.clone();
            let target = match &done.session {
                Some(issued) if issued.mfa_required => {
                    jar = jar.add(session_cookie(&state, issued));
                    state
                        .config
                        .app_url("/auth/mfa", &[("redirect", &done.redirect)])
                }
                Some(issued) => {
                    jar = jar.add(session_cookie(&state, issued));
                    state.config.app_link(&done.redirect)
                }
                None => {
                    let mut url = url::Url::parse(&state.config.app_link(&done.redirect))
                        .unwrap_or_else(|_| state.config.public_base_url.clone());
                    url.query_pairs_mut().append_pair("linked", &provider);
                    url.to_string()
                }
            };
            (jar, Redirect::to(&target)).into_response()
        }
        Err(err) => {
            let code = match &err {
                ApiError::Forbidden { code, .. }
                | ApiError::Unauthorized { code, .. }
                | ApiError::Conflict { code, .. }
                | ApiError::BadRequest { code, .. } => *code,
                ApiError::NotFound => "oidc_unknown_provider",
                _ => {
                    tracing::error!(error = %err, provider, "OIDC callback failed");
                    "oidc_failed"
                }
            };
            fail(code)
        }
    }
}

// Invitations ------------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct InvitationTokenQuery {
    pub token: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InvitationPreviewView {
    pub workspace_name: String,
    pub email: String,
    pub role: Role,
}

/// What an invitation is for, so the accept page can show it.
#[utoipa::path(get, path = "/invitations/lookup", tag = "invitations", params(InvitationTokenQuery),
    responses((status = 200, body = InvitationPreviewView), (status = 422, body = ErrorBody)))]
async fn preview_invitation(
    State(state): State<AppState>,
    Query(q): Query<InvitationTokenQuery>,
) -> ApiResult<Json<InvitationPreviewView>> {
    let p = svc::preview_invitation(&state, &q.token).await?;
    Ok(Json(InvitationPreviewView {
        workspace_name: p.workspace_name,
        email: p.email,
        role: p.role,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AcceptInvitationRequest {
    pub token: String,
    /// Required when nobody is signed in (a new account is created).
    pub name: Option<String>,
    /// Required when nobody is signed in.
    pub password: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AcceptInvitationResponse {
    pub workspace_id: WorkspaceId,
    pub user_id: UserId,
    /// Present when a new account was created and signed in.
    pub session: Option<SessionResponse>,
}

/// Accept an invitation, as the signed-in user or by creating an account.
#[utoipa::path(post, path = "/invitations/accept", tag = "invitations", request_body = AcceptInvitationRequest,
    responses((status = 200, body = AcceptInvitationResponse), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn accept_invitation(
    State(state): State<AppState>,
    OptionalSession(session): OptionalSession,
    ClientMeta(meta): ClientMeta,
    jar: CookieJar,
    Json(body): Json<AcceptInvitationRequest>,
) -> ApiResult<(CookieJar, Json<AcceptInvitationResponse>)> {
    let accepted = svc::accept_invitation(
        &state,
        session.as_ref(),
        &body.token,
        body.name.as_deref(),
        body.password.as_deref(),
        &meta,
    )
    .await?;
    let jar = match &accepted.session {
        Some(issued) => jar.add(session_cookie(&state, issued)),
        None => jar,
    };
    Ok((
        jar,
        Json(AcceptInvitationResponse {
            workspace_id: accepted.workspace_id,
            user_id: accepted.user_id,
            session: accepted.session.as_ref().map(SessionResponse::from),
        }),
    ))
}

// Desktop and mobile apps -----------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct StartAppSignInRequest {
    /// Shown when approving and in the session list, such as "Sam's MacBook".
    pub device_name: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AppSignInStarted {
    /// Show this to the person, who types it on the approval page.
    pub user_code: String,
    /// Keep this secret and poll `POST /auth/app/token` with it.
    pub poll_token: String,
    /// The approval page, to open in the browser. It deliberately does not
    /// carry the code.
    pub verification_url: String,
    pub expires_at: DateTime<Utc>,
    /// Poll no more often than this.
    pub interval_seconds: u32,
}

/// Start signing in a desktop or mobile app.
///
/// The app shows `user_code`, opens `verification_url` in the browser and
/// polls `POST /auth/app/token`. The person signs in there as usual, types
/// the code and approves.
#[utoipa::path(post, path = "/auth/app/start", tag = "auth", request_body = StartAppSignInRequest,
    responses((status = 200, body = AppSignInStarted), (status = 422, body = ErrorBody)))]
async fn start_app_sign_in(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    Json(body): Json<StartAppSignInRequest>,
) -> ApiResult<Json<AppSignInStarted>> {
    let started = app_svc::start(&state, &body.device_name, &meta).await?;
    Ok(Json(AppSignInStarted {
        user_code: started.user_code,
        poll_token: started.poll_token,
        verification_url: started.verification_url,
        expires_at: started.expires_at,
        interval_seconds: app_svc::POLL_INTERVAL_SECS,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AppSignInTokenRequest {
    pub poll_token: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AppSession {
    /// Send as `Authorization: Bearer <token>`. Store it in the device's keychain.
    pub token: String,
    pub session_id: Uuid,
    pub user_id: UserId,
    pub expires_at: DateTime<Utc>,
}

/// Collect the app's session once the sign-in is approved.
///
/// Until then this answers 400 `authorization_pending`. A refused sign-in is
/// 403 `access_denied`; an expired or already collected one is 400
/// `expired_token`.
#[utoipa::path(post, path = "/auth/app/token", tag = "auth", request_body = AppSignInTokenRequest,
    responses((status = 200, body = AppSession), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)))]
async fn app_sign_in_token(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    Json(body): Json<AppSignInTokenRequest>,
) -> ApiResult<Json<AppSession>> {
    let issued = app_svc::collect(&state, &body.poll_token, &meta).await?;
    Ok(Json(AppSession {
        token: issued.token,
        session_id: issued.session_id,
        user_id: issued.user_id,
        expires_at: issued.expires_at,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct AppSignInLookup {
    /// The code the app shows, with or without the dash.
    pub code: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PendingAppSignInView {
    pub device_name: String,
    /// Where the request came from, so the person can tell it is theirs.
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// The app waiting for a code, before approving it.
#[utoipa::path(get, path = "/auth/app/lookup", tag = "auth", params(AppSignInLookup),
    responses((status = 200, body = PendingAppSignInView), (status = 400, body = ErrorBody)),
    security(("session" = [])))]
async fn lookup_app_sign_in(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
    Query(query): Query<AppSignInLookup>,
) -> ApiResult<Json<PendingAppSignInView>> {
    let found = app_svc::lookup(&state, &session, &query.code).await?;
    Ok(Json(PendingAppSignInView {
        device_name: found.device_name,
        ip: found.ip.map(|ip| ip.to_string()),
        user_agent: found.user_agent,
        created_at: found.created_at,
        expires_at: found.expires_at,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DecideAppSignInRequest {
    pub code: String,
    /// True signs the app in as you; false refuses it.
    pub approve: bool,
}

/// Approve or refuse an app's sign-in.
#[utoipa::path(post, path = "/auth/app/decide", tag = "auth", request_body = DecideAppSignInRequest,
    responses((status = 204), (status = 400, body = ErrorBody)), security(("session" = [])))]
async fn decide_app_sign_in(
    State(state): State<AppState>,
    CurrentSession(session): CurrentSession,
    Json(body): Json<DecideAppSignInRequest>,
) -> ApiResult<StatusCode> {
    app_svc::decide(&state, &session, &body.code, body.approve).await?;
    Ok(StatusCode::NO_CONTENT)
}
