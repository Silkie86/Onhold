//! `Idempotency-Key` support for writes. A client (an offline queue
//! replaying, a retry after a dropped connection) can send the same request
//! again and get the original response back instead of acting twice.
//!
//! Keys are scoped to the caller's credential and held for 24 hours. Reusing
//! a key for a different request is rejected; a retry that arrives while the
//! first attempt is still running gets 409. Only successful (2xx) outcomes
//! are remembered: an error releases the key so a corrected retry runs for real.

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::error::ApiError;
use crate::extract::{WORKSPACE_HEADER, read_credential};

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
    // Unauthenticated writes (sign-in) are not scoped to anyone; they run as normal.
    let scope = match read_credential(req.headers()) {
        Ok(Some(cred)) => Sha256::digest(cred.secret().as_bytes()).to_vec(),
        Ok(None) => return next.run(req).await,
        Err(err) => return err.into_response(),
    };

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

    let response = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    if !response.status().is_success() {
        release(&state, &scope, &key).await;
        return response;
    }
    let (parts, body) = response.into_parts();
    let Ok(body) = to_bytes(body, MAX_BODY).await else {
        release(&state, &scope, &key).await;
        return ApiError::internal("response too large to store for idempotency").into_response();
    };
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let status = i16::try_from(parts.status.as_u16()).unwrap_or(i16::MAX);
    if let Err(err) =
        core_db::idempotency::complete(&state.db, &scope, &key, status, content_type, &body).await
    {
        tracing::error!(error = %err, "could not store idempotent response");
    }
    Response::from_parts(parts, Body::from(body))
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
