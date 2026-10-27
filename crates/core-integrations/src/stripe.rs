//! Stripe Connect: each workspace connects its own Stripe account through
//! Stripe's OAuth (Standard accounts), and payers pay invoices through
//! Checkout sessions made on that account, so the money settles to the
//! client directly. Only the few calls OnHold makes are here, with Stripe's
//! form-encoded requests and the `Stripe-Account` header for calls made on
//! a connected account's behalf.
//!
//! Webhooks are signed the way OnHold signs its own
//! (`t=<unix seconds>,v1=<hex HMAC-SHA256 of "t.body">`); the API checks
//! them with `core_engines::webhooks::verify`.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use url::Url;

pub const API_BASE: &str = "https://api.stripe.com";
pub const CONNECT_BASE: &str = "https://connect.stripe.com";
/// Stripe's webhook header.
pub const SIGNATURE_HEADER: &str = "stripe-signature";
/// How old a webhook's signed timestamp may be, as Stripe's libraries allow.
pub const SIGNATURE_TOLERANCE_SECS: i64 = 300;

/// Currencies Stripe counts in other units than ISO 4217 minor units, or
/// takes only in multiples (three-decimal ones must end in 0). Invoices in
/// them are paid by bank transfer instead.
const UNSUPPORTED_CURRENCIES: &[&str] = &[
    "BHD", "HUF", "ISK", "JOD", "KWD", "OMR", "TND", "TWD", "UGX",
];

#[derive(Debug, Clone)]
pub struct StripeConfig {
    /// The platform's secret key (`sk_live_…` or `sk_test_…`).
    pub secret_key: String,
    /// The platform's Connect client id (`ca_…`).
    pub client_id: String,
    /// The signing secret of the Connect webhook endpoint (`whsec_…`).
    pub webhook_secret: String,
    pub api_base: Url,
    pub connect_base: Url,
    /// OnHold's cut of each payment, in hundredths of a percent. 0 takes none.
    pub application_fee_bps: u32,
}

impl StripeConfig {
    pub fn new(secret_key: String, client_id: String, webhook_secret: String) -> Self {
        #[allow(clippy::expect_used)]
        Self {
            secret_key,
            client_id,
            webhook_secret,
            api_base: Url::parse(API_BASE).expect("static URL"),
            connect_base: Url::parse(CONNECT_BASE).expect("static URL"),
            application_fee_bps: 0,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StripeError {
    /// Stripe could not be reached, or failed on its side: worth trying again.
    #[error("Stripe could not be reached: {0}")]
    Unavailable(String),
    /// Stripe refused the request.
    #[error("Stripe refused the request: {message}")]
    Refused {
        status: u16,
        code: Option<String>,
        message: String,
    },
    #[error("Stripe's answer was not understood: {0}")]
    Invalid(String),
}

/// What Stripe's OAuth gives back for a connected account.
#[derive(Debug, Clone, Deserialize)]
pub struct OAuthToken {
    pub stripe_user_id: String,
    #[serde(default)]
    pub livemode: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub id: String,
    #[serde(default)]
    pub charges_enabled: bool,
    #[serde(default)]
    pub details_submitted: bool,
    pub default_currency: Option<String>,
    #[serde(default)]
    pub business_profile: Option<BusinessProfile>,
    #[serde(default)]
    pub settings: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BusinessProfile {
    pub name: Option<String>,
}

impl Account {
    /// The name the account trades under, as Stripe shows it.
    pub fn display_name(&self) -> Option<String> {
        self.settings
            .as_ref()
            .and_then(|s| s.pointer("/dashboard/display_name"))
            .and_then(Value::as_str)
            .or_else(|| {
                self.business_profile
                    .as_ref()
                    .and_then(|b| b.name.as_deref())
            })
            .map(|s| s.chars().take(200).collect())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckoutSession {
    pub id: String,
    pub url: Option<String>,
    /// open, complete or expired.
    pub status: Option<String>,
    /// paid, unpaid or no_payment_required.
    pub payment_status: Option<String>,
    pub amount_total: Option<i64>,
    pub currency: Option<String>,
    pub payment_intent: Option<String>,
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub metadata: serde_json::Map<String, Value>,
}

/// A Checkout session for one invoice.
#[derive(Debug, Clone)]
pub struct NewCheckout<'a> {
    pub amount_minor: i64,
    /// ISO 4217, any case.
    pub currency: &'a str,
    /// What the payer sees they are paying, such as `Invoice INV-2026-0001`.
    pub description: &'a str,
    pub success_url: &'a str,
    pub cancel_url: &'a str,
    pub customer_email: Option<&'a str>,
    /// Kept on the session and its PaymentIntent, and sent back in webhooks.
    pub metadata: &'a [(&'a str, String)],
    pub idempotency_key: &'a str,
}

/// An event Stripe sent to the webhook endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// The connected account it happened on; none for the platform's own.
    pub account: Option<String>,
    #[serde(default)]
    pub livemode: bool,
    pub created: Option<i64>,
    pub data: EventData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventData {
    pub object: Value,
}

/// Whether Stripe takes payments in a currency with the same minor units as OnHold.
pub fn supports_currency(currency: &str) -> bool {
    currency.len() == 3 && !UNSUPPORTED_CURRENCIES.contains(&currency.to_ascii_uppercase().as_str())
}

/// The platform's fee on a payment, rounded down.
pub fn application_fee(amount_minor: i64, bps: u32) -> i64 {
    amount_minor.saturating_mul(i64::from(bps)) / 10_000
}

#[derive(Debug, Clone)]
pub struct StripeClient {
    pub config: StripeConfig,
    http: reqwest::Client,
}

impl StripeClient {
    pub fn new(config: StripeConfig) -> Result<Self, StripeError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| StripeError::Unavailable(e.to_string()))?;
        Ok(Self { config, http })
    }

    /// Where to send a workspace admin to connect their Stripe account.
    pub fn authorize_url(&self, state: &str, redirect_uri: &str) -> Url {
        let mut url = self.config.connect_base.clone();
        url.set_path("/oauth/authorize");
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("scope", "read_write")
            .append_pair("state", state)
            .append_pair("redirect_uri", redirect_uri);
        url
    }

    /// Exchanges the code Stripe sent back for the connected account's id.
    pub async fn oauth_token(&self, code: &str) -> Result<OAuthToken, StripeError> {
        let url = endpoint(&self.config.connect_base, "/oauth/token");
        self.send(
            self.http.post(url),
            None,
            None,
            &[("grant_type", "authorization_code"), ("code", code)],
        )
        .await
    }

    /// Disconnects an account from the platform. Already disconnected is fine.
    pub async fn deauthorize(&self, account: &str) -> Result<(), StripeError> {
        let url = endpoint(&self.config.connect_base, "/oauth/deauthorize");
        let res: Result<Value, _> = self
            .send(
                self.http.post(url),
                None,
                None,
                &[
                    ("client_id", self.config.client_id.as_str()),
                    ("stripe_user_id", account),
                ],
            )
            .await;
        match res {
            Ok(_) => Ok(()),
            Err(StripeError::Refused { status, .. }) if status == 400 || status == 401 => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub async fn account(&self, account: &str) -> Result<Account, StripeError> {
        let url = endpoint(
            &self.config.api_base,
            &format!("/v1/accounts/{}", path_part(account)?),
        );
        self.send(self.http.get(url), None, None, &[]).await
    }

    /// A Checkout session on the connected account, for one payment.
    pub async fn create_checkout(
        &self,
        account: &str,
        new: &NewCheckout<'_>,
    ) -> Result<CheckoutSession, StripeError> {
        let currency = new.currency.to_ascii_lowercase();
        let amount = new.amount_minor.to_string();
        let mut form: Vec<(String, String)> = vec![
            ("mode".into(), "payment".into()),
            ("success_url".into(), new.success_url.into()),
            ("cancel_url".into(), new.cancel_url.into()),
            ("line_items[0][quantity]".into(), "1".into()),
            ("line_items[0][price_data][currency]".into(), currency),
            ("line_items[0][price_data][unit_amount]".into(), amount),
            (
                "line_items[0][price_data][product_data][name]".into(),
                new.description.into(),
            ),
        ];
        if let Some(email) = new.customer_email {
            form.push(("customer_email".into(), email.into()));
        }
        for (key, value) in new.metadata {
            form.push((format!("metadata[{key}]"), value.clone()));
            form.push((
                format!("payment_intent_data[metadata][{key}]"),
                value.clone(),
            ));
        }
        let fee = application_fee(new.amount_minor, self.config.application_fee_bps);
        if fee > 0 {
            form.push((
                "payment_intent_data[application_fee_amount]".into(),
                fee.to_string(),
            ));
        }
        let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let url = endpoint(&self.config.api_base, "/v1/checkout/sessions");
        self.send(
            self.http.post(url),
            Some(account),
            Some(new.idempotency_key),
            &pairs,
        )
        .await
    }

    /// Closes a session so it can no longer be paid. Already closed is fine.
    pub async fn expire_checkout(&self, account: &str, session: &str) -> Result<(), StripeError> {
        let url = endpoint(
            &self.config.api_base,
            &format!("/v1/checkout/sessions/{}/expire", path_part(session)?),
        );
        let res: Result<Value, _> = self
            .send(self.http.post(url), Some(account), None, &[])
            .await;
        match res {
            Ok(_) => Ok(()),
            Err(StripeError::Refused { status: 400, .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
        account: Option<&str>,
        idempotency_key: Option<&str>,
        form: &[(&str, &str)],
    ) -> Result<T, StripeError> {
        let mut req = req
            .bearer_auth(&self.config.secret_key)
            .header("stripe-version", "2024-06-20");
        if let Some(account) = account {
            req = req.header("stripe-account", account);
        }
        if let Some(key) = idempotency_key {
            req = req.header("idempotency-key", key);
        }
        if !form.is_empty() {
            let body = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(form)
                .finish();
            req = req
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body);
        }
        let res = req
            .send()
            .await
            .map_err(|e| StripeError::Unavailable(e.without_url().to_string()))?;
        let status = res.status();
        let body = res
            .text()
            .await
            .map_err(|e| StripeError::Unavailable(e.without_url().to_string()))?;
        if status.is_server_error() || status.as_u16() == 429 {
            return Err(StripeError::Unavailable(format!("status {status}")));
        }
        if !status.is_success() {
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            // API errors carry `error.message`; OAuth ones `error_description`.
            let message = v
                .pointer("/error/message")
                .or_else(|| v.get("error_description"))
                .and_then(Value::as_str)
                .unwrap_or("no reason given")
                .chars()
                .take(300)
                .collect();
            let code = v
                .pointer("/error/code")
                .or_else(|| v.get("error"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            return Err(StripeError::Refused {
                status: status.as_u16(),
                code,
                message,
            });
        }
        serde_json::from_str(&body).map_err(|e| StripeError::Invalid(e.to_string()))
    }
}

fn endpoint(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    let prefix = base.path().trim_end_matches('/');
    url.set_path(&format!("{prefix}{path}"));
    url
}

/// Stripe ids are letters, digits and underscores; anything else is refused
/// before it reaches a URL path.
fn path_part(id: &str) -> Result<&str, StripeError> {
    if !id.is_empty()
        && id.len() <= 255
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        Ok(id)
    } else {
        Err(StripeError::Invalid(format!("`{id}` is not a Stripe id")))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn fees_currencies_and_urls() {
        assert_eq!(application_fee(10_000, 0), 0);
        assert_eq!(application_fee(10_000, 150), 150);
        assert_eq!(application_fee(999, 150), 14);
        assert!(supports_currency("gbp"));
        assert!(supports_currency("JPY"));
        assert!(!supports_currency("KWD"));
        assert!(!supports_currency("huf"));

        let client = StripeClient::new(StripeConfig::new(
            "sk_test_x".into(),
            "ca_123".into(),
            "whsec_x".into(),
        ))
        .unwrap();
        let url = client.authorize_url("s t", "https://app.test/settings/payments");
        assert_eq!(url.host_str(), Some("connect.stripe.com"));
        assert_eq!(url.path(), "/oauth/authorize");
        let q: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        assert!(q.contains(&("client_id".into(), "ca_123".into())));
        assert!(q.contains(&("state".into(), "s t".into())));

        let base = Url::parse("http://127.0.0.1:9/stripe/").unwrap();
        assert_eq!(
            endpoint(&base, "/v1/accounts/acct_1").as_str(),
            "http://127.0.0.1:9/stripe/v1/accounts/acct_1"
        );
        assert!(path_part("acct_1Abc").is_ok());
        assert!(path_part("../x").is_err());
        assert!(path_part("").is_err());
    }

    #[test]
    fn events_and_accounts_parse() {
        let e: Event = serde_json::from_str(
            r#"{"id":"evt_1","type":"checkout.session.completed","account":"acct_1",
                "livemode":false,"created":1700000000,"data":{"object":{"id":"cs_1"}}}"#,
        )
        .unwrap();
        assert_eq!(e.kind, "checkout.session.completed");
        assert_eq!(e.account.as_deref(), Some("acct_1"));
        let a: Account = serde_json::from_str(
            r#"{"id":"acct_1","charges_enabled":true,"details_submitted":true,
                "default_currency":"gbp","business_profile":{"name":"Owls Ltd"},
                "settings":{"dashboard":{"display_name":"The Owls"}}}"#,
        )
        .unwrap();
        assert_eq!(a.display_name().as_deref(), Some("The Owls"));
    }
}
