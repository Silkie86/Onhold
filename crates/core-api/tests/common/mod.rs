//! Test harness: a real Postgres per test (`#[sqlx::test]`), the real router,
//! and a small client that keeps session cookies and CSRF tokens.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use core_api::services::tenancy;
use core_api::{AppState, Config};
use core_domain::authz::RequestMeta;
use core_domain::{UserId, WorkspaceId};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use uuid::Uuid;

pub const PASSWORD: &str = "a perfectly fine passphrase";

pub struct TestApp {
    pub state: AppState,
    pub router: axum::Router,
    /// Superuser pool from `#[sqlx::test]`, for assertions that must see past RLS.
    pub owner: PgPool,
}

impl TestApp {
    pub async fn new(owner: PgPool) -> Self {
        Self::with_config(owner, Config::for_development()).await
    }

    pub async fn with_config(owner: PgPool, config: Config) -> Self {
        let options = owner.connect_options().as_ref().clone();
        let app_pool = core_db::app_pool_options(PgPoolOptions::new().max_connections(5))
            .connect_with(options)
            .await
            .expect("app pool");
        let state = AppState::new(app_pool, config).expect("state");
        let router = core_api::router(state.clone());
        Self {
            state,
            router,
            owner,
        }
    }

    pub fn request(&self, method: Method, path: &str) -> Req<'_> {
        Req {
            app: self,
            method,
            path: path.to_owned(),
            headers: HeaderMap::new(),
            body: None,
        }
    }

    pub fn get(&self, path: &str) -> Req<'_> {
        self.request(Method::GET, path)
    }

    pub fn post(&self, path: &str) -> Req<'_> {
        self.request(Method::POST, path)
    }

    pub fn patch(&self, path: &str) -> Req<'_> {
        self.request(Method::PATCH, path)
    }

    pub fn put(&self, path: &str) -> Req<'_> {
        self.request(Method::PUT, path)
    }

    pub fn delete(&self, path: &str) -> Req<'_> {
        self.request(Method::DELETE, path)
    }

    /// Creates a workspace as the operator CLI would, and returns it with
    /// the owner's invitation token.
    pub async fn create_workspace(&self, slug: &str, owner_email: &str) -> (WorkspaceId, String) {
        let created = tenancy::create_workspace(
            &self.state,
            None,
            RequestMeta::default(),
            tenancy::NewWorkspace {
                slug: slug.into(),
                name: format!("{slug} agency"),
                country: "GB".into(),
                default_currency: "GBP".into(),
                default_timezone: "Europe/London".into(),
                locale: None,
                plan: None,
                perspective: None,
                plugin_slug: None,
                owner_email: owner_email.into(),
            },
        )
        .await
        .expect("workspace created");
        (
            created.workspace.id,
            token_from_url(&created.owner_invitation_url),
        )
    }

    /// A workspace with a signed-in owner.
    pub async fn workspace_with_owner(&self, slug: &str) -> (WorkspaceId, Client) {
        let email = format!("owner@{slug}.test");
        let (ws, token) = self.create_workspace(slug, &email).await;
        let client = self.accept_new(&token, &format!("{slug} owner")).await;
        (ws, client.in_workspace(ws))
    }

    /// Accepts an invitation by creating an account; returns the signed-in client.
    pub async fn accept_new(&self, token: &str, name: &str) -> Client {
        let res = self
            .post("/api/v1/invitations/accept")
            .json(json!({ "token": token, "name": name, "password": PASSWORD }))
            .send()
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        Client::from_login(&res)
    }

    /// Invites `email` into `owner`'s workspace with `role` and signs them up.
    pub async fn add_member(&self, owner: &Client, email: &str, role: &str) -> Client {
        self.post("/api/v1/invitations")
            .signed_in(owner)
            .json(json!({ "email": email, "role": role }))
            .send()
            .await
            .ok();
        let token = self
            .last_invitation_token(owner.workspace.expect("workspace"))
            .await;
        self.accept_new(&token, email)
            .await
            .in_workspace(owner.workspace.expect("workspace"))
    }

    pub async fn login(&self, email: &str, password: &str) -> Resp {
        self.post("/api/v1/auth/login")
            .json(json!({ "email": email, "password": password }))
            .send()
            .await
    }

    /// The link in an event's sealed secret: on the outbox row until the
    /// worker publishes it, then on the `mail.system` job it queued.
    async fn event_link(
        &self,
        topic: &str,
        ws: Option<WorkspaceId>,
        email: Option<&str>,
    ) -> Option<String> {
        let row: Option<(Uuid, Option<Vec<u8>>)> = sqlx::query_as(
            "SELECT o.id, coalesce(o.secret, j.secret) FROM outbox o
             LEFT JOIN jobs j ON j.kind = 'mail.system' AND j.payload->>'event_id' = o.id::text
             WHERE o.topic = $1 AND ($2::uuid IS NULL OR o.workspace_id = $2)
               AND ($3::text IS NULL OR o.payload->>'email' = $3)
             ORDER BY o.created_at DESC, o.id DESC LIMIT 1",
        )
        .bind(topic)
        .bind(ws.map(|w| w.0))
        .bind(email)
        .fetch_optional(&self.owner)
        .await
        .expect("event");
        let (id, sealed) = row?;
        let secret =
            core_api::services::open_event_secret(&self.state.config.keyring, id, &sealed?)
                .expect("secret opens");
        secret.get("url").and_then(Value::as_str).map(str::to_owned)
    }

    /// The newest invitation link in a workspace (what the email carries).
    pub async fn last_invitation_token(&self, ws: WorkspaceId) -> String {
        let url = self
            .event_link("invitation.created", Some(ws), None)
            .await
            .expect("invitation event");
        token_from_url(&url)
    }

    pub async fn last_magic_link_token(&self, email: &str) -> Option<String> {
        self.event_link("auth.magic_link_requested", None, Some(email))
            .await
            .map(|u| token_from_url(&u))
    }

    /// Publishes every pending outbox event as the worker would (stream
    /// position, secret dropped), without queueing the jobs it would.
    pub async fn publish_all(&self) {
        sqlx::query(
            "UPDATE outbox SET seq = nextval('outbox_seq'), processed_at = now(), secret = NULL
             WHERE id IN (SELECT id FROM outbox WHERE processed_at IS NULL ORDER BY created_at, id)",
        )
        .execute(&self.owner)
        .await
        .expect("publish");
    }

    pub async fn user_id(&self, email: &str) -> UserId {
        UserId(
            sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
                .bind(email)
                .fetch_one(&self.owner)
                .await
                .expect("user"),
        )
    }

    pub async fn make_superadmin(&self, email: &str) {
        sqlx::query("UPDATE users SET is_superadmin = true WHERE email = $1")
            .bind(email)
            .execute(&self.owner)
            .await
            .expect("grant");
    }

    pub async fn audit_actions(&self, ws: WorkspaceId) -> Vec<String> {
        sqlx::query_scalar("SELECT action FROM audit_log WHERE workspace_id = $1 ORDER BY id")
            .bind(ws.0)
            .fetch_all(&self.owner)
            .await
            .expect("audit")
    }
}

pub fn token_from_url(url: &str) -> String {
    url::Url::parse(url)
        .expect("url")
        .query_pairs()
        .find(|(k, _)| k == "token")
        .expect("token param")
        .1
        .into_owned()
}

/// A signed-in browser: session cookie, CSRF token and current workspace.
#[derive(Debug, Clone)]
pub struct Client {
    pub cookie: String,
    pub csrf: String,
    pub workspace: Option<WorkspaceId>,
}

impl Client {
    pub fn from_login(res: &Resp) -> Self {
        let cookie = res
            .headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with("onhold_session="))
            .and_then(|v| v.split(';').next())
            .expect("session cookie")
            .to_owned();
        let csrf = res
            .body
            .pointer("/csrf_token")
            .or_else(|| res.body.pointer("/session/csrf_token"))
            .and_then(Value::as_str)
            .expect("csrf token")
            .to_owned();
        Self {
            cookie,
            csrf,
            workspace: None,
        }
    }

    pub fn in_workspace(mut self, ws: WorkspaceId) -> Self {
        self.workspace = Some(ws);
        self
    }
}

pub struct Req<'a> {
    app: &'a TestApp,
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
}

impl Req<'_> {
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(
            header::HeaderName::from_bytes(name.as_bytes()).expect("name"),
            value.parse().expect("value"),
        );
        self
    }

    pub fn json(mut self, body: Value) -> Self {
        self.headers.insert(
            header::CONTENT_TYPE,
            "application/json".parse().expect("ct"),
        );
        self.body = Some(serde_json::to_vec(&body).expect("json"));
        self
    }

    /// A raw body, such as a file upload.
    pub fn bytes(mut self, content_type: &str, body: Vec<u8>) -> Self {
        self.headers.insert(
            header::CONTENT_TYPE,
            content_type.parse().expect("ct"),
        );
        self.body = Some(body);
        self
    }

    /// Session cookie, CSRF token and workspace header of `client`.
    pub fn signed_in(self, client: &Client) -> Self {
        let mut req = self
            .header("cookie", &client.cookie)
            .header("x-csrf-token", &client.csrf);
        if let Some(ws) = client.workspace {
            req = req.header("x-workspace-id", &ws.to_string());
        }
        req
    }

    pub fn bearer(self, key: &str) -> Self {
        self.header("authorization", &format!("Bearer {key}"))
    }

    pub fn workspace(self, ws: WorkspaceId) -> Self {
        self.header("x-workspace-id", &ws.to_string())
    }

    pub async fn send(self) -> Resp {
        let mut builder = Request::builder().method(self.method).uri(&self.path);
        for (k, v) in &self.headers {
            builder = builder.header(k, v);
        }
        let req = builder
            .body(self.body.map(Body::from).unwrap_or_else(Body::empty))
            .expect("request");
        let res = self
            .app
            .router
            .clone()
            .oneshot(req)
            .await
            .expect("response");
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        Resp {
            status,
            headers,
            body,
        }
    }
}

#[derive(Debug)]
pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
}

impl Resp {
    pub fn ok(self) -> Self {
        assert!(
            self.status.is_success(),
            "expected success, got {}: {}",
            self.status,
            self.body
        );
        self
    }

    pub fn code(&self) -> &str {
        self.body
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    #[track_caller]
    pub fn expect(&self, status: StatusCode, code: &str) {
        assert_eq!(
            (self.status, self.code()),
            (status, code),
            "body: {}",
            self.body
        );
    }

    pub fn id(&self, pointer: &str) -> Uuid {
        Uuid::parse_str(
            self.body
                .pointer(pointer)
                .and_then(Value::as_str)
                .expect("id"),
        )
        .expect("uuid")
    }
}

/// The TOTP code for `secret` (base32, as shown to the user) `offset` 30-second
/// steps from now. Each step can be used once, so tests that need several
/// codes walk through -1, 0 and 1.
pub fn totp_code(secret: &str, offset: i64) -> String {
    let raw = data_encoding::BASE32_NOPAD
        .decode(secret.as_bytes())
        .expect("base32 secret");
    let totp = totp_rs::Builder::new()
        .with_secret(raw)
        .build()
        .expect("totp");
    let now = chrono::Utc::now().timestamp() + offset * 30;
    totp.generate(u64::try_from(now).expect("time")).to_string()
}
