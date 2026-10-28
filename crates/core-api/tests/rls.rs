//! Database-level isolation: whatever the Rust code does, the role it
//! connects as cannot read or write across workspaces.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::TestApp;
use serde_json::json;
// Table names below come from the catalog, not from input.
use sqlx::{AssertSqlSafe, PgPool, Row};

async fn tables(owner: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' AND tablename <> '_sqlx_migrations' ORDER BY 1")
        .fetch_all(owner)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn every_table_has_row_level_security(pool: PgPool) {
    let missing: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') AND c.relname <> '_sqlx_migrations' AND NOT c.relrowsecurity",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        missing.is_empty(),
        "tables without row-level security: {missing:?}"
    );
    assert!(tables(&pool).await.len() >= 13);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_app_role_cannot_bypass_policies(pool: PgPool) {
    let role = sqlx::query(
        "SELECT rolsuper, rolbypassrls, rolcanlogin FROM pg_roles WHERE rolname = 'onhold_app'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!role.get::<bool, _>("rolsuper"));
    assert!(!role.get::<bool, _>("rolbypassrls"));
    assert!(!role.get::<bool, _>("rolcanlogin"));

    let owned: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE tableowner = 'onhold_app'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(owned, 0, "a table owner skips its own policies");

    let deletable: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.role_table_grants WHERE grantee = 'onhold_app' AND privilege_type IN ('DELETE', 'TRUNCATE')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    // Link rows (which talent an enquiry or booking is for) are replaced
    // when the list changes; the parent's audit entry keeps the history.
    // Busy times copied from a talent's own calendar are replaced as the
    // calendar changes; the calendar's row keeps the history. Which event
    // stands for which booking in a member's own calendar goes with the
    // event; the connection's row keeps the history.
    let links = [
        "enquiry_talent",
        "booking_talent",
        "calendar_busy",
        "calendar_push_events",
    ];
    let deletable: Vec<&String> = deletable
        .iter()
        .filter(|t| !links.contains(&t.as_str()))
        .collect();
    assert!(
        deletable.is_empty(),
        "nothing is hard-deleted by the app: {deletable:?}"
    );

    let app = TestApp::new(pool).await;
    let user: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(user, "onhold_app", "the API pool must run as the app role");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn without_context_the_app_sees_nothing(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("alpha").await;
    app.workspace_with_owner("bravo").await;
    app.post("/api/v1/api-keys")
        .signed_in(&owner)
        .json(json!({ "name": "k", "permissions": ["member:read"] }))
        .send()
        .await
        .ok();
    app.post("/api/v1/auth/magic-link")
        .json(json!({ "email": "owner@alpha.test" }))
        .send()
        .await;

    for table in tables(&app.owner).await {
        let total: i64 = sqlx::query_scalar(AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(&app.owner)
            .await
            .unwrap();
        // No SELECT grant at all (outbox, idempotency keys) is as good as no rows.
        let visible = match sqlx::query_scalar::<_, i64>(AssertSqlSafe(format!(
            "SELECT count(*) FROM {table}"
        )))
        .fetch_one(&app.state.db)
        .await
        {
            Ok(n) => n,
            Err(err) if err.to_string().contains("permission denied") => 0,
            Err(err) => panic!("{table}: {err}"),
        };
        assert_eq!(
            visible, 0,
            "{table}: {visible} of {total} rows visible with no context"
        );
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn in_one_workspace_the_app_sees_and_writes_only_that_workspace(pool: PgPool) {
    let app = TestApp::new(pool).await;
    let (alpha, owner) = app.workspace_with_owner("alpha").await;
    let (bravo, _) = app.workspace_with_owner("bravo").await;
    app.post("/api/v1/api-keys")
        .signed_in(&owner)
        .json(json!({ "name": "k", "permissions": ["member:read"] }))
        .send()
        .await
        .ok();

    let scoped: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.columns WHERE table_schema = 'public' AND column_name = 'workspace_id' ORDER BY 1",
    )
    .fetch_all(&app.owner)
    .await
    .unwrap();
    assert!(scoped.len() >= 6, "{scoped:?}");

    let mut tx = core_db::begin(&app.state.db, Some(alpha), None)
        .await
        .unwrap();
    for table in &scoped {
        if table == "outbox" || table == "idempotency_keys" {
            continue; // not readable by the app at all
        }
        let foreign: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT count(*) FROM {table} WHERE workspace_id IS DISTINCT FROM $1"
        )))
        .bind(alpha.0)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(
            foreign, 0,
            "{table} shows rows from outside the workspace in context"
        );
    }
    let workspaces: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM workspaces")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert_eq!(workspaces, [alpha.0]);

    // Writes aimed at another workspace are refused by the policy itself.
    let err = sqlx::query("INSERT INTO role_permissions (workspace_id, role, permission) VALUES ($1, 'viewer', 'audit:read')")
        .bind(bravo.0)
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("row-level security"), "{err}");
    drop(tx);

    let mut tx = core_db::begin(&app.state.db, Some(alpha), None)
        .await
        .unwrap();
    let moved = sqlx::query("UPDATE memberships SET workspace_id = $1")
        .bind(bravo.0)
        .execute(&mut *tx)
        .await;
    assert!(
        moved
            .unwrap_err()
            .to_string()
            .contains("row-level security")
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_app_cannot_promote_anyone_to_superadmin(pool: PgPool) {
    let app = TestApp::new(pool).await;
    app.workspace_with_owner("alpha").await;
    let me = app.user_id("owner@alpha.test").await;
    let mut tx = core_db::begin(&app.state.db, None, Some(me)).await.unwrap();
    let err = sqlx::query("UPDATE users SET is_superadmin = true WHERE id = $1")
        .bind(me.0)
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("permission denied"), "{err}");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_lookup_functions_answer_only_exact_keys(pool: PgPool) {
    let app = TestApp::new(pool).await;
    app.workspace_with_owner("alpha").await;
    // Pre-authentication lookups are the one window past row-level security;
    // each needs the exact secret-derived key and returns one row at most.
    let none: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT user_id FROM auth_find_session(''::bytea)")
            .fetch_optional(&app.state.db)
            .await
            .unwrap();
    assert!(none.is_none());
    let none: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT user_id FROM auth_find_user_by_email('%')")
            .fetch_optional(&app.state.db)
            .await
            .unwrap();
    assert!(none.is_none());
    let listed = sqlx::query("SELECT * FROM admin_list_workspaces()")
        .fetch_all(&app.state.db)
        .await;
    assert!(listed.unwrap_err().to_string().contains("superadmin"));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn the_worker_role_sees_only_the_queue(pool: PgPool) {
    let role = sqlx::query(
        "SELECT rolsuper, rolbypassrls, rolcanlogin FROM pg_roles WHERE rolname = 'onhold_worker'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!role.get::<bool, _>("rolsuper"));
    assert!(!role.get::<bool, _>("rolbypassrls"));
    assert!(!role.get::<bool, _>("rolcanlogin"));

    let app = TestApp::new(pool).await;
    let (_, owner) = app.workspace_with_owner("alpha").await;
    app.post("/api/v1/things")
        .signed_in(&owner)
        .json(json!({ "name": "a thing" }))
        .send()
        .await
        .ok();
    let options = app.owner.connect_options().as_ref().clone();
    let worker =
        core_db::worker_pool_options(sqlx::postgres::PgPoolOptions::new().max_connections(1))
            .connect_with(options)
            .await
            .unwrap();
    let user: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&worker)
        .await
        .unwrap();
    assert_eq!(user, "onhold_worker");

    // The queue tables, plus the few columns scheduling, cleanup and the
    // bookings sweep (holds lapsing, bookings ending, quotes expiring),
    // invoices falling overdue, file cleanup, exchange rates, mailboxes due
    // to be read and snoozed conversations coming back need.
    let queue = [
        "bookings",
        "quotes",
        "jobs",
        "job_runs",
        "outbox",
        "outbox_pruned",
        "rules",
        "workspaces",
        "idempotency_keys",
        "imports",
        "files",
        "invoices",
        "fx_rates",
        "integrations",
        "threads",
        "sender_domains",
        "talent_calendars",
        "calendar_pushes",
    ];
    for table in tables(&app.owner).await {
        let read =
            sqlx::query_scalar::<_, i64>(AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&worker)
                .await;
        if queue.contains(&table.as_str()) {
            assert!(read.is_ok(), "{table}: {read:?}");
        } else {
            let err = read.expect_err(&table).to_string();
            assert!(err.contains("permission denied"), "{table}: {err}");
        }
    }
    // Only the columns it needs, even in the tables it can read.
    for query in [
        "SELECT name FROM workspaces",
        "SELECT actions FROM rules",
        "SELECT body FROM idempotency_keys",
        "SELECT csv FROM imports",
        "UPDATE imports SET status = 'queued'",
        "SELECT title FROM bookings",
        "SELECT fee_minor FROM bookings",
        "UPDATE bookings SET status = 'expired'",
        "SELECT line_items FROM quotes",
        "SELECT text_content FROM files",
        "SELECT secret FROM integrations",
        "SELECT mail_cursor FROM integrations",
        "SELECT account FROM integrations",
        "UPDATE integrations SET next_poll_at = now()",
        "SELECT subject FROM threads",
        "SELECT participants FROM threads",
        "UPDATE threads SET status = 'open'",
        "SELECT domain FROM sender_domains",
        "SELECT provider_id FROM sender_domains",
        "UPDATE sender_domains SET next_check_at = now()",
        "SELECT secret FROM talent_calendars",
        "SELECT account FROM talent_calendars",
        "UPDATE talent_calendars SET next_poll_at = now()",
        "SELECT starts_at FROM calendar_busy",
        "SELECT secret FROM calendar_pushes",
        "SELECT account FROM calendar_pushes",
        "SELECT remote_calendar FROM calendar_pushes",
        "UPDATE calendar_pushes SET next_sync_at = now()",
        "SELECT remote_id FROM calendar_push_events",
        "SELECT total_minor FROM invoices",
        "UPDATE invoices SET status = 'paid'",
    ] {
        let err = sqlx::query(query).fetch_all(&worker).await.unwrap_err();
        assert!(
            err.to_string().contains("permission denied"),
            "{query}: {err}"
        );
    }
    // The outbox is readable (it publishes it) but not writable beyond publishing.
    let err = sqlx::query("UPDATE outbox SET payload = '{}'")
        .execute(&worker)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("permission denied"), "{err}");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn new_tables_get_no_default_grants(pool: PgPool) {
    // Privileges are granted table by table; nothing is granted on future
    // tables by default, so a new table is unreachable until a migration says otherwise.
    let defaults: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_default_acl d JOIN pg_namespace n ON n.oid = d.defaclnamespace WHERE n.nspname = 'public'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(defaults, 0);
    let public: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.role_table_grants WHERE grantee = 'PUBLIC' AND table_schema = 'public'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(public.is_empty(), "granted to everyone: {public:?}");
}
