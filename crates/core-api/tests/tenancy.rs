//! Workspaces, members, invitations, API keys, the audit log and operator tools.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{Client, PASSWORD, TestApp, totp_code};
use serde_json::{Value, json};
use sqlx::PgPool;

// Invitations ---------------------------------------------------------------------------

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_invitation_creates_the_account_and_membership(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;

    let res = app
        .post("/api/v1/invitations")
        .signed_in(&owner)
        .json(json!({ "email": "Agent@Acme.test", "role": "agent" }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    let token = app.last_invitation_token(ws).await;

    let preview = app
        .get(&format!("/api/v1/invitations/lookup?token={token}"))
        .send()
        .await
        .ok();
    assert_eq!(preview.body["workspace_name"], "acme agency");
    assert_eq!(preview.body["role"], "agent");

    let agent = app.accept_new(&token, "Agent Smith").await.in_workspace(ws);
    let me = app
        .get("/api/v1/auth/me")
        .signed_in(&agent)
        .send()
        .await
        .ok();
    assert_eq!(me.body["user"]["email_verified"], true);
    assert_eq!(me.body["workspaces"][0]["role"], "agent");

    let members = app
        .get("/api/v1/members")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(members.body.as_array().unwrap().len(), 2);

    let actions = app.audit_actions(ws).await;
    assert!(
        actions.ends_with(&[
            "invitation.created".into(),
            "member.added".into(),
            "invitation.accepted".into()
        ]),
        "{actions:?}"
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn invitation_links_never_reach_the_audit_log(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    app.post("/api/v1/invitations")
        .signed_in(&owner)
        .json(json!({ "email": "a@acme.test", "role": "agent" }))
        .send()
        .await
        .ok();
    let token = app.last_invitation_token(ws).await;
    let audit: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(a) FROM audit_log a")
        .fetch_all(&app.owner)
        .await
        .unwrap();
    assert!(!audit.iter().any(|row| row.to_string().contains(&token)));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_invitation_works_once(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    app.post("/api/v1/invitations")
        .signed_in(&owner)
        .json(json!({ "email": "a@acme.test", "role": "agent" }))
        .send()
        .await
        .ok();
    let token = app.last_invitation_token(ws).await;
    app.accept_new(&token, "A").await;

    let again = app
        .post("/api/v1/invitations/accept")
        .json(json!({ "token": token, "name": "B", "password": PASSWORD }))
        .send()
        .await;
    again.expect(StatusCode::UNPROCESSABLE_ENTITY, "invitation_invalid");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn revoked_and_expired_invitations_are_refused(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;

    let res = app
        .post("/api/v1/invitations")
        .signed_in(&owner)
        .json(json!({ "email": "a@acme.test", "role": "agent" }))
        .send()
        .await
        .ok();
    let id = res.id("/id");
    let revoked = app.last_invitation_token(ws).await;
    let res = app
        .delete(&format!("/api/v1/invitations/{id}"))
        .signed_in(&owner)
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    app.delete(&format!("/api/v1/invitations/{id}"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "invitation_closed");
    app.get(&format!("/api/v1/invitations/lookup?token={revoked}"))
        .send()
        .await
        .expect(StatusCode::UNPROCESSABLE_ENTITY, "invitation_invalid");

    app.post("/api/v1/invitations")
        .signed_in(&owner)
        .json(json!({ "email": "b@acme.test", "role": "agent" }))
        .send()
        .await
        .ok();
    let expired = app.last_invitation_token(ws).await;
    sqlx::query("UPDATE invitations SET expires_at = now() - interval '1 second' WHERE email = 'b@acme.test'").execute(&app.owner).await.unwrap();
    app.post("/api/v1/invitations/accept")
        .json(json!({ "token": expired, "name": "B", "password": PASSWORD }))
        .send()
        .await
        .expect(StatusCode::UNPROCESSABLE_ENTITY, "invitation_invalid");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn one_login_can_join_several_workspaces(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, acme_owner) = app.workspace_with_owner("acme").await;
    let (bobs, bobs_owner) = app.workspace_with_owner("bobs").await;

    app.post("/api/v1/invitations")
        .signed_in(&bobs_owner)
        .json(json!({ "email": "owner@acme.test", "role": "admin" }))
        .send()
        .await
        .ok();
    let token = app.last_invitation_token(bobs).await;

    // Signed out, an existing account must sign in first.
    app.post("/api/v1/invitations/accept")
        .json(json!({ "token": token, "name": "x", "password": PASSWORD }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "account_exists");

    let res = app
        .post("/api/v1/invitations/accept")
        .signed_in(&acme_owner)
        .json(json!({ "token": token }))
        .send()
        .await
        .ok();
    assert_eq!(res.body["workspace_id"], bobs.to_string());
    assert!(res.body["session"].is_null());

    let me = app
        .get("/api/v1/auth/me")
        .signed_in(&acme_owner)
        .send()
        .await
        .ok();
    let mut roles: Vec<(String, String)> = me.body["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["slug"].as_str().unwrap().to_owned(),
                w["role"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    roles.sort();
    assert_eq!(
        roles,
        [
            ("acme".into(), "owner".into()),
            ("bobs".into(), "admin".into())
        ]
    );
    app.get("/api/v1/members")
        .signed_in(&acme_owner.clone().in_workspace(bobs))
        .send()
        .await
        .ok();
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_invitation_is_only_for_its_own_address(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, acme_owner) = app.workspace_with_owner("acme").await;
    let (bobs, bobs_owner) = app.workspace_with_owner("bobs").await;
    app.post("/api/v1/invitations")
        .signed_in(&bobs_owner)
        .json(json!({ "email": "someone@else.test", "role": "admin" }))
        .send()
        .await
        .ok();
    let token = app.last_invitation_token(bobs).await;
    app.post("/api/v1/invitations/accept")
        .signed_in(&acme_owner)
        .json(json!({ "token": token }))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "invitation_email_mismatch");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn who_may_invite_whom(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let admin = app.add_member(&owner, "admin@acme.test", "admin").await;
    let agent = app.add_member(&owner, "agent@acme.test", "agent").await;

    let invite = |client: &Client, email: &str, role: &str| {
        app.post("/api/v1/invitations")
            .signed_in(client)
            .json(json!({ "email": email, "role": role }))
            .send()
    };
    invite(&agent, "x@acme.test", "viewer")
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
    invite(&admin, "x@acme.test", "owner")
        .await
        .expect(StatusCode::FORBIDDEN, "owner_required");
    invite(&owner, "x@acme.test", "talent")
        .await
        .expect(StatusCode::BAD_REQUEST, "role_not_assignable");
    invite(&owner, "agent@acme.test", "viewer")
        .await
        .expect(StatusCode::CONFLICT, "already_member");
    invite(&owner, "not-an-email", "viewer")
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");
    invite(&admin, "x@acme.test", "viewer").await.ok();
    invite(&owner, "y@acme.test", "owner").await.ok();
}

// Members -------------------------------------------------------------------------------

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_owner_role_is_protected(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let admin = app.add_member(&owner, "admin@acme.test", "admin").await;
    let owner_id = app.user_id("owner@acme.test").await;
    let admin_id = app.user_id("admin@acme.test").await;

    // The last owner can neither step down nor be removed.
    app.patch(&format!("/api/v1/members/{owner_id}"))
        .signed_in(&owner)
        .json(json!({ "role": "admin" }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "last_owner");
    app.delete(&format!("/api/v1/members/{owner_id}"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "last_owner");
    // Admins cannot touch owners or make new ones.
    app.delete(&format!("/api/v1/members/{owner_id}"))
        .signed_in(&admin)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "owner_required");
    app.patch(&format!("/api/v1/members/{admin_id}"))
        .signed_in(&admin)
        .json(json!({ "role": "owner" }))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "owner_required");

    // With a second owner, the first may step down.
    app.patch(&format!("/api/v1/members/{admin_id}"))
        .signed_in(&owner)
        .json(json!({ "role": "owner" }))
        .send()
        .await
        .ok();
    let res = app
        .patch(&format!("/api/v1/members/{owner_id}"))
        .signed_in(&owner)
        .json(json!({ "role": "admin" }))
        .send()
        .await
        .ok();
    assert_eq!(res.body["role"], "admin");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn member_role_changes_respect_if_match(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    app.add_member(&owner, "agent@acme.test", "agent").await;
    let id = app.user_id("agent@acme.test").await;

    let member = app
        .get(&format!("/api/v1/members/{id}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let etag = member
        .headers
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(etag, format!("\"{}\"", member.body["version"]));

    app.patch(&format!("/api/v1/members/{id}"))
        .signed_in(&owner)
        .header("if-match", &etag)
        .json(json!({ "role": "viewer" }))
        .send()
        .await
        .ok();
    // The same, now stale, version loses.
    app.patch(&format!("/api/v1/members/{id}"))
        .signed_in(&owner)
        .header("if-match", &etag)
        .json(json!({ "role": "finance" }))
        .send()
        .await
        .expect(StatusCode::PRECONDITION_FAILED, "version_conflict");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn removed_members_lose_access_immediately(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let agent = app.add_member(&owner, "agent@acme.test", "agent").await;
    let id = app.user_id("agent@acme.test").await;

    let res = app
        .delete(&format!("/api/v1/members/{id}"))
        .signed_in(&owner)
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    app.get("/api/v1/members")
        .signed_in(&agent)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "not_a_member");
    let me = app
        .get("/api/v1/auth/me")
        .signed_in(&agent)
        .send()
        .await
        .ok();
    assert_eq!(me.body["workspaces"], json!([]));

    // The row is kept (soft delete) and they can be invited back.
    let kept: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM memberships WHERE workspace_id = $1 AND deleted_at IS NOT NULL",
    )
    .bind(ws.0)
    .fetch_one(&app.owner)
    .await
    .unwrap();
    assert_eq!(kept, 1);
    app.post("/api/v1/invitations")
        .signed_in(&owner)
        .json(json!({ "email": "agent@acme.test", "role": "viewer" }))
        .send()
        .await
        .ok();
    let token = app.last_invitation_token(ws).await;
    app.post("/api/v1/invitations/accept")
        .signed_in(&agent)
        .json(json!({ "token": token }))
        .send()
        .await
        .ok();
    let me = app
        .get("/api/v1/auth/me")
        .signed_in(&agent)
        .send()
        .await
        .ok();
    assert_eq!(me.body["workspaces"][0]["role"], "viewer");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn read_only_roles_cannot_change_anything(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let viewer = app.add_member(&owner, "viewer@acme.test", "viewer").await;
    let owner_id = app.user_id("owner@acme.test").await;

    app.get("/api/v1/members")
        .signed_in(&viewer)
        .send()
        .await
        .ok();
    app.patch("/api/v1/workspace")
        .signed_in(&viewer)
        .json(json!({ "name": "Mine now" }))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
    app.delete(&format!("/api/v1/members/{owner_id}"))
        .signed_in(&viewer)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
    app.get("/api/v1/audit")
        .signed_in(&viewer)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
    app.get("/api/v1/api-keys")
        .signed_in(&viewer)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");

    let roles = app
        .get("/api/v1/workspace/roles")
        .signed_in(&viewer)
        .send()
        .await
        .ok();
    assert_eq!(
        roles.body["mine"],
        json!(["workspace:read", "member:read", "thing:read", "event:read"])
    );
    app.post("/api/v1/things")
        .signed_in(&viewer)
        .json(json!({ "name": "Mine" }))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
    app.put("/api/v1/automations")
        .signed_in(&viewer)
        .json(json!({ "paused": true }))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
    app.get("/api/v1/jobs")
        .signed_in(&viewer)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
}

// Workspace settings --------------------------------------------------------------------

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn workspace_settings_update_with_versions(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;

    let ws = app
        .get("/api/v1/workspace")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(ws.body["default_currency"], "GBP");
    assert_eq!(ws.body["locale"], "en-GB");
    let version = ws.body["version"].as_i64().unwrap();

    let res = app.patch("/api/v1/workspace").signed_in(&owner).header("if-match", &format!("\"{version}\""))
        .json(json!({ "name": "Acme Talent", "default_currency": "EUR", "enabled_modules": ["crm", "bookings"] })).send().await.ok();
    assert_eq!(res.body["name"], "Acme Talent");
    assert_eq!(res.body["version"], version + 1);
    assert_eq!(
        res.headers.get("etag").unwrap(),
        &format!("\"{}\"", version + 1)
    );

    app.patch("/api/v1/workspace")
        .signed_in(&owner)
        .header("if-match", &format!("\"{version}\""))
        .json(json!({ "name": "Lost update" }))
        .send()
        .await
        .expect(StatusCode::PRECONDITION_FAILED, "version_conflict");
    app.patch("/api/v1/workspace")
        .signed_in(&owner)
        .header("if-match", "W/nonsense")
        .json(json!({ "name": "x" }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_if_match");

    for bad in [
        json!({ "default_currency": "XXX" }),
        json!({ "country": "ZZ" }),
        json!({ "default_timezone": "Mars/Base" }),
        json!({ "enabled_modules": ["rocketry"] }),
        json!({ "branding": "red" }),
        json!({ "name": "  " }),
    ] {
        let res = app
            .patch("/api/v1/workspace")
            .signed_in(&owner)
            .json(bad.clone())
            .send()
            .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{bad} → {}", res.body);
    }

    let actions = app.audit_actions(owner.workspace.unwrap()).await;
    assert_eq!(
        actions.iter().filter(|a| *a == "workspace.updated").count(),
        1
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn billing_details_need_billing_permissions(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let admin = app.add_member(&owner, "admin@acme.test", "admin").await;
    let finance = app.add_member(&owner, "finance@acme.test", "finance").await;

    app.patch("/api/v1/workspace")
        .signed_in(&owner)
        .json(json!({ "billing_contact": { "email": "accounts@acme.test" } }))
        .send()
        .await
        .ok();
    app.patch("/api/v1/workspace")
        .signed_in(&admin)
        .json(json!({ "billing_contact": { "email": "me@evil.test" } }))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");

    let seen_by_admin = app
        .get("/api/v1/workspace")
        .signed_in(&admin)
        .send()
        .await
        .ok();
    assert!(seen_by_admin.body["billing_contact"].is_null());
    let seen_by_finance = app
        .get("/api/v1/workspace")
        .signed_in(&finance)
        .send()
        .await
        .ok();
    assert_eq!(
        seen_by_finance.body["billing_contact"]["email"],
        "accounts@acme.test"
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn closing_a_workspace_locks_everyone_out(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let admin = app.add_member(&owner, "admin@acme.test", "admin").await;

    app.post("/api/v1/workspace/close")
        .signed_in(&admin)
        .json(json!({ "confirm_slug": "acme" }))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
    app.post("/api/v1/workspace/close")
        .signed_in(&owner)
        .json(json!({ "confirm_slug": "acne" }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "confirmation_mismatch");
    let res = app
        .post("/api/v1/workspace/close")
        .signed_in(&owner)
        .json(json!({ "confirm_slug": "acme" }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);

    app.get("/api/v1/workspace")
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "workspace_closed");
    app.get("/api/v1/members")
        .signed_in(&admin)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "workspace_closed");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn session_requests_must_name_a_workspace(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let mut no_ws = owner.clone();
    no_ws.workspace = None;
    app.get("/api/v1/members")
        .signed_in(&no_ws)
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "workspace_required");
    app.get("/api/v1/members")
        .signed_in(&no_ws)
        .header("x-workspace-id", "acme")
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_workspace_header");
}

// API keys ------------------------------------------------------------------------------

async fn create_key(app: &TestApp, client: &Client, permissions: Value) -> String {
    let res = app
        .post("/api/v1/api-keys")
        .signed_in(client)
        .json(json!({ "name": "script", "permissions": permissions }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    res.body["secret"].as_str().unwrap().to_owned()
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn api_keys_act_within_their_scopes(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    let key = create_key(&app, &owner, json!(["member:read", "member:invite"])).await;
    assert!(key.starts_with("ohk_"));

    // No workspace header needed: the key implies it.
    let members = app.get("/api/v1/members").bearer(&key).send().await.ok();
    assert_eq!(members.body.as_array().unwrap().len(), 1);
    // No CSRF token needed either: bearer credentials are not sent by browsers on their own.
    let res = app
        .post("/api/v1/invitations")
        .bearer(&key)
        .json(json!({ "email": "a@acme.test", "role": "agent" }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    // Outside its scopes it is refused.
    app.patch("/api/v1/workspace")
        .bearer(&key)
        .json(json!({ "name": "x" }))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
    // Invitations made by a key cannot be for owners.
    app.post("/api/v1/invitations")
        .bearer(&key)
        .json(json!({ "email": "b@acme.test", "role": "owner" }))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "owner_required");

    let row: (Option<uuid::Uuid>, Option<uuid::Uuid>) = sqlx::query_as(
        "SELECT actor_user_id, actor_api_key_id FROM audit_log WHERE workspace_id = $1 AND action = 'invitation.created' ORDER BY id DESC LIMIT 1",
    ).bind(ws.0).fetch_one(&app.owner).await.unwrap();
    assert!(row.0.is_none() && row.1.is_some());

    let list = app
        .get("/api/v1/api-keys")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert!(list.body[0]["last_used_at"].is_string());
    assert!(list.body[0].get("secret").is_none());
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn api_keys_cannot_exceed_their_creator(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let admin = app.add_member(&owner, "admin@acme.test", "admin").await;
    let res = app
        .post("/api/v1/api-keys")
        .signed_in(&admin)
        .json(json!({ "name": "x", "permissions": ["billing:read"] }))
        .send()
        .await;
    res.expect(StatusCode::FORBIDDEN, "permission_denied");
    let res = app
        .post("/api/v1/api-keys")
        .signed_in(&admin)
        .json(json!({ "name": "x", "permissions": [] }))
        .send()
        .await;
    res.expect(StatusCode::BAD_REQUEST, "invalid_input");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn revoked_expired_and_forged_keys_are_refused(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let (other, _) = app.workspace_with_owner("bobs").await;
    let key = create_key(&app, &owner, json!(["member:read"])).await;
    let id = key.split('_').nth(1).unwrap().to_owned();
    let id = uuid::Uuid::parse_str(&id).unwrap();

    app.get("/api/v1/members")
        .bearer(&key)
        .workspace(other)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "workspace_mismatch");
    let forged = format!("{}x", &key[..key.len() - 1]);
    app.get("/api/v1/members")
        .bearer(&forged)
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "invalid_api_key");
    app.get("/api/v1/members")
        .bearer("ohk_garbage")
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "invalid_api_key");

    let expiring = create_key(&app, &owner, json!(["member:read"])).await;
    sqlx::query("UPDATE api_keys SET expires_at = now() - interval '1 second' WHERE id <> $1")
        .bind(id)
        .execute(&app.owner)
        .await
        .unwrap();
    app.get("/api/v1/members")
        .bearer(&expiring)
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "invalid_api_key");

    let res = app
        .delete(&format!("/api/v1/api-keys/{id}"))
        .signed_in(&owner)
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    app.get("/api/v1/members")
        .bearer(&key)
        .send()
        .await
        .expect(StatusCode::UNAUTHORIZED, "invalid_api_key");
    app.delete(&format!("/api/v1/api-keys/{id}"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "already_revoked");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn api_key_secrets_are_stored_hashed(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    let key = create_key(&app, &owner, json!(["member:read"])).await;
    let secret = key.rsplit('_').next().unwrap();
    let stored: Vec<u8> = sqlx::query_scalar("SELECT secret_hash FROM api_keys")
        .fetch_one(&app.owner)
        .await
        .unwrap();
    assert_eq!(stored.len(), 32);
    assert_ne!(stored.as_slice(), secret.as_bytes());
}

// Audit log -----------------------------------------------------------------------------

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_audit_log_pages_newest_first(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    for i in 0..5 {
        app.post("/api/v1/invitations")
            .signed_in(&owner)
            .json(json!({ "email": format!("p{i}@acme.test"), "role": "viewer" }))
            .send()
            .await
            .ok();
    }

    let mut seen = Vec::new();
    let mut url = "/api/v1/audit?limit=3".to_owned();
    loop {
        let page = app.get(&url).signed_in(&owner).send().await.ok();
        seen.extend(
            page.body["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["id"].as_str().unwrap().to_owned()),
        );
        match page.body["next_before"].as_str() {
            Some(before) => url = format!("/api/v1/audit?limit=3&before={before}"),
            None => break,
        }
    }
    let all = app.audit_actions(owner.workspace.unwrap()).await;
    assert_eq!(seen.len(), all.len());
    let mut sorted = seen.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(seen, sorted, "newest first");

    let first = app
        .get("/api/v1/audit?limit=1")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let entry = &first.body["entries"][0];
    assert_eq!(entry["action"], "invitation.created");
    assert_eq!(entry["after"]["email"], "p4@acme.test");
    assert_eq!(
        entry["actor_user_id"],
        app.user_id("owner@acme.test").await.to_string()
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_audit_export_is_spreadsheet_safe_csv(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    app.post("/api/v1/auth/logout")
        .signed_in(&owner)
        .send()
        .await;
    let owner = Client::from_login(&app.login("owner@acme.test", PASSWORD).await.ok())
        .in_workspace(owner.workspace.unwrap());
    app.request(axum::http::Method::PATCH, "/api/v1/workspace")
        .signed_in(&owner)
        .header("user-agent", "=HYPERLINK(\"http://evil\")")
        .json(json!({ "name": "Acme" }))
        .send()
        .await
        .ok();

    let res = app
        .get("/api/v1/audit/export")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(
        res.headers.get("content-type").unwrap(),
        "text/csv; charset=utf-8"
    );
    assert!(
        res.headers
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    let csv = res.body.as_str().unwrap();
    assert!(csv.starts_with("at,id,action,"), "{csv}");
    assert!(csv.contains("workspace.updated"));
    assert!(csv.contains("'=HYPERLINK"), "{csv}");
    assert!(!csv.contains(",=HYPERLINK"));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn audit_rows_cannot_be_changed_by_the_app(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (ws, _) = app.workspace_with_owner("acme").await;
    let mut conn = app.state.db.acquire().await.unwrap();
    sqlx::query("SELECT set_config('app.workspace_id', $1, false)")
        .bind(ws.to_string())
        .execute(&mut *conn)
        .await
        .unwrap();
    let update = sqlx::query("UPDATE audit_log SET action = 'nothing.happened'")
        .execute(&mut *conn)
        .await;
    let delete = sqlx::query("DELETE FROM audit_log")
        .execute(&mut *conn)
        .await;
    assert!(
        update
            .unwrap_err()
            .to_string()
            .contains("permission denied")
    );
    assert!(
        delete
            .unwrap_err()
            .to_string()
            .contains("permission denied")
    );
}

// Operators -----------------------------------------------------------------------------

/// A superadmin with TOTP on, signed in with it.
async fn operator(app: &TestApp) -> Client {
    let (_, ops) = app.workspace_with_owner("ops").await;
    app.make_superadmin("owner@ops.test").await;
    let setup = app
        .post("/api/v1/auth/totp/setup")
        .signed_in(&ops)
        .send()
        .await
        .ok();
    let secret = setup.body["secret"].as_str().unwrap().to_owned();
    app.post("/api/v1/auth/totp/enable")
        .signed_in(&ops)
        .json(json!({ "code": totp_code(&secret, 0) }))
        .send()
        .await
        .ok();
    Client {
        workspace: None,
        ..ops
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn operators_need_two_factor(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;
    app.get("/api/v1/admin/workspaces")
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "superadmin_required");
    app.make_superadmin("owner@acme.test").await;
    app.get("/api/v1/admin/workspaces")
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "superadmin_requires_2fa");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn operators_create_workspaces(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let ops = operator(&app).await;
    let body = json!({ "slug": "newco", "name": "NewCo", "country": "US", "default_currency": "USD",
                       "default_timezone": "America/New_York", "owner_email": "boss@newco.test" });
    let res = app
        .post("/api/v1/admin/workspaces")
        .signed_in(&ops)
        .json(body.clone())
        .send()
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    let url = res.body["owner_invitation_url"].as_str().unwrap();
    assert!(url.starts_with("http://localhost:5173/invitations/accept?token="));
    app.post("/api/v1/admin/workspaces")
        .signed_in(&ops)
        .json(body)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "slug_taken");

    let list = app
        .get("/api/v1/admin/workspaces")
        .signed_in(&ops)
        .send()
        .await
        .ok();
    let slugs: Vec<&str> = list
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.contains(&"newco") && slugs.contains(&"ops"));

    let owner = app.accept_new(&common::token_from_url(url), "Boss").await;
    let me = app
        .get("/api/v1/auth/me")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(me.body["workspaces"][0]["role"], "owner");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn impersonation_is_time_boxed_and_audited(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (customer, _) = app.workspace_with_owner("acme").await;
    let ops = operator(&app).await;
    let in_customer = ops.clone().in_workspace(customer);

    app.get("/api/v1/members")
        .signed_in(&in_customer)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "not_a_member");
    app.post("/api/v1/admin/impersonation")
        .signed_in(&ops)
        .json(json!({ "workspace_id": customer, "reason": "" }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");
    let res = app
        .post("/api/v1/admin/impersonation")
        .signed_in(&ops)
        .json(json!({ "workspace_id": customer, "reason": "ticket 42: invitations not arriving" }))
        .send()
        .await
        .ok();
    assert!(res.body["expires_at"].is_string());

    app.get("/api/v1/members")
        .signed_in(&in_customer)
        .send()
        .await
        .ok();
    app.patch("/api/v1/workspace")
        .signed_in(&in_customer)
        .json(json!({ "name": "Fixed by support" }))
        .send()
        .await
        .ok();
    let me = app.get("/api/v1/auth/me").signed_in(&ops).send().await.ok();
    assert_eq!(
        me.body["impersonation"]["workspace_id"],
        customer.to_string()
    );

    let res = app
        .delete("/api/v1/admin/impersonation")
        .signed_in(&ops)
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    app.get("/api/v1/members")
        .signed_in(&in_customer)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "not_a_member");

    let rows: Vec<(String, Option<uuid::Uuid>, Option<Value>)> = sqlx::query_as(
        "SELECT action, impersonator_id, after FROM audit_log WHERE workspace_id = $1 AND action IN ('impersonation.started', 'workspace.updated', 'impersonation.ended') ORDER BY id",
    ).bind(customer.0).fetch_all(&app.owner).await.unwrap();
    let ops_id = app.user_id("owner@ops.test").await.0;
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        [
            "impersonation.started",
            "workspace.updated",
            "impersonation.ended"
        ]
    );
    assert!(rows.iter().all(|r| r.1 == Some(ops_id)));
    assert_eq!(
        rows[0].2.as_ref().unwrap()["reason"],
        "ticket 42: invitations not arriving"
    );

    // Expiry ends it without anyone calling DELETE.
    app.post("/api/v1/admin/impersonation")
        .signed_in(&ops)
        .json(json!({ "workspace_id": customer, "reason": "again" }))
        .send()
        .await
        .ok();
    sqlx::query("UPDATE sessions SET impersonation_expires_at = now() - interval '1 second' WHERE impersonated_workspace_id IS NOT NULL").execute(&app.owner).await.unwrap();
    app.get("/api/v1/members")
        .signed_in(&in_customer)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "not_a_member");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn ordinary_users_cannot_impersonate(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (customer, _) = app.workspace_with_owner("acme").await;
    let (_, other) = app.workspace_with_owner("bobs").await;
    app.post("/api/v1/admin/impersonation")
        .signed_in(&other)
        .json(json!({ "workspace_id": customer, "reason": "curious" }))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "superadmin_required");
}
