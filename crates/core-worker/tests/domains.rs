//! M6 sending from a workspace's own domain, against stand-ins for Resend's
//! domain API, a DNS-over-HTTPS resolver and a DNS provider with Domain
//! Connect: adding a domain, finding its DNS provider, the signed link that
//! sets it up there, verification, email going out from the domain, and
//! domains removed by hand or because they were never verified.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core-api/tests/common/mod.rs"]
mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use common::{Client, TestApp};
use core_api::Config;
use core_api::config::SenderDomainsConfig;
use core_integrations::dns::{Doh, DomainConnect};
use core_integrations::domains::Resend;
use core_integrations::http::WebhookClient;
use core_integrations::mail::MemoryMailer;
use core_worker::{Settings, Worker};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use url::Url;

const KEY: &str =
    include_str!("../../core-integrations/tests/fixtures/domain_connect_test_key.pem");

/// What the stand-ins hold and were asked.
#[derive(Default)]
struct World {
    /// Resend's domains by id: (name, status).
    domains: HashMap<String, (String, String)>,
    created: usize,
    verify_calls: Vec<String>,
    deleted: Vec<String>,
    /// TXT records by name.
    txt: HashMap<String, Vec<String>>,
}

type Shared = Arc<Mutex<World>>;

fn domain_json(id: &str, name: &str, status: &str) -> Value {
    let record_status = if status == "verified" {
        "verified"
    } else {
        "pending"
    };
    json!({
        "id": id, "name": name, "status": status, "region": "eu-west-1",
        "created_at": "2026-10-28 09:00:00.000000+00",
        "records": [
            { "record": "DKIM", "name": "resend._domainkey", "type": "TXT", "ttl": "Auto",
              "status": record_status, "value": "p=MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDsc4Lh8x+/=" },
            { "record": "SPF", "name": "send", "type": "MX", "ttl": "Auto", "priority": 10,
              "status": record_status, "value": "feedback-smtp.eu-west-1.amazonses.com" },
            { "record": "SPF", "name": "send", "type": "TXT", "ttl": "Auto",
              "status": record_status, "value": "v=spf1 include:amazonses.com ~all" }
        ]
    })
}

async fn stand_ins() -> (Url, Shared) {
    let world: Shared = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let app = Router::new()
        // Resend.
        .route(
            "/resend/domains",
            post(|State(w): State<Shared>, body: axum::Json<Value>| async move {
                let name = body["name"].as_str().unwrap().to_owned();
                assert_eq!(body["region"], "eu-west-1");
                let mut w = w.lock().unwrap();
                if w.domains.values().any(|(n, _)| *n == name) {
                    return (
                        StatusCode::FORBIDDEN,
                        axum::Json(json!({ "statusCode": 403, "name": "validation_error",
                            "message": "This domain is already registered." })),
                    );
                }
                w.created += 1;
                let id = format!("d{}", w.created);
                w.domains.insert(id.clone(), (name.clone(), "not_started".into()));
                (StatusCode::OK, axum::Json(domain_json(&id, &name, "not_started")))
            }),
        )
        .route(
            "/resend/domains/{id}",
            get(|State(w): State<Shared>, Path(id): Path<String>| async move {
                let w = w.lock().unwrap();
                match w.domains.get(&id) {
                    Some((name, status)) => {
                        (StatusCode::OK, axum::Json(domain_json(&id, name, status)))
                    }
                    None => (
                        StatusCode::NOT_FOUND,
                        axum::Json(json!({ "statusCode": 404, "message": "Domain not found" })),
                    ),
                }
            })
            .delete(|State(w): State<Shared>, Path(id): Path<String>| async move {
                let mut w = w.lock().unwrap();
                w.domains.remove(&id);
                w.deleted.push(id.clone());
                axum::Json(json!({ "object": "domain", "id": id, "deleted": true }))
            }),
        )
        .route(
            "/resend/domains/{id}/verify",
            post(|State(w): State<Shared>, Path(id): Path<String>| async move {
                w.lock().unwrap().verify_calls.push(id.clone());
                axum::Json(json!({ "object": "domain", "id": id }))
            }),
        )
        // DNS over HTTPS.
        .route(
            "/dns-query",
            get(
                |State(w): State<Shared>, Query(q): Query<HashMap<String, String>>| async move {
                    assert_eq!(q["type"], "TXT");
                    let w = w.lock().unwrap();
                    match w.txt.get(&q["name"]) {
                        Some(values) => axum::Json(json!({
                            "Status": 0,
                            "Answer": values.iter().map(|v| json!({
                                "name": q["name"], "type": 16, "TTL": 300, "data": format!("\"{v}\"")
                            })).collect::<Vec<_>>()
                        })),
                        None => axum::Json(json!({ "Status": 3 })),
                    }
                },
            ),
        )
        // A DNS provider with Domain Connect that knows OnHold's template.
        .route(
            "/dc/v2/{zone}/settings",
            get(move |Path(zone): Path<String>| {
                let base = base.clone();
                async move {
                    assert_eq!(zone, "acme.test");
                    axum::Json(json!({
                        "providerId": "exampledns.test",
                        "providerName": "ExampleDNS",
                        "providerDisplayName": "Example DNS",
                        "urlSyncUX": format!("{base}/ux"),
                        "urlAPI": format!("{base}/api"),
                    }))
                }
            }),
        )
        .route(
            "/api/v2/domainTemplates/providers/{provider}/services/{service}",
            get(|Path((provider, service)): Path<(String, String)>| async move {
                if provider == "onhold.test" && service == "sending" {
                    StatusCode::OK
                } else {
                    StatusCode::NOT_FOUND
                }
            }),
        )
        .with_state(world.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (Url::parse(&format!("http://{addr}")).unwrap(), world)
}

struct Harness {
    app: TestApp,
    worker: Worker,
    mailer: Arc<MemoryMailer>,
    world: Shared,
    base: Url,
}

async fn harness(pool: PgPool) -> Harness {
    let (base, world) = stand_ins().await;
    let at = |path: &str| base.join(path).unwrap();
    let config = Config {
        sender_domains: Some(SenderDomainsConfig {
            resend: Resend::new("re_test".into(), "eu-west-1".into(), at("resend")).unwrap(),
            doh: Doh::new(at("dns-query")).unwrap(),
            domain_connect: Some(Arc::new(
                DomainConnect::new(
                    "onhold.test".into(),
                    "sending".into(),
                    "_dck1".into(),
                    KEY,
                    true,
                )
                .unwrap(),
            )),
        }),
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
        world,
        base,
    }
}

impl Harness {
    async fn drain(&self) {
        for _ in 0..50 {
            let published = self.worker.publish_once().await.unwrap();
            let ran = self.worker.run_jobs_once().await.unwrap();
            if published == 0 && ran == 0 {
                return;
            }
        }
        panic!("the queue did not settle");
    }

    /// Makes every domain due a check now and lets the worker do it.
    async fn check_all(&self) {
        sqlx::query(
            "UPDATE sender_domains SET next_check_at = now() - interval '1 second' WHERE next_check_at IS NOT NULL",
        )
        .execute(&self.app.owner)
        .await
        .unwrap();
        self.worker.schedule_once().await.unwrap();
        self.drain().await;
    }

    async fn domains(&self, who: &Client) -> Value {
        self.app
            .get("/api/v1/inbox/domains")
            .signed_in(who)
            .send()
            .await
            .ok()
            .body
    }

    /// The address the last email went out from, `None` for the platform's.
    fn last_from(&self) -> Option<String> {
        self.mailer.sent().last().unwrap().from_address.clone()
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn email_goes_out_from_a_verified_domain(pool: PgPool) {
    let h = harness(pool).await;
    let (ws, owner) = h.app.workspace_with_owner("owls").await;
    h.drain().await;
    let setup = h.domains(&owner).await;
    assert_eq!(setup["available"], true, "{setup}");
    assert_eq!(setup["domains"], json!([]));

    // Domains that are not the workspace's to verify are refused.
    for (domain, status, code) in [
        ("gmail.com", StatusCode::BAD_REQUEST, "invalid_input"),
        ("not a domain", StatusCode::BAD_REQUEST, "invalid_input"),
    ] {
        h.app
            .post("/api/v1/inbox/domains")
            .signed_in(&owner)
            .json(json!({ "domain": domain }))
            .send()
            .await
            .expect(status, code);
    }

    {
        let mut w = h.world.lock().unwrap();
        w.txt.insert(
            "_domainconnect.acme.test".into(),
            vec![format!("127.0.0.1:{}/dc", h.base.port().unwrap())],
        );
    }
    let added = h
        .app
        .post("/api/v1/inbox/domains")
        .signed_in(&owner)
        .json(json!({ "domain": "https://www.Acme.test/", "local_part": "Bookings" }))
        .send()
        .await
        .ok()
        .body;
    let id = added["id"].as_str().unwrap().to_owned();
    assert_eq!(added["domain"], "acme.test");
    assert_eq!(added["from_address"], "bookings@acme.test");
    assert_eq!(added["status"], "pending");
    assert_eq!(added["sending"], false);
    assert_eq!(added["records"].as_array().unwrap().len(), 3, "{added}");
    assert_eq!(added["records"][0]["name"], "resend._domainkey.acme.test");
    assert_eq!(added["records"][1]["priority"], 10);
    assert!(added["remove_after"].is_string());

    // Another workspace cannot take it.
    let (_other, other_owner) = h.app.workspace_with_owner("hawks").await;
    h.app
        .post("/api/v1/inbox/domains")
        .signed_in(&other_owner)
        .json(json!({ "domain": "acme.test" }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "domain_taken");
    // Only owners and admins manage domains.
    let agent = h.app.add_member(&owner, "ana@owls.test", "agent").await;
    h.app
        .get("/api/v1/inbox/domains")
        .signed_in(&agent)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");

    // The first check finds its DNS provider, and no DMARC policy.
    h.drain().await;
    let d = &h.domains(&owner).await["domains"][0];
    assert_eq!(d["automatic_setup"], "Example DNS", "{d}");
    assert_eq!(d["dmarc_record"]["value"], "v=DMARC1; p=none;");
    assert_eq!(d["dmarc_record"]["name"], "_dmarc.acme.test");
    assert!(d["checked_at"].is_string());

    let link = h
        .app
        .post(&format!("/api/v1/inbox/domains/{id}/automatic-setup"))
        .signed_in(&owner)
        .json(json!({}))
        .send()
        .await
        .ok()
        .body;
    let url = Url::parse(link["url"].as_str().unwrap()).unwrap();
    assert_eq!(
        url.path(),
        "/ux/v2/domainTemplates/providers/onhold.test/services/sending/apply"
    );
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(q["domain"], "acme.test");
    assert_eq!(q["host"], "");
    assert_eq!(q["mx"], "feedback-smtp.eu-west-1.amazonses.com");
    assert!(q["dkim"].starts_with("p=MIGf"));
    assert_eq!(q["groupId"], "send,dmarc");
    assert_eq!(q["state"], id);
    assert_eq!(q["key"], "_dck1");
    assert!(q["redirect_uri"].ends_with(&format!("/settings/email/domain?checked={id}")));
    assert!(!q["sig"].is_empty());

    // Not verified yet: email still goes out from the platform's address.
    h.app
        .post("/api/v1/invitations")
        .signed_in(&owner)
        .json(json!({ "email": "bo@owls.test", "role": "agent" }))
        .send()
        .await
        .ok();
    h.drain().await;
    assert_eq!(h.last_from(), None);

    // The owner adds the records (and a DMARC policy) and asks for a check.
    {
        let mut w = h.world.lock().unwrap();
        for (_, status) in w.domains.values_mut() {
            *status = "verified".into();
        }
        w.txt
            .insert("_dmarc.acme.test".into(), vec!["v=DMARC1; p=none;".into()]);
    }
    let asked = h
        .app
        .post(&format!("/api/v1/inbox/domains/{id}/check"))
        .signed_in(&owner)
        .json(json!({}))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(asked["status"], "pending");
    assert_eq!(h.world.lock().unwrap().verify_calls, vec!["d1".to_owned()]);
    h.check_all().await;
    let d = h.domains(&owner).await["domains"][0].clone();
    assert_eq!(d["status"], "verified", "{d}");
    // The first verified domain is the one sent from.
    assert_eq!(d["in_use"], true);
    assert_eq!(d["sending"], true);
    assert_eq!(d["records"][0]["found"], true);
    assert!(d["dmarc_record"].is_null());
    assert!(d["remove_after"].is_null());
    assert!(d["automatic_setup"].is_null());

    h.app
        .post("/api/v1/invitations")
        .signed_in(&owner)
        .json(json!({ "email": "cy@owls.test", "role": "agent" }))
        .send()
        .await
        .ok();
    h.drain().await;
    assert_eq!(h.last_from().as_deref(), Some("bookings@acme.test"));

    // Turned off, email goes out from the platform's address again.
    let version = d["version"].as_i64().unwrap();
    let off = h
        .app
        .patch(&format!("/api/v1/inbox/domains/{id}"))
        .signed_in(&owner)
        .header("if-match", &format!("\"{version}\""))
        .json(json!({ "in_use": false, "local_part": "hello" }))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(off["sending"], false);
    assert_eq!(off["from_address"], "hello@acme.test");
    h.app
        .post("/api/v1/invitations")
        .signed_in(&owner)
        .json(json!({ "email": "di@owls.test", "role": "agent" }))
        .send()
        .await
        .ok();
    h.drain().await;
    assert_eq!(h.last_from(), None);

    // Removed: gone here and at the provider, and free for others.
    let res = h
        .app
        .delete(&format!("/api/v1/inbox/domains/{id}"))
        .signed_in(&owner)
        .send()
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    h.drain().await;
    assert_eq!(h.world.lock().unwrap().deleted, vec!["d1".to_owned()]);
    assert_eq!(h.domains(&owner).await["domains"], json!([]));
    h.app
        .post("/api/v1/inbox/domains")
        .signed_in(&other_owner)
        .json(json!({ "domain": "acme.test" }))
        .send()
        .await
        .ok();

    let actions = h.app.audit_actions(ws).await;
    for a in [
        "sender_domain.added",
        "sender_domain.status_changed",
        "sender_domain.updated",
        "sender_domain.removed",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} in {actions:?}");
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn domains_never_verified_are_removed(pool: PgPool) {
    let h = harness(pool).await;
    let (ws, owner) = h.app.workspace_with_owner("owls").await;
    let added = h
        .app
        .post("/api/v1/inbox/domains")
        .signed_in(&owner)
        .json(json!({ "domain": "squat.test" }))
        .send()
        .await
        .ok()
        .body;
    h.drain().await;
    // No Domain Connect for this one: its records are added by hand.
    let d = &h.domains(&owner).await["domains"][0];
    assert!(d["automatic_setup"].is_null(), "{d}");
    let id = added["id"].as_str().unwrap();
    h.app
        .post(&format!("/api/v1/inbox/domains/{id}/automatic-setup"))
        .signed_in(&owner)
        .json(json!({}))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "automatic_setup_unavailable");

    sqlx::query("UPDATE sender_domains SET created_at = now() - interval '15 days'")
        .execute(&h.app.owner)
        .await
        .unwrap();
    h.check_all().await;
    assert_eq!(h.domains(&owner).await["domains"], json!([]));
    assert_eq!(h.world.lock().unwrap().deleted, vec!["d1".to_owned()]);
    assert!(
        h.app
            .audit_actions(ws)
            .await
            .iter()
            .any(|a| a == "sender_domain.removed")
    );
}
