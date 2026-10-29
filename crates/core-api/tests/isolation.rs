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
    (
        "/api/v1/public/",
        "secret-link holders only; the token names one quote, feed, invoice or signature; \
         or a workspace's logo for its emails, which shows nothing else",
    ),
    (
        "/api/v1/hooks/messaging/",
        "signed by Twilio or Meta; the address's token names one number, so one workspace",
    ),
    (
        "/api/v1/webhooks/stripe",
        "signed by Stripe; the event's connected account names the one workspace",
    ),
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
    b_field: uuid::Uuid,
    b_pipeline: uuid::Uuid,
    b_template: uuid::Uuid,
    b_rule: uuid::Uuid,
    b_thing: uuid::Uuid,
    b_webhook: uuid::Uuid,
    b_event: uuid::Uuid,
    b_job: uuid::Uuid,
    b_crm: Crm,
    b_work: Work,
    b_money: Money,
    b_inbox: Inbox,
    b_markers: Vec<String>,
}

/// One of everything M6 adds, in `owner`'s workspace.
struct Inbox {
    thread: uuid::Uuid,
    mailbox: uuid::Uuid,
    number: uuid::Uuid,
}

async fn inbox(app: &TestApp, owner: &Client, slug: &str) -> Inbox {
    // Written through OnHold's own sender, so no mail server is needed.
    let thread = app
        .post("/api/v1/inbox/threads")
        .signed_in(owner)
        .json(json!({ "to": [format!("booker@{slug}.test")], "subject": format!("{slug} secret subject"),
                      "body": format!("{slug} secret message") }))
        .send()
        .await
        .ok()
        .id("/thread/id");
    // A connected mailbox, as connecting would leave it.
    let mailbox = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO integrations (id, workspace_id, provider, purposes, owner_id, account, secret, mail_since)
         SELECT $1, $2, 'imap', '{mail}', m.user_id, $3, '\\x00', now()
         FROM memberships m WHERE m.workspace_id = $2 AND m.role = 'owner'",
    )
    .bind(mailbox)
    .bind(owner.workspace.unwrap().0)
    .bind(format!("mail@{slug}.test"))
    .execute(&app.owner)
    .await
    .unwrap();
    // An SMS number, as connecting would leave it.
    let number = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO messaging_channels (id, workspace_id, kind, provider, sender, account_id, secret, hook_token)
         VALUES ($1, $2, 'sms', 'twilio', $3, $4, '\\x00', $5)",
    )
    .bind(number)
    .bind(owner.workspace.unwrap().0)
    .bind(if slug == "alpha" { "+447700900101" } else { "+447700900202" })
    .bind(format!("{slug}-sms-account"))
    .bind(format!("{slug}-hook-token-0123456789"))
    .execute(&app.owner)
    .await
    .unwrap();
    Inbox {
        thread,
        mailbox,
        number,
    }
}

/// One of everything M5 adds, in `owner`'s workspace, named after `slug`.
struct Money {
    tax_code: uuid::Uuid,
    invoice: uuid::Uuid,
    payment: uuid::Uuid,
    allocation: uuid::Uuid,
    settlement: uuid::Uuid,
    document: uuid::Uuid,
    file: uuid::Uuid,
    pdf_import: uuid::Uuid,
}

async fn money(app: &TestApp, owner: &Client, slug: &str, crm: &Crm, work: &Work) -> Money {
    let post = |path: &str, body: Value| {
        let req = app.post(path).signed_in(owner).json(body);
        async move { req.send().await.ok() }
    };
    let tax_code = post(
        "/api/v1/tax-codes",
        json!({ "name": format!("{slug} secret tax"), "rate_bps": 1234 }),
    )
    .await
    .id("/id");
    let invoice = post(
        "/api/v1/invoices",
        json!({ "payer": { "organisation_id": crm.organisation }, "booking_id": work.booking,
                "line_items": [{ "description": format!("{slug} secret invoice line"), "unit_minor": 10_000 }] }),
    )
    .await
    .id("/id");
    post(&format!("/api/v1/invoices/{invoice}/issue"), json!({})).await;
    let payment = post(
        "/api/v1/payments",
        json!({ "amount": { "minor": 1_000, "currency": "GBP" }, "reference": format!("{slug} secret payment"),
                "allocations": [{ "invoice_id": invoice }] }),
    )
    .await;
    let allocation = payment.id("/allocations/0/id");
    let payment = payment.id("/id");
    let settlement = post(
        &format!("/api/v1/bookings/{}/settlements", work.booking),
        json!({}),
    )
    .await
    .id("/0/id");
    let pdf = core_pdf::write::Renderer::render(
        &core_pdf::write::PlainText,
        &format!("<p>{slug} secret contract text</p>"),
        "Secret",
    )
    .unwrap();
    let file = app
        .post(&format!("/api/v1/files?filename={slug}-secret.pdf"))
        .signed_in(owner)
        .bytes("application/pdf", pdf)
        .send()
        .await
        .ok()
        .id("/id");
    let pdf_import = post("/api/v1/pdf-imports", json!({ "file_ids": [file] }))
        .await
        .id("/0/id");
    let document = post(
        "/api/v1/documents/generate",
        json!({ "template_key": "document.contract", "related_entity": "booking", "related_id": work.booking,
                "title": format!("{slug} secret contract") }),
    )
    .await
    .id("/id");
    Money {
        tax_code,
        invoice,
        payment,
        allocation,
        settlement,
        document,
        file,
        pdf_import,
    }
}

/// One of everything M3 adds, in `owner`'s workspace, named after `slug`.
struct Crm {
    talent: uuid::Uuid,
    member: uuid::Uuid,
    calendar_link: uuid::Uuid,
    calendar: uuid::Uuid,
    organisation: uuid::Uuid,
    contact: uuid::Uuid,
    view: uuid::Uuid,
    import: uuid::Uuid,
}

async fn crm(app: &TestApp, owner: &Client, slug: &str) -> Crm {
    let post = |path: &str, body: Value| {
        let req = app.post(path).signed_in(owner).json(body);
        async move { req.send().await.ok() }
    };
    post(
        "/api/v1/custom-fields",
        json!({ "entity": "contact", "key": format!("{slug}_secret_contact_field"), "label": "Secret",
                "field_type": "text", "searchable": true }),
    )
    .await;
    let talent = post(
        "/api/v1/talent",
        json!({ "name": format!("{slug} secret act"), "kind": "act", "tags": [format!("{slug}-secret-tag")] }),
    )
    .await
    .id("/id");
    let member = post(
        &format!("/api/v1/talent/{talent}/members"),
        json!({ "name": format!("{slug} secret drummer") }),
    )
    .await
    .id("/id");
    let calendar_link = post(
        &format!("/api/v1/talent/{talent}/calendar-links"),
        json!({}),
    )
    .await
    .id("/link/id");
    // A connected calendar, as the talent's link would leave it.
    let calendar = core_domain::ids::new_id();
    sqlx::query(
        "INSERT INTO talent_calendars (id, workspace_id, talent_id, link_id, provider, account, secret, url_hash)
         SELECT $1, workspace_id, talent_id, id, 'ical', $2, '\\x00', '\\x00' FROM calendar_connect_links WHERE id = $3",
    )
    .bind(calendar)
    .bind(format!("{slug}.secret.test"))
    .bind(calendar_link)
    .execute(&app.owner)
    .await
    .unwrap();
    let organisation = post(
        "/api/v1/organisations",
        json!({ "name": format!("{slug} secret venue"), "tags": [format!("{slug}-secret-tag")] }),
    )
    .await
    .id("/id");
    let contact = post(
        "/api/v1/contacts",
        json!({ "name": format!("{slug} secret booker"), "organisation_id": organisation,
                "emails": [format!("booker@{slug}.test")], "tags": [format!("{slug}-secret-tag")],
                "custom": { format!("{slug}_secret_contact_field"): format!("{slug} hidden value") } }),
    )
    .await
    .id("/id");
    let view = post(
        "/api/v1/views",
        json!({ "entity": "contact", "name": format!("{slug} secret view"), "shared": true, "config": {} }),
    )
    .await
    .id("/id");
    let import = post(
        "/api/v1/imports",
        json!({ "entity": "contact", "filename": format!("{slug}-secret.csv"),
                "csv": format!("Name,Email\n{slug} imported person,imported@{slug}.test\n") }),
    )
    .await
    .id("/import/id");
    Crm {
        talent,
        member,
        calendar_link,
        calendar,
        organisation,
        contact,
        view,
        import,
    }
}

/// One of everything M4 adds, in `owner`'s workspace, named after `slug`.
struct Work {
    enquiry: uuid::Uuid,
    booking: uuid::Uuid,
    availability: uuid::Uuid,
    quote: uuid::Uuid,
    task: uuid::Uuid,
    feed: uuid::Uuid,
    push: uuid::Uuid,
    books: uuid::Uuid,
    travel: uuid::Uuid,
    money_line: uuid::Uuid,
    project: uuid::Uuid,
    crew: uuid::Uuid,
}

async fn work(app: &TestApp, owner: &Client, slug: &str, crm: &Crm) -> Work {
    let post = |path: &str, body: Value| {
        let req = app.post(path).signed_in(owner).json(body);
        async move { req.send().await.ok() }
    };
    let enquiry = post(
        "/api/v1/enquiries",
        json!({ "title": format!("{slug} secret enquiry"), "talent_ids": [crm.talent],
                "contact_id": crm.contact, "organisation_id": crm.organisation }),
    )
    .await
    .id("/id");
    let booking = post(
        "/api/v1/bookings",
        json!({ "title": format!("{slug} secret booking"), "enquiry_id": enquiry, "status": "confirmed",
                "starts_at": "2031-06-01T19:00:00Z", "ends_at": "2031-06-01T23:00:00Z",
                "fee": { "minor": 150000, "currency": "GBP" } }),
    )
    .await
    .id("/id");
    let availability = post(
        &format!("/api/v1/talent/{}/availability", crm.talent),
        json!({ "starts_at": "2031-07-01T00:00:00Z", "ends_at": "2031-07-08T00:00:00Z",
                "note": format!("{slug} secret holiday") }),
    )
    .await
    .id("/id");
    let quote = post(
        &format!("/api/v1/bookings/{booking}/quotes"),
        json!({ "line_items": [{ "description": format!("{slug} secret line"), "unit_minor": 150000 }] }),
    )
    .await
    .id("/id");
    let task = post(
        "/api/v1/tasks",
        json!({ "title": format!("{slug} secret task"), "related_entity": "booking", "related_id": booking }),
    )
    .await
    .id("/id");
    let feed = post(
        "/api/v1/calendar/feeds",
        json!({ "name": format!("{slug} secret feed") }),
    )
    .await
    .id("/feed/id");
    // A member's calendar that bookings are written into, as connecting
    // would leave it.
    let push = core_domain::ids::new_id();
    sqlx::query(
        "INSERT INTO calendar_pushes (id, workspace_id, user_id, provider, account, secret, remote_calendar)
         SELECT $1, workspace_id, user_id, 'google', $2, '\\x00', 'cal' FROM memberships
         WHERE workspace_id = (SELECT workspace_id FROM bookings WHERE id = $3) AND role = 'owner' LIMIT 1",
    )
    .bind(push)
    .bind(format!("{slug}.push.test"))
    .bind(booking)
    .execute(&app.owner)
    .await
    .unwrap();
    // The workspace's books, as connecting would leave them, with one
    // invoice linked.
    let books = core_domain::ids::new_id();
    sqlx::query(
        "INSERT INTO accounting_connections (id, workspace_id, provider, tenant_id, tenant_name, secret, start_date, sales_account)
         SELECT $1, workspace_id, 'xero', $2, $2, '\\x00', current_date, '200' FROM bookings WHERE id = $3",
    )
    .bind(books)
    .bind(format!("{slug}.books.test"))
    .bind(booking)
    .execute(&app.owner)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO accounting_links (id, workspace_id, connection_id, kind, local_id, remote_id, state)
         SELECT $1, workspace_id, $2, 'contact', $1, $3, 'live' FROM accounting_connections WHERE id = $2",
    )
    .bind(core_domain::ids::new_id())
    .bind(books)
    .bind(format!("{slug}.remote.test"))
    .execute(&app.owner)
    .await
    .unwrap();
    let travel = post(
        &format!("/api/v1/bookings/{booking}/travel"),
        json!({ "kind": "flight", "number": "BA 1", "reference": format!("{slug} secret flight"),
                "starts_at": "2031-06-01T08:00:00Z" }),
    )
    .await
    .id("/id");
    let money_line = post(
        &format!("/api/v1/bookings/{booking}/money"),
        json!({ "direction": "cost", "category": "crew", "description": format!("{slug} secret cost"),
                "amount": { "minor": 100, "currency": "GBP" } }),
    )
    .await
    .id("/id");
    let project = post(
        "/api/v1/projects",
        json!({ "name": format!("{slug} secret tour"), "booking_ids": [booking] }),
    )
    .await
    .id("/id");
    let crew = post(
        &format!("/api/v1/projects/{project}/crew"),
        json!({ "name": format!("{slug} secret crew") }),
    )
    .await
    .id("/id");
    Work {
        enquiry,
        booking,
        availability,
        quote,
        task,
        feed,
        push,
        books,
        travel,
        money_line,
        project,
        crew,
    }
}

/// One of everything M2 adds, in `owner`'s workspace, named after `slug`.
struct Config {
    field: uuid::Uuid,
    pipeline: uuid::Uuid,
    template: uuid::Uuid,
    rule: uuid::Uuid,
    thing: uuid::Uuid,
    webhook: uuid::Uuid,
}

async fn configure(app: &TestApp, owner: &Client, slug: &str) -> Config {
    let post = |path: &str, body: Value| {
        let req = app.post(path).signed_in(owner).json(body);
        async move { req.send().await.ok().id("/id") }
    };
    let field = post(
        "/api/v1/custom-fields",
        json!({ "entity": "thing", "key": format!("{slug}_secret_field"), "label": "Secret", "field_type": "text" }),
    )
    .await;
    let pipeline = post(
        "/api/v1/pipelines",
        json!({ "entity": "thing", "name": format!("{slug} secret pipeline"),
                "stages": [{ "key": "new", "name": "New" }, { "key": "done", "name": "Done", "outcome": "won" }] }),
    )
    .await;
    let template = post(
        "/api/v1/templates",
        json!({ "kind": "email", "key": format!("{slug}.secret"), "name": "Secret", "subject": "Hi", "body": "Hello {{ thing.name }}" }),
    )
    .await;
    let rule = post(
        "/api/v1/rules",
        json!({ "name": format!("{slug} secret rule"), "trigger": { "kind": "event", "topic": "thing.created" },
                "actions": [{ "type": "send_email", "template": format!("{slug}.secret"), "to": { "address": format!("ops@{slug}.test") } }] }),
    )
    .await;
    let thing = post(
        "/api/v1/things",
        json!({ "name": format!("{slug} secret thing") }),
    )
    .await;
    let webhook = app
        .post("/api/v1/webhooks")
        .signed_in(owner)
        .json(json!({ "url": format!("https://{slug}-hook.example/in"), "events": ["*"] }))
        .send()
        .await
        .ok()
        .id("/webhook/id");
    Config {
        field,
        pipeline,
        template,
        rule,
        thing,
        webhook,
    }
}

async fn world(app: &TestApp) -> World {
    let all = json!([
        "workspace:read",
        "workspace:update",
        "portal:use",
        "member:read",
        "member:invite",
        "member:update",
        "member:remove",
        "api_key:read",
        "api_key:write",
        "audit:read",
        "billing:read",
        "billing:write",
        "config:write",
        "thing:read",
        "thing:write",
        "event:read",
        "webhook:read",
        "webhook:write",
        "job:read",
        "job:write",
        "talent:read",
        "talent:write",
        "organisation:read",
        "organisation:write",
        "contact:read",
        "contact:write",
        "enquiry:read",
        "enquiry:write",
        "booking:read",
        "booking:write",
        "project:read",
        "project:write",
        "booking:override",
        "quote:read",
        "quote:write",
        "task:read",
        "task:write",
        "document:read",
        "document:write",
        "invoice:read",
        "invoice:write",
        "payment:write",
        "report:read",
        "finance:config",
        "inbox:read",
        "inbox:write",
        "integration:manage"
    ]);
    let mut keys = Vec::new();
    let mut invitations = Vec::new();
    let mut clients = Vec::new();
    let mut configs = Vec::new();
    let mut crms = Vec::new();
    let mut works = Vec::new();
    let mut moneys = Vec::new();
    let mut inboxes = Vec::new();
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
        configs.push(configure(app, &owner, slug).await);
        let c = crm(app, &owner, slug).await;
        let wk = work(app, &owner, slug, &c).await;
        moneys.push(money(app, &owner, slug, &c, &wk).await);
        inboxes.push(inbox(app, &owner, slug).await);
        works.push(wk);
        crms.push(c);
        clients.push(owner);
    }
    let b = clients.pop().unwrap();
    let a = clients.pop().unwrap();
    let bc = configs.pop().unwrap();
    let b_crm = crms.pop().unwrap();
    let b_work = works.pop().unwrap();
    let b_money = moneys.pop().unwrap();
    let b_inbox = inboxes.pop().unwrap();
    app.publish_all().await;
    let b_event: uuid::Uuid = sqlx::query_scalar(
        "SELECT id FROM outbox WHERE workspace_id = $1 AND topic = 'thing.created'",
    )
    .bind(b.workspace.unwrap().0)
    .fetch_one(&app.owner)
    .await
    .unwrap();
    // A dead job, so retrying it would be allowed if access were.
    let b_job = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO jobs (id, workspace_id, kind, payload, status, attempts, finished_at)
         VALUES ($1, $2, 'test.noop', '{}', 'dead', 10, now())",
    )
    .bind(b_job)
    .bind(b.workspace.unwrap().0)
    .execute(&app.owner)
    .await
    .unwrap();
    World {
        b_markers: vec![
            b.workspace.unwrap().to_string(),
            "bravo".into(),
            "agent@bravo.test".into(),
            "pending@bravo.test".into(),
            keys[1].0.to_string(),
            bc.thing.to_string(),
            b_event.to_string(),
            b_job.to_string(),
            b_crm.talent.to_string(),
            b_crm.member.to_string(),
            b_crm.calendar.to_string(),
            "bravo.secret.test".into(),
            b_crm.organisation.to_string(),
            b_crm.contact.to_string(),
            b_crm.view.to_string(),
            b_crm.import.to_string(),
            b_work.enquiry.to_string(),
            b_work.booking.to_string(),
            b_work.availability.to_string(),
            b_work.quote.to_string(),
            b_work.task.to_string(),
            b_work.feed.to_string(),
            b_work.push.to_string(),
            "bravo.push.test".into(),
            b_work.books.to_string(),
            "bravo.books.test".into(),
            "bravo.remote.test".into(),
            b_money.tax_code.to_string(),
            b_money.invoice.to_string(),
            b_money.payment.to_string(),
            b_money.settlement.to_string(),
            b_money.document.to_string(),
            b_money.file.to_string(),
            b_money.pdf_import.to_string(),
            b_inbox.thread.to_string(),
            b_inbox.mailbox.to_string(),
            "mail@bravo.test".into(),
            b_inbox.number.to_string(),
            "bravo-sms-account".into(),
            "+447700900202".into(),
        ],
        b_crm,
        b_work,
        b_money,
        b_inbox,
        b_field: bc.field,
        b_pipeline: bc.pipeline,
        b_template: bc.template,
        b_rule: bc.rule,
        b_thing: bc.thing,
        b_webhook: bc.webhook,
        b_event,
        b_job,
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
fn body_for(method: &Method, path: &str, w: &World) -> Value {
    match (method.as_str(), path) {
        ("PATCH", "/api/v1/workspace") => json!({ "name": "Taken over" }),
        ("POST", "/api/v1/workspace/close") => json!({ "confirm_slug": "bravo" }),
        ("PATCH", p) if p.starts_with("/api/v1/members/") => json!({ "role": "viewer" }),
        ("POST", p) if p.starts_with("/api/v1/talent/") && p.ends_with("/portal") => {
            json!({ "email": "intruder@alpha.test" })
        }
        ("POST", p) if p.starts_with("/api/v1/portal/dates/") => json!({ "answer": "confirmed" }),
        ("PUT", p) if p.starts_with("/api/v1/portal/profile/") => {
            json!({ "bio": "Taken over", "version": 1 })
        }
        ("POST", p) if p.starts_with("/api/v1/talent/") && p.ends_with("/portal/messages") => {
            json!({ "body": "Taken over" })
        }
        ("POST", "/api/v1/invitations") => {
            json!({ "email": "intruder@alpha.test", "role": "admin" })
        }
        ("POST", "/api/v1/api-keys") => {
            json!({ "name": "backdoor", "permissions": ["member:read"] })
        }
        ("POST", "/api/v1/custom-fields") => {
            json!({ "entity": "thing", "key": "intruder", "label": "Intruder", "field_type": "text" })
        }
        ("POST", "/api/v1/pipelines") => {
            json!({ "entity": "thing", "name": "Intruder", "stages": [{ "key": "a", "name": "A" }] })
        }
        ("POST", "/api/v1/templates") => {
            json!({ "kind": "email", "key": "intruder", "name": "Intruder", "subject": "x", "body": "x" })
        }
        ("POST", "/api/v1/templates/preview") => {
            json!({ "kind": "email", "subject": "x", "body": "x" })
        }
        ("POST", "/api/v1/rules") => json!({
            "name": "Intruder", "trigger": { "kind": "event", "topic": "thing.created" },
            "actions": [{ "type": "send_email", "template": "intruder", "to": { "address": "x@intruder.test" } }]
        }),
        ("PUT", "/api/v1/automations") => json!({ "paused": true }),
        ("POST", "/api/v1/things") => json!({ "name": "Intruder" }),
        ("POST", "/api/v1/webhooks") => {
            json!({ "url": "https://intruder.example/", "events": ["*"] })
        }
        ("POST", "/api/v1/webhooks/{id}/replay") => json!({ "event_id": w.b_event }),
        ("POST", "/api/v1/talent" | "/api/v1/organisations" | "/api/v1/contacts") => {
            json!({ "name": "Intruder" })
        }
        ("POST", p) if p.ends_with("/duplicates") => json!({ "name": "bravo secret" }),
        ("POST", p) if p.ends_with("/bulk") => {
            let ids = [w.b_crm.talent, w.b_crm.organisation, w.b_crm.contact];
            json!({ "action": "delete", "ids": ids })
        }
        ("POST" | "PUT", p) if p.contains("/members") => json!({ "name": "Intruder" }),
        ("POST" | "PUT", p) if p.starts_with("/api/v1/views") => {
            json!({ "entity": "contact", "name": "Intruder", "shared": true, "config": {} })
        }
        ("POST", "/api/v1/imports") => {
            json!({ "entity": "contact", "csv": "Name\nIntruder\n" })
        }
        ("POST", p) if p.starts_with("/api/v1/imports/") => json!({ "columns": ["name", null] }),
        ("POST", "/api/v1/enquiries" | "/api/v1/tasks") => json!({ "title": "Intruder" }),
        ("POST", "/api/v1/bookings") => json!({
            "title": "Intruder", "talent_ids": [w.b_crm.talent], "status": "confirmed",
            "starts_at": "2031-06-01T20:00:00Z", "ends_at": "2031-06-01T22:00:00Z"
        }),
        ("POST", "/api/v1/bookings/clash-check") => json!({
            "talent_ids": [w.b_crm.talent], "starts_at": "2031-06-01T20:00:00Z", "ends_at": "2031-06-01T22:00:00Z"
        }),
        ("POST", "/api/v1/bookings/{id}/status") => json!({ "status": "cancelled", "reason": "x" }),
        ("POST", "/api/v1/talent/{id}/availability") => {
            json!({ "starts_at": "2031-08-01T00:00:00Z", "ends_at": "2031-08-02T00:00:00Z" })
        }
        ("POST", "/api/v1/calendar/feeds") => json!({ "name": "Intruder" }),
        ("POST", "/api/v1/calendar/pushes/start") => json!({ "provider": "google" }),
        ("POST", "/api/v1/calendar/pushes/finish") => json!({ "code": "x", "state": "x" }),
        ("POST", "/api/v1/accounting/start") => json!({ "provider": "xero" }),
        ("POST", "/api/v1/accounting/finish") => json!({ "code": "x", "state": "x" }),
        ("PUT", "/api/v1/accounting/settings") => json!({ "sales_account": "200" }),
        ("POST", "/api/v1/quotes/{id}/send" | "/api/v1/invoices/{id}/send") => {
            json!({ "to": "intruder@alpha.test" })
        }
        ("POST", "/api/v1/tax-codes") | ("PUT", "/api/v1/tax-codes/{id}") => {
            json!({ "name": "Intruder", "rate_bps": 0 })
        }
        ("POST", "/api/v1/invoices") => json!({
            "payer": { "name": "Intruder" }, "line_items": [{ "description": "x", "unit_minor": 1 }]
        }),
        ("POST", "/api/v1/invoices/historical") => json!({
            "reference": "X-1", "payer": { "name": "Intruder" }, "currency": "GBP",
            "issue_date": "2024-01-01", "total_minor": 1
        }),
        ("POST", "/api/v1/bookings/{id}/invoices") => json!({ "purpose": "full" }),
        ("POST", "/api/v1/projects") | ("PATCH", "/api/v1/projects/{id}") => {
            json!({ "name": "Intruder" })
        }
        ("POST", "/api/v1/projects/{id}/dates") => json!({ "booking_id": w.b_work.booking }),
        ("POST", "/api/v1/projects/{id}/crew") | ("PUT", "/api/v1/project-crew/{id}") => {
            json!({ "name": "Intruder" })
        }
        ("POST", "/api/v1/bookings/{id}/travel" | "/api/v1/projects/{id}/travel")
        | ("PUT", "/api/v1/travel/{id}") => {
            json!({ "kind": "flight", "starts_at": "2031-06-01T08:00:00Z" })
        }
        ("POST", "/api/v1/bookings/{id}/money" | "/api/v1/projects/{id}/money")
        | ("PUT", "/api/v1/money-lines/{id}") => json!({
            "direction": "cost", "category": "crew", "description": "Intruder",
            "amount": { "minor": 1, "currency": "GBP" }
        }),
        ("POST", "/api/v1/payments") => json!({ "amount": { "minor": 1, "currency": "GBP" } }),
        ("POST", "/api/v1/payments/{id}/allocations") => json!({ "invoice_id": w.b_money.invoice }),
        ("POST", "/api/v1/payments/{id}/refunds") => json!({ "amount_minor": 1 }),
        ("POST", "/api/v1/documents") => json!({ "file_id": w.b_money.file, "kind": "misc" }),
        ("POST", "/api/v1/documents/generate") => json!({
            "template_key": "document.contract", "related_entity": "booking", "related_id": w.b_work.booking
        }),
        ("POST", "/api/v1/pdf-imports") => json!({ "file_ids": [w.b_money.file] }),
        ("POST", "/api/v1/documents/{id}/versions") => json!({ "file_id": w.b_money.file }),
        ("POST", "/api/v1/pdf-imports/{id}/confirm") => json!({ "kind": "misc" }),
        ("POST", "/api/v1/inbox/threads") => json!({
            "to": ["intruder@alpha.test"], "subject": "x", "body": "x", "contact_id": w.b_crm.contact
        }),
        ("POST", "/api/v1/inbox/threads/{id}/reply") => json!({ "body": "x" }),
        ("POST", "/api/v1/organisations/{id}/pitches/draft") => {
            json!({ "template_key": "first_pitch", "contact_id": w.b_crm.contact })
        }
        ("POST", "/api/v1/inbox/texts") => json!({
            "channel": "sms", "contact_id": w.b_crm.contact, "body": "x"
        }),
        ("POST", "/api/v1/inbox/numbers/sms") => json!({
            "account_sid": "ACx", "auth_token": "x", "number": "+447700900303"
        }),
        ("POST", "/api/v1/inbox/numbers/whatsapp") => json!({
            "phone_number_id": "1", "access_token": "x", "app_secret": "x"
        }),
        ("PUT", "/api/v1/follow-ups/{key}") => json!({ "on": true }),
        ("POST", "/api/v1/inbox/threads/{id}/enquiry") => json!({ "title": "Intruder" }),
        ("PATCH", "/api/v1/inbox/threads/{id}") => json!({ "status": "resolved" }),
        ("PATCH", "/api/v1/inbox/mailboxes/{id}") => json!({ "shared": true }),
        ("POST", "/api/v1/inbox/mailboxes/connect") => json!({ "provider": "google" }),
        ("POST", "/api/v1/inbox/mailboxes/connect/finish") => json!({ "code": "x", "state": "x" }),
        ("POST", "/api/v1/inbox/mailboxes/imap") => json!({
            "address": "intruder@alpha.test", "password": "x",
            "imap": { "host": "imap.alpha.test", "port": 993, "security": "tls" },
            "smtp": { "host": "smtp.alpha.test", "port": 465, "security": "tls" }
        }),
        _ => json!({}),
    }
}

fn fill_path(path: &str, w: &World) -> String {
    let object = if path.starts_with("/api/v1/travel/") {
        w.b_work.travel
    } else if path.starts_with("/api/v1/money-lines/") {
        w.b_work.money_line
    } else if path.starts_with("/api/v1/project-crew/") {
        w.b_work.crew
    } else if path.starts_with("/api/v1/projects/") {
        w.b_work.project
    } else if path.contains("/inbox/numbers/") {
        w.b_inbox.number
    } else if path.contains("/inbox/threads/") {
        w.b_inbox.thread
    } else if path.contains("/inbox/mailboxes/") {
        w.b_inbox.mailbox
    } else if path.contains("/talent/") {
        w.b_crm.talent
    } else if path.contains("/enquiries/") {
        w.b_work.enquiry
    } else if path.contains("/bookings/") {
        w.b_work.booking
    } else if path.contains("/availability/") {
        w.b_work.availability
    } else if path.contains("/quotes/") {
        w.b_work.quote
    } else if path.contains("/tasks/") {
        w.b_work.task
    } else if path.contains("/tax-codes/") {
        w.b_money.tax_code
    } else if path.contains("/invoices/") {
        w.b_money.invoice
    } else if path.contains("/payments/") {
        w.b_money.payment
    } else if path.contains("/settlements/") {
        w.b_money.settlement
    } else if path.contains("/documents/") {
        w.b_money.document
    } else if path.contains("/files/") {
        w.b_money.file
    } else if path.contains("/pdf-imports/") {
        w.b_money.pdf_import
    } else if path.contains("/calendar/feeds/") {
        w.b_work.feed
    } else if path.contains("/calendar/pushes/") {
        w.b_work.push
    } else if path.contains("/talent/") {
        w.b_crm.talent
    } else if path.contains("/organisations/") {
        w.b_crm.organisation
    } else if path.contains("/contacts/") {
        w.b_crm.contact
    } else if path.contains("/views/") {
        w.b_crm.view
    } else if path.contains("/imports/") {
        w.b_crm.import
    } else if path.contains("/members/") {
        w.b_agent
    } else if path.contains("/invitations/") {
        w.b_invitation
    } else if path.contains("/api-keys/") {
        w.b_key
    } else if path.contains("/custom-fields/") {
        w.b_field
    } else if path.contains("/pipelines/") {
        w.b_pipeline
    } else if path.contains("/templates/") {
        w.b_template
    } else if path.contains("/rules/") {
        w.b_rule
    } else if path.contains("/things/") {
        w.b_thing
    } else if path.contains("/webhooks/") {
        w.b_webhook
    } else if path.contains("/jobs/") {
        w.b_job
    } else {
        w.b_owner
    };
    path.replace("{user_id}", &object.to_string())
        .replace("{id}", &object.to_string())
        .replace("{booking_id}", &w.b_work.booking.to_string())
        .replace("{member_id}", &w.b_crm.member.to_string())
        .replace("{link_id}", &w.b_crm.calendar_link.to_string())
        .replace("{calendar_id}", &w.b_crm.calendar.to_string())
        .replace("{allocation_id}", &w.b_money.allocation.to_string())
        .replace("{file_id}", &w.b_money.file.to_string())
        // B's own email template: not a document template A can see.
        .replace("{key}", "bravo.secret")
        .replace("{table}", "invoices")
        .replace("{entity}", "contact")
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_member_of_one_workspace_cannot_reach_another(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let w = world(&app).await;
    let a_in_b = w.a.clone().in_workspace(w.b.workspace.unwrap());

    let mut checked = 0;
    for op in operations().iter().filter(|o| o.workspace_scoped) {
        let path = fill_path(&op.path, &w);
        let body = body_for(&op.method, &op.path, &w);

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
        // An entity's schema names a kind of record, not one of B's.
        // A report names a table, not one of B's records.
        .filter(|o| {
            o.workspace_scoped
                && o.path.contains('{')
                && !o.path.contains("{entity}")
                && !o.path.contains("{table}")
        })
    {
        let path = fill_path(&op.path, &w);
        let body = body_for(&op.method, &op.path, &w);
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
        // The live stream never ends; it has its own test in events.rs.
        // The books' accounts are read from Xero or QuickBooks themselves;
        // accounting.rs tests them against stand-ins.
        .filter(|o| {
            o.workspace_scoped
                && o.method == Method::GET
                && !o.path.contains('{')
                && !o.path.ends_with("/stream")
                && o.path != "/api/v1/accounting/options"
        })
    {
        for query in read_queries(&op.path) {
            assert_nothing_leaks(&app, &w, &format!("{}{query}", op.path)).await;
        }
    }
    for entity in ["talent", "organisation", "contact", "enquiry", "booking"] {
        assert_nothing_leaks(&app, &w, &format!("/api/v1/schema/{entity}")).await;
    }
}

/// What to ask each list for, so that B's records would show if they could.
fn read_queries(path: &str) -> Vec<&'static str> {
    match path {
        "/api/v1/search" => vec!["?q=bravo", "?q=secret", "?q=hidden%20value"],
        "/api/v1/tags" | "/api/v1/views" => {
            vec![
                "?entity=talent",
                "?entity=organisation",
                "?entity=contact",
                "?entity=enquiry",
                "?entity=booking",
            ]
        }
        "/api/v1/talent" | "/api/v1/organisations" | "/api/v1/contacts" => {
            vec!["", "?q=bravo", "?tag=bravo-secret-tag"]
        }
        "/api/v1/enquiries" | "/api/v1/bookings" | "/api/v1/invoices" => vec!["", "?q=bravo"],
        "/api/v1/documents" => vec!["", "?q=bravo", "?q=secret"],
        "/api/v1/pdf-imports" => vec!["", "?status=all"],
        // Rates are the same for everyone; same-currency needs no stored rate.
        "/api/v1/fx-rates" => vec!["?from=GBP&to=GBP"],
        "/api/v1/tasks" => vec!["?status=all"],
        "/api/v1/calendar" => {
            vec!["?from=2031-01-01T00:00:00Z&to=2031-12-31T00:00:00Z"]
        }
        _ => vec![""],
    }
}

async fn assert_nothing_leaks(app: &TestApp, w: &World, path: &str) {
    for res in [
        app.get(path).signed_in(&w.a).send().await,
        app.get(path).bearer(&w.a_key).send().await,
    ] {
        assert!(
            res.status.is_success(),
            "GET {path}: {} {}",
            res.status,
            res.body
        );
        let text = res.body.to_string();
        for marker in &w.b_markers {
            assert!(
                !text.contains(marker.as_str()),
                "GET {path} leaked {marker}"
            );
        }
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn bulk_actions_and_duplicate_checks_never_reach_another_workspace(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let w = world(&app).await;
    for (entity, id) in [
        ("talent", w.b_crm.talent),
        ("organisations", w.b_crm.organisation),
        ("contacts", w.b_crm.contact),
    ] {
        for action in ["delete", "add_tags"] {
            let res = app
                .post(&format!("/api/v1/{entity}/bulk"))
                .signed_in(&w.a)
                .json(json!({ "action": action, "ids": [id], "tags": ["taken"] }))
                .send()
                .await
                .ok();
            assert_eq!(res.body["done"], 0, "{entity} {action}");
            assert_eq!(res.body["failed"][0]["message"], "not found");
        }
        // The same name in B is not a duplicate in A.
        let res = app
            .post(&format!("/api/v1/{entity}/duplicates"))
            .signed_in(&w.a)
            .json(json!({ "name": "bravo secret booker", "emails": ["booker@bravo.test"] }))
            .send()
            .await
            .ok();
        let text = res.body.to_string();
        assert!(
            !text.contains(&id.to_string()) && !text.contains("bravo"),
            "{entity}: {text}"
        );
    }
    // A contact in A cannot be placed at B's organisation.
    let res = app
        .post("/api/v1/contacts")
        .signed_in(&w.a)
        .json(json!({ "name": "Mole", "organisation_id": w.b_crm.organisation }))
        .send()
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert_untouched(&app, &w).await;
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
    let thing = app
        .get(&format!("/api/v1/things/{}", w.b_thing))
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert_eq!(thing.body["version"], 1);
    let rule = app
        .get(&format!("/api/v1/rules/{}", w.b_rule))
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert_eq!(rule.body["version"], 1);
    let hook = app
        .get(&format!("/api/v1/webhooks/{}", w.b_webhook))
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert_eq!(hook.body["version"], 1);
    let paused = app
        .get("/api/v1/automations")
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert_eq!(paused.body["paused"], false);
    let job: String = sqlx::query_scalar("SELECT status::text FROM jobs WHERE id = $1")
        .bind(w.b_job)
        .fetch_one(&app.owner)
        .await
        .unwrap();
    assert_eq!(job, "dead");
    // The organisation's own enquiry and confirmed booking made it a
    // prospect, then a client: versions 2 and 3, and nothing since.
    for (path, version) in [
        (format!("/api/v1/talent/{}", w.b_crm.talent), 1),
        (format!("/api/v1/organisations/{}", w.b_crm.organisation), 3),
        (format!("/api/v1/contacts/{}", w.b_crm.contact), 1),
    ] {
        let record = app.get(&path).signed_in(&w.b).send().await.ok();
        assert_eq!(record.body["version"], version, "{path}");
    }
    let members = app
        .get(&format!("/api/v1/talent/{}/members", w.b_crm.talent))
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert_eq!(members.body[0]["name"], "bravo secret drummer");
    let calendars = app
        .get(&format!("/api/v1/talent/{}/calendars", w.b_crm.talent))
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert_eq!(
        calendars.body["calendars"][0]["account"],
        "bravo.secret.test"
    );
    assert_eq!(calendars.body["calendars"][0]["status"], "active");
    assert_eq!(calendars.body["links"].as_array().map(Vec::len), Some(1));
    let import = app
        .get(&format!("/api/v1/imports/{}", w.b_crm.import))
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert_eq!(import.body["status"], "uploaded");
    for path in [
        format!("/api/v1/enquiries/{}", w.b_work.enquiry),
        format!("/api/v1/bookings/{}", w.b_work.booking),
        format!("/api/v1/quotes/{}", w.b_work.quote),
        format!("/api/v1/tasks/{}", w.b_work.task),
    ] {
        let record = app.get(&path).signed_in(&w.b).send().await.ok();
        assert_eq!(record.body["version"], 1, "{path}");
    }
    let booking = app
        .get(&format!("/api/v1/bookings/{}", w.b_work.booking))
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert_eq!(booking.body["status"], "confirmed");
    let feeds = app
        .get("/api/v1/calendar/feeds")
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert_eq!(feeds.body.as_array().unwrap().len(), 1);
    let views = app
        .get("/api/v1/views?entity=contact")
        .signed_in(&w.b)
        .send()
        .await
        .ok();
    assert_eq!(views.body.as_array().unwrap().len(), 1);
    assert_eq!(views.body[0]["name"], "bravo secret view");
}
