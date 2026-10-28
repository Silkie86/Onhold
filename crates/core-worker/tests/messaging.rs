//! M6 text messages, against stand-ins for Twilio and Meta's WhatsApp
//! Cloud API: connecting numbers; texts in (signed) becoming threads with
//! the contact they are from; replies sent by the worker; STOP and START;
//! a delivery that failed; WhatsApp's 24-hour window and templates; a rule
//! texting a contact who agreed to texts; and disconnecting.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core-api/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use common::{Client, TestApp};
use core_api::Config;
use core_api::config::MessagingConfig;
use core_integrations::http::WebhookClient;
use core_integrations::mail::MemoryMailer;
use core_integrations::messaging::{meta_signature, twilio_signature};
use core_worker::{Settings, Worker};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use url::Url;
use uuid::Uuid;

const SID: &str = "AC0123456789abcdef";
const TOKEN: &str = "twilio-token";
const NUMBER: &str = "+447700900001";
const PHONE_ID: &str = "1029384756";
const ACCESS: &str = "meta-access";
const APP_SECRET: &str = "meta-app-secret";

/// What the stand-ins were asked to send.
#[derive(Default)]
struct World {
    sms: Vec<BTreeMap<String, String>>,
    whatsapp: Vec<Value>,
    /// Numbers Twilio refuses as having texted STOP to it.
    unsubscribed: Vec<String>,
}

type Shared = Arc<Mutex<World>>;

fn form(s: &str) -> BTreeMap<String, String> {
    url::form_urlencoded::parse(s.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

async fn handle(
    State(w): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    let path = uri.path().to_owned();
    let q = uri.query().map(form).unwrap_or_default();
    let mut w = w.lock().unwrap();
    let twilio = format!("/2010-04-01/Accounts/{SID}");
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    if let Some(rest) = path.strip_prefix(&twilio) {
        let expected = format!(
            "Basic {}",
            data_encoding::BASE64.encode(format!("{SID}:{TOKEN}").as_bytes())
        );
        if auth != expected {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({ "code": 20003, "message": "Authenticate", "status": 401 })),
            )
                .into_response();
        }
        return match (method.as_str(), rest) {
            ("GET", "/IncomingPhoneNumbers.json") => {
                let found: Vec<Value> = (q.get("PhoneNumber").map(String::as_str) == Some(NUMBER))
                    .then(|| json!({ "phone_number": NUMBER }))
                    .into_iter()
                    .collect();
                axum::Json(json!({ "incoming_phone_numbers": found })).into_response()
            }
            ("POST", "/Messages.json") => {
                let f = form(&body);
                if w.unsubscribed.contains(&f["To"]) {
                    return (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({ "code": 21610, "status": 400,
                                           "message": "Attempt to send to unsubscribed recipient" })),
                    )
                        .into_response();
                }
                w.sms.push(f);
                axum::Json(json!({ "sid": format!("SM{}", w.sms.len()) })).into_response()
            }
            _ => panic!("unexpected Twilio call {method} {path}"),
        };
    }
    assert_eq!(auth, format!("Bearer {ACCESS}"));
    match (method.as_str(), path.as_str()) {
        ("GET", p) if p == format!("/v21.0/{PHONE_ID}") => axum::Json(
            json!({ "display_phone_number": "+44 7700 900999", "verified_name": "Owls Agency", "id": PHONE_ID }),
        )
        .into_response(),
        ("POST", p) if p == format!("/v21.0/{PHONE_ID}/messages") => {
            let v: Value = serde_json::from_str(&body).unwrap();
            w.whatsapp.push(v);
            axum::Json(json!({ "messages": [{ "id": format!("wamid.{}", w.whatsapp.len()) }] }))
                .into_response()
        }
        _ => panic!("unexpected Meta call {method} {path}"),
    }
}

struct Harness {
    app: TestApp,
    worker: Worker,
    world: Shared,
}

async fn harness(pool: PgPool) -> Harness {
    let world: Shared = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stand_in = Router::new().fallback(handle).with_state(world.clone());
    tokio::spawn(async move { axum::serve(listener, stand_in).await.unwrap() });
    let base = Url::parse(&format!("http://{addr}")).unwrap();
    let mut config = Config::for_development();
    config.messaging = MessagingConfig {
        twilio_api: base.clone(),
        meta_api: base,
        ..config.messaging
    };
    let app = TestApp::with_config(pool, config).await;
    let options = app.owner.connect_options().as_ref().clone();
    let queue = core_db::worker_pool_options(PgPoolOptions::new().max_connections(3))
        .connect_with(options)
        .await
        .unwrap();
    let worker = Worker {
        app: app.state.clone(),
        queue,
        mailer: Arc::new(MemoryMailer::default()),
        webhooks: WebhookClient::new(true).unwrap(),
        settings: Settings {
            concurrency: 2,
            backoff: |_| Duration::ZERO,
            fx_rates: None,
            ..Settings::default()
        },
    };
    Harness { app, worker, world }
}

impl Harness {
    async fn drain(&self) {
        for _ in 0..50 {
            let swept = self.worker.schedule_once().await.unwrap();
            let published = self.worker.publish_once().await.unwrap();
            let ran = self.worker.run_jobs_once().await.unwrap();
            if swept.unwrap_or(0) == 0 && published == 0 && ran == 0 {
                return;
            }
        }
        panic!("the queue did not settle");
    }

    async fn get(&self, who: &Client, path: &str) -> Value {
        self.app.get(path).signed_in(who).send().await.ok().body
    }

    async fn post(&self, who: &Client, path: &str, body: Value) -> common::Resp {
        self.app.post(path).signed_in(who).json(body).send().await
    }

    /// Twilio posting `fields` to the number's address, signed.
    async fn twilio(&self, url: &str, fields: &[(&str, &str)]) -> common::Resp {
        let mut f: BTreeMap<String, String> = fields
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        f.insert("AccountSid".into(), SID.into());
        let signature = twilio_signature(TOKEN, url, &f);
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(f.iter())
            .finish();
        self.app
            .post(Url::parse(url).unwrap().path())
            .header("x-twilio-signature", &signature)
            .bytes("application/x-www-form-urlencoded", body.into_bytes())
            .send()
            .await
    }

    /// Meta posting `value` (one change) to the number's address, signed.
    async fn meta(&self, url: &str, value: Value) -> common::Resp {
        let body = serde_json::to_vec(&json!({
            "object": "whatsapp_business_account",
            "entry": [{ "id": "waba", "changes": [{ "field": "messages", "value": value }] }],
        }))
        .unwrap();
        self.app
            .post(Url::parse(url).unwrap().path())
            .header("x-hub-signature-256", &meta_signature(APP_SECRET, &body))
            .bytes("application/json", body)
            .send()
            .await
    }

    async fn thread(&self, who: &Client, channel: &str) -> Value {
        let page = self
            .get(who, &format!("/api/v1/inbox/threads?channel={channel}"))
            .await;
        let id = page["items"][0]["id"]
            .as_str()
            .expect("a thread")
            .to_owned();
        self.get(who, &format!("/api/v1/inbox/threads/{id}")).await
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn texts_come_in_and_go_out(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    let bea = h
        .post(
            &owner,
            "/api/v1/contacts",
            json!({ "name": "Bea Booker", "phones": ["07700 900123"] }),
        )
        .await
        .ok()
        .id("/id");

    // Connecting: Twilio checks the details and the number.
    h.post(
        &owner,
        "/api/v1/inbox/numbers/sms",
        json!({ "account_sid": SID, "auth_token": "wrong", "number": NUMBER }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
    let sms = h
        .post(
            &owner,
            "/api/v1/inbox/numbers/sms",
            json!({ "account_sid": SID, "auth_token": TOKEN, "number": NUMBER }),
        )
        .await
        .ok()
        .body;
    assert_eq!(sms["sender"], NUMBER);
    let url = sms["webhook_url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("http://localhost:5173/api/v1/hooks/messaging/"));
    h.post(
        &owner,
        "/api/v1/inbox/numbers/sms",
        json!({ "account_sid": SID, "auth_token": TOKEN, "number": NUMBER }),
    )
    .await
    .expect(StatusCode::CONFLICT, "already_connected");

    // A text in, signed: a thread with the contact whose number it is.
    // Unsigned, or signed for another address, it is refused.
    let text_in = [
        ("MessageSid", "SMin1"),
        ("From", "+447700900123"),
        ("To", NUMBER),
        ("Body", "Is the 14th free?"),
        ("SmsStatus", "received"),
    ];
    h.app
        .post(Url::parse(&url).unwrap().path())
        .bytes(
            "application/x-www-form-urlencoded",
            b"MessageSid=SMx".to_vec(),
        )
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "signature_invalid");
    let answer = h.twilio(&url, &text_in).await.ok();
    assert!(
        answer
            .body
            .as_str()
            .unwrap()
            .contains("<Response></Response>")
    );
    // Twilio sends again when unsure: kept once.
    h.twilio(&url, &text_in).await.ok();
    let t = h.thread(&owner, "sms").await;
    assert_eq!(t["thread"]["contact_id"], bea.to_string());
    assert_eq!(t["thread"]["unread"], true);
    assert_eq!(t["messages"].as_array().unwrap().len(), 1);
    assert_eq!(t["messages"][0]["text"], "Is the 14th free?");
    let thread = t["thread"]["id"].as_str().unwrap().to_owned();

    // A reply, sent by the worker from the number, with Twilio told where
    // to report what became of it.
    let reply = h
        .post(
            &owner,
            &format!("/api/v1/inbox/threads/{thread}/reply"),
            json!({ "body": "Yes, it is." }),
        )
        .await
        .ok()
        .body;
    assert_eq!(reply["status"], "queued");
    h.drain().await;
    {
        let w = h.world.lock().unwrap();
        assert_eq!(w.sms.len(), 1);
        assert_eq!(w.sms[0]["From"], NUMBER);
        assert_eq!(w.sms[0]["To"], "+447700900123");
        assert_eq!(w.sms[0]["Body"], "Yes, it is.");
        assert_eq!(w.sms[0]["StatusCallback"], url);
    }
    let t = h.thread(&owner, "sms").await;
    assert_eq!(t["messages"][1]["status"], "sent");

    // Twilio reports it undelivered.
    h.twilio(
        &url,
        &[
            ("MessageSid", "SM1"),
            ("MessageStatus", "undelivered"),
            ("ErrorCode", "30003"),
        ],
    )
    .await
    .ok();
    let t = h.thread(&owner, "sms").await;
    assert_eq!(t["messages"][1]["status"], "failed");

    // STOP: nothing more is sent until START.
    h.twilio(
        &url,
        &[
            ("MessageSid", "SMin2"),
            ("From", "+447700900123"),
            ("Body", "Stop"),
        ],
    )
    .await
    .ok();
    h.post(
        &owner,
        &format!("/api/v1/inbox/threads/{thread}/reply"),
        json!({ "body": "Sorry to hear that" }),
    )
    .await
    .expect(StatusCode::CONFLICT, "opted_out");
    h.twilio(
        &url,
        &[
            ("MessageSid", "SMin3"),
            ("From", "+447700900123"),
            ("Body", "START"),
        ],
    )
    .await
    .ok();
    // Texted STOP to Twilio itself: the send fails and the number is noted.
    h.world
        .lock()
        .unwrap()
        .unsubscribed
        .push("+447700900123".into());
    h.post(
        &owner,
        &format!("/api/v1/inbox/threads/{thread}/reply"),
        json!({ "body": "Welcome back" }),
    )
    .await
    .ok();
    h.drain().await;
    let t = h.thread(&owner, "sms").await;
    let last = t["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["status"], "failed", "{last}");
    h.post(
        &owner,
        &format!("/api/v1/inbox/threads/{thread}/reply"),
        json!({ "body": "Again" }),
    )
    .await
    .expect(StatusCode::CONFLICT, "opted_out");

    // Disconnected: what the provider sends is no longer taken in.
    let id = sms["id"].as_str().unwrap();
    h.app
        .delete(&format!("/api/v1/inbox/numbers/{id}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    h.twilio(&url, &text_in)
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
    let numbers = h.get(&owner, "/api/v1/inbox/numbers").await;
    assert_eq!(numbers["channels"], json!([]));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn whatsapp_keeps_to_its_window(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    let bea = h
        .post(
            &owner,
            "/api/v1/contacts",
            json!({ "name": "Bea Booker", "phones": ["+44 7700 900123"], "whatsapp_opt_in": true }),
        )
        .await
        .ok()
        .id("/id");
    let wa = h
        .post(
            &owner,
            "/api/v1/inbox/numbers/whatsapp",
            json!({ "phone_number_id": PHONE_ID, "access_token": ACCESS, "app_secret": APP_SECRET }),
        )
        .await
        .ok()
        .body;
    assert_eq!(wa["sender"], "+447700900999");
    assert_eq!(wa["display_name"], "Owls Agency");
    let url = wa["webhook_url"].as_str().unwrap().to_owned();
    let verify = wa["verify_token"].as_str().unwrap().to_owned();

    // Meta checks the callback URL with the verify token.
    let path = Url::parse(&url).unwrap().path().to_owned();
    let echoed = h
        .app
        .get(&format!(
            "{path}?hub.mode=subscribe&hub.verify_token={verify}&hub.challenge=1158201444"
        ))
        .send()
        .await
        .ok();
    assert_eq!(echoed.body, json!(1158201444));
    h.app
        .get(&format!(
            "{path}?hub.mode=subscribe&hub.verify_token=wrong&hub.challenge=1"
        ))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "signature_invalid");

    // No message from them in 24 hours: free text is refused, an approved
    // template goes.
    h.post(
        &owner,
        "/api/v1/inbox/texts",
        json!({ "channel": "whatsapp", "contact_id": bea, "body": "Hello" }),
    )
    .await
    .expect(StatusCode::CONFLICT, "whatsapp_window_closed");
    h.post(
        &owner,
        "/api/v1/templates",
        json!({ "kind": "whatsapp", "key": "hold", "name": "Hold",
                "subject": "hold_reminder", "body": "{{ contact.name }}\n{{ workspace.name }}" }),
    )
    .await
    .ok();
    h.post(
        &owner,
        "/api/v1/inbox/texts",
        json!({ "channel": "whatsapp", "contact_id": bea, "template_key": "hold" }),
    )
    .await
    .ok();
    h.drain().await;
    {
        let w = h.world.lock().unwrap();
        let sent = &w.whatsapp[0];
        assert_eq!(sent["to"], "447700900123");
        assert_eq!(sent["type"], "template");
        assert_eq!(sent["template"]["name"], "hold_reminder");
        assert_eq!(sent["template"]["language"]["code"], "en_GB");
        assert_eq!(
            sent["template"]["components"][0]["parameters"],
            json!([{ "type": "text", "text": "Bea Booker" },
                   { "type": "text", "text": "owls agency" }])
        );
    }

    // They answer: free text is open for 24 hours.
    let now = chrono::Utc::now().timestamp().to_string();
    h.meta(
        &url,
        json!({
            "messaging_product": "whatsapp",
            "metadata": { "display_phone_number": "447700900999", "phone_number_id": PHONE_ID },
            "contacts": [{ "profile": { "name": "Bea" }, "wa_id": "447700900123" }],
            "messages": [{ "from": "447700900123", "id": "wamid.in1", "timestamp": now,
                           "type": "text", "text": { "body": "Thanks!" } }],
        }),
    )
    .await
    .ok();
    let t = h.thread(&owner, "whatsapp").await;
    assert_eq!(t["messages"].as_array().unwrap().len(), 2);
    let inbound = t["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["direction"] == "inbound")
        .unwrap();
    assert_eq!(inbound["from_name"], "Bea");
    let thread = t["thread"]["id"].as_str().unwrap();
    h.post(
        &owner,
        &format!("/api/v1/inbox/threads/{thread}/reply"),
        json!({ "body": "See you then" }),
    )
    .await
    .ok();
    h.drain().await;
    let w = h.world.lock().unwrap();
    assert_eq!(w.whatsapp[1]["type"], "text");
    assert_eq!(w.whatsapp[1]["text"]["body"], "See you then");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn rules_text_only_those_who_agreed(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    h.post(
        &owner,
        "/api/v1/inbox/numbers/sms",
        json!({ "account_sid": SID, "auth_token": TOKEN, "number": NUMBER }),
    )
    .await
    .ok();
    h.post(
        &owner,
        "/api/v1/templates",
        json!({ "kind": "sms", "key": "welcome", "name": "Welcome",
                "body": "Hi {{ contact.name }}, thanks for getting in touch." }),
    )
    .await
    .ok();
    h.post(
        &owner,
        "/api/v1/rules",
        json!({ "name": "Welcome text", "trigger": { "kind": "event", "topic": "contact.created" },
                "actions": [{ "type": "send_sms", "template": "welcome" }] }),
    )
    .await
    .ok();
    // A rule's text needs an SMS template of that key.
    h.post(
        &owner,
        "/api/v1/rules",
        json!({ "name": "Nope", "trigger": { "kind": "event", "topic": "contact.created" },
                "actions": [{ "type": "send_sms", "template": "missing" }] }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");

    let quiet: Uuid = h
        .post(
            &owner,
            "/api/v1/contacts",
            json!({ "name": "Quiet Quinn", "phones": ["07700 900222"] }),
        )
        .await
        .ok()
        .id("/id");
    h.post(
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Keen Kim", "phones": ["07700 900333"], "sms_opt_in": true }),
    )
    .await
    .ok();
    h.drain().await;
    let w = h.world.lock().unwrap();
    assert_eq!(w.sms.len(), 1, "only the contact who agreed is texted");
    assert_eq!(w.sms[0]["To"], "+447700900333");
    assert_eq!(
        w.sms[0]["Body"],
        "Hi Keen Kim, thanks for getting in touch."
    );
    drop(w);
    let runs = h.get(&owner, "/api/v1/rules").await;
    let rule = runs["items"]
        .as_array()
        .or(runs.as_array())
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Welcome text")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let log = h.get(&owner, &format!("/api/v1/rules/{rule}/log")).await;
    let text = log.to_string();
    assert!(text.contains("has not agreed"), "{text}");
    assert!(text.contains(&quiet.to_string()), "{text}");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn follow_ups_switch_on_and_off(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("finches").await;
    let list = h.get(&owner, "/api/v1/follow-ups").await;
    let keys: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "enquiry_unanswered",
            "quote_chase",
            "invoice_overdue",
            "booking_reminder_text"
        ]
    );
    assert!(list.as_array().unwrap().iter().all(|f| f["on"] == false));

    let set = |key: &str, on: bool| {
        let path = format!("/api/v1/follow-ups/{key}");
        let req = h.app.put(&path).signed_in(&owner).json(json!({ "on": on }));
        async move { req.send().await }
    };
    // Every follow-up installs: its rules check out against its templates.
    for key in keys {
        let f = set(key, true).await.ok().body;
        assert_eq!(f["on"], true, "{key}");
    }
    let overdue = set("invoice_overdue", true).await.ok().body;
    let ids = overdue["rule_ids"].as_array().unwrap().clone();
    assert_eq!(ids.len(), 3, "switching on again adds nothing");
    let rules = h.get(&owner, "/api/v1/rules").await;
    let rules = rules["items"].as_array().or(rules.as_array()).unwrap();
    assert_eq!(rules.len(), 6);

    // Off deletes the rules and keeps the (perhaps reworded) templates.
    let template: Uuid = h
        .get(&owner, "/api/v1/templates?kind=email")
        .await
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "follow_up.invoice.first")
        .map(|t| t["id"].as_str().unwrap().parse().unwrap())
        .unwrap();
    let off = set("invoice_overdue", false).await.ok().body;
    assert_eq!(off["on"], false);
    assert!(off["rule_ids"].as_array().unwrap().is_empty());
    h.app
        .get(&format!("/api/v1/rules/{}", ids[0].as_str().unwrap()))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
    h.get(&owner, &format!("/api/v1/templates/{template}")).await;
    let again = set("invoice_overdue", true).await.ok().body;
    assert_eq!(again["rule_ids"].as_array().unwrap().len(), 3);
    assert_ne!(again["rule_ids"][0], ids[0]);

    set("nothing_like_it", true)
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
}
