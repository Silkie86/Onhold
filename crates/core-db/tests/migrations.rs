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

#[sqlx::test]
async fn ping_succeeds_against_a_live_database(pool: PgPool) {
    core_db::ping(&pool).await.expect("ping");
}
