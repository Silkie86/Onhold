# Web

TypeScript frontends on the same public API.

| Path | What | Arrives in |
| --- | --- | --- |
| `packages/api-client/` | Typed client generated from the OpenAPI spec (`pnpm generate`) | M1 (done) |
| `packages/ui/` | Design tokens, shadcn/ui components, `DataTable`, `EntityForm`… | M3 |
| `apps/app/` | Staff web app | M3 |
| `apps/portal/` | Talent portal | M7 |
| `tenants/<slug>/` | Per-tenant pages, widgets and overrides, loaded by slug | M3 (template in M8) |

The apps stay empty until their milestones: no frontend dependencies are added before there is code that uses them.
