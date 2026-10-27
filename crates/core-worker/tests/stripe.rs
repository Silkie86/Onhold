//! M5 card payments through Stripe Connect, against a stand-in for Stripe
//! that answers with recorded responses (`fixtures/stripe`): connecting the
//! workspace's account, the payer's Pay button, and Stripe's signed
//! webhooks recording the payment, a refund and account changes once each.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core-api/tests/common/mod.rs"]
mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use common::{Client, TestApp};
use core_api::Config;
use core_integrations::http::WebhookClient;
use core_integrations::mail::MemoryMailer;
use core_integrations::stripe::{StripeClient, StripeConfig};
use core_worker::{Settings, Worker};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use url::Url;

const WEBHOOK_SECRET: &str = "whsec_test_onhold";
const ACCOUNT: &str = "acct_1QOwlsTestAccount";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/stripe/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// What the stand-in for Stripe was asked.
#[derive(Default)]
struct Calls {
    checkouts: Vec<(HashMap<String, String>, HashMap<String, String>)>,
    expired: Vec<String>,
    deauthorized: Vec<String>,
}

type Shared = Arc<Mutex<Calls>>;

fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

fn headers_of(h: &HeaderMap) -> HashMap<String, String> {
    h.iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
        .collect()
}

async fn fake_stripe() -> (Url, Shared) {
    let calls: Shared = Arc::default();
    let app = Router::new()
        .route(
            "/oauth/token",
            post(|body: String| async move {
                let f = form(&body);
                if f.get("code").map(String::as_str) != Some("ac_test_owls") {
                    return (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({ "error": "invalid_grant",
                            "error_description": "Authorization code does not exist" })),
                    );
                }
                (StatusCode::OK, axum::Json(fixture("oauth_token")))
            }),
        )
        .route(
            "/oauth/deauthorize",
            post(|State(calls): State<Shared>, body: String| async move {
                let f = form(&body);
                calls
                    .lock()
                    .unwrap()
                    .deauthorized
                    .push(f["stripe_user_id"].clone());
                axum::Json(json!({ "stripe_user_id": f["stripe_user_id"] }))
            }),
        )
        .route(
            "/v1/accounts/{id}",
            get(|Path(id): Path<String>| async move {
                assert_eq!(id, ACCOUNT);
                axum::Json(fixture("account"))
            }),
        )
        .route(
            "/v1/checkout/sessions",
            post(
                |State(calls): State<Shared>, headers: HeaderMap, body: String| async move {
                    let f = form(&body);
                    let mut session = fixture("checkout_session");
                    let n = {
                        let mut c = calls.lock().unwrap();
                        c.checkouts.push((headers_of(&headers), f.clone()));
                        c.checkouts.len()
                    };
                    let id = format!("cs_test_a1Owls{n}");
                    session["id"] = json!(id);
                    session["url"] = json!(format!("https://checkout.stripe.com/c/pay/{id}"));
                    session["amount_total"] = json!(
                        f["line_items[0][price_data][unit_amount]"]
                            .parse::<i64>()
                            .unwrap()
                    );
                    axum::Json(session)
                },
            ),
        )
        .route(
            "/v1/checkout/sessions/{id}/expire",
            post(
                |State(calls): State<Shared>, Path(id): Path<String>| async move {
                    calls.lock().unwrap().expired.push(id.clone());
                    axum::Json(
                        json!({ "id": id, "object": "checkout.session", "status": "expired" }),
                    )
                },
            ),
        )
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (Url::parse(&format!("http://{addr}")).unwrap(), calls)
}

struct Harness {
    app: TestApp,
    worker: Worker,
    calls: Shared,
}

async fn harness(pool: PgPool) -> Harness {
    let (base, calls) = fake_stripe().await;
    let stripe = StripeClient::new(StripeConfig {
        api_base: base.clone(),
        connect_base: base,
        application_fee_bps: 150,
        ..StripeConfig::new(
            "sk_test_platform".into(),
            "ca_OnHoldPlatform".into(),
            WEBHOOK_SECRET.into(),
        )
    })
    .unwrap();
    let config = Config {
        stripe: Some(stripe),
        ..Config::for_development()
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
    Harness { app, worker, calls }
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

    async fn post(&self, who: &Client, path: &str, body: Value) -> common::Resp {
        self.app.post(path).signed_in(who).json(body).send().await
    }

    async fn get(&self, who: &Client, path: &str) -> Value {
        self.app.get(path).signed_in(who).send().await.ok().body
    }

    /// Sends an event as Stripe would, signed now.
    async fn webhook(&self, event: &Value) -> common::Resp {
        let body = serde_json::to_vec(event).unwrap();
        let signature =
            core_engines::webhooks::sign(WEBHOOK_SECRET, chrono::Utc::now().timestamp(), &body);
        self.app
            .post("/api/v1/webhooks/stripe")
            .header("stripe-signature", &signature)
            .bytes("application/json", body)
            .send()
            .await
    }

    async fn processed(&self, event_id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT outcome FROM stripe_events WHERE id = $1")
            .bind(event_id)
            .fetch_optional(&self.app.owner)
            .await
            .unwrap()
            .flatten()
    }
}

fn query(url: &str) -> HashMap<String, String> {
    Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn invoices_are_paid_and_refunded_through_stripe(pool: PgPool) {
    let h = harness(pool).await;
    let (ws, owner) = h.app.workspace_with_owner("owls").await;

    // Nothing connected yet: the server takes cards, this workspace does not.
    let status = h.get(&owner, "/api/v1/finance/stripe").await;
    assert_eq!(status["available"], true, "{status}");
    assert_eq!(status["connected"], false, "{status}");

    let organisation = h
        .post(
            &owner,
            "/api/v1/organisations",
            json!({ "name": "The Grand Hall" }),
        )
        .await
        .ok()
        .id("/id");
    let draft = h
        .post(
            &owner,
            "/api/v1/invoices",
            json!({ "payer": { "organisation_id": organisation },
                    "line_items": [{ "description": "Performance", "unit_minor": 100_000 }] }),
        )
        .await
        .ok()
        .id("/id");
    let issued = h
        .post(
            &owner,
            &format!("/api/v1/invoices/{draft}/issue"),
            json!({}),
        )
        .await
        .ok()
        .body;
    let total = issued["total"]["minor"].as_i64().unwrap();
    let sent = h
        .post(
            &owner,
            &format!("/api/v1/invoices/{draft}/send"),
            json!({ "to": "bea@grand.test" }),
        )
        .await
        .ok()
        .body;
    let token = sent["link"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    let page = h
        .app
        .get(&format!("/api/v1/public/invoices/{token}"))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(page["card_payments"], false, "{page}");
    h.app
        .post(&format!("/api/v1/public/invoices/{token}/pay"))
        .json(json!({}))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "card_payments_closed");

    // Connecting: off to Stripe and back with a code and the sealed state.
    let start = h
        .post(&owner, "/api/v1/finance/stripe/connect", json!({}))
        .await
        .ok()
        .body;
    let q = query(start["url"].as_str().unwrap());
    assert_eq!(q["client_id"], "ca_OnHoldPlatform");
    assert_eq!(q["scope"], "read_write");
    assert!(q["redirect_uri"].ends_with("/settings/payments"), "{q:?}");
    let state = q["state"].clone();
    h.post(
        &owner,
        "/api/v1/finance/stripe/connect/finish",
        json!({ "code": "ac_test_owls", "state": format!("{state}x") }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "stripe_state_invalid");
    h.post(
        &owner,
        "/api/v1/finance/stripe/connect/finish",
        json!({ "code": "ac_wrong", "state": state }),
    )
    .await
    .expect(StatusCode::CONFLICT, "stripe_refused");
    let connected = h
        .post(
            &owner,
            "/api/v1/finance/stripe/connect/finish",
            json!({ "code": "ac_test_owls", "state": state }),
        )
        .await
        .ok()
        .body;
    assert_eq!(connected["connected"], true, "{connected}");
    assert_eq!(connected["account_id"], ACCOUNT);
    assert_eq!(connected["charges_enabled"], true);
    assert_eq!(connected["display_name"], "Midnight Owls");
    assert_eq!(connected["default_currency"], "GBP");
    assert_eq!(connected["livemode"], false);

    // Another workspace sees none of it, and cannot take the same account.
    let (_, other) = h.app.workspace_with_owner("hall").await;
    let theirs = h.get(&other, "/api/v1/finance/stripe").await;
    assert_eq!(theirs["connected"], false, "{theirs}");
    let other_start = h
        .post(&other, "/api/v1/finance/stripe/connect", json!({}))
        .await
        .ok()
        .body;
    let other_state = query(other_start["url"].as_str().unwrap())["state"].clone();
    h.post(
        &owner,
        "/api/v1/finance/stripe/connect/finish",
        json!({ "code": "ac_test_owls", "state": other_state }),
    )
    .await
    .expect(StatusCode::BAD_REQUEST, "stripe_state_invalid");
    h.post(
        &other,
        "/api/v1/finance/stripe/connect/finish",
        json!({ "code": "ac_test_owls", "state": other_state }),
    )
    .await
    .expect(StatusCode::CONFLICT, "stripe_account_in_use");

    // The payer pays by card: one session on the client's account, for
    // what is owed, with OnHold's fee; pressing again gives the same one.
    let page = h
        .app
        .get(&format!("/api/v1/public/invoices/{token}"))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(page["card_payments"], true, "{page}");
    let pay = h
        .app
        .post(&format!("/api/v1/public/invoices/{token}/pay"))
        .json(json!({}))
        .send()
        .await
        .ok()
        .body;
    let pay_url = pay["url"].as_str().unwrap().to_owned();
    assert!(pay_url.starts_with("https://checkout.stripe.com/"), "{pay}");
    let again = h
        .app
        .post(&format!("/api/v1/public/invoices/{token}/pay"))
        .json(json!({}))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(again["url"], pay_url.as_str());
    let session_id = {
        let calls = h.calls.lock().unwrap();
        assert_eq!(calls.checkouts.len(), 1);
        let (headers, f) = &calls.checkouts[0];
        assert_eq!(headers["stripe-account"], ACCOUNT);
        assert_eq!(headers["authorization"], "Bearer sk_test_platform");
        assert!(headers.contains_key("idempotency-key"));
        assert_eq!(f["mode"], "payment");
        assert_eq!(f["line_items[0][price_data][currency]"], "gbp");
        assert_eq!(
            f["line_items[0][price_data][unit_amount]"],
            total.to_string()
        );
        assert_eq!(f["metadata[invoice_id]"], draft.to_string());
        assert_eq!(
            f["payment_intent_data[metadata][invoice_id]"],
            draft.to_string()
        );
        assert_eq!(
            f["payment_intent_data[application_fee_amount]"],
            (total * 150 / 10_000).to_string()
        );
        assert_eq!(f["customer_email"], "bea@grand.test");
        assert!(f["success_url"].contains(&token), "{f:?}");
        format!("cs_test_a1Owls{}", calls.checkouts.len())
    };

    // Stripe says it is paid. Unsigned or wrongly signed events are refused.
    let mut completed = fixture("event_checkout_session_completed");
    completed["data"]["object"]["id"] = json!(session_id);
    completed["data"]["object"]["amount_total"] = json!(total);
    completed["data"]["object"]["metadata"] = json!({ "invoice_id": draft.to_string() });
    let body = serde_json::to_vec(&completed).unwrap();
    h.app
        .post("/api/v1/webhooks/stripe")
        .bytes("application/json", body.clone())
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "stripe_signature_invalid");
    let forged = core_engines::webhooks::sign("whsec_other", chrono::Utc::now().timestamp(), &body);
    h.app
        .post("/api/v1/webhooks/stripe")
        .header("stripe-signature", &forged)
        .bytes("application/json", body.clone())
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "stripe_signature_invalid");
    let stale =
        core_engines::webhooks::sign(WEBHOOK_SECRET, chrono::Utc::now().timestamp() - 600, &body);
    h.app
        .post("/api/v1/webhooks/stripe")
        .header("stripe-signature", &stale)
        .bytes("application/json", body)
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "stripe_signature_invalid");
    // Stripe retries: twice is once.
    assert_eq!(h.webhook(&completed).await.status, StatusCode::NO_CONTENT);
    assert_eq!(h.webhook(&completed).await.status, StatusCode::NO_CONTENT);
    h.drain().await;
    let inv = h.get(&owner, &format!("/api/v1/invoices/{draft}")).await;
    assert_eq!(inv["status"], "paid", "{inv}");
    assert_eq!(inv["outstanding"]["minor"], 0);
    let payments = h.get(&owner, "/api/v1/payments").await;
    let items = payments["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{payments}");
    assert_eq!(items[0]["method"], "stripe");
    assert_eq!(items[0]["reference"], "pi_3QOwlsPaymentIntent");
    assert_eq!(items[0]["amount"]["minor"], total);
    let payment_id = items[0]["id"].as_str().unwrap().to_owned();
    assert!(
        h.processed("evt_1QOwlsCompleted")
            .await
            .unwrap()
            .contains("recorded")
    );

    // The same payment told again under another event is not recorded twice,
    // and a paid invoice offers no card payment.
    let mut succeeded = completed.clone();
    succeeded["id"] = json!("evt_1QOwlsAsyncSucceeded");
    succeeded["type"] = json!("checkout.session.async_payment_succeeded");
    assert_eq!(h.webhook(&succeeded).await.status, StatusCode::NO_CONTENT);
    h.drain().await;
    assert_eq!(
        h.processed("evt_1QOwlsAsyncSucceeded").await.as_deref(),
        Some("already recorded")
    );
    let payments = h.get(&owner, "/api/v1/payments").await;
    assert_eq!(payments["items"].as_array().unwrap().len(), 1);
    let page = h
        .app
        .get(&format!("/api/v1/public/invoices/{token}"))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(page["card_payments"], false, "{page}");

    // A partial refund in Stripe: the invoice owes that much again.
    let refunded = fixture("event_charge_refunded");
    assert_eq!(h.webhook(&refunded).await.status, StatusCode::NO_CONTENT);
    h.drain().await;
    let inv = h.get(&owner, &format!("/api/v1/invoices/{draft}")).await;
    assert_eq!(inv["outstanding"]["minor"], 20_000, "{inv}");
    assert_eq!(inv["status"], "part_paid", "{inv}");
    let payment = h
        .get(&owner, &format!("/api/v1/payments/{payment_id}"))
        .await;
    assert_eq!(payment["allocated"]["minor"], total, "{payment}");
    let refunds = h.get(&owner, "/api/v1/payments?kind=refund").await;
    let refund = refunds["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["kind"] == "refund")
        .cloned()
        .unwrap_or_else(|| panic!("{refunds}"));
    assert_eq!(refund["amount"]["minor"], 20_000);
    assert_eq!(refund["refund_of"], payment_id.as_str());
    // Stripe's later word on the same charge counts only what is new.
    let mut again = refunded.clone();
    again["id"] = json!("evt_3QOwlsRefundedAgain");
    assert_eq!(h.webhook(&again).await.status, StatusCode::NO_CONTENT);
    h.drain().await;
    assert_eq!(
        h.processed("evt_3QOwlsRefundedAgain").await.as_deref(),
        Some("already recorded")
    );

    // The owed balance can be paid by card again: a new session, for it.
    let pay = h
        .app
        .post(&format!("/api/v1/public/invoices/{token}/pay"))
        .json(json!({}))
        .send()
        .await
        .ok()
        .body;
    assert_ne!(pay["url"], pay_url.as_str(), "{pay}");
    {
        let calls = h.calls.lock().unwrap();
        assert_eq!(calls.checkouts.len(), 2);
        assert_eq!(
            calls.checkouts[1].1["line_items[0][price_data][unit_amount]"],
            "20000"
        );
    }

    // Stripe turns charges off until the client sends more details.
    assert_eq!(
        h.webhook(&fixture("event_account_updated")).await.status,
        StatusCode::NO_CONTENT
    );
    h.drain().await;
    let status = h.get(&owner, "/api/v1/finance/stripe").await;
    assert_eq!(status["charges_enabled"], false, "{status}");
    let page = h
        .app
        .get(&format!("/api/v1/public/invoices/{token}"))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(page["card_payments"], false, "{page}");

    // A test-mode event for this account is not taken as a live one, nor
    // the other way round; events for unknown accounts are dropped.
    let mut live = fixture("event_account_application_deauthorized");
    live["id"] = json!("evt_1QOwlsLiveMode");
    live["livemode"] = json!(true);
    assert_eq!(h.webhook(&live).await.status, StatusCode::NO_CONTENT);
    let mut stranger = fixture("event_account_updated");
    stranger["id"] = json!("evt_1QStranger");
    stranger["account"] = json!("acct_1QSomeoneElse");
    assert_eq!(h.webhook(&stranger).await.status, StatusCode::NO_CONTENT);
    h.drain().await;
    assert!(
        h.processed("evt_1QOwlsLiveMode")
            .await
            .unwrap()
            .contains("test-mode")
    );
    assert_eq!(h.processed("evt_1QStranger").await, None);
    let status = h.get(&owner, "/api/v1/finance/stripe").await;
    assert_eq!(status["connected"], true, "{status}");

    // The client disconnects OnHold from their Stripe dashboard.
    assert_eq!(
        h.webhook(&fixture("event_account_application_deauthorized"))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    h.drain().await;
    let status = h.get(&owner, "/api/v1/finance/stripe").await;
    assert_eq!(status["connected"], false, "{status}");
    assert!(status["disconnected_at"].is_string(), "{status}");
    let actions = h.app.audit_actions(ws).await;
    assert!(
        actions.iter().any(|a| a == "stripe.connected"),
        "{actions:?}"
    );
    assert!(
        actions.iter().any(|a| a == "stripe.disconnected"),
        "{actions:?}"
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn disconnecting_here_tells_stripe_and_stops_card_payments(pool: PgPool) {
    let h = harness(pool).await;
    let (_, owner) = h.app.workspace_with_owner("owls").await;
    let start = h
        .post(&owner, "/api/v1/finance/stripe/connect", json!({}))
        .await
        .ok()
        .body;
    let state = query(start["url"].as_str().unwrap())["state"].clone();
    h.post(
        &owner,
        "/api/v1/finance/stripe/connect/finish",
        json!({ "code": "ac_test_owls", "state": state }),
    )
    .await
    .ok();

    // A viewer cannot touch the connection.
    let viewer = h.app.add_member(&owner, "viewer@owls.test", "viewer").await;
    h.app
        .get("/api/v1/finance/stripe")
        .signed_in(&viewer)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");

    let gone = h
        .app
        .delete("/api/v1/finance/stripe")
        .signed_in(&owner)
        .send()
        .await
        .ok()
        .body;
    assert_eq!(gone["connected"], false, "{gone}");
    assert_eq!(
        h.calls.lock().unwrap().deauthorized,
        vec![ACCOUNT.to_owned()]
    );
    // Events Stripe still sends for it are kept but change nothing.
    let mut late = fixture("event_account_updated");
    late["id"] = json!("evt_1QLate");
    assert_eq!(h.webhook(&late).await.status, StatusCode::NO_CONTENT);
    h.drain().await;
    let status = h.get(&owner, "/api/v1/finance/stripe").await;
    assert_eq!(status["connected"], false, "{status}");

    // Connecting again brings it back.
    let start = h
        .post(&owner, "/api/v1/finance/stripe/connect", json!({}))
        .await
        .ok()
        .body;
    let state = query(start["url"].as_str().unwrap())["state"].clone();
    let back = h
        .post(
            &owner,
            "/api/v1/finance/stripe/connect/finish",
            json!({ "code": "ac_test_owls", "state": state }),
        )
        .await
        .ok()
        .body;
    assert_eq!(back["connected"], true, "{back}");
}
