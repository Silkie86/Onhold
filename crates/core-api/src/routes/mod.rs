//! HTTP handlers. Each one translates a request into a service call and the
//! result into a response; no business rules live here.

pub mod admin;
pub mod auth;
pub mod bookings;
pub mod config;
pub mod crm;
pub mod documents;
pub mod events;
pub mod finance;
pub mod inbox;
pub mod jobs;
pub mod projects;
pub mod stripe;
pub mod things;
pub mod travel;
pub mod webhooks;
pub mod workspace;

use axum::Json;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::{Cookie, SameSite};
use chrono::{DateTime, Utc};
use core_domain::UserId;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::AppState;
use crate::extract::{SESSION_COOKIE, etag};
use crate::services::auth::IssuedSession;

/// A JSON response with the record's version as its `ETag`.
pub(crate) fn with_etag<T: Serialize>(version: i64, body: T) -> Response {
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&etag(version)) {
        headers.insert(header::ETAG, v);
    }
    (headers, Json(body)).into_response()
}

/// For PATCH fields that can be cleared: absent is `None`, `null` is
/// `Some(None)`, a value is `Some(Some(v))`.
pub fn nullable<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Keyset paging for lists that are newest first.
#[derive(Debug, Deserialize, IntoParams)]
pub struct PageQuery {
    /// Return items older than this id (the previous page's `next_before`).
    pub before: Option<Uuid>,
    /// 1 to 200, default 50.
    pub limit: Option<i64>,
}

impl PageQuery {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
}

/// The cursor for the next page: the last id, if the page was full.
pub(crate) fn next_before<T>(items: &[T], limit: i64, id: impl Fn(&T) -> Uuid) -> Option<Uuid> {
    (items.len() as i64 == limit)
        .then(|| items.last().map(id))
        .flatten()
}

/// The workspace a request acts in. Required with a session cookie; with an
/// API key it may be omitted (the key's workspace is used) or must match.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Header)]
pub struct WorkspaceHeader {
    #[param(rename = "X-Workspace-Id", value_type = Option<String>, format = Uuid)]
    #[allow(dead_code)]
    pub workspace_id: Option<String>,
}

/// Retry safety for writes: send a unique key per logical operation and a
/// retried request returns the original response instead of acting twice.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Header)]
pub struct IdempotencyHeader {
    #[param(rename = "Idempotency-Key", value_type = Option<String>)]
    #[allow(dead_code)]
    pub key: Option<String>,
}

/// Optimistic concurrency: the version the client last saw, as `"<n>"`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Header)]
pub struct IfMatchHeader {
    #[param(rename = "If-Match", value_type = Option<String>)]
    #[allow(dead_code)]
    pub if_match: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionResponse {
    pub user_id: UserId,
    /// Send this as `X-CSRF-Token` on every write made with the session cookie.
    pub csrf_token: String,
    /// When true, call `POST /auth/mfa/verify` before anything else.
    pub mfa_required: bool,
    pub expires_at: DateTime<Utc>,
}

impl From<&IssuedSession> for SessionResponse {
    fn from(s: &IssuedSession) -> Self {
        Self {
            user_id: s.user_id,
            csrf_token: s.csrf_token.clone(),
            mfa_required: s.mfa_required,
            expires_at: s.expires_at,
        }
    }
}

pub fn session_cookie(state: &AppState, issued: &IssuedSession) -> Cookie<'static> {
    session_cookie_until(state, issued.token.clone(), issued.expires_at)
}

/// The session cookie for `token`, kept by the browser until `expires_at`.
pub fn session_cookie_until(
    state: &AppState,
    token: String,
    expires_at: DateTime<Utc>,
) -> Cookie<'static> {
    let max_age = (expires_at - Utc::now()).num_seconds().max(0);
    Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .secure(state.config.cookie_secure)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::seconds(max_age))
        .build()
}

pub fn clear_session_cookie(state: &AppState) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, ""))
        .path("/")
        .http_only(true)
        .secure(state.config.cookie_secure)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::ZERO)
        .build()
}
