//! Signing in the desktop and mobile apps through the browser.
//!
//! 1. The app calls [`start`] and shows the code (`BCDF-GHJK`).
//! 2. The person opens the approval page in a browser where they are signed
//!    in, types the code, checks the device name, and approves ([`decide`]).
//!    Typing the code, rather than following a link that carries it, means
//!    someone cannot send a link that signs their own device into your account.
//! 3. The app polls [`collect`] with its secret and receives a session token
//!    of its own (`ohs_…`), sent as `Authorization: Bearer`.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use core_domain::authz::RequestMeta;
use core_domain::crypto::{hash_token, new_token, random_bytes};
use uuid::Uuid;

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::extract::{APP_SESSION_PREFIX, Session};

/// Consonants only, so codes never spell words and never mix up 0/O or 1/I.
const CODE_ALPHABET: &[u8; 20] = b"BCDFGHJKLMNPQRSTVWXZ";
const CODE_LEN: usize = 8;
const POLL_PREFIX: &str = "ohp_";
/// How often the app should poll, in seconds.
pub const POLL_INTERVAL_SECS: u32 = 3;

fn new_user_code() -> String {
    random_bytes::<CODE_LEN>()
        .iter()
        // 256 is not a multiple of 20, so the first 16 letters are very
        // slightly likelier; with 20^8 codes that expire in minutes, it does not matter.
        .map(|b| CODE_ALPHABET[usize::from(*b) % CODE_ALPHABET.len()] as char)
        .collect()
}

/// `bcdf-ghjk`, `BCDFGHJK` and ` BCDF GHJK ` are the same code.
pub fn normalize_code(input: &str) -> Option<String> {
    let code: String = input
        .chars()
        .filter(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == CODE_LEN).then_some(code)
}

/// `BCDFGHJK` → `BCDF-GHJK`.
pub fn display_code(code: &str) -> String {
    let (a, b) = code.split_at(code.len() / 2);
    format!("{a}-{b}")
}

pub struct Started {
    pub user_code: String,
    pub poll_token: String,
    pub verification_url: String,
    pub expires_at: DateTime<Utc>,
}

pub async fn start(state: &AppState, device_name: &str, meta: &RequestMeta) -> ApiResult<Started> {
    let device_name = device_name.trim();
    if device_name.is_empty() || device_name.chars().count() > 100 {
        return Err(ApiError::invalid(
            "device_name",
            "give the device a name of 1 to 100 characters",
        ));
    }
    let poll_token = format!("{POLL_PREFIX}{}", new_token());
    let expires_at = Utc::now() + super::auth::to_chrono(state.config.app_sign_in_ttl);
    // A clash between two live codes is astronomically unlikely; try again if it happens.
    for _ in 0..3 {
        let user_code = new_user_code();
        let mut tx = core_db::begin(&state.db, None, None).await?;
        let started = core_db::app_sign_ins::start(
            &mut tx,
            core_db::app_sign_ins::NewAppSignIn {
                id: core_domain::ids::new_id(),
                user_code: &user_code,
                poll_hash: &hash_token(&poll_token),
                device_name,
                ip: meta.ip,
                user_agent: meta.user_agent.as_deref(),
                expires_at,
            },
        )
        .await;
        match started {
            Ok(()) => {
                tx.commit().await?;
                return Ok(Started {
                    user_code: display_code(&user_code),
                    poll_token,
                    verification_url: state.config.app_url("/link-app", &[]),
                    expires_at,
                });
            }
            Err(err) if err.is_unique_violation(Some("app_sign_ins_user_code_key")) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(ApiError::internal("could not pick an unused sign-in code"))
}

fn no_such_code() -> ApiError {
    ApiError::bad_request(
        "code_not_found",
        "no app is waiting for that code; check it, or start again in the app",
    )
}

/// The request behind a code, so the person can check it is their device.
pub async fn lookup(
    state: &AppState,
    session: &Session,
    code: &str,
) -> ApiResult<core_db::app_sign_ins::PendingAppSignIn> {
    let code = normalize_code(code).ok_or_else(no_such_code)?;
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let found = core_db::app_sign_ins::find(&mut tx, &code).await?;
    tx.commit().await?;
    found.ok_or_else(no_such_code)
}

pub async fn decide(
    state: &AppState,
    session: &Session,
    code: &str,
    approve: bool,
) -> ApiResult<()> {
    let code = normalize_code(code).ok_or_else(no_such_code)?;
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    if !core_db::app_sign_ins::decide(&mut tx, &code, approve).await? {
        return Err(no_such_code());
    }
    tx.commit().await?;
    tracing::info!(user_id = %session.user_id, approve, "app sign-in decided");
    Ok(())
}

/// A session issued to an app.
#[derive(Debug)]
pub struct AppSession {
    pub token: String,
    pub session_id: Uuid,
    pub user_id: UserId,
    pub expires_at: DateTime<Utc>,
}

pub async fn collect(
    state: &AppState,
    poll_token: &str,
    meta: &RequestMeta,
) -> ApiResult<AppSession> {
    use core_db::app_sign_ins::Collected;

    let mut tx = core_db::begin(&state.db, None, None).await?;
    let collected = core_db::app_sign_ins::collect(&mut tx, &hash_token(poll_token)).await?;
    let (user_id, device_name) = match collected {
        Collected::Pending => {
            return Err(ApiError::bad_request(
                "authorization_pending",
                "waiting for the sign-in to be approved in the browser",
            ));
        }
        Collected::Denied => {
            tx.commit().await?;
            return Err(ApiError::forbidden(
                "access_denied",
                "the sign-in was refused in the browser",
            ));
        }
        Collected::Expired => {
            return Err(ApiError::bad_request(
                "expired_token",
                "this sign-in has expired; start again in the app",
            ));
        }
        Collected::Approved {
            user_id,
            device_name,
        } => (user_id, device_name),
    };

    core_db::set_context(&mut tx, None, Some(user_id)).await?;
    let token = format!("{APP_SESSION_PREFIX}{}", new_token());
    let expires_at = Utc::now() + super::auth::to_chrono(state.config.app_session_ttl);
    let session_id = core_db::sessions::insert(
        &mut tx,
        core_db::sessions::NewSession {
            user_id,
            token_hash: &hash_token(&token),
            // Unused: bearer tokens are not sent automatically, so need no CSRF check.
            csrf_token: &new_token(),
            auth_method: "app_approval",
            // The approving browser session had passed its second factor.
            mfa_verified: true,
            ip: meta.ip,
            user_agent: meta.user_agent.as_deref(),
            expires_at,
            client: "app",
            device_name: Some(&device_name),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(%user_id, %session_id, "app signed in");
    Ok(AppSession {
        token,
        session_id,
        user_id,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_eight_consonants_and_read_back_however_typed() {
        for _ in 0..100 {
            let code = new_user_code();
            assert_eq!(code.len(), CODE_LEN);
            assert!(code.bytes().all(|b| CODE_ALPHABET.contains(&b)));
            assert_eq!(
                normalize_code(&display_code(&code)).as_deref(),
                Some(code.as_str())
            );
        }
        assert_eq!(display_code("BCDFGHJK"), "BCDF-GHJK");
        assert_eq!(normalize_code(" bcdf ghjk ").as_deref(), Some("BCDFGHJK"));
        assert_eq!(normalize_code("BCDF-GHJ"), None);
        assert_eq!(normalize_code("BCDF-GHJKL"), None);
    }
}
