# Decisions

One line per non-obvious technical decision, with the reason. Newest at the bottom.

| Date | Milestone | Decision | Why |
| --- | --- | --- | --- |
| 2026-10-27 | M0 | A ninth crate, `crates/onhold`, holds the single binary; `core-api` and `core-worker` are libraries. | The spec wants one binary for API and worker; neither library should depend on the other to provide `main`. |
| 2026-10-27 | M0 | Roles are subcommands (`onhold api`, `onhold worker`, `onhold migrate`) instead of a `--worker` flag. | Same binary and image as the spec asks; subcommands give each role its own typed options and leave room for `onhold admin …` (M8). |
| 2026-10-27 | M0 | Migrations run as a separate one-shot `onhold migrate` step, never on API start. | The spec requires migrations to run before the new version starts; doing it in-process would race when several API replicas boot. |
| 2026-10-27 | M0 | The pool connects lazily; `/healthz` never touches the database, `/readyz` does. | A database blip must make instances unready, not get every container restarted by the liveness probe. |
| 2026-10-27 | M0 | Integration tests use `#[sqlx::test]` (a fresh throwaway database per test on a real Postgres) instead of testcontainers. | Same guarantee (real Postgres, isolated per test) with no Docker-in-CI dependency; CI provides Postgres as a service container. Revisit if a test needs a different Postgres config. |
| 2026-10-27 | M0 | sqlx 0.9, axum 0.8, tower-http 0.7, Rust 1.97 pinned in `rust-toolchain.toml`. | Current stable releases at build time; the toolchain pin keeps CI, Docker and local builds identical. |
| 2026-10-27 | M0 | UUIDv7 ids are generated in Rust (`uuid` v7), not by a Postgres default. | Postgres 16 has no native `uuidv7()`; a single generator avoids two implementations disagreeing. |
| 2026-10-27 | M0 | `citext` and `pg_trgm` are enabled in the first migration. | Needed for case-insensitive emails/slugs (M1) and fuzzy search/dedup (M3); enabling extensions early keeps later migrations plain DDL. |
| 2026-10-27 | M0 | Logs are JSON by default (`LOG_FORMAT=pretty` for dev), with a `request_id` on every request span, generated or taken from `x-request-id` and echoed back. | The spec requires request ids on every log line; JSON is what log shippers ingest. `workspace_id` joins the span in M1. |
| 2026-10-27 | M0 | No frontend dependencies or CI steps yet; `web/` holds the agreed layout only. | Nothing to type-check until M1 generates the API client and M3 builds the shell; adding them earlier means maintaining unused deps. |
