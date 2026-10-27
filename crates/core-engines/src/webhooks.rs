//! Outbound webhooks: which events an endpoint wants, and the signature
//! receivers use to check a delivery came from OnHold.
//!
//! Each delivery carries `OnHold-Signature: t=<unix seconds>,v1=<hex>`
//! where the hex is HMAC-SHA256 of `"<t>.<raw body>"` keyed with the
//! endpoint's secret. Receivers recompute it and reject old timestamps.

use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub const SIGNATURE_HEADER: &str = "onhold-signature";
pub const SECRET_PREFIX: &str = "whsec_";

/// `*` matches everything, `thing.*` every `thing.` event, anything else exactly.
pub fn topic_matches(pattern: &str, topic: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some("") => true,
        Some(prefix) => prefix.ends_with('.') && topic.starts_with(prefix),
        None => pattern == topic,
    }
}

pub fn is_valid_pattern(pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let body = pattern.strip_suffix(".*").unwrap_or(pattern);
    !body.is_empty()
        && body.len() <= 100
        && body
            .split('.')
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
}

/// A new signing secret, shown to the user once.
pub fn new_secret() -> String {
    let bytes = core_domain::crypto::random_bytes::<32>();
    format!("{SECRET_PREFIX}{}", BASE64URL_NOPAD.encode(&bytes))
}

fn mac(secret: &str, timestamp: i64, body: &[u8]) -> Hmac<Sha256> {
    // HMAC takes keys of any length.
    #[allow(clippy::expect_used)]
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    mac
}

/// The `OnHold-Signature` header value for a delivery.
pub fn sign(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let tag = mac(secret, timestamp, body).finalize().into_bytes();
    format!("t={timestamp},v1={}", HEXLOWER.encode(&tag))
}

/// Checks a signature header as a receiver would: right key and body, and a
/// timestamp within `tolerance_secs` of `now`.
pub fn verify(secret: &str, header: &str, body: &[u8], now: i64, tolerance_secs: i64) -> bool {
    let mut timestamp = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", t)) => timestamp = t.parse::<i64>().ok(),
            Some(("v1", s)) => sigs.push(s),
            _ => {}
        }
    }
    let Some(t) = timestamp else { return false };
    if (now - t).abs() > tolerance_secs {
        return false;
    }
    sigs.iter().any(|s| {
        HEXLOWER
            .decode(s.as_bytes())
            .is_ok_and(|tag| mac(secret, t, body).verify_slice(&tag).is_ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(topic_matches("*", "thing.created"));
        assert!(topic_matches("thing.*", "thing.created"));
        assert!(!topic_matches("thing.*", "things.created"));
        assert!(topic_matches("thing.created", "thing.created"));
        assert!(!topic_matches("thing.created", "thing.updated"));
        assert!(!topic_matches("thing*", "thing.created"));
        for ok in ["*", "thing.*", "thing.stage_changed"] {
            assert!(is_valid_pattern(ok), "{ok}");
        }
        for bad in ["", "**", "thing.", "Thing.created", "thing.*.x", ".*"] {
            assert!(!is_valid_pattern(bad), "{bad}");
        }
    }

    #[test]
    fn signatures_verify_and_expire() {
        let secret = new_secret();
        assert!(secret.starts_with(SECRET_PREFIX));
        let body = br#"{"id":"1"}"#;
        let header = sign(&secret, 1_700_000_000, body);
        assert!(header.starts_with("t=1700000000,v1="));
        assert!(verify(&secret, &header, body, 1_700_000_100, 300));
        assert!(
            !verify(&secret, &header, body, 1_700_000_400, 300),
            "too old"
        );
        assert!(!verify("whsec_other", &header, body, 1_700_000_000, 300));
        assert!(!verify(&secret, &header, b"{}", 1_700_000_000, 300));
        assert!(!verify(&secret, "v1=abc", body, 1_700_000_000, 300));
    }

    #[test]
    fn matches_a_known_vector() {
        // echo -n '1700000000.hello' | openssl dgst -sha256 -hmac key
        assert_eq!(
            sign("key", 1_700_000_000, b"hello"),
            "t=1700000000,v1=4d583a269f4f276a3fa80ff31b5a01879a848096983222a17893d198418939aa"
        );
    }
}
