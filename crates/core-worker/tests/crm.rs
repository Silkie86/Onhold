//! The CRM's background work: spreadsheet imports run by the worker, rules
//! reacting to contacts, and the messages that land on a contact's timeline.
//! Includes M3's demonstrable outcome through the API: import a spreadsheet,
//! find a contact, add a custom field and see it in the form and the table.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core-api/tests/common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Client, TestApp};
use core_integrations::http::WebhookClient;
use core_integrations::mail::MemoryMailer;
use core_worker::{Settings, Worker};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

struct Harness {
    app: TestApp,
    worker: Worker,
    mailer: Arc<MemoryMailer>,
}

async fn harness(pool: PgPool, settings: Settings) -> Harness {
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
            concurrency: 16,
            backoff: |_| Duration::ZERO,
            ..settings
        },
    };
    Harness {
        app,
        worker,
        mailer,
    }
}

impl Harness {
    /// Publishes and runs until there is nothing left to do now.
    async fn drain(&self) {
        // Imports raise an event per record, each dispatched in its own job.
        for _ in 0..200 {
            let published = self.worker.publish_once().await.unwrap();
            let ran = self.worker.run_jobs_once().await.unwrap();
            if published == 0 && ran == 0 {
                return;
            }
        }
        panic!("the queue did not settle");
    }

    async fn post(&self, who: &Client, path: &str, body: Value) -> common::Resp {
        self.app
            .post(path)
            .signed_in(who)
            .json(body)
            .send()
            .await
            .ok()
    }

    async fn get(&self, who: &Client, path: &str) -> Value {
        self.app.get(path).signed_in(who).send().await.ok().body
    }

    /// Uploads `csv` and queues it with the suggested mapping and `options`.
    /// Returns the import's path.
    async fn queue_import(&self, who: &Client, entity: &str, csv: &str, options: Value) -> String {
        let upload = self
            .post(
                who,
                "/api/v1/imports",
                json!({ "entity": entity, "csv": csv }),
            )
            .await;
        let id = upload.id("/import/id");
        let plan = json!({ "columns": upload.body["suggested"], "options": options });
        self.post(who, &format!("/api/v1/imports/{id}/run"), plan)
            .await;
        format!("/api/v1/imports/{id}")
    }

    /// Uploads `csv`, runs it with the suggested mapping and `options`, and
    /// waits for the worker to finish it.
    async fn import(&self, who: &Client, entity: &str, csv: &str, options: Value) -> Value {
        let path = self.queue_import(who, entity, csv, options).await;
        self.drain().await;
        self.get(who, &path).await
    }
}

fn names(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect()
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_m3_demo_import_find_add_a_field_and_see_it(pool: PgPool) {
    let h = harness(pool, Settings::default()).await;
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    h.post(
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Jo Smith", "emails": ["jo@blueroom.example"], "tags": ["existing"] }),
    )
    .await;

    // 1. Import a spreadsheet, as exported from another tool.
    let csv = "First name,Last name,Email,Phone,Company,Job title,Tags\n\
               Jo,Smith,JO@blueroom.example,,Blue Room,Head booker,vip\n\
               Priya,Patel,priya@harbour.example,+44 7700 900111,Harbour Arts,Programmer,\"festival, vip\"\n\
               Marcus,Lee,marcus@blueroom.example,07700 900222,Blue Room,,\n\
               ,,,,,,\n\
               No Email,,not-an-email,,,,\n";
    let import = h
        .import(
            &owner,
            "contact",
            csv,
            json!({ "on_match": "update", "tags": ["imported"] }),
        )
        .await;
    assert_eq!(import["status"], "done", "{import}");
    assert_eq!(
        (
            &import["created"],
            &import["updated"],
            &import["skipped"],
            &import["failed"]
        ),
        (&json!(2), &json!(1), &json!(0), &json!(1))
    );
    assert_eq!(import["processed"], 4);
    assert_eq!(import["issues"][0]["row"], 6);
    assert_eq!(import["issues"][0]["field"], "emails");

    // The organisations the sheet names exist now, with their people.
    let orgs = h.get(&owner, "/api/v1/organisations").await;
    assert_eq!(names(&orgs["items"]), ["Blue Room", "Harbour Arts"]);
    assert_eq!(orgs["items"][0]["contact_count"], 2);

    // 2. Find a contact.
    let found = h.get(&owner, "/api/v1/search?q=priya").await;
    assert_eq!(found[0]["name"], "Priya Patel");
    let priya = found[0]["id"].as_str().unwrap().to_owned();
    let record = h.get(&owner, &format!("/api/v1/contacts/{priya}")).await;
    assert_eq!(record["organisation_name"], "Harbour Arts");
    assert_eq!(record["role"], "Programmer");
    assert_eq!(record["phone"], "+44 7700 900111");
    assert_eq!(record["tags"], json!(["festival", "vip", "imported"]));
    // The existing contact was updated, not duplicated, and kept its tags.
    let jo = h.get(&owner, "/api/v1/contacts?q=jo%20smith").await;
    assert_eq!(jo["total"], 1);
    assert_eq!(
        jo["items"][0]["tags"],
        json!(["existing", "vip", "imported"])
    );
    assert_eq!(jo["items"][0]["role"], "Head booker");

    // 3. Add a custom field.
    h.post(
        &owner,
        "/api/v1/custom-fields",
        json!({ "entity": "contact", "key": "dietary", "label": "Dietary needs", "field_type": "text" }),
    )
    .await;
    h.app
        .patch(&format!("/api/v1/contacts/{priya}"))
        .signed_in(&owner)
        .json(json!({ "custom": { "dietary": "vegan" } }))
        .send()
        .await
        .ok();

    // 4. See it in the form (the schema) and the table (the list, sortable).
    let schema = h.get(&owner, "/api/v1/schema/contact").await;
    let field = schema["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "custom.dietary")
        .unwrap();
    assert_eq!(field["label"], "Dietary needs");
    assert_eq!(field["in_list"], true);
    let table = h
        .get(&owner, "/api/v1/contacts?sort=-custom.dietary&limit=1")
        .await;
    assert_eq!(table["items"][0]["custom"]["dietary"], "vegan");

    // The file is not kept once the import is done.
    let kept: Option<String> = sqlx::query_scalar("SELECT csv FROM imports")
        .fetch_one(&h.app.owner)
        .await
        .unwrap();
    assert!(kept.is_none());
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn long_imports_carry_on_in_fresh_jobs_and_skip_rules_unless_asked(pool: PgPool) {
    // Every job hands over after one batch.
    let settings = Settings {
        import_slice: Duration::ZERO,
        ..Settings::default()
    };
    let h = harness(pool, settings).await;
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    h.post(
        &owner,
        "/api/v1/custom-fields",
        json!({ "entity": "talent", "key": "seen", "label": "Seen by rules", "field_type": "boolean" }),
    )
    .await;
    let rule = h
        .post(
            &owner,
            "/api/v1/rules",
            json!({ "name": "Mark", "trigger": { "kind": "event", "topic": "talent.created" },
                    "actions": [{ "type": "set_field", "field": "custom.seen", "value": true }] }),
        )
        .await
        .id("/id");

    let mut csv = String::from("Stage name,Type,Fee\n");
    for i in 0..450 {
        csv.push_str(&format!("Act {i:03},Band,\"£{i},000\"\n"));
    }
    let import = h.import(&owner, "talent", &csv, json!({})).await;
    assert_eq!(import["status"], "done", "{import}");
    assert_eq!(import["created"], 450);
    let jobs: Vec<String> = sqlx::query_scalar(
        "SELECT status::text FROM jobs WHERE kind = 'crm.import' ORDER BY created_at",
    )
    .fetch_all(&h.app.owner)
    .await
    .unwrap();
    assert_eq!(jobs, ["succeeded", "succeeded", "succeeded"]);
    let fee = h.get(&owner, "/api/v1/talent?q=act%20012").await;
    assert_eq!(
        fee["items"][0]["default_fee"],
        json!({ "minor": 1_200_000, "currency": "GBP" })
    );
    // Rules did not react to 450 imported records.
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM rule_runs WHERE rule_id = $1")
        .bind(rule)
        .fetch_one(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(runs, 0);

    // They do when the import asks them to.
    let import = h
        .import(
            &owner,
            "talent",
            "Name\nOpted In\n",
            json!({ "run_rules": true }),
        )
        .await;
    assert_eq!(import["created"], 1);
    let opted = h.get(&owner, "/api/v1/talent?q=opted%20in").await;
    assert_eq!(opted["items"][0]["custom"]["seen"], true);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_import_runs_with_what_its_member_may_still_do(pool: PgPool) {
    let h = harness(pool, Settings::default()).await;
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    let agent = h.app.add_member(&owner, "agent@acme.test", "agent").await;
    let csv = "Name,Company\nJo Smith,Blue Room\n";

    // Agents lose organisation:write after queuing: the contact naming a
    // new organisation is refused instead of creating it.
    let path = h.queue_import(&agent, "contact", csv, json!({})).await;
    sqlx::query(
        "DELETE FROM role_permissions WHERE role = 'agent' AND permission = 'organisation:write'",
    )
    .execute(&h.app.owner)
    .await
    .unwrap();
    h.drain().await;
    let import = h.get(&owner, &path).await;
    assert_eq!(import["status"], "done", "{import}");
    assert_eq!(import["failed"], 1, "{import}");
    assert_eq!(h.get(&owner, "/api/v1/organisations").await["total"], 0);

    // Demoted to viewer after queuing: the import does not run at all.
    let path = h
        .queue_import(
            &agent,
            "contact",
            csv,
            json!({ "create_organisations": false }),
        )
        .await;
    let agent_id: String =
        sqlx::query_scalar("SELECT id::text FROM users WHERE email = 'agent@acme.test'")
            .fetch_one(&h.app.owner)
            .await
            .unwrap();
    h.app
        .patch(&format!("/api/v1/members/{agent_id}"))
        .signed_in(&owner)
        .json(json!({ "role": "viewer" }))
        .send()
        .await
        .ok();
    h.drain().await;
    let import = h.get(&owner, &path).await;
    assert_eq!(import["status"], "failed", "{import}");
    assert_eq!(h.get(&owner, "/api/v1/contacts").await["total"], 0);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_import_whose_job_gives_up_is_marked_failed(pool: PgPool) {
    let h = harness(pool, Settings::default()).await;
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    h.drain().await;
    let last_attempt = "UPDATE jobs SET attempts = max_attempts - 1 \
                        WHERE kind = 'crm.import' AND status = 'queued'";

    // Its last attempt times out.
    let path = h
        .queue_import(&owner, "talent", "Name\nAct One\n", json!({}))
        .await;
    sqlx::query(last_attempt)
        .execute(&h.app.owner)
        .await
        .unwrap();
    let mut impatient = h.worker.clone();
    impatient.settings.job_timeout = Duration::ZERO;
    assert_eq!(impatient.run_jobs_once().await.unwrap(), 1);
    let import = h.get(&owner, &path).await;
    assert_eq!(import["status"], "failed", "{import}");
    assert!(
        import["error"].as_str().unwrap().contains("timed out"),
        "{import}"
    );

    // Its worker stops during the last attempt.
    let path = h
        .queue_import(&owner, "talent", "Name\nAct Two\n", json!({}))
        .await;
    sqlx::query(last_attempt)
        .execute(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(h.worker.claim(10).await.unwrap().len(), 1);
    sqlx::query(
        "UPDATE jobs SET locked_until = now() - interval '1 second' WHERE status = 'running'",
    )
    .execute(&h.app.owner)
    .await
    .unwrap();
    h.worker.schedule_once().await.unwrap();
    let import = h.get(&owner, &path).await;
    assert_eq!(import["status"], "failed", "{import}");
    assert!(
        import["error"].as_str().unwrap().contains("worker stopped"),
        "{import}"
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn rules_email_contacts_and_the_email_shows_on_their_timeline(pool: PgPool) {
    let h = harness(pool, Settings::default()).await;
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    h.post(
        &owner,
        "/api/v1/templates",
        json!({ "kind": "email", "key": "hello", "name": "Hello",
                "subject": "Hello {{ contact.name }}", "body": "<p>From {{ workspace.name }}</p>" }),
    )
    .await;
    h.post(
        &owner,
        "/api/v1/rules",
        json!({ "name": "Say hello", "trigger": { "kind": "event", "topic": "contact.created" },
                "conditions": { "field": "contact.email", "op": "is_not_empty" },
                "actions": [{ "type": "send_email", "template": "hello", "to": { "field": "contact.email" } }] }),
    )
    .await;
    // Rules sign with the workspace's signature, never a member's.
    for (path, signature) in [("workspace", "The Acme team"), ("mine", "Sam")] {
        h.app
            .put(&format!("/api/v1/signatures/{path}"))
            .signed_in(&owner)
            .json(json!({ "signature": signature }))
            .send()
            .await
            .ok();
    }
    h.drain().await;
    let before = h.mailer.sent().len();

    let org = h
        .post(
            &owner,
            "/api/v1/organisations",
            json!({ "name": "Blue Room" }),
        )
        .await
        .id("/id");
    let jo = h
        .post(
            &owner,
            "/api/v1/contacts",
            json!({ "name": "Jo", "organisation_id": org, "emails": ["jo@example.com"] }),
        )
        .await
        .id("/id");
    h.post(&owner, "/api/v1/contacts", json!({ "name": "No Email" }))
        .await;
    h.drain().await;

    let sent = h.mailer.sent();
    assert_eq!(sent.len(), before + 1);
    assert_eq!(sent.last().unwrap().to, "jo@example.com");
    assert_eq!(sent.last().unwrap().subject, "Hello Jo");
    let text = &sent.last().unwrap().text;
    assert!(text.ends_with("\n-- \nThe Acme team"), "{text}");
    let html = sent.last().unwrap().html.clone().unwrap_or_default();
    assert!(
        html.contains("The Acme team") && !html.contains("Sam<"),
        "{html}"
    );

    for path in [
        format!("/api/v1/contacts/{jo}/timeline"),
        format!("/api/v1/organisations/{org}/timeline"),
    ] {
        let timeline = h.get(&owner, &path).await;
        let message = timeline["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["action"] == "message.sent")
            .unwrap_or_else(|| panic!("{path}: {timeline}"));
        assert_eq!(message["message"]["to"], "jo@example.com");
        assert_eq!(message["message"]["subject"], "Hello Jo");
        assert_eq!(message["message"]["status"], "sent");
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_outlook_export_imports_with_its_corrections(pool: PgPool) {
    let h = harness(pool, Settings::default()).await;
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    let csv = "Title,First Name,Middle Name,Last Name,Company,Job Title,Business Phone,Mobile Phone,E-mail Address,E-mail 2 Address,Categories\n\
               Ms,Priya,,Patel,Harbour Arts,Programmer,020 7946 0000,07700 900111,priya@harbour.example,,Festivals;VIP\n\
               Mr,Tom,,Lee,,,,,tom-at-nowhere,,\n\
               Mx,Sam,,Oak,,,,,sam@oak.example,,\n";
    let view = h
        .import(
            &owner,
            "auto",
            csv,
            json!({
                "edits": [{ "row": 3, "column": 8, "value": "tom@nowhere.example" }],
                "skip_rows": [4],
            }),
        )
        .await;
    assert_eq!(view["status"], "done", "{view}");
    assert_eq!(
        (
            view["created"].as_i64(),
            view["skipped"].as_i64(),
            view["failed"].as_i64()
        ),
        (Some(2), Some(1), Some(0)),
        "{view}"
    );
    let list = h.get(&owner, "/api/v1/contacts?sort=name").await;
    assert_eq!(names(&list["items"]), ["Priya Patel", "Tom Lee"]);
    let priya = &list["items"][0];
    // Outlook's Title (Ms) is not a role; both phones and the categories come across.
    assert_eq!(priya["role"], "Programmer");
    assert_eq!(priya["phones"].as_array().map(Vec::len), Some(2));
    assert_eq!(priya["tags"], json!(["festivals", "vip"]));
    assert_eq!(list["items"][1]["emails"], json!(["tom@nowhere.example"]));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn uploads_never_run_are_cleaned_up(pool: PgPool) {
    let h = harness(pool, Settings::default()).await;
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    let mut ids = Vec::new();
    for _ in 0..2 {
        ids.push(
            h.post(
                &owner,
                "/api/v1/imports",
                json!({ "entity": "contact", "csv": "Name\nX\n" }),
            )
            .await
            .id("/import/id"),
        );
    }
    // One is old and was never run; the other ran.
    h.post(
        &owner,
        &format!("/api/v1/imports/{}/run", ids[1]),
        json!({ "columns": ["name"] }),
    )
    .await;
    sqlx::query("UPDATE imports SET created_at = now() - interval '8 days'")
        .execute(&h.app.owner)
        .await
        .unwrap();
    h.worker.cleanup_once().await.unwrap();

    let left: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM imports")
        .fetch_all(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(left, [ids[1]]);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_pitching_workbook_imports_every_sheet_with_its_people(pool: PgPool) {
    let h = harness(pool, Settings::default()).await;
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    let kemi = h
        .post(&owner, "/api/v1/talent", json!({ "name": "DJ Kemi" }))
        .await
        .id("/id");

    // Two sheets of venues with their bookers, rows coloured by how things
    // stand, and a sheet of notes that is not a list.
    let xlsx = include_bytes!("../../core-engines/tests/fixtures/pitching.xlsx");
    let upload = h
        .post(
            &owner,
            "/api/v1/imports",
            json!({ "entity": "auto", "filename": "pitching.xlsx",
                    "workbook": data_encoding::BASE64.encode(xlsx) }),
        )
        .await;
    assert_eq!(
        upload.body["import"]["entity"], "organisation",
        "{}",
        upload.body
    );
    assert_eq!(
        upload.body["sheets"],
        json!([
            { "name": "UK & Ireland", "rows": 4, "included": true },
            { "name": "Europe", "rows": 1, "included": true },
            { "name": "Notes", "rows": 1, "included": false },
        ])
    );
    assert_eq!(
        upload.body["suggested"],
        json!([
            "name",
            "address.city",
            "address.country",
            "contact.name",
            "contact.emails",
            "contact.phones",
            "interested_in",
            "follow_up_on",
            "notes",
            "region",
            "status"
        ])
    );
    let id = upload.id("/import/id");
    let plan = json!({ "columns": upload.body["suggested"] });

    let dry = h
        .post(
            &owner,
            &format!("/api/v1/imports/{id}/dry-run"),
            plan.clone(),
        )
        .await;
    let d = &dry.body;
    // The Grand's second booker is added to it rather than a second Grand.
    assert_eq!(
        (&d["create"], &d["update"], &d["fail"], &d["new_contacts"]),
        (&json!(4), &json!(1), &json!(0), &json!(4)),
        "{d}"
    );
    let to_check: Vec<(i64, String)> = d["to_check"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| {
            r["issues"].as_array().unwrap().iter().map(|i| {
                (
                    i["row"].as_i64().unwrap(),
                    i["field"].as_str().unwrap().to_owned(),
                )
            })
        })
        .collect();
    assert_eq!(
        to_check,
        [
            (4, "notes".to_owned()),
            (4, "contact.name".to_owned()),
            (5, "do_not_contact".to_owned()),
        ]
    );
    let readings: Vec<(String, String, Value, i64)> = d["readings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["field"].as_str().unwrap().to_owned(),
                r["text"].as_str().unwrap().to_owned(),
                r["read_as"].clone(),
                r["rows"].as_i64().unwrap(),
            )
        })
        .collect();
    let read = |field: &str, text: &str| {
        readings
            .iter()
            .find(|r| r.0 == field && r.1 == text)
            .map(|r| (r.2.clone(), r.3))
    };
    assert_eq!(read("region", "UK & Ireland"), Some((Value::Null, 4)));
    assert_eq!(
        read("region", "Europe"),
        Some((json!("Europe (incl. UK)"), 1))
    );
    assert_eq!(
        read("status", "green"),
        Some((json!("Booked or worked with"), 2))
    );
    assert_eq!(
        read("status", "amber"),
        Some((json!("Talking or interested"), 1))
    );
    assert_eq!(read("status", "red"), Some((Value::Null, 1)));
    assert_eq!(
        read("address.country", "England"),
        Some((json!("United Kingdom"), 1))
    );
    assert_eq!(
        read("address.country", "Frnace"),
        Some((json!("France (a near miss)"), 1))
    );
    assert_eq!(read("address.country", "Ireland"), None);
    assert_eq!(read("interested_in", "Unknown Act"), Some((Value::Null, 1)));

    h.post(&owner, &format!("/api/v1/imports/{id}/run"), plan)
        .await;
    h.drain().await;
    let view = h.get(&owner, &format!("/api/v1/imports/{id}")).await;
    assert_eq!(view["status"], "done", "{view}");
    assert_eq!(
        (
            view["created"].as_i64(),
            view["updated"].as_i64(),
            view["failed"].as_i64()
        ),
        (Some(4), Some(1), Some(0)),
        "{view}"
    );

    let orgs = h.get(&owner, "/api/v1/organisations?sort=name").await;
    assert_eq!(
        names(&orgs["items"]),
        ["Le Club", "Owl Bar", "Plain Hall", "The Grand"]
    );
    let by_name = |n: &str| {
        orgs["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == n)
            .unwrap()
            .clone()
    };
    let grand = by_name("The Grand");
    assert_eq!(grand["status"], "booked");
    assert_eq!(grand["country_code"], "GB");
    // Europe, from its country: not pinned to the sheet's region.
    assert_eq!(
        (&grand["region"], &grand["region_picked"]),
        (&json!("europe"), &json!(false)),
        "{grand}"
    );
    assert_eq!(grand["follow_up_on"], "2027-03-01");
    let grand = h
        .get(
            &owner,
            &format!("/api/v1/organisations/{}", grand["id"].as_str().unwrap()),
        )
        .await;
    assert_eq!(grand["interested_in"][0]["id"], json!(kemi));
    assert_eq!(by_name("Owl Bar")["status"], "talking");
    assert_eq!(by_name("Le Club")["country_code"], "FR");
    assert_eq!(by_name("Le Club")["status"], Value::Null);
    assert_eq!(by_name("Plain Hall")["do_not_contact"], true);

    let people: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.name, o.name FROM contacts c JOIN organisations o ON o.id = c.organisation_id ORDER BY c.name COLLATE \"C\"",
    )
    .fetch_all(&h.app.owner)
    .await
    .unwrap();
    let people: Vec<(&str, &str)> = people
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    assert_eq!(
        people,
        [
            ("Jo Booker", "The Grand"),
            ("Marie Roux", "Le Club"),
            ("Sam Promo", "The Grand"),
            ("owl@bar.test", "Owl Bar"),
        ]
    );

    // The same sheet again finds everyone: nothing new, nobody twice.
    let again = h
        .post(
            &owner,
            "/api/v1/imports",
            json!({ "entity": "organisation", "workbook": data_encoding::BASE64.encode(xlsx) }),
        )
        .await;
    let id = again.id("/import/id");
    let dry = h
        .post(
            &owner,
            &format!("/api/v1/imports/{id}/dry-run"),
            json!({ "columns": again.body["suggested"] }),
        )
        .await;
    assert_eq!(
        (
            &dry.body["create"],
            &dry.body["update"],
            &dry.body["new_contacts"]
        ),
        (&json!(0), &json!(0), &json!(0)),
        "{}",
        dry.body
    );

    // Bookers by name at the place they work count as the same person in a
    // contact import too.
    let view = h
        .import(
            &owner,
            "contact",
            "Name,Company,Role\nSam Promo,The Grand,Promoter\n",
            json!({ "on_match": "update" }),
        )
        .await;
    assert_eq!(view["updated"], 1, "{view}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM contacts WHERE name = 'Sam Promo'")
        .fetch_one(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_import_can_be_undone_keeping_what_changed_since(pool: PgPool) {
    let h = harness(pool, Settings::default()).await;
    let (_, owner) = h.app.workspace_with_owner("acme").await;
    h.post(
        &owner,
        "/api/v1/contacts",
        json!({ "name": "Already Here", "emails": ["here@example.com"] }),
    )
    .await;
    let csv = "Name,Email,Company\n\
               Ann Lee,ann@example.com,Blue Room\n\
               Ben Ode,ben@example.com,Red Room\n\
               Cy Moss,cy@example.com,Red Room\n\
               Already Here,here@example.com,\n";
    let path = h.queue_import(&owner, "contact", csv, json!({ "on_match": "update" })).await;
    let id = path.rsplit('/').next().unwrap().to_owned();
    // Not finished yet: nothing to undo.
    let early = h
        .app
        .post(&format!("{path}/undo"))
        .signed_in(&owner)
        .send()
        .await;
    assert_eq!(early.status, axum::http::StatusCode::CONFLICT, "{}", early.body);
    h.drain().await;
    let view = h.get(&owner, &path).await;
    assert_eq!((view["created"].as_i64(), view["updated"].as_i64()), (Some(3), Some(1)), "{view}");

    // Cy is edited after the import, so Cy and the Red Room stay.
    let list = h.get(&owner, "/api/v1/contacts?sort=name").await;
    let cy = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Cy Moss")
        .unwrap()
        .clone();
    let res = h
        .app
        .patch(&format!("/api/v1/contacts/{}", cy["id"].as_str().unwrap()))
        .signed_in(&owner)
        .header("If-Match", &cy["version"].to_string())
        .json(json!({ "role": "Booker" }))
        .send()
        .await;
    assert!(res.status.is_success(), "{}", res.body);

    let undo = h.post(&owner, &format!("{path}/undo"), json!({})).await;
    assert_eq!(undo.body["status"], "undoing");
    h.drain().await;
    let view = h.get(&owner, &path).await;
    assert_eq!(view["status"], "undone", "{view}");
    // Ann, Ben and the Blue Room go; Cy and the Red Room are kept.
    assert_eq!((view["undone"].as_i64(), view["kept"].as_i64()), (Some(3), Some(2)), "{view}");
    let list = h.get(&owner, "/api/v1/contacts?sort=name").await;
    assert_eq!(names(&list["items"]), ["Already Here", "Cy Moss"]);
    let orgs = h.get(&owner, "/api/v1/organisations?sort=name").await;
    assert_eq!(names(&orgs["items"]), ["Red Room"]);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM import_records WHERE import_id = $1::uuid")
        .bind(&id)
        .fetch_one(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(left, 0);

    // Once is enough.
    let again = h
        .app
        .post(&format!("{path}/undo"))
        .signed_in(&owner)
        .send()
        .await;
    assert_eq!(again.status, axum::http::StatusCode::CONFLICT);
}
