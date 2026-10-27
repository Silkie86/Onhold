# Web

TypeScript frontends on the same public API.

| Path | What | Arrives in |
| --- | --- | --- |
| `packages/api-client/` | Typed client generated from the OpenAPI spec (`pnpm generate`) | M1 (done) |
| `packages/ui/` | Design tokens, shadcn/ui-style components, `DataTable`, `EntityForm`, `Timeline`, command palette | M3 (done) |
| `apps/app/` | Staff web app | M3 (done) |
| `apps/portal/` | Talent portal | M7 |
| `tenants/<slug>/` | Per-tenant pages, widgets and overrides, loaded by slug | With the first client that needs one (template in M8) |

## Running it

Needs Node 22 and pnpm 10. From `web/`:

```sh
pnpm install
pnpm dev          # the staff app on http://localhost:5173, proxying /api to ONHOLD_API (default http://localhost:8080)
pnpm typecheck
pnpm test         # unit tests (Vitest)
pnpm build        # static files in apps/app/dist
pnpm generate     # regenerate the API types after openapi.json changes
```

The browser test of the M3 demo (`apps/app/e2e`) starts its own API and Vite: build the Rust binary, run `onhold migrate`, set `DATABASE_URL`, `ENCRYPTION_KEYS`, `PUBLIC_BASE_URL=http://localhost:5173`, `COOKIE_SECURE=false` and `ONHOLD_BIN` (the binary's path), then run `pnpm --filter @onhold/app e2e`. CI shows the full setup.
