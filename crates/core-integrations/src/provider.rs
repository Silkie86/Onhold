//! What the mail, calendar and accounting providers have in common: how
//! their failures are told apart, and a JSON-over-HTTPS client that sorts
//! a provider's answers into those failures.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

/// Why a call to a provider failed, sorted by what the caller should do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// The provider could not be reached or failed on its side: try again later.
    #[error("the provider could not be reached: {0}")]
    Unavailable(String),
    /// Too many calls: try again after the given number of seconds, or later.
    #[error("the provider asked us to slow down")]
    RateLimited { retry_after: Option<u64> },
    /// The access token was refused: refresh it and try once more.
    #[error("the provider refused the access token")]
    Unauthorized,
    /// The person's grant is gone (revoked, password changed, consent
    /// withdrawn): only connecting again helps.
    #[error("the connection needs to be made again: {0}")]
    Revoked(String),
    /// The sync position is too old for the provider to continue from:
    /// start again from a date.
    #[error("the provider no longer knows the sync position")]
    CursorExpired,
    /// The provider refused this request; retrying the same will not help.
    #[error("the provider refused the request: {message}")]
    Refused { status: u16, message: String },
    /// The provider's answer was not understood.
    #[error("the provider's answer was not understood: {0}")]
    Invalid(String),
}

impl ProviderError {
    /// Whether trying the same call again later could succeed.
    pub fn is_temporary(&self) -> bool {
        matches!(
            self,
            Self::Unavailable(_) | Self::RateLimited { .. } | Self::Unauthorized
        )
    }
}

/// How long one provider call may take.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// How long a call carrying a whole message (reading or sending one) may take.
pub const BODY_TIMEOUT: Duration = Duration::from_secs(60);

/// A client for providers' APIs: no redirects, a timeout, and OnHold's
/// user agent.
pub fn http_client() -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("OnHold/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| ProviderError::Unavailable(e.to_string()))
}

/// `base` with `path` (which may carry a query) put after its own path, so
/// a stand-in server under a path prefix works as well as the real one.
pub fn endpoint(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    let (p, q) = path.split_once('?').unwrap_or((path, ""));
    let joined = format!("{}{}", base.path().trim_end_matches('/'), p);
    url.set_path(&joined);
    url.set_query((!q.is_empty()).then_some(q));
    url
}

/// One path segment, percent-encoded, for ids that come from a provider.
pub fn segment(id: &str) -> String {
    url::form_urlencoded::byte_serialize(id.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

/// Sends a request and reads the answer as text, sorting failures.
pub async fn send_raw(req: reqwest::RequestBuilder) -> Result<(u16, Vec<u8>), ProviderError> {
    let res = req
        .send()
        .await
        .map_err(|e| ProviderError::Unavailable(e.without_url().to_string()))?;
    let status = res.status().as_u16();
    let retry_after = res
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    let body = res
        .bytes()
        .await
        .map_err(|e| ProviderError::Unavailable(e.without_url().to_string()))?
        .to_vec();
    if (200..300).contains(&status) {
        return Ok((status, body));
    }
    Err(classify(status, retry_after, &body))
}

/// Sends a request and parses the JSON answer.
pub async fn send_json<T: DeserializeOwned>(
    req: reqwest::RequestBuilder,
) -> Result<T, ProviderError> {
    let (_, body) = send_raw(req).await?;
    let body = if body.is_empty() {
        b"null".to_vec()
    } else {
        body
    };
    serde_json::from_slice(&body).map_err(|e| ProviderError::Invalid(e.to_string()))
}

/// Sorts a failed answer. Google, Microsoft, Xero and Intuit all put a
/// message under `error.message`, `error_description`, `Message` or a plain
/// `error` string.
pub fn classify(status: u16, retry_after: Option<u64>, body: &[u8]) -> ProviderError {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let message = v
        .pointer("/error/message")
        .or_else(|| v.get("error_description"))
        .or_else(|| v.get("Message"))
        .or_else(|| v.get("message"))
        .or_else(|| v.get("error").filter(|e| e.is_string()))
        .and_then(Value::as_str)
        .unwrap_or("no reason given")
        .chars()
        .take(300)
        .collect::<String>();
    let code = v
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| v.pointer("/error/code").and_then(Value::as_str))
        .unwrap_or_default();
    match status {
        429 => ProviderError::RateLimited { retry_after },
        500..=599 => ProviderError::Unavailable(format!("status {status}")),
        // OAuth token endpoints answer 400 `invalid_grant` for a revoked
        // or expired refresh token.
        400 | 401 if code == "invalid_grant" => ProviderError::Revoked(message),
        401 => ProviderError::Unauthorized,
        410 => ProviderError::CursorExpired,
        _ => ProviderError::Refused { status, message },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_keep_the_base_path() {
        let base = Url::parse("http://127.0.0.1:9/google/").expect("static URL");
        assert_eq!(
            endpoint(&base, "/gmail/v1/users/me/history?startHistoryId=5").as_str(),
            "http://127.0.0.1:9/google/gmail/v1/users/me/history?startHistoryId=5"
        );
        assert_eq!(segment("a/b c"), "a%2Fb%20c");
    }

    #[test]
    fn failures_are_sorted() {
        assert_eq!(
            classify(429, Some(7), b"{}"),
            ProviderError::RateLimited {
                retry_after: Some(7)
            }
        );
        assert!(matches!(
            classify(400, None, br#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#),
            ProviderError::Revoked(m) if m.contains("revoked")
        ));
        assert_eq!(classify(401, None, b""), ProviderError::Unauthorized);
        assert_eq!(classify(410, None, b""), ProviderError::CursorExpired);
        assert!(matches!(
            classify(403, None, br#"{"error":{"code":403,"message":"Insufficient Permission"}}"#),
            ProviderError::Refused { status: 403, message } if message == "Insufficient Permission"
        ));
        assert!(classify(503, None, b"").is_temporary());
    }
}
