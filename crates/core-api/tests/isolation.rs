//! The M1 promise: two workspaces cannot see or touch each other. Driven by
//! the OpenAPI document so that every new workspace route is covered the
//! moment it exists.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::{Method, StatusCode};
use common::{Client, TestApp};
use serde_json::{Value, json};
use sqlx::PgPool;

/// Routes that run outside any workspace, and why that is safe.
const NOT_WORKSPACE_SCOPED: &[(&str, &str)] = &[
    ("/healthz", "probe"),
    ("/readyz", "probe"),
    (
        "/api/v1/auth/",
        "acts on the signed-in user's own account, isolated per user",
    ),
    ("/api/v1/invitations/lookup", "token holders only"),
    ("/api/v1/invitations/accept", "token holders only"),
    ("/api/v1/admin/", "superadmins only"),
];

struct Operation {
    method: Method,
    path: String,
    workspace_scoped: bool,
}

fn operations() -> Vec<Operation> {
    let spec = serde_json::to_value(core_api::openapi()).unwrap();
    let mut ops = Vec::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let Ok(method) = method.to_uppercase().parse::<Method>() else {
                continue;
            };
            if !matches!(
                method,
                Method::GET | Method::POST | Method::PUT | Method::PATCH | Method::DELETE
            ) {
                continue;
            }
            let workspace_scoped = op["parameters"].as_array().is_some_and(|ps| {
                ps.iter()
                    .any(|p| p["name"] == "X-Workspace-Id" && p["in"] == "header")
            });
            ops.push(Operation {
                method,
                path: path.clone(),
                workspace_scoped,
            });
        }
    }
    ops
}

#[test]
fn every_route_is_either_workspace_scoped_or_explained() {
    let ops = operations();
    assert!(
        ops.iter().filter(|o| o.workspace_scoped).count() >= 15,
        "the spec walk found too few workspace routes"
    );
    for op in ops.iter().filter(|o| !o.workspace_scoped) {
        assert!(
            NOT_WORKSPACE_SCOPED
                .iter()
                .any(|(prefix, _)| op.path.starts_with(prefix)),
            "{} {} takes no X-Workspace-Id; scope it, or add it to NOT_WORKSPACE_SCOPED with a reason",
            op.method,
            op.path
        );
    }
}

/// Two workspaces, each with an owner, an agent, a pending invitation and an
/// API key holding every permission.
struct World {
    a: Client,
    a_key: String,
    b: Client,
    b_owner: uuid::Uuid,
    b_agent: uuid::Uuid,
    b_invitation: uuid::Uuid,
    b_key: uuid::Uuid,
    b_markers: Vec<String>,
}

async fn world(app: &TestApp) -> World {
    let all = json!([
        "workspace:read",
        "workspace:update",
        "member:read",
        "member:invite",
        "member:update",
        "member:remove",
        "api_key:read",
        "api_key:write",
        "audit:read",
        "billing:read",
        "billing:write"
    ]);
    let mut keys = Vec::new();
    let mut invitations = Vec::new();
    let mut clients = Vec::new();
    for slug in ["alpha", "bravo"] {
        let (_, owner) = app.workspace_with_owner(slug).await;
        app.add_member(&owner, &format!("agent@{slug}.test"), "agent")
            .await;
        let inv = app
            .post("/api/v1/invitations")
            .signed_in(&owner)
            .json(json!({ "email": format!("pending@{slug}.test"), "role": "viewer" }))
            .send()
            .await
            .ok();
        invitations.push(inv.id("/id"));
        let key = app
            .post("/api/v1/api-keys")
            .signed_in(&owner)
            .json(json!({ "name": format!("{slug} key"), "permissions": all }))
            .send()
            .await
            .ok();
        keys.push((
            key.id("/key/id"),
            key.body["secret"].as_str().unwrap().to_owned(),
        ));
        clients.push(owner);
    }
    let b = clients.pop().unwrap();
    let a = clients.pop().unwrap();
    World {
        b_markers: vec![
            b.workspace.unwrap().to_string(),
            "bravo".into(),
            "agent@bravo.test".into(),
            "pending@bravo.test".into(),
            keys[1].0.to_string(),
        ],
        a,
        a_key: keys[0].1.clone(),
        b_owner: app.user_id("owner@bravo.test").await.0,
        b_agent: app.user_id("agent@bravo.test").await.0,
        b_invitation: invitations[1],
        b_key: keys[1].0,
        b,
    }
}

/// A body that passes validation for each write, so a refusal is about access.
fn body_for(method: &Method, path: &str) -> Value {
    match (method.as_str(), path) {
        ("PATCH", "/api/v1/workspace") => json!({ "name": "Taken over" }),
        ("POST", "/api/v1/workspace/close") => json!({ "confirm_slug": "bravo" }),
        ("PATCH", p) if p.starts_with("/api/v1/members/") => json!({ "role": "viewer" }),
        ("POST", "/api/v1/invitations") => {
            json!({ "email": "intruder@alpha.test", "role": "admin" })
        }
        ("POST", "/api/v1/api-keys") => {
            json!({ "name": "backdoor", "permissions": ["member:read"] })
        }
        _ => json!({}),
    }
}

fn fill_path(path: &str, w: &World) -> String {
    let object = if path.contains("/members/") {
        w.b_agent
    } else if path.contains("/invitations/") {
        w.b_invitation
    } else if path.contains("/api-keys/") {
        w.b_key
    } else {
        w.b_owner
    };
    path.replace("{user_id}", &object.to_string())
        .replace("{id}", &object.to_string())
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_member_of_one_workspace_cannot_reach_another(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let w = world(&app).await;
    let a_in_b = w.a.clone().in_workspace(w.b.workspace.unwrap());

    let mut checked = 0;
    for op in operations().iter().filter(|o| o.workspace_scoped) {
        let path = fill_path(&op.path, &w);
        let body = body_for(&op.method, &op.path);

        // A's session, pointed at B.
        let res = app
            .request(op.method.clone(), &path)
            .signed_in(&a_in_b)
            .json(body.clone())
            .send()
            .await;
        assert_eq!(
            (res.status, res.code()),
            (StatusCode::FORBIDDEN, "not_a_member"),
            "{} {path}: {}",
            op.method,
            res.body
        );

        // A's all-powerful key, pointed at B.
        let res = app
            .request(op.method.clone(), &path)
            .bearer(&w.a_key)
            .workspace(w.b.workspace.unwrap())
            .json(body)
            .send()
            .await;
        assert_eq!(
            (res.status, res.code()),
            (StatusCode::FORBIDDEN, "workspace_mismatch"),
            "{} {path}: {}",
            op.method,
            res.body
        );
        checked += 1;
    }
    assert!(checked >= 15);
    assert_untouched(&app, &w).await;
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn objects_of_another_workspace_do_not_exist_from_inside_this_one(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let w = world(&app).await;

    for op in operations()
        .iter()
        .filter(|o| o.workspace_scoped && o.path.contains('{'))
    {
        let path = fill_path(&op.path, &w);
        let body = body_for(&op.method, &op.path);
        for (who, res) in [
            (
                "session",
                app.request(op.method.clone(), &path)
                    .signed_in(&w.a)
                    .json(body.clone())
                    .send()
                    .await,
            ),
            (
                "api key",
                app.request(op.method.clone(), &path)
                    .bearer(&w.a_key)
                    .json(body)
                    .send()
                    .await,
            ),
        ] {
            assert_eq!(
                (res.status, res.code()),
                (StatusCode::NOT_FOUND, "not_found"),
                "{who}: {} {path}: {}",
                op.method,
                res.body
            );
        }
    }
    assert_untouched(&app, &w).await;
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn nothing_from_another_workspace_appears_in_this_ones_reads(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let w = world(&app).await;

    for op in operations()
        .iter()
        .filter(|o| o.workspace_scoped && o.method == Method::GET && !o.path.contains('{'))
    {
        for res in [
            app.get(&op.path).signed_in(&w.a).send().await,
            app.get(&op.path).bearer(&w.a_key).send().await,
        ] {
            assert!(
                res.status.is_success(),
                "GET {}: {} {}",
                op.path,
                res.status,
                res.body
            );
            let text = res.body.to_string();
            for marker in &w.b_markers {
                assert!(
                    !text.contains(marker.as_str()),
                    "GET {} leaked {marker}",
                    op.path
                );
            }
        }
    }
}

/// B looks exactly as it did: same name, members, invitations, keys and status.
async fn assert_untouched(app: &TestApp, w: &World) {
    let ws = app
        .get("/api/v1/workspace")
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert_eq!(ws.body["name"], "bravo agency");
    assert_eq!(ws.body["status"], "trial");
    assert_eq!(ws.body["version"], 1);
    let members = app.get("/api/v1/members").signed_in(&w.b).send().await.ok();
    let mut roles: Vec<&str> = members
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    roles.sort_unstable();
    assert_eq!(roles, ["agent", "owner"]);
    let invitations = app
        .get("/api/v1/invitations")
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert!(
        invitations
            .body
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["revoked_at"].is_null())
    );
    assert!(!invitations.body.to_string().contains("intruder"));
    let keys = app
        .get("/api/v1/api-keys")
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert_eq!(keys.body.as_array().unwrap().len(), 1);
    assert!(keys.body[0]["revoked_at"].is_null());
}
