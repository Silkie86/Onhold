//! Looking at a domain's DNS, and setting it up through Domain Connect.
//!
//! Lookups go through DNS over HTTPS (a resolver's JSON API, Cloudflare's
//! by default), so no resolver library is needed and the answers are the
//! public internet's, not the server's local view.
//!
//! Domain Connect lets a domain's owner add OnHold's records at their DNS
//! provider by approving them there, instead of copying them by hand. The
//! domain's zone names its provider in a `_domainconnect` TXT record; the
//! provider's settings say where its approval page is; and OnHold sends the
//! owner to that page with a link naming its published template, the
//! values to fill in, and an RSA-SHA256 signature over the query string,
//! which the provider checks against OnHold's public key in DNS
//! (`<key>.<syncPubKeyDomain>`). Only providers that have taken in OnHold's
//! template (`docs/domain-connect/`) offer it; for the rest the owner adds
//! the records by hand.

use data_encoding::BASE64;
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde::Deserialize;
use url::Url;

use crate::http::{names_private_address, public_client};
use crate::provider::{self, ProviderError, endpoint, segment};

pub const DOH_BASE: &str = "https://cloudflare-dns.com/dns-query";

/// DNS over HTTPS, JSON flavour.
#[derive(Debug, Clone)]
pub struct Doh {
    http: reqwest::Client,
    base: Url,
}

#[derive(Deserialize)]
struct DohAnswer {
    #[serde(rename = "Status")]
    status: u32,
    #[serde(rename = "Answer", default)]
    answer: Vec<DohRecord>,
}

#[derive(Deserialize)]
struct DohRecord {
    #[serde(rename = "type")]
    kind: u16,
    data: String,
}

const TXT: u16 = 16;
/// NXDOMAIN: the name does not exist.
const NO_SUCH_NAME: u32 = 3;

/// A TXT record's text: its quoted strings joined, as one long record
/// split into 255-byte pieces is meant to be read.
fn txt_text(data: &str) -> String {
    let data = data.trim();
    if !data.starts_with('"') {
        return data.to_owned();
    }
    let mut out = String::new();
    let mut inside = false;
    let mut escaped = false;
    for c in data.chars() {
        match (inside, escaped, c) {
            (true, true, _) => {
                out.push(c);
                escaped = false;
            }
            (true, false, '\\') => escaped = true,
            (_, false, '"') => inside = !inside,
            (true, false, _) => out.push(c),
            _ => {}
        }
    }
    out
}

impl Doh {
    pub fn new(base: Url) -> Result<Self, ProviderError> {
        Ok(Self {
            http: provider::http_client()?,
            base,
        })
    }

    /// The TXT records at `name`; none when it does not exist.
    pub async fn txt(&self, name: &str) -> Result<Vec<String>, ProviderError> {
        let mut url = self.base.clone();
        url.query_pairs_mut()
            .append_pair("name", name)
            .append_pair("type", "TXT");
        let answer: DohAnswer = provider::send_json(
            self.http
                .get(url)
                .header(reqwest::header::ACCEPT, "application/dns-json"),
        )
        .await?;
        match answer.status {
            0 => Ok(answer
                .answer
                .iter()
                .filter(|r| r.kind == TXT)
                .map(|r| txt_text(&r.data))
                .collect()),
            NO_SUCH_NAME => Ok(Vec::new()),
            other => Err(ProviderError::Unavailable(format!(
                "the DNS lookup of {name} failed (status {other})"
            ))),
        }
    }

    /// Whether mail from `domain` is covered by a DMARC policy: its own, or
    /// that of a domain it is under.
    pub async fn has_dmarc(&self, domain: &str) -> Result<bool, ProviderError> {
        for name in zones_of(domain) {
            let found = self.txt(&format!("_dmarc.{name}")).await?;
            if found
                .iter()
                .any(|t| t.trim_start().to_ascii_lowercase().starts_with("v=dmarc1"))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// The names a domain may sit under, itself first, down to two labels:
/// `mail.acme.co.uk`, `acme.co.uk`, `co.uk`.
pub fn zones_of(domain: &str) -> Vec<&str> {
    let mut out = vec![domain];
    let mut rest = domain;
    while let Some((_, parent)) = rest.split_once('.') {
        if !parent.contains('.') {
            break;
        }
        out.push(parent);
        rest = parent;
    }
    out
}

/// A domain's DNS provider, found through Domain Connect, that has taken in
/// OnHold's template.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct DnsProvider {
    /// What to call it: "Cloudflare", "GoDaddy".
    pub name: String,
    /// Where its approval pages are.
    pub sync_ux: String,
    /// The zone the domain is in, and the domain's label within it (empty
    /// when the domain is the zone itself).
    pub zone: String,
    pub host: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    provider_name: String,
    #[serde(default)]
    provider_display_name: Option<String>,
    #[serde(default, rename = "urlSyncUX")]
    url_sync_ux: Option<String>,
    #[serde(rename = "urlAPI")]
    url_api: String,
}

/// Domain Connect as a service provider: OnHold's template and signing key.
pub struct DomainConnect {
    /// The template's `providerId` and `serviceId`.
    pub provider_id: String,
    pub service_id: String,
    /// The label of the public key's TXT records under the template's
    /// `syncPubKeyDomain` (`_dck1` for `_dck1.domainconnect.example.com`).
    pub key_host: String,
    key: RsaKeyPair,
    http: reqwest::Client,
    /// Development and tests: let discovery reach local stand-ins over http.
    allow_private: bool,
}

impl std::fmt::Debug for DomainConnect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DomainConnect")
            .field("provider_id", &self.provider_id)
            .field("service_id", &self.service_id)
            .field("key_host", &self.key_host)
            .finish_non_exhaustive()
    }
}

/// An RSA private key from PEM (PKCS#8 `PRIVATE KEY` or PKCS#1 `RSA
/// PRIVATE KEY`, as `openssl genrsa` writes).
fn rsa_key(pem: &str) -> Result<RsaKeyPair, String> {
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("-----"))
        .collect();
    let der = BASE64
        .decode(body.as_bytes())
        .map_err(|e| format!("the Domain Connect key is not PEM: {e}"))?;
    RsaKeyPair::from_pkcs8(&der)
        .or_else(|_| RsaKeyPair::from_der(&der))
        .map_err(|e| format!("the Domain Connect key is not an RSA private key: {e}"))
}

impl DomainConnect {
    pub fn new(
        provider_id: String,
        service_id: String,
        key_host: String,
        private_key_pem: &str,
        allow_private: bool,
    ) -> Result<Self, String> {
        Ok(Self {
            provider_id,
            service_id,
            key_host,
            key: rsa_key(private_key_pem)?,
            http: public_client(allow_private, concat!("OnHold/", env!("CARGO_PKG_VERSION")))?,
            allow_private,
        })
    }

    /// A URL the domain's own DNS gave, checked before it is called: https
    /// and not a private address (both lifted for local stand-ins).
    fn checked(&self, raw: &str) -> Option<Url> {
        let url = Url::parse(raw).ok()?;
        let scheme_ok = url.scheme() == "https" || (self.allow_private && url.scheme() == "http");
        let ok = scheme_ok
            && url.username().is_empty()
            && url.password().is_none()
            && url.host().is_some()
            && (self.allow_private || !names_private_address(&url));
        ok.then_some(url)
    }

    /// Finds the DNS provider of `domain`'s zone and whether it takes
    /// OnHold's template. `None` when there is no zone with Domain Connect,
    /// its provider has no approval page, or it does not know the template.
    pub async fn discover(
        &self,
        doh: &Doh,
        domain: &str,
    ) -> Result<Option<DnsProvider>, ProviderError> {
        for zone in zones_of(domain) {
            let Some(api) = doh
                .txt(&format!("_domainconnect.{zone}"))
                .await?
                .into_iter()
                .next()
            else {
                continue;
            };
            // The record is a host and path without a scheme.
            let scheme = if self.allow_private && api.starts_with("127.0.0.1") {
                "http"
            } else {
                "https"
            };
            let Some(base) =
                self.checked(&format!("{scheme}://{}", api.trim().trim_end_matches('/')))
            else {
                return Ok(None);
            };
            let settings_url = endpoint(&base, &format!("/v2/{}/settings", segment(zone)));
            let settings: Settings = match provider::send_json(self.http.get(settings_url)).await {
                Ok(s) => s,
                Err(ProviderError::Refused { .. } | ProviderError::Invalid(_)) => return Ok(None),
                Err(e) => return Err(e),
            };
            let (Some(sync_ux), Some(api)) = (
                settings
                    .url_sync_ux
                    .as_deref()
                    .and_then(|u| self.checked(u)),
                self.checked(&settings.url_api),
            ) else {
                return Ok(None);
            };
            let template = endpoint(
                &api,
                &format!(
                    "/v2/domainTemplates/providers/{}/services/{}",
                    segment(&self.provider_id),
                    segment(&self.service_id)
                ),
            );
            match provider::send_raw(self.http.get(template)).await {
                Ok(_) => {}
                Err(ProviderError::Refused { .. }) => return Ok(None),
                Err(e) => return Err(e),
            }
            // A provider applies a template to the zone itself or to one
            // label under it.
            let host = domain
                .strip_suffix(zone)
                .map(|h| h.trim_end_matches('.'))
                .unwrap_or_default();
            if host.contains('.') {
                return Ok(None);
            }
            return Ok(Some(DnsProvider {
                name: settings
                    .provider_display_name
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or(settings.provider_name)
                    .chars()
                    .take(100)
                    .collect(),
                sync_ux: sync_ux.to_string(),
                zone: zone.to_owned(),
                host: host.to_owned(),
            }));
        }
        Ok(None)
    }

    /// The signed link that applies the template at `provider`: the owner
    /// approves the records there and comes back to `redirect_uri`.
    pub fn apply_url(
        &self,
        provider: &DnsProvider,
        values: &[(&str, &str)],
        groups: &[&str],
        redirect_uri: &str,
        state: &str,
    ) -> Result<Url, String> {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("domain", &provider.zone);
        query.append_pair("host", &provider.host);
        for (name, value) in values {
            query.append_pair(name, value);
        }
        if !groups.is_empty() {
            query.append_pair("groupId", &groups.join(","));
        }
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("state", state);
        let query = query.finish();
        let sig = self.sign(query.as_bytes())?;
        let signed = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("sig", &sig)
            .append_pair("key", &self.key_host)
            .finish();
        let sync_ux = self
            .checked(&provider.sync_ux)
            .ok_or("the DNS provider's approval page is not a public https address")?;
        let mut url = endpoint(
            &sync_ux,
            &format!(
                "/v2/domainTemplates/providers/{}/services/{}/apply",
                segment(&self.provider_id),
                segment(&self.service_id)
            ),
        );
        url.set_query(Some(&format!("{query}&{signed}")));
        Ok(url)
    }

    /// RSA-SHA256 (PKCS#1 v1.5) over `message`, base64.
    fn sign(&self, message: &[u8]) -> Result<String, String> {
        let mut sig = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), message, &mut sig)
            .map_err(|_| "signing the Domain Connect link failed".to_owned())?;
        Ok(BASE64.encode(&sig))
    }

    /// The public key, base64 DER (SubjectPublicKeyInfo), as it goes in the
    /// `d=` parts of the key's TXT records.
    pub fn public_key(&self) -> String {
        BASE64.encode(&spki(self.key.public().as_ref()))
    }
}

/// Wraps a PKCS#1 `RSAPublicKey` in the SubjectPublicKeyInfo DER that
/// `openssl rsa -pubout -outform DER` writes.
fn spki(pkcs1: &[u8]) -> Vec<u8> {
    // AlgorithmIdentifier: rsaEncryption with NULL parameters.
    const ALGORITHM: &[u8] = &[
        0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05, 0x00,
    ];
    fn der_len(len: usize) -> Vec<u8> {
        if len < 0x80 {
            return vec![u8::try_from(len).unwrap_or(0)];
        }
        let bytes: Vec<u8> = len
            .to_be_bytes()
            .into_iter()
            .skip_while(|b| *b == 0)
            .collect();
        let mut out = vec![0x80 | u8::try_from(bytes.len()).unwrap_or(0)];
        out.extend(bytes);
        out
    }
    let mut bits = vec![0x03];
    bits.extend(der_len(pkcs1.len() + 1));
    bits.push(0);
    bits.extend_from_slice(pkcs1);
    let mut out = vec![0x30];
    out.extend(der_len(ALGORITHM.len() + bits.len()));
    out.extend_from_slice(ALGORITHM);
    out.extend(bits);
    out
}

#[cfg(test)]
mod tests {
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    use super::*;

    /// A 2048-bit key made for these tests only.
    const TEST_KEY: &str = include_str!("../tests/fixtures/domain_connect_test_key.pem");

    #[test]
    fn reads_txt_answers() {
        assert_eq!(txt_text("\"v=DMARC1; p=none\""), "v=DMARC1; p=none");
        assert_eq!(
            txt_text("\"p=1,a=RS256,\" \"d=MIIB\""),
            "p=1,a=RS256,d=MIIB"
        );
        assert_eq!(txt_text("\"say \\\"hi\\\"\""), "say \"hi\"");
        assert_eq!(txt_text("plain"), "plain");
    }

    #[test]
    fn lists_zones() {
        assert_eq!(
            zones_of("mail.acme.co.uk"),
            vec!["mail.acme.co.uk", "acme.co.uk", "co.uk"]
        );
        assert_eq!(zones_of("acme.com"), vec!["acme.com"]);
    }

    #[test]
    fn signs_the_query_string() {
        let dc = DomainConnect::new(
            "onhold.test".into(),
            "sending".into(),
            "_dck1".into(),
            TEST_KEY,
            false,
        )
        .expect("key");
        let provider = DnsProvider {
            name: "Example DNS".into(),
            sync_ux: "https://dns.example.net/manage".into(),
            zone: "acme.com".into(),
            host: String::new(),
        };
        let url = dc
            .apply_url(
                &provider,
                &[
                    ("dkim", "p=MIGf+ab/c="),
                    ("mx", "feedback-smtp.eu-west-1.amazonses.com"),
                ],
                &["send", "dmarc"],
                "https://app.onhold.test/settings/email?domain=1",
                "abc",
            )
            .expect("signs");
        assert_eq!(
            url.path(),
            "/manage/v2/domainTemplates/providers/onhold.test/services/sending/apply"
        );
        let query = url.query().expect("query");
        let (signed, rest) = query.split_once("&sig=").expect("sig");
        assert!(signed.starts_with("domain=acme.com&host=&dkim=p%3DMIGf%2Bab%2Fc%3D&"));
        assert!(signed.contains("groupId=send%2Cdmarc"));
        let (sig, key) = rest.split_once("&key=").expect("key");
        assert_eq!(key, "_dck1");
        let sig: String = url::form_urlencoded::parse(format!("s={sig}").as_bytes())
            .next()
            .expect("sig")
            .1
            .into_owned();
        let sig = BASE64.decode(sig.as_bytes()).expect("base64");
        // Checked the way a DNS provider does, with the published key.
        let spki = BASE64.decode(dc.public_key().as_bytes()).expect("base64");
        assert_eq!(&spki[..2], &[0x30, 0x82]);
        let pkcs1 = dc.key.public().as_ref().to_vec();
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, pkcs1)
            .verify(signed.as_bytes(), &sig)
            .expect("the signature checks out");
        assert!(rsa_key("not a key").is_err());
    }

    #[test]
    fn refuses_private_urls() {
        let dc = DomainConnect::new("p".into(), "s".into(), "_dck1".into(), TEST_KEY, false)
            .expect("key");
        assert!(dc.checked("https://api.example.net/dc").is_some());
        for bad in [
            "http://api.example.net/",
            "https://127.0.0.1/",
            "https://169.254.169.254/",
            "https://u:p@api.example.net/",
        ] {
            assert!(dc.checked(bad).is_none(), "{bad}");
        }
    }
}
