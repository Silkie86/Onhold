//! M5 documents: uploads and their sniffed type, documents made from
//! templates and printed, signing by email link until sealed with its
//! certificate, the booking pack, and past PDFs read, matched to records
//! and confirmed as documents and past invoices.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core-api/tests/common/mod.rs"]
mod common;

use std::io::Read as _;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use common::{Client, TestApp};
use core_integrations::http::WebhookClient;
use core_integrations::mail::MemoryMailer;
use core_pdf::write::Renderer as _;
use core_worker::{Settings, Worker};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

struct Harness {
    app: TestApp,
    worker: Worker,
    mailer: Arc<MemoryMailer>,
}

async fn harness(pool: PgPool) -> Harness {
    let app = TestApp::new(pool).await;
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
            concurrency: 4,
            backoff: |_| Duration::ZERO,
            fx_rates: None,
            ..Settings::default()
        },
    };
    Harness {
        app,
        worker,
        mailer,
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

    async fn post(&self, who: &Client, path: &str, body: Value) -> common::Resp {
        let res = self.app.post(path).signed_in(who).json(body).send().await;
        assert!(res.status.is_success(), "{path}: {} {}", res.status, res.body);
        res
    }

    async fn get(&self, who: &Client, path: &str) -> Value {
        self.app.get(path).signed_in(who).send().await.ok().body
    }

    async fn upload(&self, who: &Client, name: &str, content_type: &str, bytes: Vec<u8>) -> common::Resp {
        self.app
            .post(&format!("/api/v1/files?filename={name}"))
            .signed_in(who)
            .bytes(content_type, bytes)
            .send()
            .await
    }
}

/// A PDF with real text in it, as an old invoice would have.
fn old_invoice_pdf() -> Vec<u8> {
    let html = "<p>INVOICE</p>\
        <p>From: The Midnight Owls</p>\
        <p>Invoice number: OWL-2023-017</p>\
        <p>Invoice date: 14/09/2023</p>\
        <p>Bill to: The Grand Hall</p>\
        <p>bea@grand.test</p>\
        <p>Performance, 9 September 2023</p>\
        <p>Subtotal: £1,000.00</p>\
        <p>VAT 20%: £200.00</p>\
        <p>Total: £1,200.00</p>";
    core_pdf::write::PlainText.render(html, "Invoice OWL-2023-017").unwrap()
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn documents_are_made_signed_sealed_and_packed(pool: PgPool) {
    let h = harness(pool).await;
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    let talent = h.post(&owner, "/api/v1/talent", json!({ "name": "The Midnight Owls" })).await.id("/id");
    let org = h.post(&owner, "/api/v1/organisations", json!({ "name": "The Grand Hall" })).await.id("/id");
    let contact = h
        .post(
            &owner,
            "/api/v1/contacts",
            json!({ "name": "Bea Booker", "organisation_id": org, "emails": ["bea@grand.test"] }),
        )
        .await
        .id("/id");
    let booking = h
        .post(
            &owner,
            "/api/v1/bookings",
            json!({ "title": "Gala night", "talent_ids": [talent], "status": "confirmed",
                    "organisation_id": org, "contact_id": contact,
                    "fee": { "minor": 200_000, "currency": "GBP" }, "deposit_percent": 25.0,
                    "starts_at": "2031-06-01T19:00:00Z", "ends_at": "2031-06-01T23:00:00Z" }),
        )
        .await
        .id("/id");

    // The built-in templates, which a workspace may reword.
    let templates = h.get(&owner, "/api/v1/document-templates").await;
    let keys: Vec<&str> = templates
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["key"].as_str().unwrap())
        .collect();
    for k in ["document.contract", "document.confirmation", "document.advance_sheet"] {
        assert!(keys.contains(&k), "{keys:?}");
    }

    // A contract from the booking, printed straight away.
    let made = h
        .post(
            &owner,
            "/api/v1/documents/generate",
            json!({ "template_key": "document.contract", "related_entity": "booking", "related_id": booking }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let doc = made.id("/id");
    h.drain().await;
    let d = h.get(&owner, &format!("/api/v1/documents/{doc}")).await;
    assert_eq!(d["kind"], "contract", "{d}");
    assert_eq!(d["status"], "ready", "{d}");
    assert_eq!(d["versions"][0]["purpose"], "original", "{d}");
    let pdf = h
        .app
        .get(&format!("/api/v1/documents/{doc}/content"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(pdf.headers["content-type"], "application/pdf");
    assert_eq!(pdf.headers["x-content-type-options"], "nosniff");
    let raw = raw_get(&h.app, &owner, &format!("/api/v1/documents/{doc}/content")).await;
    let text = pdf_text(&raw);
    assert!(text.contains("Gala night"), "{text}");

    // Signing goes to the booking's contact by default.
    let before = h.mailer.sent().len();
    let requested = h
        .post(&owner, &format!("/api/v1/documents/{doc}/signatures"), json!({}))
        .await
        .body;
    assert_eq!(requested["signature_status"], "awaiting", "{requested}");
    h.drain().await;
    let mail = h.mailer.sent()[before].clone();
    assert_eq!(mail.to, "bea@grand.test");
    let html = mail.html.unwrap().replace("&#x2f;", "/");
    let token = html.split("/sign/").nth(1).unwrap().split('"').next().unwrap().to_owned();

    // The signer sees the document without signing in; nothing private shows.
    let public = h.app.get(&format!("/api/v1/public/sign/{token}")).send().await.ok().body;
    assert_eq!(public["signer_name"], "Bea Booker");
    assert_eq!(public["status"], "pending", "{public}");
    assert!(!public.to_string().contains(&booking.to_string()));
    let shown = h.app.get(&format!("/api/v1/public/sign/{token}/pdf")).send().await.ok();
    assert_eq!(shown.headers["content-type"], "application/pdf");

    // Signing needs the box ticked and a name.
    h.app
        .post(&format!("/api/v1/public/sign/{token}"))
        .json(json!({ "sign": true, "signed_name": "Bea Booker" }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");
    let signed = h
        .app
        .post(&format!("/api/v1/public/sign/{token}"))
        .json(json!({ "sign": true, "signed_name": "Bea Booker", "agree": true }))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(signed["status"], "signed", "{signed}");
    h.app
        .post(&format!("/api/v1/public/sign/{token}"))
        .json(json!({ "sign": true, "signed_name": "Bea Booker", "agree": true }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "signature_closed");

    // The only signer: sealed as a new version with the certificate page.
    h.drain().await;
    let d = h.get(&owner, &format!("/api/v1/documents/{doc}")).await;
    assert_eq!(d["signature_status"], "signed", "{d}");
    assert_eq!(d["current_version"], 2);
    let sealed = raw_get(&h.app, &owner, &format!("/api/v1/documents/{doc}/content")).await;
    let text = pdf_text(&sealed);
    assert!(text.contains("Bea Booker"), "{text}");
    let topics: Vec<String> = sqlx::query_scalar("SELECT topic FROM outbox")
        .fetch_all(&h.app.owner)
        .await
        .unwrap();
    assert!(topics.iter().any(|t| t == "document.signed"));

    // An upload: the type comes from the bytes, not the name.
    let up = h.upload(&owner, "rider.pdf", "application/pdf", b"not a pdf at all".to_vec()).await;
    assert_eq!(up.body["content_type"], "application/octet-stream", "{}", up.body);
    let up = h.upload(&owner, "rider.pdf", "application/octet-stream", old_invoice_pdf()).await;
    assert_eq!(up.status, StatusCode::CREATED, "{}", up.body);
    assert_eq!(up.body["content_type"], "application/pdf");
    let file = up.id("/id");
    let rider = h
        .post(
            &owner,
            "/api/v1/documents",
            json!({ "file_id": file, "kind": "rider", "title": "Tech rider",
                    "related_entity": "booking", "related_id": booking }),
        )
        .await
        .id("/id");

    // The pack: every document on the booking, zipped.
    let pack = h
        .app
        .get(&format!("/api/v1/bookings/{booking}/pack"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert_eq!(pack.headers["content-type"], "application/zip");
    let raw = raw_get(&h.app, &owner, &format!("/api/v1/bookings/{booking}/pack")).await;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(raw)).unwrap();
    assert_eq!(zip.len(), 2);
    let mut names: Vec<String> = (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().to_owned()).collect();
    names.sort();
    assert_eq!(names, ["contract-Contract-Gala-night-signed.pdf", "rider-rider.pdf"]);
    let mut first = Vec::new();
    zip.by_index(0).unwrap().read_to_end(&mut first).unwrap();
    assert!(first.starts_with(b"%PDF-"));

    // Deleting a document is a soft delete.
    h.app
        .delete(&format!("/api/v1/documents/{rider}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    h.app
        .get(&format!("/api/v1/documents/{rider}"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
}

/// The response body as bytes (the harness reads bodies as JSON or text).
async fn raw_get(app: &TestApp, who: &Client, path: &str) -> Vec<u8> {
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;
    let mut req = axum::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("cookie", &who.cookie)
        .header("x-csrf-token", &who.csrf);
    if let Some(ws) = who.workspace {
        req = req.header("x-workspace-id", ws.to_string());
    }
    let res = app
        .router
        .clone()
        .oneshot(req.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(res.status().is_success(), "{}", res.status());
    res.into_body().collect().await.unwrap().to_bytes().to_vec()
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn past_pdfs_are_read_matched_and_kept(pool: PgPool) {
    let h = harness(pool).await;
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    let talent = h.post(&owner, "/api/v1/talent", json!({ "name": "The Midnight Owls" })).await.id("/id");
    let org = h.post(&owner, "/api/v1/organisations", json!({ "name": "The Grand Hall" })).await.id("/id");
    h.post(
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Bea Booker", "organisation_id": org, "emails": ["bea@grand.test"] }),
    )
    .await;

    // Uploaded in bulk, then read in the background.
    let file = h.upload(&owner, "owl-017.pdf", "application/pdf", old_invoice_pdf()).await.ok().id("/id");
    let created = h
        .post(&owner, "/api/v1/pdf-imports", json!({ "file_ids": [file] }))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let import = Uuid::parse_str(created.body[0]["id"].as_str().unwrap()).unwrap();
    h.drain().await;

    let i = h.get(&owner, &format!("/api/v1/pdf-imports/{import}")).await;
    assert_eq!(i["status"], "ready", "{i}");
    assert_eq!(i["found"]["kinds"][0]["kind"], "invoice", "{i}");
    let best = |field: &str| -> Value {
        i["found"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["field"] == field)
            .map(|f| f["value"]["value"].clone())
            .unwrap_or(Value::Null)
    };
    assert_eq!(best("invoice_number"), "OWL-2023-017", "{i}");
    assert_eq!(best("issue_date"), "2023-09-14", "{i}");
    assert_eq!(best("total"), json!({ "minor": 120_000, "currency": "GBP" }), "{i}");
    let matched: Vec<(&str, &str)> = i["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["entity"].as_str().unwrap(), m["name"].as_str().unwrap()))
        .collect();
    assert!(matched.contains(&("organisation", "The Grand Hall")), "{matched:?}");
    assert!(matched.contains(&("talent", "The Midnight Owls")), "{matched:?}");
    assert!(matched.contains(&("contact", "Bea Booker")), "{matched:?}");

    // What was read can be fetched again for the file.
    let reading = h.get(&owner, &format!("/api/v1/files/{file}/reading")).await;
    assert!(reading["suggestions"]["fields"].as_array().is_some_and(|f| !f.is_empty()), "{reading}");

    // Confirmed as a document on the buyer, and as a paid past invoice.
    let confirmed = h
        .post(
            &owner,
            &format!("/api/v1/pdf-imports/{import}/confirm"),
            json!({ "kind": "invoice", "related_entity": "organisation", "related_id": org,
                    "document_date": "2023-09-14",
                    "invoice": { "reference": "OWL-2023-017", "payer": { "organisation_id": org },
                                 "issuer_talent_id": talent, "currency": "GBP",
                                 "issue_date": "2023-09-14", "total_minor": 120_000, "paid": true } }),
        )
        .await
        .body;
    assert_eq!(confirmed["status"], "confirmed", "{confirmed}");
    let doc = confirmed["document_id"].as_str().unwrap();
    let inv = confirmed["invoice_id"].as_str().unwrap();
    let d = h.get(&owner, &format!("/api/v1/documents/{doc}")).await;
    assert_eq!(d["source"], "imported");
    assert_eq!(d["related_id"], org.to_string());
    let v = h.get(&owner, &format!("/api/v1/invoices/{inv}")).await;
    assert_eq!(v["historical"], true);
    assert_eq!(v["status"], "paid");
    assert_eq!(v["document_id"], doc);

    // Its words are searchable, though they are only in the PDF.
    let found = h.get(&owner, "/api/v1/documents?q=performance").await;
    assert_eq!(found["items"][0]["id"], doc, "{found}");
    let none = h.get(&owner, "/api/v1/documents?q=kazoo").await;
    assert_eq!(none["items"].as_array().unwrap().len(), 0);

    // The same PDF again is flagged as already recorded.
    let again = h.upload(&owner, "owl-017-copy.pdf", "application/pdf", old_invoice_pdf()).await.ok().id("/id");
    let second = h
        .post(&owner, "/api/v1/pdf-imports", json!({ "file_ids": [again] }))
        .await
        .body;
    let second = Uuid::parse_str(second[0]["id"].as_str().unwrap()).unwrap();
    h.drain().await;
    let i = h.get(&owner, &format!("/api/v1/pdf-imports/{second}")).await;
    assert_eq!(i["matches"][0]["entity"], "invoice", "{i}");
    let discarded = h
        .post(&owner, &format!("/api/v1/pdf-imports/{second}/discard"), json!({}))
        .await
        .body;
    assert_eq!(discarded["status"], "discarded");
}

fn pdf_text(bytes: &[u8]) -> String {
    core_pdf::extract::extract(bytes, &core_pdf::extract::Limits::default())
        .map(|e| e.text())
        .unwrap_or_default()
}
