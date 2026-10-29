use core_db::MIGRATOR;
use sqlx::PgPool;

#[sqlx::test(migrations = false)]
async fn every_migration_applies_and_reverts(pool: PgPool) {
    MIGRATOR.run(&pool).await.expect("migrations apply");
    MIGRATOR.undo(&pool, 0).await.expect("migrations revert");
    MIGRATOR
        .run(&pool)
        .await
        .expect("migrations re-apply after revert");
}

#[sqlx::test(migrations = false)]
async fn messaging_reverts_with_texts_stored(pool: PgPool) {
    MIGRATOR.run(&pool).await.expect("migrations apply");
    sqlx::raw_sql(
        r#"INSERT INTO workspaces (id, slug, name, country, default_currency, default_timezone, locale)
           VALUES ('00000000-0000-0000-0000-000000000001', 'owls', 'Owls', 'GB', 'GBP', 'Europe/London', 'en-GB');
           INSERT INTO messaging_channels (id, workspace_id, kind, provider, sender, account_id, secret, hook_token)
           VALUES ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000001',
                   'sms', 'twilio', '+447700900123', 'AC1', '\x00', 'hook-token-0123456789abcdef');
           INSERT INTO threads (id, workspace_id, channel, messaging_channel_id, external_id)
           VALUES ('00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000001',
                   'sms', '00000000-0000-0000-0000-000000000002', '+447700900456'),
                  ('00000000-0000-0000-0000-000000000004', '00000000-0000-0000-0000-000000000001',
                   'email', NULL, NULL);
           INSERT INTO thread_messages (id, workspace_id, thread_id, direction, status, sent_at, messaging_channel_id)
           VALUES ('00000000-0000-0000-0000-000000000005', '00000000-0000-0000-0000-000000000001',
                   '00000000-0000-0000-0000-000000000003', 'inbound', 'received', now(),
                   '00000000-0000-0000-0000-000000000002');
           INSERT INTO files (id, workspace_id, filename, content_type, size_bytes, sha256, storage,
                              storage_key, attached_at)
           SELECT ('00000000-0000-0000-0000-00000000001' || n)::uuid,
                  '00000000-0000-0000-0000-000000000001', 'photo.jpg', 'image/jpeg', 1,
                  sha256('x'), 'database', 'k' || n, now()
           FROM generate_series(0, 1) n;
           INSERT INTO thread_messages (id, workspace_id, thread_id, direction, status, sent_at)
           VALUES ('00000000-0000-0000-0000-000000000006', '00000000-0000-0000-0000-000000000001',
                   '00000000-0000-0000-0000-000000000004', 'outbound', 'sent', now());
           -- One photo came by text only; the other was also sent by email.
           INSERT INTO thread_message_files (id, workspace_id, message_id, file_id, filename,
                                             content_type, size_bytes)
           VALUES ('00000000-0000-0000-0000-000000000020', '00000000-0000-0000-0000-000000000001',
                   '00000000-0000-0000-0000-000000000005', '00000000-0000-0000-0000-000000000010',
                   'photo.jpg', 'image/jpeg', 1),
                  ('00000000-0000-0000-0000-000000000021', '00000000-0000-0000-0000-000000000001',
                   '00000000-0000-0000-0000-000000000005', '00000000-0000-0000-0000-000000000011',
                   'photo.jpg', 'image/jpeg', 1),
                  ('00000000-0000-0000-0000-000000000022', '00000000-0000-0000-0000-000000000001',
                   '00000000-0000-0000-0000-000000000006', '00000000-0000-0000-0000-000000000011',
                   'photo.jpg', 'image/jpeg', 1);"#,
    )
    .execute(&pool)
    .await
    .expect("a text thread");
    // Back to before texts came in.
    MIGRATOR
        .undo(&pool, 20261028000041)
        .await
        .expect("messaging reverts");
    let left: Vec<String> = sqlx::query_scalar("SELECT channel FROM threads")
        .fetch_all(&pool)
        .await
        .expect("threads");
    assert_eq!(left, ["email"]);
    // The text's own photo is let go for the sweep; the shared one stays.
    let attached: Vec<(String, bool)> =
        sqlx::query_as("SELECT id::text, attached_at IS NOT NULL FROM files ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("files");
    assert_eq!(
        attached,
        [
            ("00000000-0000-0000-0000-000000000010".to_owned(), false),
            ("00000000-0000-0000-0000-000000000011".to_owned(), true),
        ]
    );
}

#[sqlx::test]
async fn ping_succeeds_against_a_live_database(pool: PgPool) {
    core_db::ping(&pool).await.expect("ping");
}
