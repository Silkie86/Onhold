//! M6 email inbox, against stand-ins for Google, Gmail, Microsoft login and
//! Microsoft Graph that answer with recorded responses (`fixtures/mail`):
//! connecting a mailbox, reading it in the background, threading and linking
//! messages to contacts, replying from it, privacy between members, and a
//! grant that stops working.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core-api/tests/common/mod.rs"]
mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use common::{Client, TestApp};
use core_api::Config;
use core_api::config::MailboxConfig;
use core_integrations::http::WebhookClient;
use core_integrations::mail::MemoryMailer;
use core_integrations::oauth::OAuthApp;
use core_worker::{Settings, Worker};
use data_encoding::{BASE64, BASE64URL};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use url::Url;

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/mail/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path).unwrap()
}

/// A recorded message answering `in_reply_to`, dated `minutes` from now.
fn dated(name: &str, in_reply_to: &str, minutes: i64) -> String {
    let date = (chrono::Utc::now() + chrono::Duration::minutes(minutes)).to_rfc2822();
    fixture(name)
        .replace("{{IN_REPLY_TO}}", in_reply_to)
        .replace("{{DATE}}", &date)
}

fn json_fixture(name: &str) -> Value {
    serde_json::from_str(&fixture(&format!("{name}.json"))).unwrap()
}

/// A message in the stand-in Gmail mailbox.
#[derive(Clone)]
struct GmailMessage {
    id: String,
    thread: String,
    labels: Vec<String>,
    raw: Vec<u8>,
    history: u64,
}

/// A message in a stand-in Outlook folder.
#[derive(Clone)]
struct GraphMessage {
    id: String,
    conversation: String,
    raw: Vec<u8>,
    seq: u64,
}

/// The stand-in providers' mailboxes and what they were asked.
#[derive(Default)]
struct Fake {
    base: String,
    gmail: Vec<GmailMessage>,
    history: u64,
    /// What OnHold sent through Gmail: the JSON body.
    gmail_sent: Vec<Value>,
    graph_inbox: Vec<GraphMessage>,
    graph_sent: Vec<GraphMessage>,
    seq: u64,
    /// What OnHold sent through Graph: the MIME message.
    graph_sendmail: Vec<String>,
    /// Refresh tokens withdrawn.
    revoked: Vec<String>,
    /// The person withdrew OnHold's access: APIs answer 401 and refreshing fails.
    withdrawn: bool,
    refreshes: usize,
}

type Shared = Arc<Mutex<Fake>>;

impl Fake {
    fn add_gmail(&mut self, id: &str, thread: &str, labels: &[&str], raw: &str) {
        self.history += 1;
        self.gmail.push(GmailMessage {
            id: id.into(),
            thread: thread.into(),
            labels: labels.iter().map(|l| (*l).to_owned()).collect(),
            raw: raw.as_bytes().to_vec(),
            history: self.history,
        });
    }

    fn add_graph(&mut self, folder: &str, id: &str, conversation: &str, raw: &[u8]) {
        self.seq += 1;
        let m = GraphMessage {
            id: id.into(),
            conversation: conversation.into(),
            raw: raw.to_vec(),
            seq: self.seq,
        };
        match folder {
            "inbox" => self.graph_inbox.push(m),
            _ => self.graph_sent.push(m),
        }
    }
}

fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({ "error": { "code": 401, "message": "Request had invalid authentication credentials.",
            "status": "UNAUTHENTICATED" } })),
    )
        .into_response()
}

fn refused(f: &Fake, headers: &HeaderMap) -> bool {
    f.withdrawn
        || !headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("Bearer "))
}

async fn token(State(fake): State<Shared>, body: String) -> Response {
    let f = form(&body);
    let mut s = fake.lock().unwrap();
    let microsoft = f.get("client_id").map(String::as_str) == Some("ms-client");
    let mut tokens = json_fixture(if microsoft {
        "microsoft_token"
    } else {
        "google_token"
    });
    match (f["grant_type"].as_str(), f.get("code").map(String::as_str)) {
        ("authorization_code", Some("good-code")) => {}
        ("authorization_code", Some("no-offline")) => {
            tokens.as_object_mut().unwrap().remove("refresh_token");
        }
        ("refresh_token", _) if !s.withdrawn => {
            s.refreshes += 1;
            tokens["access_token"] = json!(format!("refreshed-{}", s.refreshes));
            // Google keeps the refresh token; it is not sent again.
            tokens.as_object_mut().unwrap().remove("refresh_token");
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({ "error": "invalid_grant",
                    "error_description": "Token has been expired or revoked." })),
            )
                .into_response();
        }
    }
    axum::Json(tokens).into_response()
}

async fn revoke(State(fake): State<Shared>, body: String) -> StatusCode {
    let f = form(&body);
    fake.lock().unwrap().revoked.push(f["token"].clone());
    StatusCode::OK
}

async fn gmail_profile(State(fake): State<Shared>, headers: HeaderMap) -> Response {
    let s = fake.lock().unwrap();
    if refused(&s, &headers) {
        return unauthorized();
    }
    let mut p = json_fixture("gmail_profile");
    p["historyId"] = json!(s.history.to_string());
    axum::Json(p).into_response()
}

async fn gmail_list(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let s = fake.lock().unwrap();
    if refused(&s, &headers) {
        return unauthorized();
    }
    assert!(q["q"].contains("after:"), "{q:?}");
    assert!(q["q"].contains("-in:spam"), "{q:?}");
    // Newest first, as Gmail lists, without what the search leaves out.
    let messages: Vec<Value> = s
        .gmail
        .iter()
        .rev()
        .filter(|m| {
            !m.labels
                .iter()
                .any(|l| l == "SPAM" || l.starts_with("CATEGORY_"))
        })
        .map(|m| json!({ "id": m.id, "threadId": m.thread }))
        .collect();
    axum::Json(json!({ "messages": messages, "resultSizeEstimate": messages.len() }))
        .into_response()
}

async fn gmail_message(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let s = fake.lock().unwrap();
    if refused(&s, &headers) {
        return unauthorized();
    }
    assert_eq!(q.get("format").map(String::as_str), Some("raw"));
    let Some(m) = s.gmail.iter().find(|m| m.id == id) else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(
                json!({ "error": { "code": 404, "message": "Requested entity was not found." } }),
            ),
        )
            .into_response();
    };
    axum::Json(json!({
        "id": m.id,
        "threadId": m.thread,
        "labelIds": m.labels,
        "snippet": "",
        "historyId": m.history.to_string(),
        "sizeEstimate": m.raw.len(),
        "raw": BASE64URL.encode(&m.raw),
    }))
    .into_response()
}

async fn gmail_thread(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let s = fake.lock().unwrap();
    if refused(&s, &headers) {
        return unauthorized();
    }
    let messages: Vec<Value> = s
        .gmail
        .iter()
        .filter(|m| m.thread == id)
        .map(|m| json!({ "id": m.id, "threadId": m.thread, "labelIds": m.labels }))
        .collect();
    axum::Json(json!({ "id": id, "messages": messages })).into_response()
}

async fn gmail_history(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let s = fake.lock().unwrap();
    if refused(&s, &headers) {
        return unauthorized();
    }
    let start: u64 = q["startHistoryId"].parse().unwrap();
    let history: Vec<Value> = s
        .gmail
        .iter()
        .filter(|m| m.history > start)
        .map(|m| {
            json!({ "id": m.history.to_string(), "messagesAdded": [
                { "message": { "id": m.id, "threadId": m.thread, "labelIds": m.labels } }
            ] })
        })
        .collect();
    axum::Json(json!({ "history": history, "historyId": s.history.to_string() })).into_response()
}

async fn gmail_send(
    State(fake): State<Shared>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let mut s = fake.lock().unwrap();
    if refused(&s, &headers) {
        return unauthorized();
    }
    let raw = BASE64URL
        .decode(body["raw"].as_str().unwrap().as_bytes())
        .unwrap();
    let n = s.gmail_sent.len() + 1;
    let id = format!("sent-{n}");
    let thread = body["threadId"]
        .as_str()
        .map_or_else(|| format!("new-thread-{n}"), str::to_owned);
    s.gmail_sent.push(body);
    // Gmail files what was sent in the mailbox too, and it comes back
    // with the next history read.
    let raw = String::from_utf8(raw).unwrap();
    s.add_gmail(&id, &thread, &["SENT"], &raw);
    axum::Json(json!({ "id": id, "threadId": thread, "labelIds": ["SENT"] })).into_response()
}

async fn graph_me(State(fake): State<Shared>, headers: HeaderMap) -> Response {
    let s = fake.lock().unwrap();
    if refused(&s, &headers) {
        return unauthorized();
    }
    axum::Json(json_fixture("graph_me")).into_response()
}

async fn graph_delta(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(folder): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let s = fake.lock().unwrap();
    if refused(&s, &headers) {
        return unauthorized();
    }
    let after: u64 = match q.get("$deltatoken") {
        Some(t) => t.parse().unwrap(),
        None => {
            assert!(q["$filter"].starts_with("receivedDateTime ge "), "{q:?}");
            0
        }
    };
    let items = match folder.as_str() {
        "inbox" => &s.graph_inbox,
        "sentitems" => &s.graph_sent,
        other => panic!("unexpected folder {other}"),
    };
    let value: Vec<Value> = items
        .iter()
        .filter(|m| m.seq > after)
        .map(|m| json!({ "@odata.etag": "W/\"1\"", "id": m.id, "conversationId": m.conversation, "isDraft": false }))
        .collect();
    let delta = format!(
        "{}/v1.0/me/mailFolders/{folder}/messages/delta?$deltatoken={}",
        s.base, s.seq
    );
    axum::Json(json!({ "value": value, "@odata.deltaLink": delta })).into_response()
}

async fn graph_mime(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let s = fake.lock().unwrap();
    if refused(&s, &headers) {
        return unauthorized();
    }
    let m = s
        .graph_inbox
        .iter()
        .chain(&s.graph_sent)
        .find(|m| m.id == id)
        .expect("a known message");
    ([("content-type", "message/rfc822")], m.raw.clone()).into_response()
}

async fn graph_sendmail(State(fake): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let mut s = fake.lock().unwrap();
    if refused(&s, &headers) {
        return unauthorized();
    }
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "text/plain"
    );
    let raw = BASE64.decode(body.trim().as_bytes()).unwrap();
    s.graph_sendmail
        .push(String::from_utf8(raw.clone()).unwrap());
    // Outlook files a copy in Sent Items, read with the next delta.
    let id = format!("AAMkSent{}", s.graph_sendmail.len());
    s.add_graph("sentitems", &id, "AAQkConv-cal", &raw);
    StatusCode::ACCEPTED.into_response()
}

async fn fake_providers() -> (Url, Shared) {
    let fake: Shared = Arc::default();
    let app = Router::new()
        .route("/token", post(token))
        .route("/revoke", post(revoke))
        .route("/common/oauth2/v2.0/token", post(token))
        .route("/gmail/v1/users/me/profile", get(gmail_profile))
        .route("/gmail/v1/users/me/messages", get(gmail_list))
        .route("/gmail/v1/users/me/messages/send", post(gmail_send))
        .route("/gmail/v1/users/me/messages/{id}", get(gmail_message))
        .route("/gmail/v1/users/me/history", get(gmail_history))
        .route("/gmail/v1/users/me/threads/{id}", get(gmail_thread))
        .route("/v1.0/me", get(graph_me))
        .route(
            "/v1.0/me/mailFolders/{folder}/messages/delta",
            get(graph_delta),
        )
        .route("/v1.0/me/messages/{id}/$value", get(graph_mime))
        .route("/v1.0/me/sendMail", post(graph_sendmail))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = Url::parse(&format!("http://{addr}")).unwrap();
    fake.lock().unwrap().base = base.as_str().trim_end_matches('/').to_owned();
    (base, fake)
}

struct Harness {
    app: TestApp,
    worker: Worker,
    mailer: Arc<MemoryMailer>,
    fake: Shared,
}

async fn harness(pool: PgPool) -> Harness {
    let (base, fake) = fake_providers().await;
    let config = Config {
        mailboxes: MailboxConfig {
            google: Some(
                OAuthApp::google("google-client".into(), "google-secret".into()).at(&base),
            ),
            microsoft: Some(OAuthApp::microsoft_at(
                "ms-client".into(),
                "ms-secret".into(),
                "common",
                base.as_str(),
            )),
            gmail_api: base.clone(),
            graph_api: base,
            ..MailboxConfig::default()
        },
        ..Config::for_development()
    };
    let app = TestApp::with_config(pool, config).await;
    let options = app.owner.connect_options().as_ref().clone();
    let queue = core_db::worker_pool_options(PgPoolOptions::new().max_connections(3))
        .connect_with(options)
        .await
        .unwrap();
    let mailer = Arc::new(MemoryMailer::default());
    let worker = Worker {
        app: app.state.clone(),
        queue,
        mailer: mailer.clone(),
        webhooks: WebhookClient::new(true).unwrap(),
        settings: Settings {
            concurrency: 2,
            backoff: |_| Duration::ZERO,
            fx_rates: None,
            ..Settings::default()
        },
    };
    Harness {
        app,
        worker,
        mailer,
        fake,
    }
}

impl Harness {
    /// What OnHold's own sender sent to `to` (invitations go through it too).
    fn sent_to(&self, to: &str) -> Vec<core_integrations::mail::Email> {
        self.mailer
            .sent()
            .into_iter()
            .filter(|e| e.to == to)
            .collect()
    }

    async fn drain(&self) {
        for _ in 0..50 {
            let published = self.worker.publish_once().await.unwrap();
            self.worker.schedule_once().await.unwrap();
            let ran = self.worker.run_jobs_once().await.unwrap();
            if published == 0 && ran == 0 {
                return;
            }
        }
        panic!("the queue did not settle");
    }

    async fn post(&self, who: &Client, path: &str, body: Value) -> common::Resp {
        self.app.post(path).signed_in(who).json(body).send().await
    }

    async fn patch(&self, who: &Client, path: &str, body: Value) -> common::Resp {
        self.app.patch(path).signed_in(who).json(body).send().await
    }

    async fn get(&self, who: &Client, path: &str) -> common::Resp {
        self.app.get(path).signed_in(who).send().await
    }

    /// Connects `provider` for `who` the way the Settings → Email page does.
    async fn connect(&self, who: &Client, provider: &str, code: &str) -> common::Resp {
        let start = self
            .post(
                who,
                "/api/v1/inbox/mailboxes/connect",
                json!({ "provider": provider }),
            )
            .await
            .ok()
            .body;
        let url = Url::parse(start["url"].as_str().unwrap()).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert!(q["redirect_uri"].ends_with("/settings/email"), "{q:?}");
        self.post(
            who,
            "/api/v1/inbox/mailboxes/connect/finish",
            json!({ "code": code, "state": q["state"] }),
        )
        .await
    }

    async fn threads(&self, who: &Client) -> Value {
        self.get(who, "/api/v1/inbox/threads").await.ok().body
    }

    async fn thread(&self, who: &Client, id: &str) -> Value {
        self.get(who, &format!("/api/v1/inbox/threads/{id}"))
            .await
            .ok()
            .body
    }
}

fn find<'a>(page: &'a Value, subject: &str) -> &'a Value {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["subject"] == subject)
        .unwrap_or_else(|| panic!("no thread `{subject}` in {page}"))
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn gmail_enquiries_arrive_linked_and_replies_go_out_from_the_mailbox(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    let bea = h
        .post(
            &owner,
            "/api/v1/contacts",
            json!({ "name": "Bea Grand", "emails": ["bea@grand.test"] }),
        )
        .await
        .ok()
        .id("/id");
    {
        let mut f = h.fake.lock().unwrap();
        f.history = 9000;
        f.add_gmail(
            "g-enquiry",
            "t-bea",
            &["INBOX", "UNREAD"],
            &fixture("enquiry.eml"),
        );
        f.add_gmail("g-news", "t-news", &["INBOX"], &fixture("newsletter.eml"));
        f.add_gmail("g-spam", "t-spam", &["SPAM"], &fixture("spam.eml"));
    }

    let setup = h.get(&owner, "/api/v1/inbox/mailboxes").await.ok().body;
    assert_eq!(setup["google"], true, "{setup}");
    assert_eq!(setup["mailboxes"], json!([]), "{setup}");

    // Google must let OnHold stay connected, or there is nothing to read later.
    h.connect(&owner, "google", "no-offline")
        .await
        .expect(StatusCode::BAD_REQUEST, "offline_access_missing");
    // A state from somewhere else is refused.
    h.post(
        &owner,
        "/api/v1/inbox/mailboxes/connect/finish",
        json!({ "code": "good-code", "state": "not-a-state" }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "connect_state_invalid");

    let mailbox = h.connect(&owner, "google", "good-code").await.ok().body;
    assert_eq!(mailbox["mailbox"], "ana@owls.test", "{mailbox}");
    assert_eq!(mailbox["status"], "active", "{mailbox}");
    assert_eq!(mailbox["shared"], false, "{mailbox}");
    let mailbox_id = mailbox["id"].as_str().unwrap().to_owned();
    // The grant is stored sealed, never in the clear.
    let secret: Vec<u8> = sqlx::query_scalar("SELECT secret FROM integrations")
        .fetch_one(&h.app.owner)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&secret).contains("1//0gtest-owls-refresh"));

    // The worker reads the mailbox: the enquiry is linked to Bea; spam and
    // the newsletter stay in the mailbox only.
    h.drain().await;
    let page = h.threads(&owner).await;
    assert_eq!(page["total"], 1, "{page}");
    let thread = find(&page, "Booking enquiry for 12 June");
    assert_eq!(thread["contact_id"], bea.to_string(), "{thread}");
    assert_eq!(thread["unread"], true);
    assert_eq!(thread["status"], "open");
    assert_eq!(thread["participants"], json!(["bea@grand.test"]));
    let thread_id = thread["id"].as_str().unwrap().to_owned();

    let detail = h.thread(&owner, &thread_id).await;
    let first = &detail["messages"][0];
    assert_eq!(first["direction"], "inbound");
    assert_eq!(first["from_address"], "bea@grand.test");
    assert!(first["text"].as_str().unwrap().contains("free on 12 June"));
    // Nothing that runs, tracks or reaches out survives.
    let html = first["html"].as_str().unwrap();
    assert!(!html.contains("<script"), "{html}");
    assert!(!html.contains("track.test"), "{html}");
    assert!(
        html.contains("rel=\"noopener noreferrer nofollow\""),
        "{html}"
    );
    // The attachment stays in the mailbox: it is listed, read from Gmail
    // when opened, and reached through the thread only.
    let file = &first["files"][0];
    assert_eq!(file["filename"], "rider.txt");
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM files")
        .fetch_one(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(stored, 0);
    let file_id = file["id"].as_str().unwrap();
    let download = h
        .get(
            &owner,
            &format!("/api/v1/inbox/threads/{thread_id}/files/{file_id}"),
        )
        .await;
    assert_eq!(download.status, StatusCode::OK);
    assert_eq!(download.body, "Two vocal mics, one DI.");
    h.get(&owner, &format!("/api/v1/files/{file_id}"))
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");

    // Bea's contact page shows the conversation.
    let bea_threads = h
        .get(&owner, &format!("/api/v1/inbox/threads?contact_id={bea}"))
        .await
        .ok()
        .body;
    assert_eq!(bea_threads["total"], 1);

    // Reading marks it read, without needing write access.
    let read = h
        .patch(
            &owner,
            &format!("/api/v1/inbox/threads/{thread_id}"),
            json!({ "unread": false }),
        )
        .await
        .ok()
        .body;
    assert_eq!(read["thread"]["unread"], false);

    // The reply goes out from Ana's Gmail, in the same Gmail thread, signed.
    h.app
        .put("/api/v1/signatures/mine")
        .signed_in(&owner)
        .json(json!({ "signature": "Ana Owl\nThe Owls" }))
        .send()
        .await
        .ok();
    let reply = h
        .post(
            &owner,
            &format!("/api/v1/inbox/threads/{thread_id}/reply"),
            json!({ "body": "Thanks Bea, 12 June is free." }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.body["status"], "queued");
    assert_eq!(reply.body["to"], json!(["bea@grand.test"]));
    h.drain().await;
    let sent = h.fake.lock().unwrap().gmail_sent.clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["threadId"], "t-bea");
    let raw = String::from_utf8(
        BASE64URL
            .decode(sent[0]["raw"].as_str().unwrap().as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert!(raw.contains("In-Reply-To: <bea-1@grand.test>"), "{raw}");
    assert!(
        raw.contains("Subject: Re: Booking enquiry for 12 June"),
        "{raw}"
    );
    assert!(raw.contains("From: "), "{raw}");
    assert!(raw.contains("ana@owls.test"), "{raw}");
    let our_id = raw
        .lines()
        .find_map(|l| l.strip_prefix("Message-ID: "))
        .unwrap()
        .trim()
        .to_owned();
    let detail = h.thread(&owner, &thread_id).await;
    assert_eq!(detail["messages"][1]["status"], "sent", "{detail}");
    let text = detail["messages"][1]["text"].as_str().unwrap();
    assert!(text.ends_with("free.\n\n-- \nAna Owl\nThe Owls"), "{text}");
    let html = detail["messages"][1]["html"].as_str().unwrap();
    assert!(html.contains("Ana Owl<br>The Owls"), "{html}");
    assert!(
        h.sent_to("bea@grand.test").is_empty(),
        "nothing goes through OnHold's own sender"
    );

    // Bea answers, and her out-of-office answers too. The copy Gmail filed
    // under Sent is recognised, not added twice.
    {
        let mut f = h.fake.lock().unwrap();
        let answer = dated("bea_reply.eml", &our_id, 1);
        f.add_gmail("g-answer", "t-bea", &["INBOX", "UNREAD"], &answer);
        let ooo = dated("out_of_office.eml", &our_id, 2);
        f.add_gmail("g-ooo", "t-bea", &["INBOX"], &ooo);
    }
    h.post(
        &owner,
        &format!("/api/v1/inbox/mailboxes/{mailbox_id}/sync"),
        json!({}),
    )
    .await
    .ok();
    h.drain().await;
    let detail = h.thread(&owner, &thread_id).await;
    let messages = detail["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4, "{detail}");
    assert_eq!(messages[2]["from_address"], "bea@grand.test");
    assert_eq!(messages[3]["automatic"], true);
    assert_eq!(detail["thread"]["unread"], true);
    // The preview is what Bea wrote, not her out-of-office.
    let preview = detail["thread"]["preview"].as_str().unwrap();
    assert!(
        preview.starts_with("Lovely, please hold it for us."),
        "{preview}"
    );
    let page = h.threads(&owner).await;
    assert_eq!(page["total"], 1, "no new threads: {page}");

    // Another member does not see a private mailbox until it is shared.
    let agent = h.app.add_member(&owner, "cy@owls.test", "agent").await;
    assert_eq!(h.threads(&agent).await["total"], 0);
    h.get(&agent, &format!("/api/v1/inbox/threads/{thread_id}"))
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
    h.get(
        &agent,
        &format!("/api/v1/inbox/threads/{thread_id}/files/{file_id}"),
    )
    .await
    .expect(StatusCode::NOT_FOUND, "not_found");
    h.patch(
        &agent,
        &format!("/api/v1/inbox/mailboxes/{mailbox_id}"),
        json!({ "shared": true }),
    )
    .await
    .expect(StatusCode::NOT_FOUND, "not_found");
    h.patch(
        &owner,
        &format!("/api/v1/inbox/mailboxes/{mailbox_id}"),
        json!({ "shared": true }),
    )
    .await
    .ok();
    assert_eq!(h.threads(&agent).await["total"], 1);

    // Another workspace sees nothing at all.
    let (_other, stranger) = h.app.workspace_with_owner("hawks").await;
    assert_eq!(h.threads(&stranger).await["total"], 0);
    h.get(&stranger, &format!("/api/v1/inbox/threads/{thread_id}"))
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");

    // The conversation becomes an enquiry for Bea.
    let made = h
        .post(
            &agent,
            &format!("/api/v1/inbox/threads/{thread_id}/enquiry"),
            json!({ "title": "Grand Hall, 12 June" }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let enquiry = made.body["enquiry_id"].as_str().unwrap().to_owned();
    assert_eq!(made.body["contact_id"], bea.to_string());
    let detail = h.thread(&owner, &thread_id).await;
    assert_eq!(detail["thread"]["enquiry_id"], enquiry);

    // Snoozed, it comes back on its own.
    let until = chrono::Utc::now() + chrono::Duration::seconds(2);
    let snoozed = h
        .patch(
            &owner,
            &format!("/api/v1/inbox/threads/{thread_id}"),
            json!({ "status": "snoozed", "snoozed_until": until }),
        )
        .await
        .ok()
        .body;
    assert_eq!(snoozed["thread"]["status"], "snoozed");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    h.drain().await;
    let detail = h.thread(&owner, &thread_id).await;
    assert_eq!(detail["thread"]["status"], "open", "{detail}");

    // Ana withdraws OnHold's access in her Google account: the mailbox asks
    // to be connected again rather than retrying for ever.
    h.fake.lock().unwrap().withdrawn = true;
    h.post(
        &owner,
        &format!("/api/v1/inbox/mailboxes/{mailbox_id}/sync"),
        json!({}),
    )
    .await
    .ok();
    h.drain().await;
    let setup = h.get(&owner, "/api/v1/inbox/mailboxes").await.ok().body;
    assert_eq!(setup["mailboxes"][0]["status"], "reconnect", "{setup}");
    // Replies still go out, through OnHold's own sender instead.
    h.post(
        &owner,
        &format!("/api/v1/inbox/threads/{thread_id}/reply"),
        json!({ "body": "Held until Friday." }),
    )
    .await
    .ok();
    h.drain().await;
    let fallback = h.sent_to("bea@grand.test");
    assert_eq!(fallback.len(), 1);
    // Answers still go to Ana's own mailbox.
    assert_eq!(fallback[0].reply_to.as_deref(), Some("ana@owls.test"));

    // Connecting again picks up where it stopped.
    h.fake.lock().unwrap().withdrawn = false;
    let again = h.connect(&owner, "google", "good-code").await.ok().body;
    assert_eq!(again["id"], mailbox_id.as_str());
    assert_eq!(again["status"], "active");

    // Ana writes to someone new from Gmail itself: that stays in Gmail. When
    // they answer (through a booking site, asking for answers elsewhere),
    // the conversation comes in whole, and a reply goes where they asked.
    {
        let mut f = h.fake.lock().unwrap();
        f.add_gmail(
            "g-out",
            "t-cleo",
            &["SENT"],
            "From: Ana <ana@owls.test>\nTo: bookings@stages.test\nSubject: Owls in May\n\
             Date: Mon, 26 Oct 2026 09:00:00 +0000\nMessage-ID: <out1@owls.test>\n\n\
             Are you booking for May?\n",
        );
        f.add_gmail(
            "g-in",
            "t-cleo",
            &["INBOX"],
            "From: Stages <bookings@stages.test>\nReply-To: Cleo <cleo@client.test>\n\
             To: ana@owls.test\nSubject: Re: Owls in May\nDate: Mon, 26 Oct 2026 10:00:00 +0000\n\
             Message-ID: <in1@stages.test>\nIn-Reply-To: <out1@owls.test>\n\n\
             Yes, 14 May please.\n",
        );
    }
    h.drain().await;
    let page = h.threads(&owner).await;
    let cleo = find(&page, "Owls in May");
    assert_eq!(cleo["message_count"], 2, "{cleo}");
    let cleo_id = cleo["id"].as_str().unwrap().to_owned();
    h.post(
        &owner,
        &format!("/api/v1/inbox/threads/{cleo_id}/reply"),
        json!({ "body": "Booked." }),
    )
    .await
    .ok();
    h.drain().await;
    let sent = h.fake.lock().unwrap().gmail_sent.last().cloned().unwrap();
    let raw = BASE64URL
        .decode(sent["raw"].as_str().unwrap().as_bytes())
        .unwrap();
    assert!(
        String::from_utf8_lossy(&raw).contains("To: Cleo <cleo@client.test>")
            || String::from_utf8_lossy(&raw).contains("To: cleo@client.test"),
        "{}",
        String::from_utf8_lossy(&raw)
    );

    // Disconnecting forgets the grant and tells Google; the threads stay.
    let gone = h
        .app
        .delete(&format!("/api/v1/inbox/mailboxes/{mailbox_id}"))
        .signed_in(&owner)
        .send()
        .await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT, "{}", gone.body);
    assert_eq!(
        h.fake.lock().unwrap().revoked,
        vec!["1//0gtest-owls-refresh".to_owned()]
    );
    assert_eq!(h.threads(&owner).await["total"], 2);
    // Its attachments stayed in the mailbox, so they cannot be opened now.
    let detail = h.thread(&owner, &thread_id).await;
    let file_id = detail["messages"][0]["files"][0]["id"].as_str().unwrap();
    h.get(
        &owner,
        &format!("/api/v1/inbox/threads/{thread_id}/files/{file_id}"),
    )
    .await
    .expect(StatusCode::CONFLICT, "mailbox_unavailable");
    let actions = h.app.audit_actions(_ws).await;
    for action in [
        "integration.connected",
        "integration.reconnected",
        "integration.disconnected",
        "thread.replied",
    ] {
        assert!(actions.iter().any(|a| a == action), "{action}: {actions:?}");
    }

    // Connected again after disconnecting, it is the same mailbox: what it
    // read before is recognised rather than read in twice.
    let back = h.connect(&owner, "google", "good-code").await.ok().body;
    assert_eq!(back["id"], mailbox_id.as_str());
    h.drain().await;
    assert_eq!(h.threads(&owner).await["total"], 2);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_shared_mailbox_connected_again_by_another_member_carries_on(pool: PgPool) {
    let h = harness(pool).await;
    let (ws, owner) = h.app.workspace_with_owner("owls").await;
    h.fake
        .lock()
        .unwrap()
        .add_gmail("g-bea", "t-bea", &["INBOX"], &fixture("enquiry.eml"));
    let mailbox_id = h
        .connect(&owner, "google", "good-code")
        .await
        .ok()
        .id("/id")
        .to_string();
    h.patch(
        &owner,
        &format!("/api/v1/inbox/mailboxes/{mailbox_id}"),
        json!({ "shared": true }),
    )
    .await
    .ok();
    h.drain().await;
    assert_eq!(h.threads(&owner).await["total"], 1);
    let gone = h
        .app
        .delete(&format!("/api/v1/inbox/mailboxes/{mailbox_id}"))
        .signed_in(&owner)
        .send()
        .await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT, "{}", gone.body);

    // Another member connects it: the same connection, now theirs, so the
    // mail it read is not read in again.
    let agent = h.app.add_member(&owner, "cy@owls.test", "agent").await;
    let back = h.connect(&agent, "google", "good-code").await.ok().body;
    assert_eq!(back["id"], mailbox_id.as_str(), "{back}");
    let theirs: bool = sqlx::query_scalar(
        "SELECT owner_id = (SELECT id FROM users WHERE email = 'cy@owls.test')
         FROM integrations WHERE id = $1::uuid",
    )
    .bind(&mailbox_id)
    .fetch_one(&h.app.owner)
    .await
    .unwrap();
    assert!(theirs);
    h.drain().await;
    assert_eq!(h.threads(&agent).await["total"], 1);
    let actions = h.app.audit_actions(ws).await;
    assert!(actions.iter().any(|a| a == "integration.reconnected"));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn outlook_mail_is_read_and_replies_are_matched_to_their_sent_copy(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    h.fake.lock().unwrap().add_graph(
        "inbox",
        "AAMkCal1",
        "AAQkConv-cal",
        fixture("graph_inbound.eml").as_bytes(),
    );

    let mailbox = h.connect(&owner, "microsoft", "good-code").await.ok().body;
    // Addresses are kept in lower case.
    assert_eq!(mailbox["mailbox"], "ben@owls.test", "{mailbox}");
    h.drain().await;
    let page = h.threads(&owner).await;
    assert_eq!(page["total"], 1, "{page}");
    let thread = find(&page, "Festival slot");
    assert_eq!(
        thread["contact_id"],
        Value::Null,
        "an unknown sender stays unlinked"
    );
    let thread_id = thread["id"].as_str().unwrap().to_owned();
    let unlinked = h
        .get(&owner, "/api/v1/inbox/threads?unlinked=true")
        .await
        .ok()
        .body;
    assert_eq!(unlinked["total"], 1);

    h.post(
        &owner,
        &format!("/api/v1/inbox/threads/{thread_id}/reply"),
        json!({ "body": "Yes, Sunday works." }),
    )
    .await
    .ok();
    h.drain().await;
    let sent = h.fake.lock().unwrap().graph_sendmail.clone();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].contains("In-Reply-To: <cal-1@promo.test>"),
        "{}",
        sent[0]
    );
    assert!(sent[0].contains("To: cal@promo.test"), "{}", sent[0]);

    // The copy in Sent Items arrives with the next read and is matched to
    // the reply by its Message-ID, not added again.
    let mailbox_id = mailbox["id"].as_str().unwrap();
    h.post(
        &owner,
        &format!("/api/v1/inbox/mailboxes/{mailbox_id}/sync"),
        json!({}),
    )
    .await
    .ok();
    h.drain().await;
    let detail = h.thread(&owner, &thread_id).await;
    assert_eq!(detail["messages"].as_array().unwrap().len(), 2, "{detail}");
    assert_eq!(detail["messages"][1]["status"], "sent");
    assert_eq!(h.threads(&owner).await["total"], 1);

    // Make an enquiry, adding the unknown sender as a contact.
    let made = h
        .post(
            &owner,
            &format!("/api/v1/inbox/threads/{thread_id}/enquiry"),
            json!({ "title": "Festival, Sunday stage" }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let contact = made.body["contact_id"].as_str().unwrap();
    let c = h
        .get(&owner, &format!("/api/v1/contacts/{contact}"))
        .await
        .ok()
        .body;
    assert_eq!(c["name"], "Cal Promoter", "{c}");
    assert_eq!(c["email"], "cal@promo.test", "{c}");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn without_a_mailbox_messages_go_through_onhold_and_answers_join_the_thread(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    let bea = h
        .post(
            &owner,
            "/api/v1/contacts",
            json!({ "name": "Bea Grand", "emails": ["bea@grand.test"] }),
        )
        .await
        .ok()
        .id("/id");

    // Copies need a mailbox to send from.
    h.post(
        &owner,
        "/api/v1/inbox/threads",
        json!({ "to": ["bea@grand.test"], "cc": ["boss@grand.test"], "subject": "Hello", "body": "Hi" }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");

    let made = h
        .post(
            &owner,
            "/api/v1/inbox/threads",
            json!({ "to": ["bea@grand.test"], "subject": "Dates for June", "body": "Hi Bea,\n\nWe have 12 and 19 June." }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    assert_eq!(made.body["thread"]["contact_id"], bea.to_string());
    assert_eq!(made.body["thread"]["integration_id"], Value::Null);
    let thread_id = made.body["thread"]["id"].as_str().unwrap().to_owned();
    h.drain().await;
    let sent = h.sent_to("bea@grand.test");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].reply_to.as_deref(), Some("owner@owls.test"));
    let html = sent[0].html.as_deref().unwrap();
    assert!(html.contains("We have 12 and 19 June."), "{html}");
    let our_id = sent[0].message_id.clone();

    // Bea answers the member directly; once their Gmail is connected, the
    // answer joins the conversation started here.
    {
        let mut f = h.fake.lock().unwrap();
        f.history = 100;
        let answer = dated("bea_reply.eml", &our_id, 1);
        f.add_gmail("g-answer", "t-new", &["INBOX"], &answer);
    }
    h.connect(&owner, "google", "good-code").await.ok();
    h.drain().await;
    let page = h.threads(&owner).await;
    assert_eq!(page["total"], 1, "{page}");
    let detail = h.thread(&owner, &thread_id).await;
    assert_eq!(detail["messages"].as_array().unwrap().len(), 2, "{detail}");
    assert!(detail["thread"]["integration_id"].is_string(), "{detail}");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn mail_waiting_to_go_is_not_sent_once_the_workspace_closes(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    let made = h
        .post(
            &owner,
            "/api/v1/inbox/threads",
            json!({ "to": ["bea@grand.test"], "subject": "Dates for June", "body": "Hi Bea" }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let thread_id: uuid::Uuid = made.body["thread"]["id"].as_str().unwrap().parse().unwrap();
    h.post(
        &owner,
        "/api/v1/workspace/close",
        json!({ "confirm_slug": "owls" }),
    )
    .await
    .ok();
    h.drain().await;
    assert!(h.sent_to("bea@grand.test").is_empty());
    let (status, error): (String, Option<String>) =
        sqlx::query_as("SELECT status, error FROM thread_messages WHERE thread_id = $1")
            .bind(thread_id)
            .fetch_one(&h.app.owner)
            .await
            .unwrap();
    assert_eq!(status, "failed");
    assert_eq!(error.as_deref(), Some("the workspace was closed"));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn mail_waiting_in_a_suspended_workspace_goes_once_it_is_back(pool: PgPool) {
    let h = harness(pool).await;
    let (ws, owner) = h.app.workspace_with_owner("owls").await;
    let made = h
        .post(
            &owner,
            "/api/v1/inbox/threads",
            json!({ "to": ["bea@grand.test"], "subject": "Dates for June", "body": "Hi Bea" }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let thread_id: uuid::Uuid = made.body["thread"]["id"].as_str().unwrap().parse().unwrap();
    let set_status = |status: &'static str| {
        let pool = h.app.owner.clone();
        async move {
            sqlx::query("UPDATE workspaces SET status = $2::text::workspace_status WHERE id = $1")
                .bind(ws.0)
                .bind(status)
                .execute(&pool)
                .await
                .unwrap();
        }
    };
    set_status("suspended").await;
    h.drain().await;
    assert!(h.sent_to("bea@grand.test").is_empty());
    let message_status = || {
        let pool = h.app.owner.clone();
        async move {
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM thread_messages WHERE thread_id = $1",
            )
            .bind(thread_id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(message_status().await, "queued");

    // Back in business: the message goes when it is next tried.
    set_status("active").await;
    sqlx::query("UPDATE jobs SET run_at = now() WHERE kind = 'inbox.send' AND status = 'queued'")
        .execute(&h.app.owner)
        .await
        .unwrap();
    h.drain().await;
    assert_eq!(h.sent_to("bea@grand.test").len(), 1);
    assert_eq!(message_status().await, "sent");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn messages_too_long_to_store_are_refused_not_failed(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    // Within the length allowed, but its HTML is longer than can be stored.
    let quotes = "\"".repeat(100_000);
    let res = h
        .post(
            &owner,
            "/api/v1/inbox/threads",
            json!({ "to": ["bea@grand.test"], "subject": "Quotes", "body": quotes }),
        )
        .await;
    res.expect(StatusCode::BAD_REQUEST, "invalid_input");

    // A template that makes far more than is kept of mail read in.
    let line = "x".repeat(100);
    h.post(
        &owner,
        "/api/v1/templates",
        json!({ "kind": "email", "key": "huge", "name": "Huge", "subject": "Long",
                "body": format!("{{% for i in range(3000) %}}<p>{line}</p>{{% endfor %}}") }),
    )
    .await
    .ok();
    let res = h
        .post(
            &owner,
            "/api/v1/inbox/threads",
            json!({ "to": ["bea@grand.test"], "template_key": "huge" }),
        )
        .await;
    res.expect(StatusCode::BAD_REQUEST, "invalid_input");
    assert_eq!(res.body["error"]["field"], "template_key", "{}", res.body);
}

// IMAP and SMTP ------------------------------------------------------------------------

/// A stand-in mail server's INBOX (UID, raw message) and what was sent
/// through its SMTP side.
#[derive(Default)]
struct MailServer {
    inbox: Vec<(u32, String)>,
    smtp_sent: Vec<String>,
    logins: Vec<String>,
}

type Server = Arc<Mutex<MailServer>>;

const IMAP_PASSWORD: &str = "app-password-1";

/// Just enough IMAP4rev1 for OnHold: LOGIN, SELECT, UID SEARCH, UID FETCH, LOGOUT.
async fn fake_imap(server: Server) -> u16 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let server = server.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                write
                    .write_all(b"* OK [CAPABILITY IMAP4rev1] Owls mail ready\r\n")
                    .await
                    .unwrap();
                while let Ok(Some(line)) = lines.next_line().await {
                    let (tag, rest) = line.split_once(' ').unwrap();
                    let upper = rest.to_ascii_uppercase();
                    let reply = if upper.starts_with("LOGIN") {
                        server.lock().unwrap().logins.push(rest.to_owned());
                        if rest.contains(IMAP_PASSWORD) {
                            format!("{tag} OK LOGIN completed\r\n")
                        } else {
                            format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n")
                        }
                    } else if upper.starts_with("SELECT") {
                        let s = server.lock().unwrap();
                        format!(
                            "* {} EXISTS\r\n* 0 RECENT\r\n* FLAGS (\\Seen \\Answered)\r\n\
                             * OK [UIDVALIDITY 7] UIDs valid\r\n* OK [UIDNEXT {}] next\r\n\
                             {tag} OK [READ-WRITE] SELECT completed\r\n",
                            s.inbox.len(),
                            s.inbox.last().map_or(1, |m| m.0 + 1)
                        )
                    } else if upper.starts_with("UID SEARCH") {
                        let s = server.lock().unwrap();
                        let uids: Vec<String> =
                            if let Some(range) = upper.strip_prefix("UID SEARCH UID ") {
                                let from: u32 = range.split(':').next().unwrap().parse().unwrap();
                                // `n:*` always includes the highest UID.
                                let mut v: Vec<u32> =
                                    s.inbox.iter().map(|m| m.0).filter(|u| *u >= from).collect();
                                if v.is_empty() {
                                    v.extend(s.inbox.last().map(|m| m.0));
                                }
                                v.iter().map(u32::to_string).collect()
                            } else {
                                assert!(upper.starts_with("UID SEARCH SINCE "), "{line}");
                                s.inbox.iter().map(|m| m.0.to_string()).collect()
                            };
                        format!(
                            "* SEARCH {}\r\n{tag} OK SEARCH completed\r\n",
                            uids.join(" ")
                        )
                    } else if upper.starts_with("UID FETCH") {
                        let set = rest.split(' ').nth(2).unwrap();
                        let wanted: Vec<u32> = set.split(',').map(|u| u.parse().unwrap()).collect();
                        let s = server.lock().unwrap();
                        let mut out = String::new();
                        for (n, (uid, raw)) in s.inbox.iter().enumerate() {
                            if wanted.contains(uid) {
                                out.push_str(&format!(
                                    "* {} FETCH (UID {uid} BODY[] {{{}}}\r\n{raw})\r\n",
                                    n + 1,
                                    raw.len()
                                ));
                            }
                        }
                        out.push_str(&format!("{tag} OK FETCH completed\r\n"));
                        out
                    } else if upper.starts_with("LOGOUT") {
                        let bye = format!("* BYE logging out\r\n{tag} OK LOGOUT completed\r\n");
                        write.write_all(bye.as_bytes()).await.unwrap();
                        return;
                    } else {
                        format!("{tag} BAD unknown command\r\n")
                    };
                    if write.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// Just enough ESMTP: EHLO, AUTH PLAIN, NOOP, MAIL, RCPT, DATA, QUIT.
async fn fake_smtp(server: Server) -> u16 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let server = server.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                write
                    .write_all(b"220 smtp.owls.test ESMTP\r\n")
                    .await
                    .unwrap();
                while let Ok(Some(line)) = lines.next_line().await {
                    let upper = line.to_ascii_uppercase();
                    let reply = if upper.starts_with("EHLO") {
                        "250-smtp.owls.test\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n".to_owned()
                    } else if let Some(creds) = line.strip_prefix("AUTH PLAIN ") {
                        let decoded = BASE64.decode(creds.trim().as_bytes()).unwrap();
                        if String::from_utf8_lossy(&decoded).ends_with(IMAP_PASSWORD) {
                            "235 2.7.0 Authentication successful\r\n".to_owned()
                        } else {
                            "535 5.7.8 Authentication credentials invalid\r\n".to_owned()
                        }
                    } else if upper.starts_with("DATA") {
                        write.write_all(b"354 go ahead\r\n").await.unwrap();
                        let mut message = String::new();
                        while let Ok(Some(l)) = lines.next_line().await {
                            if l == "." {
                                break;
                            }
                            message.push_str(&l);
                            message.push_str("\r\n");
                        }
                        server.lock().unwrap().smtp_sent.push(message);
                        "250 2.0.0 queued\r\n".to_owned()
                    } else if upper.starts_with("QUIT") {
                        let _ = write.write_all(b"221 bye\r\n").await;
                        return;
                    } else {
                        "250 OK\r\n".to_owned()
                    };
                    if write.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_imap_mailbox_is_read_and_replies_go_out_by_smtp(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    let server: Server = Arc::default();
    server
        .lock()
        .unwrap()
        .inbox
        .push((41, fixture("enquiry.eml").replace('\n', "\r\n")));
    let imap = fake_imap(server.clone()).await;
    let smtp = fake_smtp(server.clone()).await;
    let body = |password: &str| {
        json!({
            "address": "Ana@Owls.test",
            "display_name": "Ana at The Owls",
            "imap": { "host": "127.0.0.1", "port": imap, "security": "none" },
            "smtp": { "host": "127.0.0.1", "port": smtp, "security": "none" },
            "password": password,
        })
    };

    // A wrong password shows when the mailbox is added, not at the first read.
    h.post(&owner, "/api/v1/inbox/mailboxes/imap", body("wrong"))
        .await
        .expect(StatusCode::BAD_REQUEST, "provider_refused");
    let mailbox = h
        .post(&owner, "/api/v1/inbox/mailboxes/imap", body(IMAP_PASSWORD))
        .await
        .ok()
        .body;
    assert_eq!(mailbox["mailbox"], "ana@owls.test", "{mailbox}");
    assert_eq!(mailbox["provider"], "imap");
    // The password is never shown back, and is stored sealed.
    assert!(!mailbox.to_string().contains(IMAP_PASSWORD));
    let stored: String =
        sqlx::query_scalar("SELECT settings::text || encode(secret, 'escape') FROM integrations")
            .fetch_one(&h.app.owner)
            .await
            .unwrap();
    assert!(!stored.contains(IMAP_PASSWORD));
    assert!(
        server.lock().unwrap().logins[1].contains("ana@owls.test"),
        "the username defaults to the address"
    );

    h.drain().await;
    let page = h.threads(&owner).await;
    assert_eq!(page["total"], 1, "{page}");
    let thread_id = page["items"][0]["id"].as_str().unwrap().to_owned();
    let detail = h.thread(&owner, &thread_id).await;
    assert_eq!(detail["messages"][0]["files"][0]["filename"], "rider.txt");
    // Opening it reads the message from the server again.
    let file_id = detail["messages"][0]["files"][0]["id"].as_str().unwrap();
    let download = h
        .get(
            &owner,
            &format!("/api/v1/inbox/threads/{thread_id}/files/{file_id}"),
        )
        .await
        .ok();
    assert_eq!(download.body, "Two vocal mics, one DI.");

    h.app
        .put("/api/v1/signatures/workspace")
        .signed_in(&owner)
        .json(json!({ "signature": "The Owls" }))
        .send()
        .await
        .ok();
    h.post(
        &owner,
        &format!("/api/v1/inbox/threads/{thread_id}/reply"),
        json!({ "body": "Thanks Bea, 12 June is free.", "signature": false }),
    )
    .await
    .ok();
    h.drain().await;
    let sent = server.lock().unwrap().smtp_sent.clone();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].contains("In-Reply-To: <bea-1@grand.test>"),
        "{}",
        sent[0]
    );
    assert!(sent[0].contains("Ana at The Owls"), "{}", sent[0]);
    let detail = h.thread(&owner, &thread_id).await;
    assert_eq!(detail["messages"][1]["status"], "sent", "{detail}");
    let text = detail["messages"][1]["text"].as_str().unwrap();
    assert!(!text.contains("-- "), "left unsigned when asked: {text}");

    // A new message arrives; only it is read.
    server.lock().unwrap().inbox.push((
        42,
        dated("bea_reply.eml", "<bea-1@grand.test>", 1).replace('\n', "\r\n"),
    ));
    let mailbox_id = mailbox["id"].as_str().unwrap();
    h.post(
        &owner,
        &format!("/api/v1/inbox/mailboxes/{mailbox_id}/sync"),
        json!({}),
    )
    .await
    .ok();
    h.drain().await;
    let detail = h.thread(&owner, &thread_id).await;
    assert_eq!(detail["messages"].as_array().unwrap().len(), 3, "{detail}");
    assert_eq!(h.threads(&owner).await["total"], 1);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_answer_copied_to_two_private_mailboxes_stays_in_each(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    let ana = h.app.add_member(&owner, "ana@owls.test", "agent").await;
    let cy = h.app.add_member(&owner, "cy@owls.test", "agent").await;
    // Ana reads her Gmail here, Cy his own mailbox by IMAP; both private.
    let ana_box = h.connect(&ana, "google", "good-code").await.ok().body;
    let server: Server = Arc::default();
    let imap = fake_imap(server.clone()).await;
    let smtp = fake_smtp(server.clone()).await;
    let cy_box = h
        .post(
            &cy,
            "/api/v1/inbox/mailboxes/imap",
            json!({
                "address": "cy@owls.test",
                "imap": { "host": "127.0.0.1", "port": imap, "security": "none" },
                "smtp": { "host": "127.0.0.1", "port": smtp, "security": "none" },
                "password": IMAP_PASSWORD,
            }),
        )
        .await
        .ok()
        .body;
    h.drain().await;

    // The owner, with no mailbox, writes to a client through OnHold.
    h.post(
        &owner,
        "/api/v1/inbox/threads",
        json!({ "to": ["dee@client.test"], "subject": "Summer dates", "body": "Which dates?",
                "from_onhold": true }),
    )
    .await
    .ok();
    h.drain().await;
    let first: String = sqlx::query_scalar("SELECT message_id FROM thread_messages")
        .fetch_one(&h.app.owner)
        .await
        .unwrap();

    // The client answers, copying Ana and Cy: each mailbox gets a copy.
    let answer = format!(
        "From: Dee <dee@client.test>\r\nTo: owner@owls.test\r\nCc: ana@owls.test, cy@owls.test\r\n\
         Subject: Re: Summer dates\r\nDate: Tue, 27 Oct 2026 10:00:00 +0000\r\n\
         Message-ID: <dee1@client.test>\r\nIn-Reply-To: <{first}>\r\n\r\nPrivate: 3 July.\r\n"
    );
    h.fake
        .lock()
        .unwrap()
        .add_gmail("g-dee", "t-dee", &["INBOX"], &answer);
    server.lock().unwrap().inbox.push((50, answer.clone()));
    for (who, mailbox) in [(&ana, &ana_box), (&cy, &cy_box)] {
        let id = mailbox["id"].as_str().unwrap();
        h.post(
            who,
            &format!("/api/v1/inbox/mailboxes/{id}/sync"),
            json!({}),
        )
        .await
        .ok();
    }
    h.drain().await;

    // Ana's copy joins the conversation; Cy's stays in a thread of his own
    // mailbox, never in Ana's private one (and the reverse).
    let ana_page = h.threads(&ana).await;
    let cy_page = h.threads(&cy).await;
    let ana_thread = find(&ana_page, "Summer dates");
    let cy_thread = find(&cy_page, "Summer dates");
    assert_ne!(ana_thread["id"], cy_thread["id"], "{ana_page} {cy_page}");
    assert_ne!(ana_thread["integration_id"], cy_thread["integration_id"]);
    assert_eq!(cy_thread["message_count"], 1, "{cy_thread}");
    let cy_id = cy_thread["id"].as_str().unwrap();
    h.get(&ana, &format!("/api/v1/inbox/threads/{cy_id}"))
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
}

/// Mail sent from the mailbox outside OnHold, dated `days_ago`.
fn sent_mail(id: &str, subject: &str, days_ago: i64) -> String {
    let date = (chrono::Utc::now() - chrono::Duration::days(days_ago)).to_rfc2822();
    format!(
        "From: Ana Owl <ana@owls.test>\r\nTo: Bea Grand <bea@grand.test>\r\nSubject: {subject}\r\n\
         Message-ID: <{id}@owls.test>\r\nDate: {date}\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nHi Bea, have a listen.\r\n"
    )
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_answer_asking_for_replies_to_a_contact_stays_one_when_the_pitch_is_edited(
    pool: PgPool,
) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    let venue = h
        .post(
            &owner,
            "/api/v1/organisations",
            json!({ "name": "The Venue" }),
        )
        .await
        .ok()
        .id("/id");
    h.post(
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Jane Venue", "emails": ["jane@venue.test"], "organisation_id": venue }),
    )
    .await
    .ok();
    let happened = (chrono::Utc::now() - chrono::Duration::days(5)).to_rfc3339();
    let pitched = h
        .post(
            &owner,
            "/api/v1/pitches",
            json!({ "organisation_id": venue, "kind": "action", "type_key": "pitched",
                    "channel": "phone", "happened_at": happened }),
        )
        .await
        .ok()
        .id("/id");
    // Their booking system writes, asking for replies to go to Jane.
    let date = (chrono::Utc::now() - chrono::Duration::days(2)).to_rfc2822();
    h.fake.lock().unwrap().add_gmail(
        "g-jane",
        "t-jane",
        &["INBOX"],
        &format!(
            "From: Venue Bookings <bookings@venuesystem.test>\r\nReply-To: Jane <jane@venue.test>\r\n\
             To: ana@owls.test\r\nSubject: Your pitch\r\nMessage-ID: <jane1@venuesystem.test>\r\n\
             Date: {date}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
             We would love to have you.\r\n"
        ),
    );
    h.connect(&owner, "google", "good-code").await.ok();
    h.drain().await;
    let answered = |log: &Value| log["items"][0]["answered_at"].clone();
    let log = h
        .get(&owner, &format!("/api/v1/organisations/{venue}/pitches"))
        .await
        .ok()
        .body;
    assert!(answered(&log).is_string(), "{log}");

    // Editing the note works the answer out again, and finds the same one.
    let edited = h
        .app
        .patch(&format!("/api/v1/pitches/{pitched}"))
        .signed_in(&owner)
        .header("If-Match", &format!("\"{}\"", log["items"][0]["version"]))
        .json(json!({ "note": "Sent the live video too" }))
        .send()
        .await
        .ok();
    assert_eq!(
        edited.body["answered_at"],
        answered(&log),
        "{}",
        edited.body
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn answers_to_pitches_and_pitches_in_sent_are_picked_up(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    let hall = h
        .post(
            &owner,
            "/api/v1/organisations",
            json!({ "name": "The Grand Hall" }),
        )
        .await
        .ok()
        .id("/id");
    let bea = h
        .post(
            &owner,
            "/api/v1/contacts",
            json!({ "name": "Bea Grand", "emails": ["bea@grand.test"], "organisation_id": hall }),
        )
        .await
        .ok()
        .id("/id");
    // Pitched by phone before Bea wrote; and a call after, not answered yet.
    let pitched = h
        .post(
            &owner,
            "/api/v1/pitches",
            json!({ "organisation_id": hall, "contact_id": bea, "kind": "action", "type_key": "pitched",
                    "channel": "phone", "happened_at": "2026-10-18T10:00:00Z" }),
        )
        .await
        .ok()
        .id("/id");
    {
        let mut f = h.fake.lock().unwrap();
        f.add_gmail("g-bea", "t-bea", &["INBOX"], &fixture("enquiry.eml"));
        f.add_gmail(
            "g-out1",
            "t-out1",
            &["SENT"],
            &sent_mail("out1", "The Owls for June", 3),
        );
        f.add_gmail(
            "g-out2",
            "t-out2",
            &["SENT"],
            &sent_mail("out2", "Invoice copy", 2),
        );
    }
    h.connect(&owner, "google", "good-code").await.ok();
    h.drain().await;

    // Bea's mail answered the pitch.
    let log = h
        .get(&owner, &format!("/api/v1/organisations/{hall}/pitches"))
        .await
        .ok()
        .body;
    let entry = &log["items"][0];
    assert_eq!(entry["id"], pitched.to_string(), "{log}");
    assert!(
        entry["answered_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-10-20"),
        "{entry}"
    );
    let threads = h.threads(&owner).await;
    let answer_thread = find(&threads, "Booking enquiry for 12 June")["id"].clone();
    assert_eq!(entry["answer"]["thread_id"], answer_thread);

    // Mail to them from Sent is offered, newest first, to log or set aside.
    let unlogged = h
        .get(
            &owner,
            &format!("/api/v1/organisations/{hall}/pitches/unlogged"),
        )
        .await
        .ok()
        .body;
    assert_eq!(unlogged.as_array().unwrap().len(), 2, "{unlogged}");
    assert_eq!(unlogged[0]["subject"], "Invoice copy");
    assert_eq!(unlogged[1]["contact_name"], "Bea Grand");
    // Another member can't see mail in the owner's own mailbox, nor log it.
    let cy = h.app.add_member(&owner, "cy@owls.test", "agent").await;
    let theirs = h
        .get(
            &cy,
            &format!("/api/v1/organisations/{hall}/pitches/unlogged"),
        )
        .await
        .ok()
        .body;
    assert_eq!(theirs, json!([]));
    let res = h
        .post(
            &cy,
            "/api/v1/pitches",
            json!({ "organisation_id": hall, "kind": "action", "type_key": "pitched",
                    "channel": "email", "message_id": unlogged[0]["message_id"] }),
        )
        .await;
    assert!(res.status.is_client_error(), "{}", res.body);
    let res = h
        .post(
            &cy,
            "/api/v1/pitches/not-a-pitch",
            json!({ "message_id": unlogged[0]["message_id"] }),
        )
        .await;
    assert!(res.status.is_client_error(), "{}", res.body);
    // An action logged late, from before Bea wrote, finds her answer.
    let late = h
        .post(
            &owner,
            "/api/v1/pitches",
            json!({ "organisation_id": hall, "contact_id": bea, "kind": "action", "type_key": "pitched",
                    "channel": "email", "happened_at": "2026-10-19T10:00:00Z" }),
        )
        .await
        .ok()
        .body;
    assert!(
        late["answered_at"]
            .as_str()
            .unwrap_or_default()
            .starts_with("2026-10-20"),
        "{late}"
    );
    assert_eq!(late["answer"]["channel"], "email", "{late}");
    let spotted = h
        .post(
            &owner,
            "/api/v1/pitches",
            json!({ "organisation_id": hall, "contact_id": bea, "kind": "action", "type_key": "pitched",
                    "channel": "email", "message_id": unlogged[1]["message_id"],
                    "happened_at": unlogged[1]["sent_at"] }),
        )
        .await;
    assert_eq!(spotted.status, StatusCode::CREATED, "{}", spotted.body);
    assert_eq!(spotted.body["source"], "spotted");
    assert_eq!(spotted.body["message"]["status"], "sent");
    // Only once.
    h.post(
        &owner,
        "/api/v1/pitches",
        json!({ "organisation_id": hall, "kind": "action", "type_key": "pitched",
                "message_id": unlogged[1]["message_id"] }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "invalid_input");
    let res = h
        .post(
            &owner,
            "/api/v1/pitches/not-a-pitch",
            json!({ "message_id": unlogged[0]["message_id"] }),
        )
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let unlogged = h
        .get(
            &owner,
            &format!("/api/v1/organisations/{hall}/pitches/unlogged"),
        )
        .await
        .ok()
        .body;
    assert_eq!(unlogged, json!([]));
}
