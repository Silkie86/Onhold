# Milestone 0: Skeleton

**Demonstrable:** `docker compose -f infra/docker-compose.yml up --build` gives a running API that answers `/healthz`, and CI is green.

## Built

- Cargo workspace with the spec's crate layout: `core-domain`, `core-db`, `core-engines`, `core-modules`, `core-integrations`, `core-api`, `core-worker`, `plugin-sdk`, plus `crates/onhold` for the binary and `crates/plugins/` for tenant plugins. Workspace lints forbid `unsafe` and warn on `unwrap`, `todo!` and `dbg!`.
- `onhold` binary with `api`, `worker` and `migrate` subcommands, configured from environment variables, with clean shutdown on SIGTERM and Ctrl-C (in-flight requests drain).
- `core-db`: lazy Postgres pool, embedded reversible migrations (`*.up.sql` / `*.down.sql`), compile-time-checked queries with offline data in `.sqlx/`.
- `core-api`: axum router with request ids (generated or propagated via `x-request-id`), a tracing span per request, 30s timeout, 1 MiB body limit, an empty `/api/v1` mount, `/healthz` (liveness) and `/readyz` (database reachable).
- `core-worker`: the worker lifecycle (start, heartbeat, stop) with its own `/healthz` and `/readyz` on port 8081.
- Structured JSON logging via `tracing`, levels from `RUST_LOG`.
- Docker: one image for every role, running as a non-root user; compose stack with Postgres 16, a one-shot `migrate`, `api` and `worker`, all with health checks.
- CI (GitHub Actions): `cargo fmt --check`, offline sqlx build, `clippy -D warnings`, migrations against Postgres 16, `sqlx prepare --check`, `cargo test`, `cargo audit`, Docker image build. Dependabot for Cargo, Actions and Docker.

## Verified

- 10 tests pass: probes with and without a database, request-id generation and propagation, 404 on unknown routes, every migration applies, reverts and re-applies, worker stops promptly on shutdown.
- `docker compose up`: migrate ran and exited 0, `api` and `worker` became healthy, `/healthz` → 200 `{"status":"ok"}`, `/readyz` → 200 `{"status":"ready"}` on both, `docker compose stop` logged a clean shutdown.

## Skipped, and why

- **OpenTelemetry export, Grafana/Axiom, Sentry.** No collector or account exists yet; `tracing` is in place so adding an OTLP layer is a few lines once hosting is chosen.
- **OpenAPI (`utoipa`) and the generated TS client.** There are no API endpoints yet beyond probes; both arrive with the first real routes in M1.
- **Frontend CI (typecheck, unit tests, Playwright).** No frontend code until M3 (see DECISIONS.md).
- **Staging/prod deploy on merge.** Waiting on the hosting decision (Fly.io vs Hetzner + Coolify, EU vs EU + US).
- **testcontainers.** Replaced by `#[sqlx::test]` (see DECISIONS.md).

## Open before M1

- Can one login belong to many workspaces? The spec assumes yes.
