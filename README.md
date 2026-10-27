# OnHold

The multi-tenant platform core: one Rust API and worker, one Postgres database, one React frontend, every tenant isolated by row-level security. The full spec is in [`docs/SPEC.md`](docs/SPEC.md); progress is in `docs/MILESTONE-N.md` and decisions in [`docs/DECISIONS.md`](docs/DECISIONS.md).

## Run it

```sh
docker compose -f infra/docker-compose.yml up --build
curl localhost:8080/healthz   # {"status":"ok"}
curl localhost:8080/readyz    # {"status":"ready"}
```

## Develop

Needs Rust (the version in `rust-toolchain.toml` installs itself), Postgres 16 and `sqlx-cli` (`cargo install sqlx-cli --no-default-features --features postgres,rustls`).

```sh
cp .env.example .env
docker compose -f infra/docker-compose.yml up -d postgres
sqlx migrate run --source crates/core-db/migrations
cargo run -p onhold -- api        # or: worker, migrate
cargo test --workspace
```

After adding or changing a `query!` macro, run `cargo sqlx prepare --workspace` and commit `.sqlx/`; CI fails otherwise.

New migrations are always reversible: `sqlx migrate add -r --source crates/core-db/migrations <name>`.
