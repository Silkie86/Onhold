# OnHold

The multi-tenant platform core: one Rust API and worker, one Postgres database, one React frontend, every tenant isolated by row-level security. The full spec is in [`docs/SPEC.md`](docs/SPEC.md); progress is in `docs/MILESTONE-N.md` and decisions in [`docs/DECISIONS.md`](docs/DECISIONS.md).

## Run it

```sh
docker compose -f infra/docker-compose.yml up --build
curl localhost:8080/healthz   # {"status":"ok"}
curl localhost:8080/readyz    # {"status":"ready"}
```

Then create the first workspace. The owner's invitation is emailed by the worker; in this stack every email lands in Mailpit at `localhost:8025`, and the command also prints the link:

```sh
docker compose -f infra/docker-compose.yml run --rm api admin workspace create \
  --slug demo --name "Demo Agency" --country GB --currency GBP \
  --timezone Europe/London --owner-email you@example.com
```

The API is described at `localhost:8080/api/openapi.json`. Operator tools live under `onhold admin` (`--help` lists them): creating workspaces, granting superadmin to a user who has signed in, generating encryption keys.

## Develop

Needs Rust (the version in `rust-toolchain.toml` installs itself), Postgres 16 and `sqlx-cli` (`cargo install sqlx-cli --no-default-features --features postgres,rustls`).

```sh
cp .env.example .env              # then set ENCRYPTION_KEYS: cargo run -p onhold -- admin generate-key
docker compose -f infra/docker-compose.yml up -d postgres mailpit
sqlx migrate run --source crates/core-db/migrations
cargo run -p onhold -- api        # in another terminal: cargo run -p onhold -- worker
cargo test --workspace
```

The worker publishes events, runs automation rules and sends email and webhooks. Without `SMTP_URL` it writes emails to its log instead of sending them, with links removed; it will not start that way when `PUBLIC_BASE_URL` is https.

After adding or changing a `query!` macro, run `cargo sqlx prepare --workspace` and commit `.sqlx/`; CI fails otherwise.

After changing the API, refresh the TypeScript client and commit both files (CI fails otherwise):

```sh
cargo run -p onhold -- openapi > web/packages/api-client/openapi.json
pnpm --dir web install && pnpm --dir web generate
```

New migrations are always reversible: `sqlx migrate add -r --source crates/core-db/migrations <name>`.

## Desktop and mobile apps

The apps (Windows, macOS, Linux; iOS and Android next) are the same web app in a [Tauri](https://tauri.app) shell, `web/apps/app/src-tauri`, with an offline copy of the CRM and a queue of changes made without a connection. They sign in by showing a code that the person approves on the website (`/link-app`). Building them needs the Rust toolchain plus, on Linux, `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev patchelf` (see [Tauri's prerequisites](https://tauri.app/start/prerequisites/) for Windows and macOS).

```sh
cd web/apps/app
CORS_ORIGINS=http://localhost:5173 cargo run -p onhold -- serve   # the API, letting the dev app call it
VITE_API_URL=http://localhost:8080 pnpm app:dev                     # the app in a window, reloading on change
VITE_API_URL=https://onhold.example.com pnpm app:build              # installers in src-tauri/target/release/bundle
node scripts/brand.mjs brands/example.json                          # a client's own name and icon (see the file)
```

`VITE_APP_MODE=app pnpm dev` runs the app's code in a plain browser; the `app` Playwright project tests sign-in, working offline and settling a conflict that way. CI builds unsigned installers for all three desktop systems on every change (the `desktop-*` artifacts).
