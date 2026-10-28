//! Sender domains at the mail provider: a workspace's own domain is added
//! to the provider's account, which gives back the DNS records (DKIM, and
//! SPF with a bounce MX on a `send` subdomain) the domain's owner adds.
//! Once the provider finds them, mail sent through its SMTP server may
//! carry an address at that domain in `From`, signed for it.
//!
//! Resend is the provider (`RESEND_API_KEY`); its SMTP server must then be
//! the platform's `SMTP_URL`, since only the account that verified a
//! domain may send from it.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::provider::{self, ProviderError, endpoint, segment};

pub const RESEND_API: &str = "https://api.resend.com";

/// One DNS record the domain needs, as the provider gives it. `name` is
/// relative to the domain (`resend._domainkey`, `send`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsRecord {
    /// What it is for: `DKIM`, `SPF` or `DMARC`.
    pub record: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    /// The provider's own check: `not_started`, `pending`, `verified`,
    /// `failed` or `temporary_failure`.
    #[serde(default)]
    pub status: String,
}

/// A domain as the provider holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDomain {
    pub id: String,
    pub name: String,
    /// `not_started`, `pending`, `verified`, `partially_verified`,
    /// `partially_failed`, `failed` or `temporary_failure`.
    pub status: String,
    pub records: Vec<DnsRecord>,
}

impl ProviderDomain {
    pub fn verified(&self) -> bool {
        self.status == "verified"
    }

    /// The provider has given up on it: its records were looked for long
    /// enough and not found.
    pub fn failed(&self) -> bool {
        self.status == "failed"
    }
}

#[derive(Deserialize)]
struct DomainBody {
    id: String,
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    records: Vec<RecordBody>,
}

/// Resend gives `ttl` as `"Auto"` and `priority` only on MX records.
#[derive(Deserialize)]
struct RecordBody {
    #[serde(default)]
    record: String,
    name: String,
    #[serde(rename = "type")]
    kind: String,
    value: String,
    #[serde(default)]
    priority: Option<i32>,
    #[serde(default)]
    status: String,
}

impl From<DomainBody> for ProviderDomain {
    fn from(d: DomainBody) -> Self {
        Self {
            id: d.id,
            name: d.name,
            status: d.status,
            records: d
                .records
                .into_iter()
                .map(|r| DnsRecord {
                    record: r.record,
                    name: r.name,
                    kind: r.kind,
                    value: r.value,
                    priority: r.priority,
                    status: r.status,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Resend {
    http: reqwest::Client,
    base: Url,
    key: String,
    /// Where the provider sends from: `us-east-1`, `eu-west-1`,
    /// `sa-east-1` or `ap-northeast-1`.
    region: String,
}

impl Resend {
    pub fn new(key: String, region: String, base: Url) -> Result<Self, ProviderError> {
        Ok(Self {
            http: provider::http_client()?,
            base,
            key,
            region,
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, endpoint(&self.base, path))
            .bearer_auth(&self.key)
    }

    /// Adds a domain to the account, for sending only.
    pub async fn create(&self, domain: &str) -> Result<ProviderDomain, ProviderError> {
        let body = json!({ "name": domain, "region": self.region });
        let d: DomainBody =
            provider::send_json(self.request(reqwest::Method::POST, "/domains").json(&body))
                .await?;
        Ok(d.into())
    }

    pub async fn get(&self, id: &str) -> Result<ProviderDomain, ProviderError> {
        let path = format!("/domains/{}", segment(id));
        let d: DomainBody = provider::send_json(self.request(reqwest::Method::GET, &path)).await?;
        Ok(d.into())
    }

    /// Asks the provider to look for the records now rather than at its
    /// own pace. The outcome shows in a later [`Self::get`].
    pub async fn verify(&self, id: &str) -> Result<(), ProviderError> {
        let path = format!("/domains/{}/verify", segment(id));
        provider::send_json::<Value>(self.request(reqwest::Method::POST, &path)).await?;
        Ok(())
    }

    /// Removes a domain from the account. One already gone counts as removed.
    pub async fn delete(&self, id: &str) -> Result<(), ProviderError> {
        let path = format!("/domains/{}", segment(id));
        match provider::send_json::<Value>(self.request(reqwest::Method::DELETE, &path)).await {
            Ok(_) | Err(ProviderError::Refused { status: 404, .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_created_domain() {
        let body: DomainBody = serde_json::from_value(json!({
            "id": "4dd369bc-aa82-4ff3-97de-514ae3000ee0",
            "name": "example.com",
            "created_at": "2026-03-28 17:12:02.059593+00",
            "status": "not_started",
            "records": [
                { "record": "DKIM", "name": "resend._domainkey", "value": "p=MIGf",
                  "type": "TXT", "status": "not_started", "ttl": "Auto" },
                { "record": "SPF", "name": "send", "type": "MX", "ttl": "Auto",
                  "status": "not_started", "value": "feedback-smtp.eu-west-1.amazonses.com",
                  "priority": 10 },
                { "record": "SPF", "name": "send", "value": "v=spf1 include:amazonses.com ~all",
                  "type": "TXT", "ttl": "Auto", "status": "not_started" }
            ],
            "region": "eu-west-1"
        }))
        .expect("parses");
        let d = ProviderDomain::from(body);
        assert_eq!(d.records.len(), 3);
        assert_eq!(d.records[1].priority, Some(10));
        assert_eq!(d.records[0].kind, "TXT");
        assert!(!d.verified() && !d.failed());
    }
}
