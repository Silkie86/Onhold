//! `Idempotency-Key` support for writes. A client (an offline queue
//! replaying, a retry after a dropped connection) can send the same request
//! again and get the original response back instead of acting twice.
//!
//! Keys are scoped to the caller (the person or API key, not the session, so
//! a queued write replayed after signing in again is still recognised) and the
//! workspace, and held for 24 hours. Reusing a key for a different request is
//! rejected; a retry that arrives while the first attempt is still running gets
//! 409. Only successful (2xx) outcomes are remembered: an error, or a request
//! cut off by the timeout or a dropped connection, releases the key so a retry
//! runs for real.

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use core_domain::crypto::{constant_time_eq, hash_token};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::extract::{Credential, WORKSPACE_HEADER, read_credential};

pub const HEADER: &str = "idempotency-key";
pub const REPLAYED_HEADER: &str = "idempotent-replayed";
const MAX_BODY: usize = 1024 * 1024;

pub async fn layer(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let Some(key) = req.headers().get(HEADER).cloned() else {
        return next.run(req).await;
    };
    let Some(key) = key
        .to_str()
        .ok()
        .filter(|k| (1..=255).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_graphic()))
        .map(str::to_owned)
    else {
        return ApiError::bad_request(
            "invalid_idempotency_key",
            "Idempotency-Key must be 1 to 255 visible ASCII characters",
        )
        .into_response();
    };
    // Unauthenticated writes (sign-in) are not scoped to anyone; they run as
    // normal, and so does a write with a credential that does not check out,
    // which the handler then refuses.
    let caller = match caller(&state, req.headers()).await {
        Ok(Some(caller)) => caller,
        Ok(None) => return next.run(req).await,
        Err(err) => return err.into_response(),
    };
    let mut scope = Sha256::new();
    scope.update(caller.as_bytes());
    scope.update(b"\n");
    scope.update(
        req.headers()
            .get(WORKSPACE_HEADER)
            .map(HeaderValue::as_bytes)
            .unwrap_or_default(),
    );
    let scope = scope.finalize().to_vec();

    let (parts, body) = req.into_parts();
    let Ok(bytes) = to_bytes(body, crate::MAX_UPLOAD_BODY_BYTES).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response();
    };
    let mut hasher = Sha256::new();
    hasher.update(parts.method.as_str());
    hasher.update(b" ");
    hasher.update(
        parts
            .uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or_default(),
    );
    hasher.update(b"\n");
    hasher.update(
        parts
            .headers
            .get(WORKSPACE_HEADER)
            .map(HeaderValue::as_bytes)
            .unwrap_or_default(),
    );
    hasher.update(b"\n");
    hasher.update(&bytes);
    let request_hash = hasher.finalize().to_vec();

    match core_db::idempotency::claim(&state.db, &scope, &key, &request_hash).await {
        Err(err) => return ApiError::from(err).into_response(),
        Ok(Some(stored)) => return replay(stored, &request_hash),
        Ok(None) => {}
    }
    let claim = Claim {
        state: state.clone(),
        scope,
        key,
        armed: true,
    };

    let response = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    if !response.status().is_success() {
        claim.release().await;
        return response;
    }
    // The write is done. Store its response in a task of its own, so that a
    // request cut off now still records it rather than leaving the key claimed.
    let (parts, body) = response.into_parts();
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let status = i16::try_from(parts.status.as_u16()).unwrap_or(i16::MAX);
    let stored = tokio::spawn(store(claim, status, content_type, body)).await;
    match stored {
        Ok(Some(body)) => Response::from_parts(parts, Body::from(body)),
        Ok(None) => {
            ApiError::internal("response too large to store for idempotency").into_response()
        }
        Err(err) => ApiError::internal(err).into_response(),
    }
}

/// Who is calling, checked: a person (whichever session they use) or an API
/// key. `None` for no credential, or one that is not valid.
async fn caller(state: &AppState, headers: &axum::http::HeaderMap) -> ApiResult<Option<String>> {
    let Some(cred) = read_credential(headers)? else {
        return Ok(None);
    };
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let caller = match cred {
        Credential::Session { token, bearer } => {
            let expected_client = if bearer { "app" } else { "browser" };
            core_db::auth::find_session(&mut tx, &hash_token(&token))
                .await?
                .filter(|f| f.client == expected_client)
                .map(|f| format!("user:{}", f.user_id))
        }
        Credential::ApiKey { key_id, secret } => core_db::auth::find_api_key(&mut tx, key_id)
            .await?
            .filter(|k| constant_time_eq(&hash_token(&secret), &k.secret_hash))
            .map(|_| format!("api_key:{key_id}")),
    };
    tx.commit().await?;
    Ok(caller)
}

/// A key this request has claimed. Dropped while still armed (the timeout
/// fired or the client went away mid-request), it releases the key in the
/// background, so the retry is not told "still running" for a day.
struct Claim {
    state: AppState,
    scope: Vec<u8>,
    key: String,
    armed: bool,
}

impl Claim {
    async fn release(mut self) {
        release(&self.state, &self.scope, &self.key).await;
        self.armed = false;
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let state = self.state.clone();
        let scope = std::mem::take(&mut self.scope);
        let key = std::mem::take(&mut self.key);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { release(&state, &scope, &key).await });
        }
    }
}

/// Stores a successful response against its key; `None` (and the key
/// released) when the body is too large to keep.
async fn store(
    mut claim: Claim,
    status: i16,
    content_type: Option<String>,
    body: Body,
) -> Option<Bytes> {
    let Ok(body) = to_bytes(body, MAX_BODY).await else {
        claim.release().await;
        return None;
    };
    claim.armed = false;
    if let Err(err) = core_db::idempotency::complete(
        &claim.state.db,
        &claim.scope,
        &claim.key,
        status,
        content_type.as_deref(),
        &body,
    )
    .await
    {
        tracing::error!(error = %err, "could not store idempotent response");
    }
    Some(body)
}

async fn release(state: &AppState, scope: &[u8], key: &str) {
    if let Err(err) = core_db::idempotency::release(&state.db, scope, key).await {
        tracing::error!(error = %err, "could not release idempotency key");
    }
}

fn replay(stored: core_db::idempotency::Stored, request_hash: &[u8]) -> Response {
    if stored.request_hash != request_hash {
        return ApiError::Unprocessable {
            code: "idempotency_key_reused",
            message: "this Idempotency-Key was already used for a different request".into(),
        }
        .into_response();
    }
    let Some(status) = stored.status else {
        return ApiError::conflict(
            "request_in_progress",
            "the original request with this Idempotency-Key is still running",
        )
        .into_response();
    };
    let status = u16::try_from(status)
        .ok()
        .and_then(|s| StatusCode::from_u16(s).ok())
        .unwrap_or(StatusCode::OK);
    let mut response = Response::new(Body::from(Bytes::from(stored.body.unwrap_or_default())));
    *response.status_mut() = status;
    if let Some(ct) = stored
        .content_type
        .and_then(|ct| HeaderValue::from_str(&ct).ok())
    {
        response.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    response
        .headers_mut()
        .insert(REPLAYED_HEADER, HeaderValue::from_static("true"));
    response
}
