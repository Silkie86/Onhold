# Core Platform Build Spec

Oct 27, 2026 · @Solomon Rose

## How to use this prompt

This document is the master build prompt for a reusable platform core (working name: **OnHold**). Paste it, whole, as the first message to Claude Code in an empty repository, then work through the milestones in the last sections. Every later bespoke client app is built *on top of* this core, never by forking it.

Rules that override everything else in this document:

1. **The core is a product, not a prototype.** Write it as if 50 paying agencies will run on it. No shortcuts that would need undoing.
2. **Bespoke = data + frontend + thin modules.** Anything a single client needs that differs from another client must be expressible as configuration, custom fields, rules, templates or a frontend module. Changing the Rust core for one client is a design failure; flag it rather than do it.
3. **Ask before inventing.** Where this spec is silent, stop and ask one precise question. Where it is ambiguous, pick the option that keeps the core generic and say which you picked.
4. **Correctness over cleverness.** Prefer boring, well-typed code. Every public API has a test. Every migration is reversible.
5. **No AI features in v1.** Do not add Claude API calls, chat assistants or "smart" automation. Leave clean seams (an events bus and a drafts table) so they can be added later.
6. **Work milestone by milestone.** Finish and verify one milestone before starting the next. At the end of each, write a short `MILESTONE-N.md` saying what was built, what was skipped and why.
7. **Keep a running `DECISIONS.md`.** One line per non-obvious technical decision with the reason.

Terms used throughout: **tenant** = one paying customer (an agency, a manager, a freelance agent); **workspace** = a tenant's data boundary; **talent** = an artist, performer or creative the tenant represents; **client contact** = a venue, brand, promoter or buyer who books talent; **module** = a self-contained feature area that a tenant can switch on or off.

## Product vision and business model

OnHold is the operations backbone for the creative industries: anyone who sells or schedules people, time, spaces or kit and needs the admin around it organised and automated. First targets are freelance booking agents, small agencies and artist managers, but the core is built for every segment listed below. It replaces the spreadsheet, the inbox search and the shared calendar with one system that runs the enquiry-to-payment loop and chases the things humans forget.

**Who buys it**

| Segment | Side | Typical shape | Pain the core must solve first |
| --- | --- | --- | --- |
| Freelance booking agent | Sells | 1 person, 5–30 acts | Chasing deposits and contracts; double-booking; quotes rewritten from scratch |
| Booking agency | Sells | 2–10 staff, 30–200 acts | Who-is-handling-what; shared availability; consistent invoicing; staff handover |
| Artist / talent manager | Sells | 1–3 people, 1–10 artists, heavy logistics per date | Advancing shows; collecting fees; keeping artists informed |
| Record label manager | Sells | Small label, 5–50 artists | Release campaigns as projects; artist comms; tracking who owes what; shows around releases |
| Speaker bureau, model / influencer / creator agency | Sells | 2–20 staff, large roster | Rate cards; usage terms in contracts; many short bookings; brand-side approvals |
| Corporate / wedding entertainment agency | Sells | 1–10 staff, hundreds of acts | High enquiry volume; fast quoting; supplier confirmations; day-of logistics |
| Event manager / promoter | Buys | 1–5 people, 20–200 events a year | Line-ups with many acts per event; offers out and holds; settlements; run sheets |
| Club / venue manager, festival programmer | Buys | Venue team, weekly programme | Calendar of nights and slots; offers to agents; fee and door-split settlements; advancing |
| Tour / production manager | Both | Per tour or season | Routing dates; crew and suppliers; day sheets; expense reconciliation |

Sellers represent talent and send quotes; buyers book talent and send offers. The core supports both and a workspace can be either or both, so a label that also promotes its own nights runs on one account.

**The wider market.** The first targets above are a starting point. The core serves anyone in the creative industries who sells or schedules time, people, spaces or kit and wants the admin around it organised and automated. Nothing in the core may assume live music, a stage or a gig; those words live only in presets and vocabulary tables.

| Who | Books or sells | Typical loop the core runs |
| --- | --- | --- |
| Photographers, videographers, directors | Their own time and crew | Enquiry → quote → shoot date → deposit → delivery → balance |
| Recording, rehearsal, photo and dance studios | Rooms and engineers by the hour or day | Availability calendar → booking → deposit → session → invoice; recurring block bookings |
| Production companies, crew and session-musician fixers | A pool of people with skills and day rates | Call sheet → availability check across the pool → confirmations → timesheets → payroll export |
| Equipment and backline hire | Kit with serial numbers and quantities | Quote → hold stock → dispatch and return → damage and late fees |
| Illustrator, designer, writer and voice agents | Creatives with rate cards and usage terms | Brief → estimate → contract with licence terms → milestones → invoice |
| Galleries, curators and art consultants | Artists, works and exhibition slots | Exhibition as Project; loans and sales as bookings and invoices; consignment statements |
| Theatre, dance and comedy companies and producers | Casts, venues, tour dates | Season as Project; venue offers; contracts; settlement per performance |
| Podcast, content and media producers | Guests, studios, editors | Guest pipeline; recording slots; release schedule as Project milestones |
| Music and arts tutors, workshop leaders | Their own hours and rooms | Recurring lessons; term invoicing; attendance; parent portal in place of talent portal |
| Creative collectives and co-ops | Members' time and shared space | Member portal; fair-allocation of enquiries; shared calendar; member statements |

Each of these is a preset plus vocabulary, provided the core treats the bookable thing generically (see the domain model), supports recurring bookings and quantities, and keeps money, documents and messaging vertical-agnostic.

**How it makes money**

- One-off setup fee per client: covers onboarding, data import, branding and the first round of bespoke configuration or modules.
- Monthly fee per client: hosting, backups, bug fixes, small feature requests. Priced per workspace, optionally tiered by number of active talent or staff seats.
- Bespoke feature work beyond the monthly allowance: quoted separately, delivered as per-tenant modules on the shared core.

The core must therefore make three things cheap: **onboarding a new tenant** (minutes, not days), **shipping a fix to every tenant at once** (one deploy), and **adding a tenant-specific feature without touching anyone else** (the extension model in the next sections).

**Out of scope for v1**: AI drafting or triage, a public marketplace, ticketing, royalty or publishing accounting, mobile native apps (the web app must work well on a phone instead).

## Vertical presets and perspective

A workspace has a **perspective**: `sells` (represents talent), `buys` (books talent for its own events) or `both`. Perspective changes vocabulary and defaults, never the data model: an Enquiry becomes an **Offer** when the workspace is the one making it, a Quote becomes the agent's reply, and the same Booking row sits at the end of either flow. Every list, template and rule in the core reads its labels from a vocabulary table seeded by the preset, so "act", "artist", "speaker", "creator" and "talent" are the same entity with different names.

A **preset** is a seed bundle applied at workspace creation: perspective, vocabulary, enabled modules, custom fields, pipeline stages, tax defaults, templates and rules. Presets are data in `core-modules/presets/*.toml`, and adding a vertical means adding a file, not code.

| Preset | Perspective | Seeds beyond the shared default |
| --- | --- | --- |
| `agency` | sells | Multi-agent assignment; commission per talent; deposit + balance invoicing; hold approval off |
| `manager` | sells | Few artists, deep records; advance-sheet, rider and travel templates; artist portal fully on |
| `freelance` | sells | Single user; simplest pipeline; fast quote templates; mobile-first defaults |
| `creative` | sells | A freelance photographer, designer, writer or similar selling their own time: packages as rate cards, brief form, usage and licence terms, milestone invoicing, delivery stage |
| `label` | sells | Release and campaign Projects with milestone tasks; artist portal with release view; per-artist statements |
| `bureau` | sells | Rate cards per talent; usage and exclusivity fields on contracts; brand approval stage |
| `entertainment` | sells | High-volume enquiry form; auto-quote from rate card; supplier confirmation rules |
| `studio` | sells | Rooms and engineers as bookables; hourly and day rates; recurring block bookings; public availability page; no-show and cancellation policy rules |
| `production` | both | Crew pool with skills and day rates; call sheets; availability sweep across many people; timesheets; payroll and expense export |
| `hire` | sells | Equipment as bookables with quantities and serials; stock holds; dispatch and return checklists; damage and late-return fees |
| `promoter` | buys | Events with line-up slots; offer pipeline (hold → offer → contracted); settlement with door split and costs |
| `venue` | buys | Recurring nights as Event templates; slot calendar; promoter and agent contacts; advancing checklist |
| `tour` | both | Projects as tours with routed dates; crew as Talent members; day sheets; expense tracking |
| `collective` | both | Members as talent and users; member portal; enquiry allocation rules; shared space calendar; member statements |
| `tuition` | sells | Recurring lessons; term and block invoicing; attendance; guardian portal variant |

Presets are starting points. Everything they seed is editable in Settings afterwards, and a workspace can switch preset only by applying another bundle on top, never by losing data.

## Architecture principles

One shared, multi-tenant platform: one Rust API, one Postgres database, one React frontend, every tenant isolated by a `workspace_id` on every row and enforced at the database level. Bespoke work lives in three places only — tenant configuration (data), the frontend (per-tenant module directories) and thin backend plugins that hook the core's extension points. The core itself never carries a client's name.

&#91;embedded content: platform architecture · frontends, API, core, data, integrations\]

Requests flow top to bottom; only the worker reaches outside, and only the two highlighted boxes change from one client to the next.

**Principles the code must follow**

1. **Workspace isolation is a database guarantee, not an application habit.** Postgres row-level security on every tenant table, keyed by a session variable the API sets per request. A query that forgets the filter still cannot leak.
2. **Configuration over code.** Pipeline stages, statuses, custom fields, automation rules, email and document templates, currencies, tax rates, branding and enabled modules are rows in the database with a UI to edit them. Defaults are seeded per tenant at creation.
3. **Every meaningful change emits an event.** `booking.confirmed`, `invoice.overdue`, `contact.created` and so on go onto an internal event bus (Postgres-backed outbox table, not an external broker in v1). Automation rules, integrations, webhooks and audit logs all consume events. Nothing calls an integration directly from a request handler.
4. **The API is the product.** Every feature is reachable through a versioned REST API with an OpenAPI 3.1 spec generated from the Rust types. The frontend uses the same API a customer's own scripts would. No hidden endpoints.
5. **Generic engines, specific modules.** Custom fields, rules, templates, documents, jobs and events are engines that know nothing about bookings. The Bookings module is built *using* those engines. A new module for a new vertical should need no engine changes.
6. **Extension points are explicit and few.** Backend plugins may: subscribe to events, register automation actions, register document and template helpers, register scheduled jobs, add custom-field validators, add API routes under `/api/v1/x/<plugin>/`. They may not touch core tables except through the core's own service layer.
7. **Soft delete, full audit.** Nothing business-critical is hard-deleted. Every write records who, when, from where, and the before/after diff.
8. **Idempotent and retryable.** All outbound effects (emails, calendar writes, Stripe calls) carry an idempotency key and run from the job queue with retries and dead-lettering.
9. **Money is never a float.** Minor-unit integers plus an ISO 4217 currency code everywhere, including the API and the frontend.

## Tech stack and repository layout

Rust for everything server-side, TypeScript for everything in the browser, Postgres for all state. One monorepo. If a listed crate is unmaintained or a clearly better option exists at build time, substitute it and record why in `DECISIONS.md`.

| Layer | Choice | Why |
| --- | --- | --- |
| API server | Rust, `axum` + `tokio` | Mature async web stack; compiler catches most generated-code mistakes |
| Database access | `sqlx` (compile-time checked queries) + `sqlx migrate` | SQL stays visible; queries checked against the real schema at build |
| Database | Postgres 16 | RLS, JSONB for custom fields, `LISTEN/NOTIFY`, one dependency |
| Job queue / scheduler | Postgres-backed queue (`SKIP LOCKED` pattern) run by the same binary in worker mode | No Redis or broker to host; jobs are transactional with the data |
| Auth | Own implementation: argon2 passwords, opaque session tokens, TOTP; `openidconnect` crate for Google and Microsoft sign-in | Full control; no third-party auth bill per tenant |
| API contract | `utoipa` → OpenAPI 3.1; TS client generated from it in CI | Frontend and core can never drift |
| Templates | `minijinja` for email, documents and SMS | Jinja syntax is widely known; safe sandboxing |
| PDF | HTML template → PDF via headless Chromium (`chromiumoxide`) in the worker | Documents are designed as HTML/CSS, which tenants can brand |
| Email send | SMTP via tenant's Google/Microsoft account; fallback transactional provider (Postmark or SES) for system mail | Mail comes from the agent's real address |
| Email receive | Gmail API / Graph API polling + webhooks (push where available) | Threads enquiries into the CRM |
| Frontend | React 19, TypeScript, Vite, TanStack Router + Query, Tailwind, shadcn/ui, `react-hook-form` + `zod` | Fast to vibe-code; huge component ecosystem; generated API types |
| Realtime | Server-sent events from the API | Simpler than WebSockets; enough for live lists and toasts |
| Infra | Docker images; `docker-compose` for dev; Fly.io or Hetzner + Coolify for prod; Cloudflare in front; Postgres managed (Neon, Supabase-Postgres or Fly Postgres) | Cheap, multi-region capable, one deploy for all tenants |
| Observability | `tracing` → OpenTelemetry → Grafana Cloud free tier or Axiom; Sentry for frontend | See every tenant's errors from one screen |

**Repository layout**

```
onhold/
  Cargo.toml                 # workspace
  crates/
    core-domain/             # entities, value types (Money, Tz), errors — no I/O
    core-db/                 # sqlx queries, migrations, RLS helpers
    core-engines/            # custom fields, rules, templates, documents, events, jobs
    core-modules/            # crm, bookings, calendar, messaging, finance, portal
    core-integrations/       # google, microsoft, stripe, xero, quickbooks, whatsapp, sms
    core-api/                # axum routes, auth, OpenAPI, SSE
    core-worker/             # job runner, schedulers, pollers (same binary, --worker flag)
    plugin-sdk/              # traits a tenant plugin implements
    plugins/<tenant-slug>/   # bespoke backend plugins, one crate each, feature-gated
  web/
    packages/api-client/     # generated from OpenAPI
    packages/ui/             # shared components, theme tokens
    apps/app/                # staff web app
    apps/portal/             # talent portal (separate build, same API)
    tenants/<tenant-slug>/   # bespoke pages, widgets, overrides; loaded by slug
  infra/                    # Dockerfiles, compose, deploy scripts, backup scripts
  docs/                     # this spec, DECISIONS.md, MILESTONE-N.md, runbooks
```

Plugins and tenant frontend directories are compiled into the single deploy but activated only for the tenant whose slug matches, so one build still serves everyone.

## Tenancy, auth, roles and permissions

A **workspace** is the unit of isolation and billing. A **user** is a person who can sign in; one user can belong to several workspaces (an agent who also manages a band). Every request resolves to exactly one `(user, workspace, role)` and the API sets `SET LOCAL app.workspace_id` so row-level security applies for the whole transaction.

**Workspace record** holds: slug, display name, plan, enabled modules, default currency, default timezone, locale, branding (logo, colours, email footer, document header), billing contact, status (trial, active, suspended, closed), and `plugin_slug` (which bespoke plugin and frontend directory activate for it).

**Authentication**

- Email + password (argon2id), magic-link sign-in, Google and Microsoft OIDC sign-in. Passkeys are a stretch goal.
- TOTP two-factor, enforceable per workspace for staff roles.
- Opaque session tokens in an `HttpOnly`, `Secure`, `SameSite=Lax` cookie for browsers; bearer API keys (per workspace, scoped, revocable, hashed at rest) for scripts and integrations.
- Invitations by email with expiry; a user accepting an invite is attached to the workspace with the invited role.
- Platform-level **superadmin** role for you (the operator): can impersonate a workspace for support, with every impersonated action audited and visibly flagged in the UI.

**Roles** (per workspace membership; a fixed set in v1, custom roles later)

| Role | Scope | Can |
| --- | --- | --- |
| Owner | Whole workspace | Everything, including billing, members, deleting the workspace |
| Admin | Whole workspace | Everything except billing and closing the workspace |
| Agent | Whole workspace, or a subset of talent if restricted | Create and edit contacts, enquiries, bookings, invoices; send messages; run documents |
| Finance | Whole workspace | Invoices, payments, payouts, accounting sync; read-only on everything else |
| Viewer | Whole workspace | Read-only |
| Talent | Only their own talent record and its bookings | Portal access: see and confirm bookings, update availability, upload documents, read messages sent to them |

**Authorization model**: a permission is `(resource, action)` such as `booking:write` or `invoice:send`. Roles map to permission sets in a table seeded at workspace creation. Checks happen in the service layer, once, through one `authorize(ctx, resource, action, object)` function; handlers never check roles ad hoc. Talent and restricted Agents additionally pass an ownership filter (`talent_id IN allowed_talent_ids`) applied in SQL, not in Rust.

**Audit log**: append-only table of `(workspace_id, actor_user_id, impersonator_id?, action, resource, object_id, before, after, ip, user_agent, at)`. Viewable by Owner and Admin. Exportable.

## Core domain model

Every table below has `id` (UUIDv7), `workspace_id`, `created_at`, `updated_at`, `deleted_at`, `created_by`, `updated_by`, and a `custom` JSONB column governed by the custom-field engine. Timestamps are UTC `timestamptz`; event dates carry their own IANA timezone.

**Talent is the generic bookable.** Its `kind` is `person`, `act` (a group of people), `space` (a room, studio, stage) or `equipment` (kit with a quantity and optional serials). Everything that checks availability, holds dates, quotes, contracts and invoices works on a bookable regardless of kind, so a studio room and a session drummer go through the same code. Kind-specific fields (members for an act, capacity for a space, quantity and serials for equipment) are custom-field groups seeded by the preset, and the vocabulary table renames "Talent" to "Act", "Room", "Crew", "Kit" or "Member" per workspace. A Booking can hold several bookables at once (a room plus an engineer plus a drum kit), each with its own availability check and line items, and can be **recurring** (weekly lesson, monthly residency) with per-occurrence overrides.

| Entity | What it is | Key fields | Relationships |
| --- | --- | --- | --- |
| Talent | An act, artist or creative the tenant represents | stage name, legal name, type (solo, band, DJ, speaker…), bio, genres/tags, base location, default fee + currency, commission %, status, portal user link | has many Availability, Bookings, Documents, Members |
| TalentMember | A person in a band or crew | name, role, contact details, dietary/travel notes | belongs to Talent |
| Organisation | A venue, promoter, brand, festival, agency | name, type, addresses, VAT/tax id, default currency, payment terms, notes | has many Contacts, Enquiries, Bookings, Invoices |
| Contact | A person at an Organisation (or standalone) | name, emails, phones, role, preferred channel, opt-in flags | belongs to Organisation; linked to Threads |
| Enquiry | An inbound or outbound request for talent | source (email, form, manual, portal), requested talent(s), event date(s), venue, budget, stage (pipeline), owner, lost reason | becomes 0..n Quotes; linked Thread |
| Quote | A priced offer | line items (fee, travel, extras, each with tax code), currency, validity date, status (draft, sent, viewed, accepted, declined, expired), signed snapshot | belongs to Enquiry; becomes 1 Booking on acceptance |
| Booking | A confirmed or held engagement | talent, organisation, contact, event name, venue, start/end (tz-aware), load-in/soundcheck/set times, status (hold, pending, confirmed, cancelled, completed), fee breakdown, deposit terms, contract document, settlement | has many Invoices, Documents, Tasks; appears on Calendar |
| Availability | A talent's blocked or open time | talent, range, kind (unavailable, tentative, available-only), source (manual, portal, external calendar), note | belongs to Talent; drives clash detection |
| Invoice | A request for money | number (per-workspace sequence), issuer (workspace or talent), payer organisation, line items, currency, tax lines, due date, status (draft, sent, part-paid, paid, overdue, void), Stripe link, accounting sync id | belongs to Booking (optional); has many Payments |
| Payment | Money received or paid out | amount, currency, method, received at, reference, allocation to invoices, payout to talent | belongs to Invoice(s) |
| Thread / Message | A conversation on any channel | channel (email, WhatsApp, SMS, portal note), direction, participants, subject, body (text + sanitised HTML), attachments, external ids, read state | belongs to Contact/Organisation and optionally Enquiry/Booking |
| Document | A generated or uploaded file | kind (contract, rider, tech spec, EPK, invoice PDF, misc), template used, version, signature state, storage key | belongs to Talent, Booking, Organisation or Invoice |
| Task | A to-do with a due date | title, due, assignee, related object, status, origin (manual or rule) | polymorphic link to any entity |
| Template | An email, SMS, WhatsApp or document template | kind, name, subject, body (Jinja), variables schema, locale | per workspace |
| Pipeline / Stage | An ordered set of stages for Enquiries (and optionally Bookings) | name, order, colour, terminal flag, SLA days | per workspace |
| Rule | An automation rule | trigger event, conditions, actions, schedule offset, enabled | per workspace; see engines |
| Integration | A connected external account | provider, owner user, scopes, encrypted tokens, sync cursors, health | per workspace |

Two further entities make the buyer side and labels work. Both use the same engines and conventions as the table above.

| Entity | What it is | Key fields | Relationships |
| --- | --- | --- | --- |
| Event | A show, night, festival day, conference or shoot that the workspace runs or sells into | name, venue (Organisation), date(s) and timezone, doors/curfew, capacity, status (idea, confirmed, on sale, done, cancelled), budget, settlement, recurring template link | has many Slots, Bookings, Tasks, Documents, Invoices (costs and income) |
| Slot | A position in an Event's line-up or schedule | event, order, set times, stage/room, fee budget, status (open, hold, offered, confirmed) | belongs to Event; filled by 0..1 Booking |
| Project | A container for related work over time: a release campaign, a tour, a season, a brand campaign | kind, name, dates, owner, milestones (Tasks), budget, linked talent | has many Bookings, Events, Tasks, Documents |

A seller's Booking links to a Slot only when the buyer is also on the platform (two workspaces, one booking seen from both sides, each editing only its own fields); otherwise the Slot just records the external act's details.

**Invariants the core enforces**

- A Booking cannot be `confirmed` if its talent has an overlapping `confirmed` Booking or `unavailable` Availability, unless an Admin overrides and the override is audited.
- A Quote's accepted snapshot is immutable; editing creates a new version.
- Invoice numbers are gapless per workspace per issuing entity and never reused, even after void.
- Deleting a Talent, Organisation or Contact is a soft delete that keeps every Booking and Invoice intact.
- All monetary fields are `BIGINT` minor units with a `currency CHAR(3)` beside them; totals are computed, stored and re-verified on read.

## Generic engines

Six engines carry all the per-client variation. Each is a crate in `core-engines/`, knows nothing about bookings or talent, and is exercised by its own test suite before any module uses it.

**1. Custom fields.** Per workspace, per entity type, an ordered list of field definitions: key, label, type (text, long text, number, money, date, datetime, boolean, select, multi-select, url, email, phone, file, relation-to-entity), required, default, validation (regex, min/max, options), visibility (staff, portal, both), searchable, and a `group` for layout. Values live in each row's `custom` JSONB and are validated server-side against the definition on every write. The API exposes definitions so the frontend renders forms and list columns from them automatically. Definitions are versioned; removing a field hides it rather than deleting data.

**2. Pipelines and statuses.** Any entity can be declared pipeline-driven (Enquiry is; Booking uses a fixed status set but allows additional sub-statuses). A pipeline is an ordered list of stages with optional SLA (days before a stage is flagged stale), allowed transitions, required fields to enter a stage, and whether the stage is terminal-won or terminal-lost. Stage changes emit `entity.stage_changed` with before/after.

**3. Automation rules.** A rule is `WHEN <trigger> IF <conditions> THEN <actions>`, stored as JSON and edited through a form builder in the UI.

- Triggers: any domain event (`booking.confirmed`, `invoice.sent`, `message.received`, `enquiry.stage_changed`…), a time offset relative to a date field (`booking.start - 7 days`, `invoice.due + 3 days`), or a schedule (cron per workspace timezone).
- Conditions: field comparisons on the triggering object and its parents (`booking.fee > 500000`, `organisation.country == "US"`, `custom.needs_visa == true`), including custom fields.
- Actions: send email/SMS/WhatsApp from a template, create a task, change stage or status, set a field, generate a document, notify a user in-app, call a webhook, enqueue a plugin action.
- Guards: once-per-object-per-rule by default; dry-run mode that logs what would have happened; per-rule execution log visible in the UI; kill switch per workspace.

**4. Templates and rendering.** Jinja templates (`minijinja`, sandboxed, no filesystem access) for email subject/body, SMS, WhatsApp, document HTML, and in-app notifications. The render context is a typed, documented tree (`workspace`, `talent`, `booking`, `organisation`, `contact`, `invoice`, `user`, `custom.*`) with safe formatting filters for money (`{{ booking.fee | money }}` → `£1,250.00`), dates in the booking's timezone, and addresses. Templates can `include` workspace-level partials (signature, legal footer). Every send stores the rendered output for audit.

**5. Documents.** An HTML template rendered to PDF in the worker, stored in object storage (S3-compatible), versioned, with optional e-signature: a signing link that records signer name, email, IP, timestamp and a hash of the PDF, embedded into a final signed copy. Good enough for booking contracts; swap in a third-party e-sign provider through the integration layer later if clients need qualified signatures.

**6. Events, jobs and webhooks.** Every service-layer write appends a row to `outbox` in the same transaction. The worker drains the outbox into `jobs` (Postgres queue, `FOR UPDATE SKIP LOCKED`, priorities, scheduled `run_at`, exponential retry, dead-letter after N attempts, per-job log). Consumers: automation rules, integrations, audit, in-app notifications, SSE fan-out, and outbound webhooks (per workspace, signed with HMAC, with delivery log and replay). Scheduled jobs (daily digests, pollers, reminders) are cron rows per workspace.

**Engine API surface** (all under `/api/v1/`): `custom-fields`, `pipelines`, `rules`, `rules/{id}/dry-run`, `rules/{id}/log`, `templates`, `templates/{id}/preview`, `documents`, `documents/{id}/sign`, `events` (read-only stream for the workspace), `jobs` (admin-only status), `webhooks`.

## Functional modules

Six modules ship in v1, each switchable per workspace. They are built only from the engines and the service layer; they never talk to integrations directly.

&#91;embedded content: enquiry to settlement · 8 statuses, the event each emits\]

Any status can also end in lost, declined, expired or cancelled, each with its own event; what triggers each arrow (deposit percentage, who signs first, chase timing) is configuration, not code.

**CRM** — Talent, Organisations, Contacts. Global search across all three plus bookings and messages (Postgres full-text, `pg_trgm` for fuzzy). Deduplication prompts on create (same email, same phone, similar name). Import from CSV with a column-mapping step and a dry run. Each record has an activity timeline merging messages, bookings, documents, tasks and audit entries. Tags are free-form per workspace. Talent records hold the public-facing assets (photos, bio, tech rider, stage plot, EPK links) that documents and portal pages pull from.

**Enquiry → Quote → Booking → Invoice** — the money loop.

1. Enquiry arrives by email (parsed into a thread and linked to a Contact by address), by the public enquiry form (per workspace, embeddable, with spam protection), through the portal, or by hand. It lands in stage 1 of the workspace's enquiry pipeline with an owner assigned by a rule (round-robin or by talent).
2. The agent checks availability (clash detection across confirmed bookings and talent unavailability, shown inline) and places a **hold** on the date, which is a Booking with status `hold` and an expiry.
3. A Quote is built from the talent's default fee and the workspace's standard line items, sent as a branded PDF plus a web link where the buyer can accept with one click. Acceptance snapshots the Quote, converts the hold to `pending`, and triggers the contract document and deposit invoice through rules.
4. Signed contract plus paid deposit moves the Booking to `confirmed`; it then appears on the calendar as firm, syncs to connected external calendars, and starts the pre-event rule chain (advance sheet requests, balance invoice, logistics tasks).
5. After the event date the Booking auto-completes, the balance invoice is chased until paid, and settlement computes commission and talent payout.

Every step is a stage or status change that emits an event, so each client's variations (deposit percentage, who signs first, whether a hold needs approval) are rules and configuration.

**Calendar and availability** — a workspace calendar with lanes per talent and per staff member, day/week/month/agenda views, holds shown distinctly from confirmed, drag to reschedule (with clash re-check), filters by status and tag. Talent availability is editable by staff and by the talent through the portal. Two-way sync with Google and Microsoft calendars per user and optionally per talent (export bookings as events; import external busy time as unavailability). iCal feed per talent and per workspace, token-protected.

**Messaging and follow-ups** — a unified inbox per workspace showing email (via connected Google or Microsoft accounts), WhatsApp and SMS threads, linked to contacts and to the enquiry or booking they concern. Reply from inside the app using templates with merge fields; attachments stored as documents. Snooze, assign and resolve. Follow-up sequences are rules: for example, enquiry not replied to in 2 days → reminder task; quote not opened in 3 days → chase email; invoice overdue 1, 7 and 14 days → three escalating templates then a task for a human. Opt-out and bounce handling per channel. Rendered sends are stored, and nothing is sent without a template or a human-written body.

**Documents** — generated from templates (contract, confirmation, advance sheet, invoice, quote, rider pack) with the workspace's branding, stored and versioned per booking or talent, optionally sent for e-signature. Uploads (signed PDFs, insurance certificates, passports for travel, stage plots) with expiry dates and a reminder rule for expiring items. A **Document pack** for a booking is a one-click zip of everything a venue or artist needs.

**Events and projects** (on by default for `buys` and `both` perspectives, available to all) — Events with a line-up of Slots, each slot running its own hold → offer → contracted mini-pipeline against the agent or act; a programme calendar showing events, slots and their fill state; recurring event templates for weekly nights; run sheets and day sheets generated from slot times; event settlement (fees, door split, costs, net) with a statement document. Projects group bookings, events and tasks under a release campaign, tour or season with milestone tasks and a budget, and give labels and tour managers a single page per campaign.

## Integrations layer

Every provider implements one of four traits in `core-integrations` — `MailProvider`, `CalendarProvider`, `PaymentProvider`, `AccountingProvider` (plus `MessagingProvider` for WhatsApp/SMS) — so modules code against the trait and a workspace picks the provider. OAuth tokens are encrypted at rest (AES-GCM, key from the environment, rotated per deploy), refreshed by the worker, and a failing integration shows a red health badge in the UI with the last error and a reconnect button, never a silent failure.

| Provider | Trait | v1 scope | Notes |
| --- | --- | --- | --- |
| Google Workspace / Gmail | Mail | Send as user; receive via Gmail API watch (Pub/Sub) with polling fallback; thread + label mapping; attachments | One connection per user; workspace shared inbox optional |
| Google Calendar | Calendar | Push confirmed bookings and holds (holds as tentative); import busy blocks as unavailability; incremental sync tokens | Per user and per talent calendar |
| Microsoft 365 (Graph) | Mail + Calendar | Same scope as Google via Microsoft Graph subscriptions with polling fallback | Shared mailbox support |
| Stripe | Payment | Stripe Connect (Standard) per workspace so money settles to the client, not to you; payment links on quotes and invoices; deposit and balance; card + bank debit; webhooks for paid/failed/refunded; multi-currency per workspace's Stripe account | Platform fee optional, off by default |
| Xero | Accounting | Push invoices and payments; pull paid status; contact mapping; tax-rate mapping table per workspace | OAuth 2, tenant id per org |
| QuickBooks Online | Accounting | Same scope as Xero |  |
| WhatsApp Business (Cloud API) | Messaging | Template messages for reminders; session replies into the inbox; media | Needs Meta business verification per client — document the onboarding steps |
| SMS (Twilio, with Vonage as alternate) | Messaging | Outbound reminders, inbound replies into the inbox, STOP handling | Per workspace sender number |

**Rules for every integration**

- All calls go through the job queue with idempotency keys; a request handler never awaits a third party.
- Each provider keeps a per-workspace **sync cursor** and a **mapping table** (`external_id ↔ internal_id`) so re-syncs are safe.
- Rate limits are respected with a per-provider token bucket in the worker.
- Webhook endpoints verify signatures and acknowledge immediately; processing is a job.
- A provider can be swapped (Twilio → Vonage) without touching modules.
- Secrets never appear in logs; the audit log records that a token was refreshed, not the token.
- A workspace with no integrations connected still works fully: email falls back to the system transactional sender with a reply-to of the agent's address, calendars are internal only, payments are manual (mark as paid).

## Money: multi-currency, tax, invoicing, payments

International from day one. A workspace has a **base currency** for reporting; every quote, booking, invoice and payment carries its own currency; conversions are stored with the rate and date used, never recomputed silently.

- **Representation**: `Money { minor: i64, currency: Currency }` in Rust, `{ minor: number, currency: string }` on the wire, formatted client-side with `Intl.NumberFormat` and server-side with the same locale rules. No decimals in storage.
- **Rates**: daily ECB and Open Exchange Rates fetch into a `fx_rates` table; a booking may pin a manual rate. Reporting converts at the rate on the document date.
- **Tax**: a per-workspace table of tax codes (name, rate, jurisdiction, applies-to) seeded with sensible defaults for the workspace's country (UK VAT 20/5/0/exempt; EU VAT with reverse-charge flag; US sales tax left to the accounting system; AU GST). Each line item carries a tax code; invoice totals show tax lines grouped by code. Tax-exempt and reverse-charge wording is template-driven.
- **Invoice compliance**: configurable number format (`INV-{YYYY}-{0000}`), issuer legal name, address and tax id, payer details, issue and due dates, payment terms, bank details per currency, late-fee text. A locked PDF is generated on send. Credit notes reverse invoices rather than editing them.
- **Who invoices whom**: three settings per workspace — agency invoices the buyer and pays talent out; talent invoices the buyer directly and agency invoices talent for commission; or a split (deposit to agency, balance to talent). The model must support all three because agencies in different countries do it differently.
- **Payments**: Stripe payment links on quotes and invoices; manual payments (bank transfer, cash) recorded by hand; partial payments and over-payments allocated across invoices; refunds. Status moves to `paid` only when allocations equal the total.
- **Settlement and payouts**: on booking completion, compute gross fee, agency commission (per talent default, per booking override), expenses, talent net; generate a statement document; record the payout as a Payment with method and reference. Export to accounting.
- **Reporting** (v1 minimum): bookings and revenue by month, by talent, by organisation, in base currency; outstanding and overdue invoices; conversion funnel from enquiry stages; CSV export of everything.

## Talent portal

A separate, deliberately simple web app (`web/apps/portal`) on the same API, branded per workspace, for the people the tenant represents. It must work well on a phone and need no training.

- Sign-in by magic link or password; the talent sees only their own talent record(s).
- **Dates**: upcoming and past bookings with status, venue, times, fee (if the workspace allows), documents (contract, advance sheet, rider); a confirm/decline button on pending dates that moves the booking and notifies the agent.
- **Availability**: mark unavailable or tentative ranges; optionally connect their own Google or Microsoft calendar to push busy time.
- **Messages**: a thread per booking plus a general thread with the agent; replies land in the staff inbox.
- **Profile and assets**: update bio, photos, tech rider, stage plot, social links, travel details, band members; staff can require approval before changes go live.
- **Money** (switchable per workspace): statements, payout history, their own invoices to the agency if that model is used.
- Everything the talent can see or do is a permission the workspace Owner toggles, so a cautious agency can start with read-only dates.

## Frontend application

The staff app is a single-page React application that renders itself from the API: entity forms and list columns come from custom-field definitions, stages from the pipeline, actions from permissions. A client's bespoke screens are a directory under `web/tenants/<slug>/` that the app loads when the signed-in workspace's `plugin_slug` matches.

**Structure**

- `packages/api-client`: generated from the OpenAPI spec in CI; the build fails if the spec changed and the client was not regenerated.
- `packages/ui`: design tokens (colour, spacing, type) as CSS variables that a workspace's branding overrides at runtime; shadcn/ui components; a `DataTable` (server-side sort, filter, paginate, column chooser, saved views, bulk actions), `EntityForm` (renders core + custom fields from a schema), `Timeline`, `Calendar`, `Inbox`, `MoneyInput`, `DateTimeTzInput`, `DocumentViewer`, `RuleBuilder`, `TemplateEditor` with live preview.
- `apps/app`: routes per module (`/crm`, `/enquiries`, `/bookings`, `/calendar`, `/inbox`, `/documents`, `/finance`, `/settings`), a global command palette (⌘K: search, create, navigate), keyboard-first lists, optimistic updates with TanStack Query, SSE subscription for live changes.
- `apps/portal`: the talent portal, same packages, a quarter of the surface.

**Extension points the frontend exposes to a tenant directory**: add routes and nav items; add a tab to any entity page; add a widget to the dashboard; override a template's preview; register a custom field renderer; register a bulk action; replace the branding theme. Each is a typed registration function; nothing in `apps/app` imports from `tenants/` directly.

**Quality bar**: works on a 375px phone and a 1440px desktop; every list loads under 300ms at p95 with 10,000 rows per table per workspace; WCAG AA contrast; no layout shift on load; empty states that tell the user what to do next; dark mode via tokens.

## Per-client bespoke work

A new client's "custom app" is the core plus, in order of preference: configuration, frontend tenant directory, backend plugin. Choosing the lowest layer that works is the whole discipline that keeps one deploy serving everyone.

| Client asks for | Layer | Example |
| --- | --- | --- |
| Different fields, labels, stages, statuses, templates, reminders, branding | Configuration (no code) | "We need a 'tour name' on every booking and a 'rider received' stage" → custom field + pipeline stage, done in Settings |
| A screen, dashboard, report or widget arranged their way | Frontend tenant directory | "A tour grid showing all dates for one artist with travel between them" → `tenants/acme/TourGrid.tsx` registered as a route |
| Logic the rules engine cannot express | Backend plugin crate | "Commission is 15% but 10% on repeat bookings with the same venue within 12 months" → plugin registers a `compute_commission` hook |
| A new integration | Core (shared) | "Sync to Pipedrive" → built as a provider so every tenant can have it |

**Onboarding a new client** (target: under two hours of your time)

1. `onhold admin workspace create --slug acme --name "Acme Agency" --country GB --currency GBP --plan standard` seeds defaults: roles, pipeline, tax codes, templates, rules, custom fields for the vertical preset chosen (`agency`, `manager`, `freelance`).
2. Import their spreadsheet with the CSV importer; connect Google or Microsoft; connect Stripe.
3. Adjust configuration with them on a call; most "bespoke" asks end here.
4. If they need a tenant directory or plugin, create it from the templates in `web/tenants/_template` and `crates/plugins/_template`, ship it behind their slug.
5. Hand over, start the monthly fee.

**Operator admin area** (superadmin only): list workspaces with plan, health, last activity, usage counters; create and suspend workspaces; impersonate; see failing jobs and integrations across all tenants; feature flags per workspace; release notes broadcast.

### Plugin SDK contract

A backend plugin is a crate implementing `onhold_plugin_sdk::Plugin`, compiled into the worker and API binaries, and activated only for workspaces whose `plugin_slug` matches. The SDK is the only way a plugin touches the system.

```rust
pub trait Plugin: Send + Sync {
    fn slug(&self) -> &'static str;
    fn register(&self, reg: &mut Registry);   // called once at startup
}

pub struct Registry { /* ... */ }
impl Registry {
    pub fn on_event(&mut self, pattern: &str, handler: EventHandler);
    pub fn rule_action(&mut self, name: &str, schema: JsonSchema, run: ActionFn);
    pub fn rule_condition(&mut self, name: &str, eval: ConditionFn);
    pub fn template_filter(&mut self, name: &str, f: FilterFn);
    pub fn document_kind(&mut self, kind: &str, template: &str);
    pub fn scheduled_job(&mut self, cron: &str, job: JobFn);
    pub fn field_validator(&mut self, entity: &str, key: &str, v: ValidatorFn);
    pub fn hook<H: Hook>(&mut self, h: H);     // e.g. ComputeCommission, BeforeBookingConfirm
    pub fn route(&mut self, path: &str, router: axum::Router<PluginState>);  // mounted at /api/v1/x/{slug}/
}
```

Plugins receive a `Ctx` with the workspace, the acting user, a service handle (`ctx.bookings()`, `ctx.invoices()`, `ctx.messages()`…) and a scoped database connection that already has RLS applied. They get no raw table access. Named hooks (`ComputeCommission`, `BeforeBookingConfirm`, `AfterQuoteAccepted`, `InvoiceNumbering`, `AvailabilityCheck`) have typed inputs and outputs and a core default, so a plugin overrides one decision without re-implementing a module. Every plugin ships with its own tests and a `README.md` naming the client and the ask it exists for.

## Operations: hosting, deployment, backups, observability, security, GDPR

One production deployment serves every tenant; the monthly fee is paying for this section, so it must be boring and automated.

**Hosting and deploy**

- Two containers from one image: `api` and `worker` (same binary, different flag), plus static frontends on a CDN (Cloudflare Pages or the API serving them). Managed Postgres with point-in-time recovery. S3-compatible object storage (Cloudflare R2) for documents.
- Environments: `dev` (docker-compose, seeded demo workspace), `staging` (mirrors prod, anonymised copy of data on demand), `prod`.
- CI on every push: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `sqlx prepare --check`, OpenAPI diff, TypeScript typecheck, frontend unit tests, Playwright smoke suite against a fresh database. Merge to `main` deploys to staging; a tagged release deploys to prod with a one-command rollback.
- Migrations run before the new version starts and must be backwards-compatible with the previous version for one release (expand/contract pattern) so deploys are zero-downtime.
- Data residency: the stack must be deployable as a second, isolated region (EU and US at minimum) by changing config only; a workspace is pinned to one region at creation.

**Backups and recovery**: continuous WAL archiving plus nightly logical dumps kept 30 days; object storage versioning; a monthly restore drill script that restores to staging and runs the smoke suite; per-workspace export (JSON + CSV + documents zip) on demand, which doubles as the GDPR data-portability export.

**Observability**: structured logs with `workspace_id` and request id on every line; traces for every request and job; dashboards for request latency, job queue depth and age, integration error rate, email bounce rate, per-workspace activity; alerts to your phone for 5xx spikes, queue age over 10 minutes, failed backups, expiring certificates and OAuth tokens. A `/healthz` and `/readyz` on both containers.

**Security**

- RLS on every tenant table; a test that asserts every table with `workspace_id` has a policy.
- OWASP basics enforced in middleware: CSRF on cookie sessions, rate limits per IP and per user, strict CSP, security headers, input size limits, HTML sanitisation of inbound email and user content.
- Secrets from environment or a secret store, never committed; `cargo audit` and `npm audit` in CI; dependabot.
- Uploaded files virus-scanned (ClamAV in the worker) and served through signed, expiring URLs.
- Per-workspace API keys scoped to permissions; all keys hashed; last-used tracking.
- Password policy, breached-password check (k-anonymity API), session listing and revocation per user, mandatory 2FA for superadmin.

**GDPR and privacy** (applies regardless of region)

- Data processing agreement template and a per-workspace list of sub-processors (your hosting, email, SMS, payment providers).
- Right of access and portability: the workspace export above, plus per-contact export.
- Right to erasure: a `forget contact` action that anonymises personal fields while keeping financial records that law requires, and a retention policy per workspace that auto-anonymises stale contacts.
- Consent and opt-out tracked per channel per contact; marketing sends respect them; transactional sends are allowed.
- Cookie-free analytics for the product itself (Plausible or none).
- Breach runbook in `docs/runbooks/`.

## Quality bar, testing and definition of done

Nothing is done until it is tested, documented in the OpenAPI spec, and usable by a non-technical agent without being shown.

- **Unit tests** for every engine and for all money, date and timezone logic, with property-based tests (`proptest`) on money arithmetic and clash detection.
- **Integration tests** against a real Postgres (testcontainers) for every API endpoint, including a negative test proving tenant isolation (user in workspace A cannot read, update or delete anything in workspace B by any route).
- **Contract tests** that replay recorded provider responses (Google, Microsoft, Stripe, Xero, QuickBooks, Twilio, WhatsApp) so integrations are tested without live accounts; a separate opt-in live-sandbox suite.
- **End-to-end** Playwright flows: onboard workspace → import CSV → enquiry by form → hold → quote → accept → contract signed → deposit paid → confirmed → appears on calendar and Google Calendar → balance invoice chased → paid → settlement statement generated.
- **Performance**: seed a workspace with 500 talent, 5,000 organisations, 50,000 bookings and 200,000 messages; every list and search p95 under 300ms, calendar month view under 500ms.
- **Accessibility**: axe checks in Playwright; keyboard navigation for every list and form.
- **Docs**: OpenAPI published at `/api/docs`; `docs/runbooks/` for deploy, rollback, restore, rotate secrets, onboard tenant, offboard tenant; `DECISIONS.md` and `MILESTONE-N.md` kept current.
- **Definition of done per milestone**: CI green, smoke suite green on staging, the milestone's user story demonstrable end-to-end by clicking, no `TODO` left in code without an issue reference.

### Seed and demo data

`onhold admin seed --demo` creates a fictional workspace with 12 talent, 40 organisations, a year of bookings across statuses, open enquiries at each stage, overdue and paid invoices, and sample rules, so every screen has realistic content on first run and the E2E suite has a fixture. Names and companies are invented; no real people, venues or brands.

## Build order and milestones

Build bottom-up: isolation and engines before any feature, so every module written afterwards is already multi-tenant, audited, event-driven and configurable. Do not start a milestone until the previous one's definition of done is met.

1. **M0 — Skeleton.** Monorepo, Cargo workspace, Docker dev environment, Postgres, migrations, `tracing`, CI pipeline, `/healthz`. Demonstrable: `docker compose up` gives a running API that answers `/healthz` and CI is green.
2. **M1 — Tenancy and auth.** Workspaces, users, memberships, roles, permissions, sessions, API keys, invitations, OIDC sign-in, 2FA, RLS on every table, audit log, superadmin impersonation. Demonstrable: the isolation test suite passes; two workspaces cannot see each other.
3. **M2 — Engines.** Custom fields, pipelines, outbox + jobs + scheduler, events, templates with preview, webhooks. No business entities yet; tested with a dummy `Thing` entity. Demonstrable: a rule on `thing.created` sends a templated email through the queue with retries.
4. **M3 — CRM.** Talent, Organisations, Contacts, tags, search, dedup, CSV import, activity timeline. Frontend shell: auth, nav, `DataTable`, `EntityForm` driven by custom fields, settings pages for fields and pipelines. Demonstrable: import a spreadsheet, find a contact, add a custom field, see it in the form and the table.
5. **M4 — Bookings and calendar.** Enquiry pipeline, holds, quotes with web acceptance, bookings with statuses and clash detection, availability, workspace calendar views, iCal feeds, tasks. Demonstrable: enquiry → hold → quote → accept → confirmed booking on the calendar, with the clash test blocking a double booking.
6. **M5 — Documents and money.** Document engine with PDF and e-signature; contract, confirmation and advance-sheet templates; invoices, credit notes, payments, settlement, tax codes, FX, reporting; Stripe Connect with payment links and webhooks. Demonstrable: the full E2E flow through deposit paid and balance chased, in two currencies.
7. **M6 — Messaging and integrations.** Google and Microsoft mail and calendar (send, receive, two-way calendar), unified inbox, follow-up rules, SMS, WhatsApp, Xero and QuickBooks sync. Demonstrable: an email enquiry arrives in the inbox linked to its contact; a confirmed booking appears in Google Calendar; a paid invoice shows paid in Xero.
8. **M7 — Talent portal.** Portal app with dates, confirm/decline, availability, messages, profile, money view, per-workspace permission toggles. Demonstrable: a talent confirms a pending date from a phone and the agent is notified.
9. **M8 — Operator tooling and hardening.** Admin area, workspace CLI, vertical presets and seeds, tenant export, erasure, retention, backups and restore drill, performance seeding and tuning, security review, runbooks, plugin and tenant-directory templates with one worked example each. Demonstrable: onboard a new client from CLI to first login in under two hours; restore drill passes.

After M8 the core is v1. Bespoke client work starts at M3 if a first customer is waiting — their configuration and frontend directory can be built as modules land.

## Assumptions made and decisions still open

**Assumed** (change these in the spec before starting if wrong):

- Working name **OnHold (written OnHold in the product, onhold in code); it appears only in crate names, the CLI and this document**.
- Rust backend, TypeScript frontend, Postgres, one shared deployment, as discussed.
- Vertical presets at launch: `agency`, `manager`, `freelance`; the full preset list and the buyer/seller perspective are in the Vertical presets section.
- Stripe Connect Standard so each client holds their own Stripe account and the money never passes through you.
- E-signature is built in at a simple, evidential level; qualified e-signatures are a later integration.
- Email ingestion uses the client's own Google or Microsoft account; no separate inbound mail domain in v1.
- No AI features in v1; the events bus and a `drafts` table are the seams for later.

**Open** (answer before the relevant milestone):

- [ ] M1: should a user be able to belong to many workspaces, or one workspace per login? (Spec assumes many.)
- [ ] M4: do holds expire automatically, require a reason, or need Admin approval by default? (Spec assumes auto-expire with a configurable default of 7 days.)
- [ ] M5: which of the three invoicing models is the default for each vertical preset?
- [ ] M5: minimum set of document templates to ship per preset.
- [ ] M6: WhatsApp Business verification is per client and takes days; is it a launch requirement or a per-client add-on?
- [ ] M7: can talent see their fees by default, or is that off until the agency turns it on? (Spec assumes off.)
- [ ] M8: your pricing tiers, so plan limits (seats, talent count, storage) can be enforced.
- [ ] Hosting: Fly.io versus Hetzner + Coolify; EU-only at launch or EU + US regions from day one?
