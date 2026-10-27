//! Sign-in and account security: passwords, sessions, TOTP, magic links and
//! accepting invitations.

use chrono::{Duration as ChronoDuration, Utc};
use core_db::Tx;
use core_domain::authz::RequestMeta;
use core_domain::crypto::{hash_token, new_token};
use core_domain::{Actor, Ctx, Role, UserId, WorkspaceId};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::extract::Session;
use crate::password;
use crate::services::{Change, record};
use crate::totp;

/// A session just created. The token goes in the cookie and nowhere else.
#[derive(Debug)]
pub struct IssuedSession {
    pub token: String,
    pub csrf_token: String,
    pub session_id: Uuid,
    pub user_id: UserId,
    /// True when the user has TOTP on and must enter a code before the session is usable.
    pub mfa_required: bool,
    pub expires_at: chrono::DateTime<Utc>,
}

pub(crate) fn to_chrono(d: std::time::Duration) -> ChronoDuration {
    ChronoDuration::from_std(d).unwrap_or(ChronoDuration::MAX)
}

fn invalid_credentials() -> ApiError {
    ApiError::unauthorized(
        "invalid_credentials",
        "wrong email or password, or the account is temporarily locked after too many attempts",
    )
}

/// Creates a session for `user` inside `tx`, which must have `user` in context.
pub async fn issue_session(
    state: &AppState,
    tx: &mut Tx,
    user: UserId,
    method: &str,
    meta: &RequestMeta,
) -> ApiResult<IssuedSession> {
    let totp = core_db::users::totp_state(tx, user)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mfa_required = totp.enabled_at.is_some();
    let ttl = if mfa_required {
        state.config.mfa_pending_ttl
    } else {
        state.config.session_ttl
    };
    let expires_at = Utc::now() + to_chrono(ttl);
    let token = new_token();
    let csrf_token = new_token();
    let session_id = core_db::sessions::insert(
        tx,
        core_db::sessions::NewSession {
            user_id: user,
            token_hash: &hash_token(&token),
            csrf_token: &csrf_token,
            auth_method: method,
            mfa_verified: !mfa_required,
            ip: meta.ip,
            user_agent: meta.user_agent.as_deref(),
            expires_at,
            client: "browser",
            device_name: None,
        },
    )
    .await?;
    Ok(IssuedSession {
        token,
        csrf_token,
        session_id,
        user_id: user,
        mfa_required,
        expires_at,
    })
}

/// Clears the failed sign-in count once a session is fully signed in. A
/// session still waiting for its second factor leaves it alone, so wrong
/// codes keep counting towards the lockout across fresh sign-ins.
pub(crate) async fn reset_failures_if_signed_in(
    tx: &mut Tx,
    issued: &IssuedSession,
) -> ApiResult<()> {
    if !issued.mfa_required {
        core_db::users::reset_login_failures(tx, issued.user_id).await?;
    }
    Ok(())
}

pub async fn login(
    state: &AppState,
    email: &str,
    password_input: &str,
    meta: &RequestMeta,
) -> ApiResult<IssuedSession> {
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let found = core_db::auth::find_user_by_email(&mut tx, email.trim()).await?;
    let hash = found.as_ref().and_then(|f| f.password_hash.as_deref());
    let password_ok = password::verify(password_input, hash).await?;

    let Some(found) = found else {
        return Err(invalid_credentials());
    };
    core_db::set_context(&mut tx, None, Some(found.user_id)).await?;
    let locked = found.locked_until.is_some_and(|until| until > Utc::now());
    if !password_ok || locked {
        if !locked {
            core_db::users::record_login_failure(
                &mut tx,
                found.user_id,
                state.config.max_login_failures,
                state.config.lockout.as_secs_f64(),
            )
            .await?;
        }
        tx.commit().await?;
        tracing::info!(user_id = %found.user_id, locked, "sign-in failed");
        return Err(invalid_credentials());
    }
    let issued = issue_session(state, &mut tx, found.user_id, "password", meta).await?;
    reset_failures_if_signed_in(&mut tx, &issued).await?;
    tx.commit().await?;
    tracing::info!(user_id = %found.user_id, "signed in with password");
    Ok(issued)
}

pub async fn logout(state: &AppState, session: &Session) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    core_db::sessions::revoke(&mut tx, session.id).await?;
    tx.commit().await?;
    Ok(())
}

/// Finishes sign-in with a TOTP code or a recovery code. Returns when the
/// session now expires.
pub async fn verify_mfa(
    state: &AppState,
    session: &Session,
    code: Option<&str>,
    recovery_code: Option<&str>,
) -> ApiResult<chrono::DateTime<Utc>> {
    if session.mfa_verified {
        return Ok(session.expires_at);
    }
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let too_many =
        || ApiError::unauthorized("session_expired", "too many wrong codes; sign in again");
    // Wrong codes also count against the account, so signing in again does
    // not buy more guesses; a locked account takes no codes at all.
    if core_db::users::is_locked(&mut tx, session.user_id).await? {
        core_db::sessions::revoke(&mut tx, session.id).await?;
        tx.commit().await?;
        return Err(too_many());
    }
    let ok = match (code, recovery_code) {
        (Some(code), _) => check_current_totp(state, &mut tx, session.user_id, code).await?,
        (None, Some(recovery)) => {
            core_db::users::use_recovery_code(
                &mut tx,
                session.user_id,
                &totp::hash_recovery_code(recovery),
            )
            .await?
        }
        (None, None) => {
            return Err(ApiError::bad_request(
                "code_required",
                "send `code` or `recovery_code`",
            ));
        }
    };
    if !ok {
        let still_usable = core_db::sessions::record_mfa_failure(
            &mut tx,
            session.id,
            state.config.max_mfa_failures,
        )
        .await?;
        core_db::users::record_login_failure(
            &mut tx,
            session.user_id,
            state.config.max_login_failures,
            state.config.lockout.as_secs_f64(),
        )
        .await?;
        let locked = core_db::users::is_locked(&mut tx, session.user_id).await?;
        if locked {
            core_db::sessions::revoke(&mut tx, session.id).await?;
        }
        tx.commit().await?;
        tracing::info!(user_id = %session.user_id, locked, "wrong second-factor code");
        return Err(if still_usable && !locked {
            ApiError::unauthorized("invalid_code", "that code is not valid")
        } else {
            too_many()
        });
    }
    let expires_at = Utc::now() + to_chrono(state.config.session_ttl);
    core_db::sessions::mark_mfa_verified(&mut tx, session.id, expires_at).await?;
    core_db::users::reset_login_failures(&mut tx, session.user_id).await?;
    tx.commit().await?;
    Ok(expires_at)
}

/// Checks a code against the user's active TOTP secret, rejecting replays.
async fn check_current_totp(
    state: &AppState,
    tx: &mut Tx,
    user: UserId,
    code: &str,
) -> ApiResult<bool> {
    let st = core_db::users::totp_state(tx, user)
        .await?
        .ok_or(ApiError::NotFound)?;
    let Some(sealed) = st.secret_enc.filter(|_| st.enabled_at.is_some()) else {
        return Ok(false);
    };
    let secret = totp::decrypt_secret(&state.config.keyring, user, &sealed)?;
    match totp::check(&secret, code, now_unix())? {
        Some(step) => Ok(core_db::users::advance_totp_step(tx, user, step).await?),
        None => Ok(false),
    }
}

fn now_unix() -> u64 {
    u64::try_from(Utc::now().timestamp()).unwrap_or_default()
}

pub struct TotpSetup {
    pub otpauth_url: String,
    pub secret: String,
}

pub async fn totp_setup(state: &AppState, session: &Session) -> ApiResult<TotpSetup> {
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let user = core_db::users::get(&mut tx, session.user_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    if user.totp_enabled_at.is_some() {
        return Err(ApiError::conflict(
            "totp_already_enabled",
            "two-factor authentication is already on",
        ));
    }
    let secret = totp::new_secret();
    let sealed = totp::encrypt_secret(&state.config.keyring, user.id, &secret);
    core_db::users::set_totp_pending(&mut tx, user.id, &sealed).await?;
    tx.commit().await?;
    let (otpauth_url, manual) = totp::provisioning(&secret, &user.email)?;
    Ok(TotpSetup {
        otpauth_url,
        secret: manual,
    })
}

/// Turns TOTP on once the user proves their app has the pending secret.
/// Returns the recovery codes, shown once.
pub async fn totp_enable(
    state: &AppState,
    session: &Session,
    code: &str,
) -> ApiResult<Vec<String>> {
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let st = core_db::users::totp_state(&mut tx, session.user_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    if st.enabled_at.is_some() {
        return Err(ApiError::conflict(
            "totp_already_enabled",
            "two-factor authentication is already on",
        ));
    }
    let sealed = st
        .pending_secret_enc
        .ok_or_else(|| ApiError::bad_request("totp_not_started", "start setup first"))?;
    let secret = totp::decrypt_secret(&state.config.keyring, session.user_id, &sealed)?;
    let step = totp::check(&secret, code, now_unix())?.ok_or_else(|| {
        ApiError::bad_request(
            "invalid_code",
            "that code is not valid; check your device's clock",
        )
    })?;
    core_db::users::enable_totp(&mut tx, session.user_id, step).await?;
    let (codes, hashes) = totp::new_recovery_codes();
    core_db::users::replace_recovery_codes(&mut tx, session.user_id, &hashes).await?;
    tx.commit().await?;
    tracing::info!(user_id = %session.user_id, "two-factor authentication enabled");
    Ok(codes)
}

pub async fn totp_disable(state: &AppState, session: &Session, code: &str) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    if !check_current_totp(state, &mut tx, session.user_id, code).await? {
        return Err(ApiError::bad_request(
            "invalid_code",
            "that code is not valid",
        ));
    }
    core_db::users::disable_totp(&mut tx, session.user_id).await?;
    tx.commit().await?;
    tracing::info!(user_id = %session.user_id, "two-factor authentication disabled");
    Ok(())
}

pub async fn regenerate_recovery_codes(
    state: &AppState,
    session: &Session,
    code: &str,
) -> ApiResult<Vec<String>> {
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    if !check_current_totp(state, &mut tx, session.user_id, code).await? {
        return Err(ApiError::bad_request(
            "invalid_code",
            "that code is not valid",
        ));
    }
    let (codes, hashes) = totp::new_recovery_codes();
    core_db::users::replace_recovery_codes(&mut tx, session.user_id, &hashes).await?;
    tx.commit().await?;
    Ok(codes)
}

/// Sets a new password and signs out every other session. Users who already
/// have a password must give it.
pub async fn change_password(
    state: &AppState,
    session: &Session,
    current: Option<&str>,
    new: &str,
) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let user = core_db::users::get(&mut tx, session.user_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    if user.has_password {
        let hash = core_db::users::password_hash(&mut tx, user.id).await?;
        let current = current.ok_or_else(|| {
            ApiError::bad_request("current_password_required", "enter your current password")
        })?;
        if !password::verify(current, hash.as_deref()).await? {
            return Err(ApiError::BadRequest {
                code: "invalid_credentials",
                message: "your current password is wrong".into(),
                field: Some("current_password"),
            });
        }
    }
    password::check_policy(new, &user.email)?;
    let hash = password::hash(new).await?;
    core_db::users::set_password(&mut tx, user.id, &hash).await?;
    core_db::sessions::revoke_all_except(&mut tx, user.id, Some(session.id)).await?;
    tx.commit().await?;
    tracing::info!(user_id = %user.id, "password changed");
    Ok(())
}

/// Emails a one-time sign-in link if the address has an account. Always
/// succeeds from the caller's point of view, so it cannot be used to probe
/// which addresses exist.
pub async fn request_magic_link(state: &AppState, email: &str) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let Some(found) = core_db::auth::find_user_by_email(&mut tx, email.trim()).await? else {
        return Ok(());
    };
    core_db::set_context(&mut tx, None, Some(found.user_id)).await?;
    let user = core_db::users::get(&mut tx, found.user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let token = new_token();
    let expires_at = Utc::now() + to_chrono(state.config.magic_link_ttl);
    core_db::users::insert_magic_link(&mut tx, user.id, &hash_token(&token), expires_at).await?;
    let url = state.config.app_url("/auth/magic", &[("token", &token)]);
    let event_id = core_domain::ids::new_id();
    let sealed =
        crate::services::seal_event_secret(&state.config.keyring, event_id, &json!({ "url": url }));
    core_db::outbox::emit_with_secret(
        &mut tx,
        event_id,
        None,
        "auth.magic_link_requested",
        &json!({ "user_id": user.id, "email": user.email, "name": user.name, "expires_at": expires_at }),
        Some(&sealed),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn consume_magic_link(
    state: &AppState,
    token: &str,
    meta: &RequestMeta,
) -> ApiResult<IssuedSession> {
    let invalid = || {
        ApiError::unauthorized(
            "link_invalid",
            "that sign-in link has expired or was already used",
        )
    };
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let link = core_db::auth::find_magic_link(&mut tx, &hash_token(token))
        .await?
        .ok_or_else(invalid)?;
    core_db::set_context(&mut tx, None, Some(link.user_id)).await?;
    if !core_db::users::use_magic_link(&mut tx, link.magic_link_id).await? {
        return Err(invalid());
    }
    core_db::users::mark_email_verified(&mut tx, link.user_id).await?;
    let issued = issue_session(state, &mut tx, link.user_id, "magic_link", meta).await?;
    reset_failures_if_signed_in(&mut tx, &issued).await?;
    tx.commit().await?;
    Ok(issued)
}

pub async fn sessions(
    state: &AppState,
    session: &Session,
) -> ApiResult<Vec<core_db::sessions::SessionInfo>> {
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let list = core_db::sessions::list_active(&mut tx, session.user_id).await?;
    tx.commit().await?;
    Ok(list)
}

pub async fn revoke_session(state: &AppState, session: &Session, id: Uuid) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    // Row-level security limits this to the caller's own sessions.
    if !core_db::sessions::revoke(&mut tx, id).await? {
        return Err(ApiError::NotFound);
    }
    tx.commit().await?;
    Ok(())
}

// Invitations ---------------------------------------------------------------------

pub struct InvitationPreview {
    pub workspace_name: String,
    pub email: String,
    pub role: Role,
}

pub async fn preview_invitation(state: &AppState, token: &str) -> ApiResult<InvitationPreview> {
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let inv = core_db::auth::find_invitation(&mut tx, &hash_token(token))
        .await?
        .ok_or_else(invitation_invalid)?;
    tx.commit().await?;
    Ok(InvitationPreview {
        workspace_name: inv.workspace_name,
        email: inv.email,
        role: inv.role,
    })
}

fn invitation_invalid() -> ApiError {
    ApiError::Unprocessable {
        code: "invitation_invalid",
        message: "that invitation has expired, was revoked or was already used".into(),
    }
}

pub struct Accepted {
    pub workspace_id: WorkspaceId,
    pub user_id: UserId,
    /// Present when accepting created the account and signed it in.
    pub session: Option<IssuedSession>,
}

/// Accepts an invitation as the signed-in user, or by creating an account
/// with `name` and `password` when nobody is signed in.
pub async fn accept_invitation(
    state: &AppState,
    session: Option<&Session>,
    token: &str,
    name: Option<&str>,
    password_input: Option<&str>,
    meta: &RequestMeta,
) -> ApiResult<Accepted> {
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let inv = core_db::auth::find_invitation(&mut tx, &hash_token(token))
        .await?
        .ok_or_else(invitation_invalid)?;

    let (user_id, created) = match session {
        Some(session) => {
            core_db::set_context(&mut tx, None, Some(session.user_id)).await?;
            let user = core_db::users::get(&mut tx, session.user_id)
                .await?
                .ok_or(ApiError::Unauthenticated)?;
            if !user.email.eq_ignore_ascii_case(&inv.email) {
                return Err(ApiError::forbidden(
                    "invitation_email_mismatch",
                    format!(
                        "this invitation is for {}; sign in with that address to accept it",
                        inv.email
                    ),
                ));
            }
            (user.id, false)
        }
        None => {
            if core_db::auth::find_user_by_email(&mut tx, &inv.email)
                .await?
                .is_some()
            {
                return Err(ApiError::conflict(
                    "account_exists",
                    "an account with this email already exists; sign in, then accept the invitation",
                ));
            }
            let name = core_domain::workspace::validate_name(
                "name",
                name.ok_or_else(|| ApiError::bad_request("name_required", "enter your name"))?,
            )?;
            let password_input = password_input
                .ok_or_else(|| ApiError::bad_request("password_required", "choose a password"))?;
            password::check_policy(password_input, &inv.email)?;
            let hash = password::hash(password_input).await?;
            let user_id = UserId::new();
            core_db::set_context(&mut tx, None, Some(user_id)).await?;
            core_db::users::insert(
                &mut tx,
                core_db::users::NewUser {
                    id: user_id,
                    email: &inv.email,
                    name: &name,
                    password_hash: Some(&hash),
                    // Holding the token proves the inbox is theirs.
                    email_verified: true,
                },
            )
            .await?;
            (user_id, true)
        }
    };

    join_from_invitation(&mut tx, &inv, user_id, meta).await?;
    let session = if created {
        Some(issue_session(state, &mut tx, user_id, "invitation", meta).await?)
    } else {
        None
    };
    tx.commit().await?;
    Ok(Accepted {
        workspace_id: inv.workspace_id,
        user_id,
        session,
    })
}

/// Adds `user` to the invitation's workspace and marks it accepted. Leaves
/// the transaction's context on that workspace and user.
pub async fn join_from_invitation(
    tx: &mut Tx,
    inv: &core_db::auth::InvitationLookup,
    user: UserId,
    meta: &RequestMeta,
) -> ApiResult<()> {
    core_db::set_context(tx, Some(inv.workspace_id), Some(user)).await?;
    if !core_db::invitations::mark_accepted(tx, inv.invitation_id, user).await? {
        return Err(invitation_invalid());
    }
    let ctx = Ctx {
        workspace_id: inv.workspace_id,
        actor: Actor::User {
            user_id: user,
            impersonator: None,
        },
        role: Some(inv.role),
        permissions: Default::default(),
        meta: meta.clone(),
    };
    if core_db::members::role_of(tx, user).await?.is_none() {
        let membership =
            core_db::members::insert(tx, inv.workspace_id, user, inv.role, Some(user)).await?;
        record(
            tx,
            &ctx,
            Change {
                topic: "member.added",
                resource: "member",
                object_id: Some(user.0),
                before: None,
                after: Some(json!({ "user_id": user, "role": inv.role, "membership_id": membership, "via": "invitation", "invitation_id": inv.invitation_id })),
                event_extra: None,
            },
        )
        .await?;
    }
    record(
        tx,
        &ctx,
        Change {
            topic: "invitation.accepted",
            resource: "invitation",
            object_id: Some(inv.invitation_id),
            before: None,
            after: Some(json!({ "accepted_by": user })),
            event_extra: None,
        },
    )
    .await?;
    Ok(())
}
