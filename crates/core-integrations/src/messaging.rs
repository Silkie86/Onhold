//! Text messages on a workspace's own numbers: SMS through Twilio and
//! WhatsApp through Meta's Cloud API. Sending, checking the credentials a
//! workspace gives, checking the signature on what the providers send us,
//! and reading what they send (messages in, and what became of messages out).

use std::collections::BTreeMap;

use data_encoding::{BASE64, HEXLOWER_PERMISSIVE};
use serde_json::{Value, json};
use url::Url;

use crate::provider::{self, ProviderError, endpoint, segment};

pub const TWILIO_API: &str = "https://api.twilio.com";
pub const META_API: &str = "https://graph.facebook.com";
/// Meta's Graph API version OnHold is written against.
const META_VERSION: &str = "v21.0";
/// Twilio's header with its signature over the request.
pub const TWILIO_SIGNATURE: &str = "x-twilio-signature";
/// Meta's header with its signature over the body.
pub const META_SIGNATURE: &str = "x-hub-signature-256";
/// Twilio splits longer texts into parts, and refuses past 1,600 characters.
pub const MAX_SMS: usize = 1600;
/// WhatsApp's limit for a text message.
pub const MAX_WHATSAPP: usize = 4096;

/// A number in E.164 (`+447700900123`) from what someone typed or a
/// provider sent (`447700900123`, `+44 7700 900123`, `whatsapp:+44…`).
/// `None` when it cannot be one.
pub fn e164(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let raw = raw.strip_prefix("whatsapp:").unwrap_or(raw);
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.strip_prefix("00").unwrap_or(&digits);
    // A number without its country code (07700…) cannot be texted
    // reliably; Meta and Twilio give numbers with one.
    if !(7..=15).contains(&digits.len()) || digits.starts_with('0') {
        return None;
    }
    Some(format!("+{digits}"))
}

/// A number as kept on a contact, in E.164: written with its country code
/// (`+44…`, `0044…`), or nationally (`07700 900123`) in the workspace's
/// country (`GB`). `None` when it cannot be told.
pub fn international(raw: &str, country: &str) -> Option<String> {
    let t = raw.trim();
    if t.starts_with('+') || t.starts_with("00") || t.starts_with("whatsapp:") {
        return e164(t);
    }
    let code = calling_code(country)?;
    let digits: String = t.chars().filter(char::is_ascii_digit).collect();
    // Most countries dial a 0 before a national number; North America does not.
    let national = digits.strip_prefix('0').unwrap_or(&digits);
    if national.is_empty() || national.starts_with('0') {
        return None;
    }
    e164(&format!("+{code}{national}"))
}

/// Countries' calling codes, for numbers written without one.
fn calling_code(country: &str) -> Option<&'static str> {
    Some(match country.trim().to_ascii_uppercase().as_str() {
        "GB" | "IM" | "JE" | "GG" => "44",
        "US" | "CA" => "1",
        "IE" => "353",
        "AU" => "61",
        "NZ" => "64",
        "ZA" => "27",
        "FR" => "33",
        "DE" => "49",
        "ES" => "34",
        "IT" => "39",
        "NL" => "31",
        "BE" => "32",
        "PT" => "351",
        "AT" => "43",
        "CH" => "41",
        "SE" => "46",
        "NO" => "47",
        "DK" => "45",
        "FI" => "358",
        "PL" => "48",
        "NG" => "234",
        "GH" => "233",
        "KE" => "254",
        "IN" => "91",
        "JP" => "81",
        "BR" => "55",
        "MX" => "52",
        _ => return None,
    })
}

/// What a provider said when a message was handed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub id: String,
}

/// A message that came in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    /// The provider's id for it.
    pub id: String,
    /// Who sent it, in E.164.
    pub from: String,
    /// Their name, when the provider knows it (WhatsApp's profile name).
    pub name: Option<String>,
    pub text: String,
    /// When it was sent, as seconds since 1970, when given.
    pub at: Option<i64>,
}

/// What became of a message sent from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub id: String,
    /// `sent`, `delivered`, `read`, or `failed` (with why).
    pub status: String,
    pub error: Option<String>,
}

/// What one request from a provider carried.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Received {
    pub messages: Vec<Incoming>,
    pub outcomes: Vec<Outcome>,
}

// Twilio -----------------------------------------------------------------------------------

/// A workspace's Twilio account.
#[derive(Debug, Clone)]
pub struct Twilio {
    http: reqwest::Client,
    base: Url,
    account_sid: String,
    auth_token: String,
}

impl Twilio {
    pub fn new(http: reqwest::Client, base: Url, account_sid: &str, auth_token: &str) -> Self {
        Self {
            http,
            base,
            account_sid: account_sid.to_owned(),
            auth_token: auth_token.to_owned(),
        }
    }

    fn url(&self, path: &str) -> Url {
        endpoint(
            &self.base,
            &format!("/2010-04-01/Accounts/{}{path}", segment(&self.account_sid)),
        )
    }

    /// Checks the credentials and that `number` is one of the account's.
    pub async fn check(&self, number: &str) -> Result<(), ProviderError> {
        let mut url = self.url("/IncomingPhoneNumbers.json");
        url.query_pairs_mut().append_pair("PhoneNumber", number);
        let v: Value = provider::send_json(
            self.http
                .get(url)
                .basic_auth(&self.account_sid, Some(&self.auth_token)),
        )
        .await?;
        let found = v
            .get("incoming_phone_numbers")
            .and_then(Value::as_array)
            .is_some_and(|n| !n.is_empty());
        if found {
            Ok(())
        } else {
            Err(ProviderError::Refused {
                status: 404,
                message: format!("{number} is not a number in this Twilio account"),
            })
        }
    }

    /// Sends a text; Twilio reports what became of it to `status_callback`.
    pub async fn send(
        &self,
        from: &str,
        to: &str,
        body: &str,
        status_callback: &str,
    ) -> Result<Sent, ProviderError> {
        let v: Value = provider::send_json(
            self.http
                .post(self.url("/Messages.json"))
                .basic_auth(&self.account_sid, Some(&self.auth_token))
                .form(&[
                    ("From", from),
                    ("To", to),
                    ("Body", body),
                    ("StatusCallback", status_callback),
                ]),
        )
        .await?;
        v.get("sid")
            .and_then(Value::as_str)
            .map(|id| Sent { id: id.to_owned() })
            .ok_or_else(|| ProviderError::Invalid("no message sid in the answer".into()))
    }
}

/// Whether `signature` is Twilio's for a request to `url` with these form
/// fields: base64 of HMAC-SHA1, keyed with the auth token, over the URL
/// followed by each field's name and value in name order.
pub fn twilio_signed(
    auth_token: &str,
    url: &str,
    form: &BTreeMap<String, String>,
    signature: &str,
) -> bool {
    let Ok(expected) = BASE64.decode(signature.trim().as_bytes()) else {
        return false;
    };
    ring::hmac::verify(
        &twilio_key(auth_token),
        twilio_data(url, form).as_bytes(),
        &expected,
    )
    .is_ok()
}

fn twilio_key(auth_token: &str) -> ring::hmac::Key {
    ring::hmac::Key::new(
        ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
        auth_token.as_bytes(),
    )
}

fn twilio_data(url: &str, form: &BTreeMap<String, String>) -> String {
    let mut data = url.to_owned();
    for (k, v) in form {
        data.push_str(k);
        data.push_str(v);
    }
    data
}

/// Twilio's signature for a request to `url` with these form fields (what
/// [`twilio_signed`] checks), for tests and stand-ins.
pub fn twilio_signature(auth_token: &str, url: &str, form: &BTreeMap<String, String>) -> String {
    let tag = ring::hmac::sign(&twilio_key(auth_token), twilio_data(url, form).as_bytes());
    BASE64.encode(tag.as_ref())
}

/// Meta's `X-Hub-Signature-256` for `body`, for tests and stand-ins.
pub fn meta_signature(app_secret: &str, body: &[u8]) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, app_secret.as_bytes());
    format!(
        "sha256={}",
        data_encoding::HEXLOWER.encode(ring::hmac::sign(&key, body).as_ref())
    )
}

/// What Twilio sent: a message in (`MessageSid` with a `Body`), or a
/// status callback for one sent from here (`MessageStatus`).
pub fn twilio_received(form: &BTreeMap<String, String>) -> Received {
    let mut out = Received::default();
    let Some(id) = form.get("MessageSid").or_else(|| form.get("SmsSid")) else {
        return out;
    };
    if let Some(status) = form.get("MessageStatus").or_else(|| form.get("SmsStatus")) {
        let status = status.to_ascii_lowercase();
        let outbound_status = matches!(
            status.as_str(),
            "queued" | "sending" | "sent" | "delivered" | "undelivered" | "failed" | "read"
        );
        if outbound_status && !form.contains_key("Body") {
            let failed = matches!(status.as_str(), "undelivered" | "failed");
            out.outcomes.push(Outcome {
                id: id.clone(),
                status: if failed { "failed".into() } else { status },
                error: failed.then(|| twilio_error(form.get("ErrorCode").map(String::as_str))),
            });
            return out;
        }
    }
    if let (Some(from), Some(body)) = (form.get("From").and_then(|f| e164(f)), form.get("Body")) {
        let media: usize = form
            .get("NumMedia")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        out.messages.push(Incoming {
            id: id.clone(),
            from,
            name: None,
            text: with_media_note(body, media),
            at: None,
        });
    }
    out
}

fn twilio_error(code: Option<&str>) -> String {
    match code {
        Some("21610") => "they have asked for no more texts (STOP)".into(),
        Some("21614" | "21211") => "not a mobile number that can take texts".into(),
        Some("30003") => "their phone is off or out of reach".into(),
        Some("30004") => "the message was blocked".into(),
        Some("30005") => "the number is unknown or no longer in use".into(),
        Some("30006") => "a landline, or a carrier that cannot take texts".into(),
        Some("30007") => "the carrier filtered it as spam".into(),
        Some(c) => format!("not delivered (Twilio error {c})"),
        None => "not delivered".into(),
    }
}

fn with_media_note(text: &str, media: usize) -> String {
    let text = text.trim();
    match media {
        0 => text.to_owned(),
        n => {
            let note = if n == 1 {
                "[1 picture or file, not kept]".to_owned()
            } else {
                format!("[{n} pictures or files, not kept]")
            };
            if text.is_empty() {
                note
            } else {
                format!("{text}\n{note}")
            }
        }
    }
}

// Meta (WhatsApp) -------------------------------------------------------------------------

/// A workspace's WhatsApp number in Meta's Cloud API.
#[derive(Debug, Clone)]
pub struct WhatsApp {
    http: reqwest::Client,
    base: Url,
    phone_number_id: String,
    token: String,
}

/// The WhatsApp number a phone number id stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatsAppNumber {
    pub number: String,
    pub name: Option<String>,
}

impl WhatsApp {
    pub fn new(http: reqwest::Client, base: Url, phone_number_id: &str, token: &str) -> Self {
        Self {
            http,
            base,
            phone_number_id: phone_number_id.to_owned(),
            token: token.to_owned(),
        }
    }

    fn url(&self, path: &str) -> Url {
        endpoint(
            &self.base,
            &format!("/{META_VERSION}/{}{path}", segment(&self.phone_number_id)),
        )
    }

    /// Checks the token, and reads the number and its verified name.
    pub async fn check(&self) -> Result<WhatsAppNumber, ProviderError> {
        let mut url = self.url("");
        url.query_pairs_mut()
            .append_pair("fields", "display_phone_number,verified_name");
        let v: Value = provider::send_json(self.http.get(url).bearer_auth(&self.token)).await?;
        let number = v
            .get("display_phone_number")
            .and_then(Value::as_str)
            .and_then(e164)
            .ok_or_else(|| ProviderError::Invalid("no phone number in the answer".into()))?;
        Ok(WhatsAppNumber {
            number,
            name: v
                .get("verified_name")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    async fn post(&self, body: Value) -> Result<Sent, ProviderError> {
        let v: Value = provider::send_json(
            self.http
                .post(self.url("/messages"))
                .bearer_auth(&self.token)
                .json(&body),
        )
        .await?;
        v.pointer("/messages/0/id")
            .and_then(Value::as_str)
            .map(|id| Sent { id: id.to_owned() })
            .ok_or_else(|| ProviderError::Invalid("no message id in the answer".into()))
    }

    /// A free-text message: only within 24 hours of their last message.
    pub async fn send_text(&self, to: &str, body: &str) -> Result<Sent, ProviderError> {
        self.post(json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": to.trim_start_matches('+'),
            "type": "text",
            "text": { "preview_url": false, "body": body },
        }))
        .await
    }

    /// A template approved in WhatsApp Manager, with its body's
    /// placeholders filled in order.
    pub async fn send_template(
        &self,
        to: &str,
        name: &str,
        language: &str,
        parameters: &[String],
    ) -> Result<Sent, ProviderError> {
        let mut template = json!({ "name": name, "language": { "code": language } });
        if !parameters.is_empty() {
            let params: Vec<Value> = parameters
                .iter()
                .map(|p| json!({ "type": "text", "text": p }))
                .collect();
            template["components"] = json!([{ "type": "body", "parameters": params }]);
        }
        self.post(json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": to.trim_start_matches('+'),
            "type": "template",
            "template": template,
        }))
        .await
    }
}

/// Whether `signature` (`sha256=<hex>`) is Meta's over `body`, keyed with
/// the app secret.
pub fn meta_signed(app_secret: &str, body: &[u8], signature: &str) -> bool {
    let Some(hex) = signature.trim().strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = HEXLOWER_PERMISSIVE.decode(hex.as_bytes()) else {
        return false;
    };
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, app_secret.as_bytes());
    ring::hmac::verify(&key, body, &expected).is_ok()
}

/// What Meta sent for the number `phone_number_id`: messages in, and what
/// became of messages sent from here. Other numbers' entries are left out.
pub fn meta_received(body: &Value, phone_number_id: &str) -> Received {
    let mut out = Received::default();
    let changes = body
        .get("entry")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| e.get("changes").and_then(Value::as_array))
        .flatten()
        .filter(|c| c.get("field").and_then(Value::as_str) == Some("messages"))
        .filter_map(|c| c.get("value"));
    for value in changes {
        if value
            .pointer("/metadata/phone_number_id")
            .and_then(Value::as_str)
            != Some(phone_number_id)
        {
            continue;
        }
        let names: BTreeMap<String, String> = value
            .get("contacts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| {
                Some((
                    c.get("wa_id")?.as_str()?.to_owned(),
                    c.pointer("/profile/name")?.as_str()?.to_owned(),
                ))
            })
            .collect();
        for m in value
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(id), Some(raw_from)) = (
                m.get("id").and_then(Value::as_str),
                m.get("from").and_then(Value::as_str),
            ) else {
                continue;
            };
            let Some(from) = e164(raw_from) else {
                continue;
            };
            let kind = m.get("type").and_then(Value::as_str).unwrap_or("unknown");
            let text = match kind {
                "text" => m
                    .pointer("/text/body")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                // A tapped button on a template.
                "button" => m
                    .pointer("/button/text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                "interactive" => m
                    .pointer("/interactive/button_reply/title")
                    .or_else(|| m.pointer("/interactive/list_reply/title"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                "image" | "video" | "audio" | "document" | "sticker" => {
                    let caption = m
                        .pointer(&format!("/{kind}/caption"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    with_media_note(caption, 1)
                }
                "location" => "[a location, not kept]".to_owned(),
                other => format!("[a {other} message, not kept]"),
            };
            out.messages.push(Incoming {
                id: id.to_owned(),
                name: names.get(raw_from).cloned(),
                from,
                text,
                at: m
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(|t| t.parse().ok()),
            });
        }
        for s in value
            .get("statuses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(id), Some(status)) = (
                s.get("id").and_then(Value::as_str),
                s.get("status").and_then(Value::as_str),
            ) else {
                continue;
            };
            let error = (status == "failed").then(|| {
                let e = s.pointer("/errors/0");
                let code = e.and_then(|e| e.get("code")).and_then(Value::as_i64);
                match code {
                    Some(131047) => {
                        "more than 24 hours since their last message: send an approved template"
                            .to_owned()
                    }
                    Some(131026) => "not a WhatsApp number, or it cannot take messages".to_owned(),
                    Some(131050) => "they have asked for no more messages from you".to_owned(),
                    _ => e
                        .and_then(|e| e.get("title").or_else(|| e.get("message")))
                        .and_then(Value::as_str)
                        .unwrap_or("not delivered")
                        .to_owned(),
                }
            });
            out.outcomes.push(Outcome {
                id: id.to_owned(),
                status: status.to_owned(),
                error,
            });
        }
    }
    out
}

/// Keywords a person texts to stop or start messages from a number.
pub fn opt_keyword(text: &str) -> Option<bool> {
    let word = text
        .trim()
        .trim_end_matches(['.', '!'])
        .to_ascii_uppercase();
    match word.as_str() {
        "STOP" | "STOPALL" | "UNSUBSCRIBE" | "CANCEL" | "END" | "QUIT" | "OPTOUT" | "OPT OUT" => {
            Some(true)
        }
        "START" | "UNSTOP" | "YES" | "OPTIN" | "OPT IN" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contacts_numbers_are_made_international() {
        assert_eq!(
            international("07700 900123", "GB").as_deref(),
            Some("+447700900123")
        );
        assert_eq!(
            international("+353 85 123 4567", "GB").as_deref(),
            Some("+353851234567")
        );
        assert_eq!(
            international("(415) 555-0100", "US").as_deref(),
            Some("+14155550100")
        );
        assert_eq!(international("07700 900123", "XX"), None);
    }

    #[test]
    fn numbers_are_read_as_e164() {
        assert_eq!(e164("+44 7700 900123").as_deref(), Some("+447700900123"));
        assert_eq!(e164("447700900123").as_deref(), Some("+447700900123"));
        assert_eq!(e164("0044 7700 900123").as_deref(), Some("+447700900123"));
        assert_eq!(
            e164("whatsapp:+15551234567").as_deref(),
            Some("+15551234567")
        );
        assert_eq!(e164("07700 900123"), None);
        assert_eq!(e164("12"), None);
    }

    #[test]
    fn twilio_signatures_are_checked() {
        // Worked out separately (Python's hmac) for Twilio's example request.
        let mut form = BTreeMap::new();
        for (k, v) in [
            ("CallSid", "CA1234567890ABCDE"),
            ("Caller", "+14158675310"),
            ("Digits", "1234"),
            ("From", "+14158675310"),
            ("To", "+18005551212"),
        ] {
            form.insert(k.to_owned(), v.to_owned());
        }
        let url = "https://mycompany.com/myapp.php?foo=1&bar=2";
        assert!(twilio_signed(
            "12345",
            url,
            &form,
            "GvWf1cFY/Q7PnoempGyD5oXAezc="
        ));
        assert!(!twilio_signed(
            "12346",
            url,
            &form,
            "GvWf1cFY/Q7PnoempGyD5oXAezc="
        ));
        assert!(!twilio_signed("12345", url, &form, "not base64"));
    }

    #[test]
    fn meta_signatures_are_checked() {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"secret");
        let tag = ring::hmac::sign(&key, b"{}");
        let header = format!("sha256={}", data_encoding::HEXLOWER.encode(tag.as_ref()));
        assert!(meta_signed("secret", b"{}", &header));
        assert!(!meta_signed("other", b"{}", &header));
        assert!(!meta_signed("secret", b"{ }", &header));
        assert!(!meta_signed("secret", b"{}", "sha1=00"));
    }

    #[test]
    fn twilio_requests_are_read() {
        let form: BTreeMap<String, String> = [
            ("MessageSid", "SM1"),
            ("From", "+447700900123"),
            ("To", "+447700900999"),
            ("Body", "Is Saturday still on?"),
            ("NumMedia", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let r = twilio_received(&form);
        assert_eq!(r.messages[0].from, "+447700900123");
        assert_eq!(
            r.messages[0].text,
            "Is Saturday still on?\n[1 picture or file, not kept]"
        );
        let status: BTreeMap<String, String> = [
            ("MessageSid", "SM2"),
            ("MessageStatus", "undelivered"),
            ("ErrorCode", "30005"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let r = twilio_received(&status);
        assert!(r.messages.is_empty());
        assert_eq!(r.outcomes[0].status, "failed");
        assert!(
            r.outcomes[0]
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("unknown")
        );
    }

    #[test]
    fn meta_requests_are_read() {
        let body = json!({ "object": "whatsapp_business_account", "entry": [{ "id": "waba", "changes": [{
            "field": "messages",
            "value": {
                "messaging_product": "whatsapp",
                "metadata": { "display_phone_number": "447700900999", "phone_number_id": "pn-1" },
                "contacts": [{ "profile": { "name": "Kerry" }, "wa_id": "447700900123" }],
                "messages": [
                    { "from": "447700900123", "id": "wamid.1", "timestamp": "1700000000", "type": "text",
                      "text": { "body": "Hi!" } },
                    { "from": "447700900123", "id": "wamid.2", "timestamp": "1700000001", "type": "image",
                      "image": { "caption": "The stage" } },
                ],
                "statuses": [{ "id": "wamid.out", "status": "failed",
                               "errors": [{ "code": 131047, "title": "Re-engagement message" }] }],
            } }] }] });
        let r = meta_received(&body, "pn-1");
        assert_eq!(r.messages.len(), 2);
        assert_eq!(r.messages[0].name.as_deref(), Some("Kerry"));
        assert_eq!(r.messages[0].from, "+447700900123");
        assert_eq!(
            r.messages[1].text,
            "The stage\n[1 picture or file, not kept]"
        );
        assert!(
            r.outcomes[0]
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("24 hours")
        );
        assert_eq!(meta_received(&body, "pn-2"), Received::default());
    }

    #[test]
    fn stop_and_start() {
        assert_eq!(opt_keyword(" stop "), Some(true));
        assert_eq!(opt_keyword("Unsubscribe."), Some(true));
        assert_eq!(opt_keyword("START"), Some(false));
        assert_eq!(opt_keyword("stop the music"), None);
    }
}
