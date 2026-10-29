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
                   '00000000-0000-0000-0000-000000000002');"#,
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
}

#[sqlx::test]
async fn ping_succeeds_against_a_live_database(pool: PgPool) {
    core_db::ping(&pool).await.expect("ping");
}
