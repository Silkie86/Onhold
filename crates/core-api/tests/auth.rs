//! Signing in and account security.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{Client, PASSWORD, TestApp, totp_code};
use serde_json::{Value, json};
use sqlx::PgPool;

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn password_login_sets_an_http_only_session_cookie(pool: PgPool) {
    let app = TestApp::new(pool).await;
    app.workspace_with_owner("acme").await;

    let res = app.login("owner@acme.test", PASSWORD).await.ok();
    let cookie = res
        .headers
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.starts_with("onhold_session="));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    assert_eq!(res.body["mfa_required"], false);

    let client = Client::from_login(&res);
    let me = app
        .get("/api/v1/auth/me")
        .signed_in(&client)
        .send()
        .await
        .ok();
    assert_eq!(me.body["user"]["email"], "owner@acme.test");
    assert_eq!(me.body["workspaces"][0]["slug"], "acme");
    assert_eq!(me.body["workspaces"][0]["role"], "owner");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn email_is_case_insensitive(pool: PgPool) {
    let app = TestApp::new(pool).await;
    app.workspace_with_owner("acme").await;
    app.login("  Owner@ACME.test ", PASSWORD).await.ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn wrong_password_and_unknown_email_look_the_same(pool: PgPool) {
    let app = TestApp::new(pool).await;
    app.workspace_with_owner("acme").await;

    let wrong = app
        .login("owner@acme.test", "not the password at all")
        .await;
    let unknown = app.login("nobody@acme.test", PASSWORD).await;
    wrong.expect(StatusCode::UNAUTHORIZED, "invalid_credentials");
    unknown.expect(StatusCode::UNAUTHORIZED, "invalid_credentials");
    assert_eq!(wrong.body, unknown.body);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn repeated_failures_lock_the_account(pool: PgPool) {
    let app = TestApp::new(pool).await;
    app.workspace_with_owner("acme").await;

    for _ in 0..app.state.config.max_login_failures {
        app.login("owner@acme.test", "wrong wrong wrong")
            .await
            .expect(StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
    // The right password no longer works until the lockout passes.
    app.login("owner@acme.test", PASSWORD)
        .await
        .expect(StatusCode::UNAUTHORIZED, "invalid_credentials");

    sqlx::query("UPDATE users SET locked_until = now() - interval '1 second'")
        .execute(&app.owner)
        .await
        .unwrap();
    app.login("owner@acme.test", PASSWORD).await.ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn requests_without_a_session_are_unauthenticated(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let res = app.get("/api/v1/auth/me").send().await;
    res.expect(StatusCode::UNAUTHORIZED, "unauthenticated");
    let res = app
        .get("/api/v1/auth/me")
        .header("cookie", "onhold_session=forged")
        .send()
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn cookie_writes_need_the_csrf_token(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;

    let res = app
        .post("/api/v1/invitations")
        .header("cookie", &owner.cookie)
        .workspace(owner.workspace.unwrap())
        .json(json!({ "email": "a@acme.test", "role": "agent" }))
        .send()
        .await;
    res.expect(StatusCode::FORBIDDEN, "csrf_failed");

    let res = app
        .post("/api/v1/invitations")
        .header("cookie", &owner.cookie)
        .header("x-csrf-token", "guess")
        .workspace(owner.workspace.unwrap())
        .json(json!({ "email": "a@acme.test", "role": "agent" }))
        .send()
        .await;
    res.expect(StatusCode::FORBIDDEN, "csrf_failed");

    // Reads do not need it.
    app.get("/api/v1/members")
        .header("cookie", &owner.cookie)
        .workspace(owner.workspace.unwrap())
        .send()
        .await
        .ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn logout_revokes_the_session(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;

    let res = app
        .post("/api/v1/auth/logout")
        .signed_in(&owner)
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    let cleared = res.headers.get("set-cookie").unwrap().to_str().unwrap();
    assert!(cleared.contains("Max-Age=0"));
    app.get("/api/v1/auth/me")
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "session_expired");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn sessions_can_be_listed_and_revoked(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, first) = app.workspace_with_owner("acme").await;
    let second = Client::from_login(&app.login("owner@acme.test", PASSWORD).await.ok());

    let list = app
        .get("/api/v1/auth/sessions")
        .signed_in(&first)
        .send()
        .await
        .ok();
    let sessions = list.body.as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    let other = sessions.iter().find(|s| s["current"] == false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let res = app
        .delete(&format!("/api/v1/auth/sessions/{other}"))
        .signed_in(&first)
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    app.get("/api/v1/auth/me")
        .signed_in(&second)
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "session_expired");
    app.get("/api/v1/auth/me")
        .signed_in(&first)
        .send()
        .await
        .ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_user_cannot_revoke_someone_elses_session(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, alice) = app.workspace_with_owner("acme").await;
    let (_, bob) = app.workspace_with_owner("bobs").await;

    let bob_session = app
        .get("/api/v1/auth/sessions")
        .signed_in(&bob)
        .send()
        .await
        .ok()
        .body[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    app.delete(&format!("/api/v1/auth/sessions/{bob_session}"))
        .signed_in(&alice)
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
    app.get("/api/v1/auth/me").signed_in(&bob).send().await.ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn changing_the_password_signs_out_other_sessions(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, first) = app.workspace_with_owner("acme").await;
    let second = Client::from_login(&app.login("owner@acme.test", PASSWORD).await.ok());

    let res = app.post("/api/v1/auth/password").signed_in(&first)
        .json(json!({ "current_password": "wrong one entirely", "new_password": "another fine passphrase" })).send().await;
    res.expect(StatusCode::BAD_REQUEST, "invalid_credentials");

    let res = app
        .post("/api/v1/auth/password")
        .signed_in(&first)
        .json(json!({ "current_password": PASSWORD, "new_password": "short" }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);

    let res = app
        .post("/api/v1/auth/password")
        .signed_in(&first)
        .json(json!({ "current_password": PASSWORD, "new_password": "another fine passphrase" }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);

    app.get("/api/v1/auth/me")
        .signed_in(&second)
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "session_expired");
    app.get("/api/v1/auth/me")
        .signed_in(&first)
        .send()
        .await
        .ok();
    app.login("owner@acme.test", PASSWORD)
        .await
        .expect(StatusCode::UNAUTHORIZED, "invalid_credentials");
    app.login("owner@acme.test", "another fine passphrase")
        .await
        .ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn magic_links_sign_in_once(pool: PgPool) {
    let app = TestApp::new(pool).await;
    app.workspace_with_owner("acme").await;

    let res = app
        .post("/api/v1/auth/magic-link")
        .json(json!({ "email": "owner@acme.test" }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    let token = app
        .last_magic_link_token("owner@acme.test")
        .await
        .expect("link emitted");

    let res = app
        .post("/api/v1/auth/magic-link/consume")
        .json(json!({ "token": token }))
        .send()
        .await
        .ok();
    let client = Client::from_login(&res);
    app.get("/api/v1/auth/me")
        .signed_in(&client)
        .send()
        .await
        .ok();

    app.post("/api/v1/auth/magic-link/consume")
        .json(json!({ "token": token }))
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "link_invalid");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn magic_links_do_not_reveal_which_emails_exist(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let res = app
        .post("/api/v1/auth/magic-link")
        .json(json!({ "email": "nobody@nowhere.test" }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    assert!(
        app.last_magic_link_token("nobody@nowhere.test")
            .await
            .is_none()
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn expired_magic_links_are_refused(pool: PgPool) {
    let app = TestApp::new(pool).await;
    app.workspace_with_owner("acme").await;
    app.post("/api/v1/auth/magic-link")
        .json(json!({ "email": "owner@acme.test" }))
        .send()
        .await;
    let token = app.last_magic_link_token("owner@acme.test").await.unwrap();
    sqlx::query("UPDATE magic_links SET expires_at = now() - interval '1 second'")
        .execute(&app.owner)
        .await
        .unwrap();
    app.post("/api/v1/auth/magic-link/consume")
        .json(json!({ "token": token }))
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "link_invalid");
}

/// Turns TOTP on for `client` and returns the secret and recovery codes.
async fn enable_totp(app: &TestApp, client: &Client) -> (String, Vec<String>) {
    let setup = app
        .post("/api/v1/auth/totp/setup")
        .signed_in(client)
        .send()
        .await
        .ok();
    let secret = setup.body["secret"].as_str().unwrap().to_owned();
    assert!(
        setup.body["otpauth_url"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/")
    );

    app.post("/api/v1/auth/totp/enable")
        .signed_in(client)
        .json(json!({ "code": "000000" }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_code");
    let res = app
        .post("/api/v1/auth/totp/enable")
        .signed_in(client)
        .json(json!({ "code": totp_code(&secret, -1) }))
        .send()
        .await
        .ok();
    let codes = res.body["recovery_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(codes.len(), 10);
    (secret, codes)
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn totp_gates_new_sessions_until_a_code_is_given(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let (secret, _) = enable_totp(&app, &owner).await;

    let res = app.login("owner@acme.test", PASSWORD).await.ok();
    assert_eq!(res.body["mfa_required"], true);
    let pending = Client::from_login(&res).in_workspace(owner.workspace.unwrap());

    // A pending session can see who it is, but nothing in any workspace.
    let me = app
        .get("/api/v1/auth/me")
        .signed_in(&pending)
        .send()
        .await
        .ok();
    assert_eq!(me.body["session"]["mfa_verified"], false);
    assert_eq!(me.body["workspaces"], json!([]));
    app.get("/api/v1/members")
        .signed_in(&pending)
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "mfa_required");

    app.post("/api/v1/auth/mfa/verify")
        .signed_in(&pending)
        .json(json!({ "code": "123456" }))
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "invalid_code");
    let res = app
        .post("/api/v1/auth/mfa/verify")
        .signed_in(&pending)
        .json(json!({ "code": totp_code(&secret, 0) }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    app.get("/api/v1/members")
        .signed_in(&pending)
        .send()
        .await
        .ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_totp_code_cannot_be_replayed(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let (secret, _) = enable_totp(&app, &owner).await;
    let code = totp_code(&secret, 0);

    let first = Client::from_login(&app.login("owner@acme.test", PASSWORD).await.ok());
    let res = app
        .post("/api/v1/auth/mfa/verify")
        .signed_in(&first)
        .json(json!({ "code": code }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);

    let second = Client::from_login(&app.login("owner@acme.test", PASSWORD).await.ok());
    app.post("/api/v1/auth/mfa/verify")
        .signed_in(&second)
        .json(json!({ "code": code }))
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "invalid_code");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn recovery_codes_work_once(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let (_, codes) = enable_totp(&app, &owner).await;

    let first = Client::from_login(&app.login("owner@acme.test", PASSWORD).await.ok());
    // Typed loosely: upper case, no dashes.
    let loose = codes[0].replace('-', "").to_uppercase();
    let res = app
        .post("/api/v1/auth/mfa/verify")
        .signed_in(&first)
        .json(json!({ "recovery_code": loose }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);

    let second = Client::from_login(&app.login("owner@acme.test", PASSWORD).await.ok());
    app.post("/api/v1/auth/mfa/verify")
        .signed_in(&second)
        .json(json!({ "recovery_code": codes[0] }))
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "invalid_code");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn too_many_wrong_codes_kill_the_pending_session(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let (secret, _) = enable_totp(&app, &owner).await;

    let pending = Client::from_login(&app.login("owner@acme.test", PASSWORD).await.ok());
    let max = app.state.config.max_mfa_failures;
    for attempt in 1..=max {
        let res = app
            .post("/api/v1/auth/mfa/verify")
            .signed_in(&pending)
            .json(json!({ "code": "000000" }))
            .send()
            .await;
        let expected = if attempt < max {
            "invalid_code"
        } else {
            "session_expired"
        };
        res.expect(StatusCode::UNAUTHORIZED, expected);
    }
    let res = app
        .post("/api/v1/auth/mfa/verify")
        .signed_in(&pending)
        .json(json!({ "code": totp_code(&secret, 0) }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn wrong_codes_lock_the_account_however_often_the_password_is_given(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let (secret, _) = enable_totp(&app, &owner).await;
    let wrong = || {
        let code = (totp_code(&secret, 0).parse::<u32>().unwrap() + 500_000) % 1_000_000;
        format!("{code:06}")
    };
    // Opened before the guessing starts, to try the right code afterwards.
    let spare = Client::from_login(&app.login("owner@acme.test", PASSWORD).await.ok());

    let mut guesses = 0;
    while guesses < app.state.config.max_login_failures {
        // Signing in again with the right password gives a fresh session,
        // but not a fresh allowance of guesses.
        let pending = Client::from_login(&app.login("owner@acme.test", PASSWORD).await.ok());
        for _ in 0..app.state.config.max_mfa_failures {
            let res = app
                .post("/api/v1/auth/mfa/verify")
                .signed_in(&pending)
                .json(json!({ "code": wrong() }))
                .send()
                .await;
            assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
            guesses += 1;
        }
    }
    app.login("owner@acme.test", PASSWORD)
        .await
        .expect(StatusCode::UNAUTHORIZED, "invalid_credentials");
    app.post("/api/v1/auth/mfa/verify")
        .signed_in(&spare)
        .json(json!({ "code": totp_code(&secret, 0) }))
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "session_expired");

    // Once the lockout passes, a full sign-in clears the count.
    sqlx::query("UPDATE users SET locked_until = now() - interval '1 second'")
        .execute(&app.owner)
        .await
        .unwrap();
    let pending = Client::from_login(&app.login("owner@acme.test", PASSWORD).await.ok());
    let res = app
        .post("/api/v1/auth/mfa/verify")
        .signed_in(&pending)
        .json(json!({ "code": totp_code(&secret, 0) }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let count: i32 = sqlx::query_scalar("SELECT failed_login_count FROM users")
        .fetch_one(&app.owner)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn guesses_sent_all_at_once_still_stop_at_the_lockout(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let (secret, _) = enable_totp(&app, &owner).await;
    let code = (totp_code(&secret, 0).parse::<u32>().unwrap() + 500_000) % 1_000_000;
    let wrong = json!({ "code": format!("{code:06}") });
    let max = app.state.config.max_login_failures;
    let mut sessions = vec![];
    for _ in 0..max * 2 {
        sessions.push(Client::from_login(
            &app.login("owner@acme.test", PASSWORD).await.ok(),
        ));
    }
    futures_util::future::join_all(sessions.iter().map(|s| {
        app.post("/api/v1/auth/mfa/verify")
            .signed_in(s)
            .json(wrong.clone())
            .send()
    }))
    .await;
    let count: i32 = sqlx::query_scalar("SELECT failed_login_count FROM users")
        .fetch_one(&app.owner)
        .await
        .unwrap();
    assert_eq!(count, max, "no guess is judged once the account is locked");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_cookie_lasts_the_full_session_once_the_code_is_given(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let (secret, _) = enable_totp(&app, &owner).await;
    let max_age = |res: &common::Resp| -> (String, i64) {
        let cookie = res
            .headers
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap())
            .find(|v| v.starts_with("onhold_session="))
            .expect("session cookie")
            .to_owned();
        let secs = cookie
            .split(';')
            .find_map(|p| p.trim().strip_prefix("Max-Age="))
            .unwrap()
            .parse()
            .unwrap();
        (cookie.split(';').next().unwrap().to_owned(), secs)
    };

    let res = app.login("owner@acme.test", PASSWORD).await.ok();
    let (token, pending_age) = max_age(&res);
    assert!(pending_age <= 600, "{pending_age}");
    let pending = Client::from_login(&res);
    let res = app
        .post("/api/v1/auth/mfa/verify")
        .signed_in(&pending)
        .json(json!({ "code": totp_code(&secret, 0) }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let (same, age) = max_age(&res);
    assert_eq!(same, token, "the same session");
    let ttl = i64::try_from(app.state.config.session_ttl.as_secs()).unwrap();
    assert!(age > ttl - 60 && age <= ttl, "{age}");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn totp_can_be_turned_off_with_a_code(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let (secret, _) = enable_totp(&app, &owner).await;

    app.post("/api/v1/auth/totp/setup")
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "totp_already_enabled");
    app.post("/api/v1/auth/totp/disable")
        .signed_in(&owner)
        .json(json!({ "code": "000000" }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_code");
    let res = app
        .post("/api/v1/auth/totp/disable")
        .signed_in(&owner)
        .json(json!({ "code": totp_code(&secret, 0) }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);

    let res = app.login("owner@acme.test", PASSWORD).await.ok();
    assert_eq!(res.body["mfa_required"], false);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn totp_secrets_are_encrypted_at_rest(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let (secret, _) = enable_totp(&app, &owner).await;

    let stored: Vec<u8> = sqlx::query_scalar("SELECT totp_secret_enc FROM users")
        .fetch_one(&app.owner)
        .await
        .unwrap();
    let raw = data_encoding::BASE32_NOPAD
        .decode(secret.as_bytes())
        .unwrap();
    assert!(!stored.windows(raw.len()).any(|w| w == raw.as_slice()));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn workspaces_can_require_staff_two_factor(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let agent = app.add_member(&owner, "agent@acme.test", "agent").await;
    sqlx::query("UPDATE workspaces SET require_staff_2fa = true WHERE id = $1")
        .bind(ws.0)
        .execute(&app.owner)
        .await
        .unwrap();

    app.get("/api/v1/members")
        .signed_in(&agent)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "mfa_enrollment_required");
    let (_, _) = enable_totp(&app, &agent).await;
    app.get("/api/v1/members")
        .signed_in(&agent)
        .send()
        .await
        .ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn session_tokens_are_stored_hashed(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let token = owner
        .cookie
        .trim_start_matches("onhold_session=")
        .to_owned();
    let hashes: Vec<Vec<u8>> = sqlx::query_scalar("SELECT token_hash FROM sessions")
        .fetch_all(&app.owner)
        .await
        .unwrap();
    assert!(!hashes.is_empty());
    assert!(
        hashes
            .iter()
            .all(|h| h.len() == 32 && h.as_slice() != token.as_bytes())
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn passwords_are_stored_as_argon2id(pool: PgPool) {
    let app = TestApp::new(pool).await;
    app.workspace_with_owner("acme").await;
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users")
        .fetch_one(&app.owner)
        .await
        .unwrap();
    assert!(hash.starts_with("$argon2id$"), "{hash}");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_oidc_provider_list_is_empty_without_configuration(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let res = app.get("/api/v1/auth/oidc/providers").send().await.ok();
    assert_eq!(res.body, Value::Array(vec![]));
    let res = app.get("/api/v1/auth/oidc/google/start").send().await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}
