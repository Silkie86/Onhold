# Web

TypeScript frontends on the same public API.

| Path | What | Arrives in |
| --- | --- | --- |
| `packages/api-client/` | Client generated from the OpenAPI spec | M1 |
| `packages/ui/` | Design tokens, shadcn/ui components, `DataTable`, `EntityForm`… | M3 |
| `apps/app/` | Staff web app | M3 |
| `apps/portal/` | Talent portal | M7 |
| `tenants/<slug>/` | Per-tenant pages, widgets and overrides, loaded by slug | M3 (template in M8) |

Empty until then on purpose: no frontend dependencies are added before there is code that uses them.
