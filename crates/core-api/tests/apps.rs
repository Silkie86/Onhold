//! Signing in the desktop and mobile apps through the browser, their bearer
//! sessions, the per-plan switch, and cross-origin access from their web views.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{Client, TestApp};
use core_api::Config;
use serde_json::{Value, json};
use sqlx::PgPool;

struct Started {
    code: String,
    poll: String,
}

async fn start(app: &TestApp, device: &str) -> Started {
    let res = app
        .post("/api/v1/auth/app/start")
        .header("user-agent", "OnHold/0.1 (Macintosh)")
        .json(json!({ "device_name": device }))
        .send()
        .await
        .ok();
    let code = res.body["user_code"].as_str().unwrap().to_owned();
    assert!(
        code.len() == 9 && code.as_bytes()[4] == b'-',
        "code is shown as ABCD-EFGH: {code}"
    );
    assert_eq!(res.body["interval_seconds"], 3);
    // The approval link never carries the code: the person types it.
    assert_eq!(
        res.body["verification_url"],
        "http://localhost:5173/link-app"
    );
    Started {
        code,
        poll: res.body["poll_token"].as_str().unwrap().to_owned(),
    }
}

async fn poll(app: &TestApp, started: &Started) -> common::Resp {
    app.post("/api/v1/auth/app/token")
        .json(json!({ "poll_token": started.poll }))
        .send()
        .await
}

async fn decide(app: &TestApp, browser: &Client, code: &str, approve: bool) -> common::Resp {
    app.post("/api/v1/auth/app/decide")
        .signed_in(browser)
        .json(json!({ "code": code, "approve": approve }))
        .send()
        .await
}

/// Signs an app in as `browser`'s user and returns its bearer token.
async fn app_token(app: &TestApp, browser: &Client) -> String {
    let started = start(app, "Sam's MacBook").await;
    assert_eq!(
        decide(app, browser, &started.code, true).await.status,
        StatusCode::NO_CONTENT
    );
    poll(app, &started).await.ok().body["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_app_signs_in_by_approving_its_code_in_the_browser(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, browser) = app.workspace_with_owner("acme").await;
    let started = start(&app, "Sam's MacBook").await;

    // Nothing to collect until someone approves.
    poll(&app, &started)
        .await
        .expect(StatusCode::BAD_REQUEST, "authorization_pending");

    // The approval page shows which device is asking, however the code is typed.
    let typed = started.code.to_lowercase().replace('-', " ");
    let pending = app
        .get(&format!(
            "/api/v1/auth/app/lookup?code={}",
            typed.replace(' ', "%20")
        ))
        .signed_in(&browser)
        .send()
        .await
        .ok();
    assert_eq!(pending.body["device_name"], "Sam's MacBook");
    assert_eq!(pending.body["user_agent"], "OnHold/0.1 (Macintosh)");
    // Signed-out browsers cannot look codes up.
    app.get(&format!("/api/v1/auth/app/lookup?code={}", started.code))
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "unauthenticated");

    assert_eq!(
        decide(&app, &browser, &typed, true).await.status,
        StatusCode::NO_CONTENT
    );
    let session = poll(&app, &started).await.ok();
    let token = session.body["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("ohs_"));

    // The session is handed over exactly once.
    poll(&app, &started)
        .await
        .expect(StatusCode::BAD_REQUEST, "expired_token");
    // And the code cannot be approved again.
    decide(&app, &browser, &started.code, true)
        .await
        .expect(StatusCode::BAD_REQUEST, "code_not_found");

    // The app works with its bearer token alone: no cookie, no CSRF token.
    let me = app.get("/api/v1/auth/me").bearer(&token).send().await.ok();
    assert_eq!(me.body["user"]["email"], "owner@acme.test");
    assert_eq!(me.body["session"]["client"], "app");
    assert_eq!(me.body["workspaces"][0]["apps_allowed"], true);
    let created = app
        .post("/api/v1/contacts")
        .bearer(&token)
        .workspace(ws)
        .json(json!({ "name": "Priya Patel" }))
        .send()
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    // The browser sees the app among its sessions and can sign it out.
    let sessions = app
        .get("/api/v1/auth/sessions")
        .signed_in(&browser)
        .send()
        .await
        .ok();
    let listed: Vec<&Value> = sessions
        .body
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["client"] == "app")
        .collect();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["device_name"], "Sam's MacBook");
    assert_eq!(listed[0]["auth_method"], "app_approval");
    let id = listed[0]["id"].as_str().unwrap();
    let revoked = app
        .delete(&format!("/api/v1/auth/sessions/{id}"))
        .signed_in(&browser)
        .send()
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    app.get("/api/v1/auth/me")
        .bearer(&token)
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "session_expired");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn refused_expired_and_unknown_sign_ins_give_no_session(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, browser) = app.workspace_with_owner("acme").await;

    let refused = start(&app, "Not my phone").await;
    assert_eq!(
        decide(&app, &browser, &refused.code, false).await.status,
        StatusCode::NO_CONTENT
    );
    poll(&app, &refused)
        .await
        .expect(StatusCode::FORBIDDEN, "access_denied");
    poll(&app, &refused)
        .await
        .expect(StatusCode::FORBIDDEN, "access_denied");

    let late = start(&app, "Slow laptop").await;
    sqlx::query("UPDATE app_sign_ins SET expires_at = now() - interval '1 second' WHERE device_name = 'Slow laptop'")
        .execute(&app.owner)
        .await
        .unwrap();
    app.get(&format!("/api/v1/auth/app/lookup?code={}", late.code))
        .signed_in(&browser)
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "code_not_found");
    decide(&app, &browser, &late.code, true)
        .await
        .expect(StatusCode::BAD_REQUEST, "code_not_found");
    poll(&app, &late)
        .await
        .expect(StatusCode::BAD_REQUEST, "expired_token");

    // An approval that the app never collects stops being collectable.
    let abandoned = start(&app, "Abandoned").await;
    decide(&app, &browser, &abandoned.code, true).await;
    sqlx::query("UPDATE app_sign_ins SET decided_at = now() - interval '11 minutes' WHERE device_name = 'Abandoned'")
        .execute(&app.owner)
        .await
        .unwrap();
    poll(&app, &abandoned)
        .await
        .expect(StatusCode::BAD_REQUEST, "expired_token");

    for code in ["BCDF-GHJ", "nonsense", "BCDF-GHJK"] {
        decide(&app, &browser, code, true)
            .await
            .expect(StatusCode::BAD_REQUEST, "code_not_found");
    }
    app.post("/api/v1/auth/app/token")
        .json(json!({ "poll_token": "ohp_made_up" }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "expired_token");
    app.post("/api/v1/auth/app/start")
        .json(json!({ "device_name": "  " }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");

    // Only the hash of the polling secret is stored.
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_sign_ins WHERE poll_hash = convert_to($1, 'UTF8')",
    )
    .bind(&refused.poll)
    .fetch_one(&app.owner)
    .await
    .unwrap();
    assert_eq!(stored, 0);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn app_tokens_and_browser_cookies_cannot_stand_in_for_each_other(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, browser) = app.workspace_with_owner("acme").await;
    let token = app_token(&app, &browser).await;

    // An app token sent as a cookie is not a browser session…
    app.get("/api/v1/auth/me")
        .header("cookie", &format!("onhold_session={token}"))
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "session_expired");
    // …and a browser's cookie value sent as a bearer token is not an app session.
    let cookie_value = browser.cookie.trim_start_matches("onhold_session=");
    app.get("/api/v1/auth/me")
        .bearer(cookie_value)
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "invalid_api_key");
    app.get("/api/v1/auth/me")
        .bearer(&format!("ohs_{cookie_value}"))
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "session_expired");

    // Signing out from the app ends only the app's session.
    let out = app.post("/api/v1/auth/logout").bearer(&token).send().await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    app.get("/api/v1/auth/me")
        .bearer(&token)
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "session_expired");
    app.get("/api/v1/auth/me")
        .signed_in(&browser)
        .send()
        .await
        .ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn plans_can_leave_the_apps_out_while_the_website_keeps_working(pool: PgPool) {
    let config = Config {
        app_plans: Some(vec!["pro".into()]),
        ..Config::for_development()
    };
    let app = TestApp::with_config(pool, config).await;
    let (ws, browser) = app.workspace_with_owner("acme").await;
    let token = app_token(&app, &browser).await;

    let me = app.get("/api/v1/auth/me").bearer(&token).send().await.ok();
    assert_eq!(me.body["workspaces"][0]["apps_allowed"], false);
    app.get("/api/v1/contacts")
        .bearer(&token)
        .workspace(ws)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "apps_not_in_plan");
    app.get("/api/v1/contacts")
        .signed_in(&browser)
        .send()
        .await
        .ok();

    sqlx::query("UPDATE workspaces SET plan = 'pro' WHERE id = $1")
        .bind(ws.0)
        .execute(&app.owner)
        .await
        .unwrap();
    app.get("/api/v1/contacts")
        .bearer(&token)
        .workspace(ws)
        .send()
        .await
        .ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn only_the_apps_origins_may_call_the_api_cross_origin(pool: PgPool) {
    let app = TestApp::new(pool).await;
    for origin in [
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
    ] {
        let res = app
            .request(axum::http::Method::OPTIONS, "/api/v1/contacts")
            .header("origin", origin)
            .header("access-control-request-method", "PATCH")
            .header(
                "access-control-request-headers",
                "authorization, content-type, if-match, idempotency-key, x-workspace-id",
            )
            .send()
            .await;
        assert!(res.status.is_success(), "{origin}: {}", res.status);
        assert_eq!(res.headers["access-control-allow-origin"], origin);
        let allowed = res.headers["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .to_lowercase();
        for h in [
            "authorization",
            "if-match",
            "idempotency-key",
            "x-workspace-id",
        ] {
            assert!(allowed.contains(h), "{h} in {allowed}");
        }
        // Never with credentials: cookies stay first-party.
        assert!(!res.headers.contains_key("access-control-allow-credentials"));
    }

    let res = app
        .request(axum::http::Method::OPTIONS, "/api/v1/contacts")
        .header("origin", "https://evil.example")
        .header("access-control-request-method", "GET")
        .send()
        .await;
    assert!(!res.headers.contains_key("access-control-allow-origin"));

    // Responses to the apps expose the ETag they need for If-Match.
    let res = app
        .get("/healthz")
        .header("origin", "tauri://localhost")
        .send()
        .await;
    assert_eq!(
        res.headers["access-control-allow-origin"],
        "tauri://localhost"
    );
    let exposed = res.headers["access-control-expose-headers"]
        .to_str()
        .unwrap()
        .to_lowercase();
    assert!(exposed.contains("etag"), "{exposed}");
}
