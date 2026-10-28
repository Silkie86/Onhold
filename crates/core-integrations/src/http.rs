//! The HTTP client webhooks are delivered with.
//!
//! A webhook URL is chosen by a tenant, so the client must not become a way
//! to reach OnHold's own network (server-side request forgery). Hosts are
//! resolved by a resolver that drops private, loopback, link-local and
//! other non-public addresses, and the connection goes to exactly the
//! addresses it returned, so a DNS answer cannot change between the check
//! and the connect. Literal IP addresses are checked the same way.
//! Redirects are not followed and proxies are bypassed.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

pub const TIMEOUT: Duration = Duration::from_secs(10);
/// How much of a response body is kept for the delivery log.
pub const EXCERPT_BYTES: usize = 1024;

/// Whether an address is on the public internet.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (seg[0] & 0xffc0) == 0xfe80 // link local fe80::/10
                || (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation
                || (seg[0] == 0x0064 && seg[1] == 0xff9b) // NAT64, could reach v4 private
                || seg[0] == 0x0100 && seg[1..4] == [0, 0, 0]) // discard-only
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // shared address space 100.64/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking 198.18/15
        || o[0] >= 240) // reserved
}

struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| is_public(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} does not resolve to a public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// A client for URLs a tenant (or a tenant's DNS) chose: names resolve to
/// public addresses only (unless `allow_private`, for development and
/// tests), redirects are not followed and proxies are bypassed.
pub fn public_client(allow_private: bool, user_agent: &str) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .user_agent(user_agent);
    if !allow_private {
        builder = builder.dns_resolver(Arc::new(PublicOnly));
    }
    builder.build().map_err(|e| e.to_string())
}

/// Whether a URL's host is a literal non-public address (or `localhost`),
/// which the resolver of [`public_client`] never sees.
pub fn names_private_address(url: &url::Url) -> bool {
    let literal = match url.host() {
        Some(url::Host::Ipv4(v4)) => IpAddr::V4(v4),
        Some(url::Host::Ipv6(v6)) => IpAddr::V6(v6),
        Some(url::Host::Domain(d)) if d.eq_ignore_ascii_case("localhost") => {
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        }
        Some(url::Host::Domain(_)) | None => return false,
    };
    !is_public(literal)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub excerpt: String,
    pub duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeliveryError {
    /// The URL is not allowed (not http(s), or a non-public address).
    #[error("{0}")]
    Blocked(String),
    /// The request failed: DNS, connect, TLS, timeout.
    #[error("{message}")]
    Failed { message: String, duration: Duration },
}

#[derive(Clone)]
pub struct WebhookClient {
    client: reqwest::Client,
    allow_private: bool,
}

impl WebhookClient {
    /// `allow_private` lifts the address check, for local development and
    /// tests that deliver to `localhost`. Never set it in production.
    pub fn new(allow_private: bool) -> Result<Self, String> {
        Ok(Self {
            client: public_client(
                allow_private,
                concat!("OnHold-Webhooks/", env!("CARGO_PKG_VERSION")),
            )?,
            allow_private,
        })
    }

    /// Checks a URL before it is saved or called: http(s) only, no
    /// credentials, and no literal non-public address.
    pub fn check_url(&self, raw: &str) -> Result<url::Url, DeliveryError> {
        let url =
            url::Url::parse(raw).map_err(|_| DeliveryError::Blocked("not a valid URL".into()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(DeliveryError::Blocked("use an http or https URL".into()));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(DeliveryError::Blocked(
                "put credentials in the receiver, not the URL".into(),
            ));
        }
        if url.host().is_none() {
            return Err(DeliveryError::Blocked("the URL has no host".into()));
        }
        if !self.allow_private && names_private_address(&url) {
            return Err(DeliveryError::Blocked(
                "webhooks cannot be sent to private or local addresses".into(),
            ));
        }
        Ok(url)
    }

    /// POSTs a JSON body with the given headers.
    pub async fn post(
        &self,
        raw_url: &str,
        headers: &[(&str, String)],
        body: Vec<u8>,
    ) -> Result<Response, DeliveryError> {
        let url = self.check_url(raw_url)?;
        let mut req = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        for (name, value) in headers {
            req = req.header(*name, value);
        }
        let started = Instant::now();
        let failed = |e: reqwest::Error, started: Instant| {
            // Walk to the root cause: "error sending request" alone says little.
            let mut message = e.to_string();
            let mut source = std::error::Error::source(&e);
            while let Some(s) = source {
                message = s.to_string();
                source = s.source();
            }
            DeliveryError::Failed {
                message,
                duration: started.elapsed(),
            }
        };
        let res = req
            .body(body)
            .send()
            .await
            .map_err(|e| failed(e, started))?;
        let status = res.status().as_u16();
        let bytes = res.bytes().await.unwrap_or_default();
        let cut = bytes.len().min(EXCERPT_BYTES);
        Ok(Response {
            status,
            excerpt: String::from_utf8_lossy(&bytes[..cut]).into_owned(),
            duration: started.elapsed(),
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[test]
    fn classifies_addresses() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public(private.parse().expect("ip")), "{private}");
        }
        for public in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(is_public(public.parse().expect("ip")), "{public}");
        }
    }

    #[test]
    fn urls_are_checked() {
        let client = WebhookClient::new(false).expect("client");
        assert!(client.check_url("https://example.com/hook").is_ok());
        for bad in [
            "ftp://example.com",
            "http://127.0.0.1/x",
            "http://[::1]/x",
            "http://localhost:8080/",
            "http://169.254.169.254/latest/meta-data",
            "https://user:pass@example.com/",
            "not a url",
        ] {
            assert!(client.check_url(bad).is_err(), "{bad}");
        }
        let dev = WebhookClient::new(true).expect("client");
        assert!(dev.check_url("http://127.0.0.1:9/x").is_ok());
    }

    #[tokio::test]
    async fn names_that_resolve_privately_are_refused() {
        let client = WebhookClient::new(false).expect("client");
        // `localtest.me`-style tricks resolve to loopback; so does this one.
        let err = client
            .post("http://localhost.localdomain./x", &[], b"{}".to_vec())
            .await
            .expect_err("blocked");
        assert!(matches!(err, DeliveryError::Failed { .. }), "{err}");
    }

    #[tokio::test]
    async fn posts_and_reads_the_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.expect("read");
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            sock.write_all(
                b"HTTP/1.1 202 Accepted\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
            )
            .await
            .expect("write");
            req
        });
        let client = WebhookClient::new(true).expect("client");
        let res = client
            .post(
                &format!("http://{addr}/hook"),
                &[("onhold-signature", "t=1,v1=ab".into())],
                b"{\"a\":1}".to_vec(),
            )
            .await
            .expect("delivered");
        assert_eq!((res.status, res.excerpt.as_str()), (202, "ok"));
        let req = server.await.expect("server");
        assert!(req.starts_with("POST /hook"), "{req}");
        assert!(
            req.to_lowercase().contains("onhold-signature: t=1,v1=ab"),
            "{req}"
        );
    }
}
