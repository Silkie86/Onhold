# Core Platform Build Spec

Oct 27, 2026 · @Solomon Rose · updated Oct 28, 2026 (10:30 UTC) with every decision made since M0

## How to use this prompt

This document is the master build prompt for a reusable platform core (working name: **OnHold**). It started as the first message to Claude Code in an empty repository; the build has since worked through the milestones in the last sections, and this document is kept current with the decisions taken on the way. Every later bespoke client app is built *on top of* this core, never by forking it.

**Changes since the first version** (Oct 27–28, 2026, each from Solomon; details in the sections below and in `DECISIONS.md`):

- **Desktop and mobile apps that work offline**, replacing "no native apps in v1": native Tauri 2 apps for Windows, macOS, Linux, iOS and Android, wrapping the same web app.
- **The installable website comes first.** People get "the app" by installing the website (Add to Home Screen, works offline, the client's own name and icon). The native apps stay in the repo (desktop builds in CI, mobile still to come) but unpublished until they make financial sense.
- **Apps may be paywalled.** A per-plan, server-side "apps allowed" switch; the website stays fully usable on phones on every plan. Plans are sold on the website, never in the app stores.
- **Per-client white-label branding:** each client's logo, colours, name and custom domain, with the installable website under their name and icon. Store apps, when published, are **one shared OnHold app** with OnHold's name and icon that shows the client's logo and colours after sign-in; no per-client store listings.
- **Every operating system:** the web app works in any browser, and the code builds and tests on Windows, macOS and Linux.
- **Importing past PDFs** (old contracts, invoices and so on) without AI: text layer extraction, optional OCR, field suggestions a person confirms, and past invoices kept as historical records.
- **Own-domain email:** any mailbox by IMAP and SMTP beside Google and Microsoft, and sending from a workspace's own verified domain with automatic DNS setup through Domain Connect and a guided manual fallback.
- **A small footprint** in the cloud and on devices is a standing rule for every change.
- **The talent side too:** OnHold also serves artists, DJs, producers and other creatives, across creative fields. In v1 that is a free artist viewer (M7) with visible-but-locked paid features; presets are combinable (built with the onboarding questionnaire); the full talent version (fee splits, royalties, releases) comes after v1.
- **Hosting at near-zero cost** while there are no clients, easy to scale (see Operations).
- **Theming in two layers:** clients set their name, logo and one brand colour; OnHold's team sets a full theme in a theme studio. The theme reaches every screen, email, PDF and public page.
- **Email signatures** for each member and the workspace, and **bookings written into members' own Google or Outlook calendars** (a calendar OnHold makes there, never their main one).
- **An onboarding questionnaire sets up each client** (built): they tick what they will use OnHold for and leave comments; the answers choose presets, modules and words, and the app's sections, menus and buttons scale with that setup (simple for a solo artist, full for an agency). A live price estimate updates as they tick. Nothing is locked in: the setup can be changed any time.
- **A master admin for OnHold's team** inside the same app ("OnHold team": Client workspaces, audited visits and Theme studio are built; each client's setup and requests, the Streamline panel and the price list are built too).
- **Three levels of customisation, never a fork:** shared modules switched on per client, configuration per workspace, and small per-client plugins at fixed hook points.
- **Travel on any booking and tours as projects** (being built): structured flights, trains, ground transport and hotels on any booking, a "does it pay?" check of fee and buyouts against costs, and tours (or any field's equivalent run of dates) as projects with a profit and loss per date and overall.

Rules that override everything else in this document:

1. **The core is a product, not a prototype.** Write it as if 50 paying agencies will run on it. No shortcuts that would need undoing.
2. **Bespoke = data + frontend + thin modules.** Anything a single client needs that differs from another client must be expressible as configuration, custom fields, rules, templates or a frontend module. Changing the Rust core for one client is a design failure; flag it rather than do it.
3. **Ask before inventing.** Where this spec is silent, stop and ask one precise question. Where it is ambiguous, pick the option that keeps the core generic and say which you picked.
4. **Correctness over cleverness.** Prefer boring, well-typed code. Every public API has a test. Every migration is reversible.
5. **No AI features in v1.** Do not add Claude API calls, chat assistants or "smart" automation. Leave clean seams (the events bus, the outbox and stored outbound messages; a `drafts` table is still to add) so they can be added later.
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

**The talent side.** OnHold also serves the talent themselves (solo artists, DJs, producers and other creatives), and stays diverse across creative fields rather than assuming music. Many people wear several hats at once (artist, label manager, promoter, producer, studio owner), so one login moves between every workspace it belongs to, and one workspace can combine several setups (see presets). In v1 the talent side is light and reached mostly through managers: an artist invited by their manager gets a free viewer (the talent portal, M7) and can start their own workspace from it. The full talent version (solo presets as a product, fee splits, royalties and releases) comes after v1.

**How it makes money**

- One-off setup fee per client: covers onboarding, data import, branding and the first round of bespoke configuration or modules.
- Monthly fee per client: hosting, backups, bug fixes, small feature requests. Priced per workspace, optionally tiered by number of active talent or staff seats.
- Bespoke feature work beyond the monthly allowance: quoted separately, delivered as per-tenant modules on the shared core.
- Paid plans for talent: the invited-artist viewer is free; artists upgrade to their own paid workspace, choosing the modules they need. There are no ads or direct upsell in the free viewer: paid features show as visible but locked (see Talent portal).
- The desktop and mobile apps may be a paid-plan feature, because of app-store costs; the website stays fully usable on every plan.
- Every plan is sold on OnHold's website, never through in-app store purchases (avoiding the 15–30% store cut; Apple allows B2B sales outside in-app purchase, single-user plans are a grey area to check before launch). OnHold's own subscription billing is separate from clients' Stripe Connect accounts and is not built yet; pricing tiers are still open.

The core must therefore make three things cheap: **onboarding a new tenant** (minutes, not days), **shipping a fix to every tenant at once** (one deploy), and **adding a tenant-specific feature without touching anyone else** (the extension model in the next sections).

**Out of scope for v1**: AI drafting or triage, a public marketplace, ticketing, royalty or publishing accounting, and the full talent version (fee splits, royalties, releases). Desktop and mobile apps are in scope (see Distribution: website and apps), but the web app must still work well on a phone on its own.

## Vertical presets and perspective

A workspace has a **perspective**: `sells` (represents talent), `buys` (books talent for its own events) or `both`. Perspective changes vocabulary and defaults, never the data model: an Enquiry becomes an **Offer** when the workspace is the one making it, a Quote becomes the agent's reply, and the same Booking row sits at the end of either flow. Every list, template and rule in the core reads its labels from vocabulary seeded by the presets (per module; M8), so "act", "artist", "speaker", "creator" and "talent" are the same entity with different names.

A **preset** is a seed bundle: perspective, vocabulary, enabled modules, custom fields, pipeline stages, tax defaults, templates and rules. Presets are data in `core-modules/presets/*.toml`, and adding a vertical means adding a file, not code.

**Presets combine.** A lot of people have a lot of roles, so a workspace is not limited to one preset: several can be applied together at creation, and more can be added later without losing data (an artist who also runs a label and promotes nights uses `solo` + `label` + `promoter` in one workspace). Combining merges modules, fields, stages, templates and rules; where perspectives differ the workspace becomes `both`. Vocabulary is set per module, not per workspace, so each area keeps its own words.

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
| `solo` (artist, DJ, producer) | sells | One person running their own bookings: simple pipeline, own availability and connected personal calendar, rider and travel templates, own invoices; can be linked to a manager's workspace |

Combinable presets as data files, including simple solo artist, DJ and producer setups, are built with the onboarding questionnaire; rider and travel templates for every preset that represents talent follow in M8; selling them as a full talent product comes after v1.

Presets are starting points. Everything they seed is editable in Settings afterwards, and a workspace can switch preset only by applying another bundle on top, never by losing data.

**The setup form.** A new workspace's owner is offered a short tick-box form in Settings → Your setup: what they do (each role is a preset), what they want OnHold to do (each feature switches on a module or an extra), who or what they book, and how many people will use it, plus a comments box whose requests reach OnHold's team on the Client workspaces page. The answers are kept on the workspace and can be changed at any time. They set its presets, perspective, modules, words per module and **extras**, the finer switches inside modules that keep a one-person setup simple: `team` (assigning work to each other), `commission` (settlements and who invoices) and `advanced` (custom fields, pipelines and imports in the menu). Menus, pages and buttons read these, so a solo artist sees a small app and an agency sees everything. Switching something off only hides it. While the form is filled in, a live estimate is worked out from OnHold's price list (a base price, plus each feature, plus team size), which OnHold's team edits on the Client workspaces page; it charges nothing. The presets so far seed perspective, modules, extras and words; their custom fields, pipeline stages, templates and rules arrive in M8. On top of that, OnHold's team can **streamline** a client's app (Client workspaces → Streamline, on an audited visit): hide any menu item, part of a page or optional field it doesn't need. That is kept apart from the form's answers, so it survives the client sending the form again; nothing is deleted, and required fields always show.

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
8. **Idempotent and retryable.** All outbound effects (emails, calendar writes, Stripe calls) carry an idempotency key and run from the job queue with retries and dead-lettering. The one exception is mail already handed to a provider: it is never resent automatically, and a send that was cut off is marked failed for a member to check.
9. **Money is never a float.** Minor-unit integers plus an ISO 4217 currency code everywhere, including the API and the frontend.
10. **Keep it small.** OnHold must not take up much space in the cloud or on people's devices. Keep only what the business needs (for example, the inbox stores only mail from people and contacts, and never copies attachments; calendars store busy or free only), cap what is stored, keep device copies opt-in and capped, and keep the apps light. Every review checks storage size.
11. **Offline-ready writes.** Every write accepts an `Idempotency-Key`, versioned records use `ETag`/`If-Match`, and the events feed names what changed, so devices can queue changes offline and replay them safely.

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
| PDF | HTML template → PDF, tried at once in the request with a worker job as fallback; headless Chromium when present, otherwise a built-in plain PDF writer | Documents are designed as HTML/CSS, which tenants can brand; the fallback keeps small hosts working without Chromium |
| PDF import | `core-pdf` crate: text layer via `pdf-extract`, optional Tesseract OCR for scans, fixed label patterns and per-issuer templates for field suggestions | Reads past PDFs with no AI; a person confirms every suggested field |
| Email send | The member's own mailbox (Gmail API, Microsoft Graph, or any provider by SMTP); a workspace's own verified domain through Resend; OnHold's own sender (any SMTP server, Resend in production) as fallback and for system mail | Mail comes from the agent's real address or their own domain |
| Email receive | Polling Gmail history, Graph delta queries and IMAP (push can come later behind the same trait) | Threads enquiries into the CRM; works with any mailbox |
| Frontend | React 19, TypeScript, Vite, TanStack Router + Query, Tailwind 4, Radix primitives in the shadcn/ui style; forms use the app's own `EntityForm` | Fast to build; huge component ecosystem; generated API types |
| Installable website | Web app manifest + a service worker generated at build time; offline copy in IndexedDB when installed | The first way people get "the app": Add to Home Screen, works offline, no store and no cost |
| Desktop and mobile apps | Tauri 2 wrapping the same web app (Windows, macOS, Linux, iOS, Android); session token in the system keychain | One codebase for every platform; small installers (a 2.7 MB `.deb`) |
| Realtime | Server-sent events from the API | Simpler than WebSockets; enough for live lists and toasts |
| File storage | Postgres by default, or a directory or any S3-compatible store (R2, S3, MinIO) via `STORAGE` | Nothing extra to host at launch; moves to object storage when it grows |
| Infra | Docker images; `docker-compose` for dev; at launch Neon (free Postgres), one auto-stopping Fly.io machine running `onhold serve`, Cloudflare Pages for the web app, Resend free tier for mail (it also verifies clients' own sending domains, so the platform's `SMTP_URL` is Resend's) | Near-zero cost and operations while there are no clients; scales by keeping a machine running, adding machines or splitting out the worker |
| Observability | `tracing` → OpenTelemetry → Grafana Cloud free tier or Axiom; Sentry for frontend | See every tenant's errors from one screen |

**Repository layout**

```
onhold/
  Cargo.toml                 # workspace
  crates/
    core-domain/             # entities, value types (Money, Tz), errors — no I/O
    core-db/                 # sqlx queries, migrations, RLS helpers
    core-engines/            # custom fields, rules, templates, documents, events, jobs
    core-modules/            # vertical presets as TOML data and the setup form's questions; the modules themselves are services in core-api on core-db queries
    core-integrations/       # google, microsoft, imap/smtp, resend, stripe, xero, quickbooks, whatsapp, sms
    core-pdf/                # reading imported PDFs: text, OCR, field suggestions (no AI)
    core-api/                # axum routes, auth, OpenAPI, SSE, services
    core-worker/             # job runner, schedulers, pollers (library)
    onhold/                  # the single binary: `onhold serve | api | worker | migrate | admin`
    plugin-sdk/              # traits a tenant plugin implements
    plugins/<tenant-slug>/   # bespoke backend plugins, one crate each, feature-gated
  web/
    packages/api-client/     # generated from OpenAPI
    packages/ui/             # shared components, theme tokens
    apps/app/                # staff web app, installable website (PWA) and offline engine
    apps/app/src-tauri/      # desktop and mobile app shell (Tauri 2)
    apps/app/brands/         # brand files for per-client branded app builds
    tenants/<tenant-slug>/   # bespoke pages, widgets, overrides; loaded by slug
  infra/                    # Dockerfiles, compose, deploy scripts, backup scripts
  docs/                     # this spec, DECISIONS.md, MILESTONE-N.md, runbooks
```

Plugins and tenant frontend directories are compiled into the single deploy but activated only for the tenant whose slug matches, so one build still serves everyone.

## Tenancy, auth, roles and permissions

A **workspace** is the unit of isolation and billing. A **user** is a person who can sign in; one user can belong to several workspaces (an agent who also manages a band). Every request resolves to exactly one `(user, workspace, role)` and the API sets `SET LOCAL app.workspace_id` so row-level security applies for the whole transaction.

**Workspace record** holds: slug, display name, plan, enabled modules, default currency, default timezone, locale, branding (logo, colours, name, custom domain, app icon, email footer, document header), billing contact, status (trial, active, suspended, closed), and `plugin_slug` (which bespoke plugin and frontend directory activate for it).

**White-label branding.** Each client can run OnHold under their own logo, colours, name and custom domain. The website installed from their domain carries their name and icon. Theming has two layers:

- **Clients** (owners and admins, Settings → Branding) set their business name, a logo and one brand colour; the text on it is chosen white or black for contrast. The logo is resized in the browser and kept in the workspace's branding as a small image (at most 14 kB), so it travels everywhere with no file store.
- **OnHold's team** sets a client's full theme at onboarding in the **theme studio**: every colour in light and dark (including success, warning and info), one of seven system font stacks (no font downloads) and corner radius. The studio flags any colour pair under 4.5:1 contrast, so every client's app stays readable. Only a superadmin with two-factor sign-in, or one on an audited visit into the workspace, can change it.

The theme reaches everything a client's staff or customers see: every screen, the quote, invoice, signing and invitation pages and emails (logo or name, and a shade of the brand colour that reads on white), PDFs (the theme's typeface), and the talent portal. In dark mode a logo sits on a light tile. The sign-in page stays OnHold's, since it does not know the workspace yet. Store apps, when published, are one shared OnHold app with OnHold's name and icon, showing the workspace's logo and colours after sign-in; per-client branded desktop builds stay possible from a brand file but there are no per-client store listings.

There is no open sign-up in v1: accounts are created by accepting an invitation, and workspaces are created by the operator together with the owner's invitation. The talent "run your own bookings" path (M7) is the first self-service workspace creation.

**Authentication**

- Email + password (argon2id), magic-link sign-in, Google and Microsoft OIDC sign-in. Passkeys are a stretch goal.
- TOTP two-factor, enforceable per workspace for staff roles.
- Opaque session tokens in an `HttpOnly`, `Secure`, `SameSite=Lax` cookie for browsers; bearer API keys (per workspace, scoped, revocable, hashed at rest) for scripts and integrations.
- Desktop and mobile app sessions: the app shows a short code, the person approves it at `/link-app` on the website (RFC 8628 device flow), and the app gets a 90-day bearer session (`ohs_…`) kept apart from cookie sessions and listed with the person's other sessions. CORS allows only the Tauri origins.
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
| Talent | Only their own talent record and its bookings | Portal access (the free viewer): see and confirm bookings, update availability, connect their calendar, upload documents, read messages sent to them |

**Authorization model**: a permission is `(resource, action)` such as `booking:write` or `invoice:send`. Roles map to permission sets in a table seeded at workspace creation. Checks happen in the service layer, once, through one `authorize(ctx, resource, action, object)` function; handlers never check roles ad hoc. Talent and restricted Agents additionally pass an ownership filter (`talent_id IN allowed_talent_ids`) applied in SQL, not in Rust.

**Audit log**: append-only table of `(workspace_id, actor_user_id, impersonator_id?, action, resource, object_id, before, after, ip, user_agent, at)`. Viewable by Owner and Admin. Exportable.

## Core domain model

Every table below has `id` (UUIDv7), `workspace_id`, `created_at`, `updated_at`, `deleted_at`, `created_by`, `updated_by`, and a `custom` JSONB column governed by the custom-field engine. Timestamps are UTC `timestamptz`; event dates carry their own IANA timezone.

**Talent is the generic bookable.** Its `kind` is `person`, `act` (a group of people), `space` (a room, studio, stage) or `equipment` (kit with a quantity and optional serials). Everything that checks availability, holds dates, quotes, contracts and invoices works on a bookable regardless of kind, so a studio room and a session drummer go through the same code. Kind-specific fields (members for an act, capacity for a space, quantity and serials for equipment) are custom-field groups seeded by the preset, and the vocabulary (set per module, arriving with presets in M8) renames "Talent" to "Act", "Room", "Crew", "Kit" or "Member". A Booking can hold several bookables at once (a room plus an engineer plus a drum kit), each with its own availability check and line items, and can be **recurring** (weekly lesson, monthly residency) with per-occurrence overrides.

| Entity | What it is | Key fields | Relationships |
| --- | --- | --- | --- |
| Talent | An act, artist or creative the tenant represents | stage name, legal name, type (solo, band, DJ, speaker…), bio, genres/tags, base location, default fee + currency, commission %, status, portal user link | has many Availability, Bookings, Documents, Members |
| TalentMember | A person in a band or crew | name, role, contact details, dietary/travel notes | belongs to Talent |
| Organisation | A venue, promoter, brand, festival, agency | name, type, addresses, VAT/tax id, default currency, payment terms, notes | has many Contacts, Enquiries, Bookings, Invoices |
| Contact | A person at an Organisation (or standalone) | name, emails, phones, role, preferred channel, opt-in flags | belongs to Organisation; linked to Threads |
| Enquiry | An inbound or outbound request for talent | source (email, form, manual, portal), requested talent(s), event date(s), venue, budget, stage (pipeline), owner, lost reason | has 0..n holds (Bookings), which carry the Quotes; linked Thread |
| Quote | A priced offer | number (Q-0001, revisions Q-0001-R2), line items (fee, travel, extras; tax is applied on invoices), currency, validity date, status (draft, sent, viewed, accepted, declined, expired, withdrawn when a newer quote is sent), accepted snapshot | belongs to a Booking (usually a hold); accepting moves the hold to pending |
| Booking | A confirmed or held engagement | talent, organisation, contact, event name, venue, start/end (tz-aware), load-in/soundcheck/set times, travel (flights with airports and times, trains, ground transport, hotels), income and cost lines (buyouts, expenses), status (hold, expired for a lapsed hold that can be held again, pending, confirmed, cancelled, completed), fee breakdown, deposit terms, contract document, settlement | has many Invoices, Documents, Tasks; appears on Calendar |
| Availability | A talent's blocked or open time | talent, range, kind (unavailable, tentative, available-only), source (manual, portal, external calendar, connected personal calendar), note | belongs to Talent; drives clash detection |
| Invoice | A request for money | number (per-workspace sequence), issuer (workspace or talent), payer organisation, line items, currency, tax lines, due date, status (draft, issued, part-paid, paid, overdue, credited, void), historical flag for imported past invoices (own reference, never sent), Stripe link, accounting sync id | belongs to Booking (optional); has many Payments |
| Payment | Money received or paid out | amount, currency, method, received at, reference, allocation to invoices, payout to talent | belongs to Invoice(s) |
| Thread / Message | A conversation on any channel | channel (email, WhatsApp, SMS, portal note), direction, participants, subject, body (text + sanitised HTML), attachments, external ids, read state | belongs to Contact/Organisation and optionally Enquiry/Booking |
| Document | A generated, uploaded or imported file | kind (contract, rider, tech spec, EPK, invoice PDF, misc), template used, version, signature state, storage key, date, expiry, extracted text (imported PDFs are searchable) | belongs to Talent, Enquiry, Booking, Organisation, Contact or Invoice |
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
| Project | A container for related work over time: a tour, a season, a speaking circuit, a travelling exhibition, a shoot schedule, a release or brand campaign | kind, name, dates, owner, milestones (Tasks), budget, linked talent, crew, project-wide travel and costs | has many Bookings, Events, Tasks, Documents; profit and loss per date and overall |

A seller's Booking links to a Slot only when the buyer is also on the platform (two workspaces, one booking seen from both sides, each editing only its own fields); otherwise the Slot just records the external act's details.

**Invariants the core enforces**

- A Booking cannot be `confirmed` if its talent has an overlapping `confirmed` Booking or `unavailable` Availability, unless an owner or admin (`booking:override`) confirms over it with a reason, which is kept and audited; a Postgres exclusion constraint backs this. Holds, pending bookings and tentative time only warn.
- A Quote's accepted snapshot is immutable; editing creates a new version.
- Invoice numbers are gapless per workspace per issuing entity and never reused, even after void.
- Deleting a Talent, Organisation or Contact is a soft delete that keeps every Booking and Invoice intact.
- All monetary fields are `BIGINT` minor units with a `currency CHAR(3)` beside them; totals are computed, stored and re-verified on read.

## Generic engines

Six engines carry all the per-client variation. Each is a module of the `core-engines` crate (pure functions; storage in `core-db`, running in the worker and services), knows nothing about bookings or talent, and is exercised by its own test suite before any module uses it.

**1. Custom fields.** Per workspace, per entity type, an ordered list of field definitions: key, label, type (text, long text, number, money, date, datetime, boolean, select, multi-select, url, email, phone, file, relation-to-entity), required, default, validation (regex, min/max, options), visibility (staff, portal, both), searchable, and a `group` for layout. Values live in each row's `custom` JSONB and are validated server-side against the definition on every write. The API exposes definitions so the frontend renders forms and list columns from them automatically. Definitions are versioned; removing a field hides it rather than deleting data.

**2. Pipelines and statuses.** Any entity can be declared pipeline-driven (Enquiry is; Booking uses a fixed status set but allows additional sub-statuses). A pipeline is an ordered list of stages with optional SLA (days before a stage is flagged stale), allowed transitions, required fields to enter a stage, and whether the stage is terminal-won or terminal-lost. Stage changes emit `entity.stage_changed` with before/after.

**3. Automation rules.** A rule is `WHEN <trigger> IF <conditions> THEN <actions>`, stored as JSON and edited through a form builder in the UI.

- Triggers: any domain event (`booking.confirmed`, `invoice.sent`, `message.received`, `enquiry.stage_changed`…), a time offset relative to a date field (`booking.start - 7 days`, `invoice.due + 3 days`), or a schedule (cron per workspace timezone).
- Conditions: field comparisons on the triggering object and its parents (`booking.fee > 500000`, `organisation.country == "US"`, `custom.needs_visa == true`), including custom fields.
- Actions: send email/SMS/WhatsApp from a template, create a task, change stage or status, set a field, generate a document, notify a user in-app, call a webhook, enqueue a plugin action.
- Guards: once-per-object-per-rule by default; dry-run mode that logs what would have happened; per-rule execution log visible in the UI; kill switch per workspace.

**4. Templates and rendering.** Jinja templates (`minijinja`, sandboxed, no filesystem access) for email subject/body, SMS, WhatsApp, document HTML, and in-app notifications. The render context is a typed, documented tree (`workspace`, `talent`, `booking`, `organisation`, `contact`, `invoice`, `user`, `custom.*`) with safe formatting filters for money (`{{ booking.fee | money }}` → `£1,250.00`), dates in the booking's timezone, and addresses. Templates can `include` workspace-level partials (signature, legal footer). Every send stores the rendered output for audit.

**5. Documents.** An HTML template rendered to PDF (in the request, with a worker job as fallback), stored through the file store (Postgres by default, S3-compatible when it grows), versioned, with optional e-signature: a private emailed link where each signer types their name and ticks an agreement, recording time, IP and user agent, then a sealed, frozen copy with a signing page. Good enough for booking contracts; swap in a third-party e-sign provider through the integration layer later if clients need qualified signatures.

**6. Events, jobs and webhooks.** Every service-layer write appends a row to `outbox` in the same transaction. The worker drains the outbox into `jobs` (Postgres queue, `FOR UPDATE SKIP LOCKED`, priorities, scheduled `run_at`, exponential retry, dead-letter after N attempts, per-job log). Consumers: automation rules, integrations, audit, in-app notifications, SSE fan-out, and outbound webhooks (per workspace, signed with HMAC, with delivery log and replay). Scheduled jobs (daily digests, pollers, reminders) are cron rows per workspace.

**Engine API surface** (all under `/api/v1/`): `custom-fields`, `pipelines`, `rules`, `rules/{id}/dry-run`, `rules/{id}/log`, `templates`, `templates/{id}/preview`, `documents`, `documents/{id}/signatures` (signers use `/public/sign/{token}`), `events` (read-only stream for the workspace), `jobs` (the workspace's own jobs, owners and admins), `admin/jobs` (operators, queue depth across workspaces), `webhooks`.

## Functional modules

Six modules ship in v1 (importing past PDFs is part of Documents), each switchable per workspace. They are built only from the engines and the service layer; they never talk to integrations directly.

&#91;embedded content: enquiry to settlement · 8 statuses, the event each emits\]

Any status can also end in lost, declined, expired or cancelled, each with its own event; what triggers each arrow (deposit percentage, who signs first, chase timing) is configuration, not code.

**CRM** — Talent, Organisations, Contacts. Global search across all three plus bookings and messages (Postgres full-text, `pg_trgm` for fuzzy). Deduplication prompts on create (same email, same phone, similar name). Import from CSV with a column-mapping step and a dry run. Each record has an activity timeline merging messages, bookings, documents, tasks and audit entries. Tags are free-form per workspace. Talent records hold the public-facing assets (photos, bio, tech rider, stage plot, EPK links) that documents and portal pages pull from.

**Enquiry → Quote → Booking → Invoice** — the money loop.

1. Enquiry arrives by email (parsed into a thread and linked to a Contact by address), by the public enquiry form (per workspace, embeddable, with spam protection), through the portal, or by hand. It lands in stage 1 of the workspace's enquiry pipeline with an owner assigned by a rule (round-robin or by talent).
2. The agent checks availability (clash detection across confirmed bookings and talent unavailability, shown inline) and places a **hold** on the date, which is a Booking with status `hold` and an expiry.
3. A Quote is built from the talent's default fee and the workspace's standard line items, sent as a branded PDF plus a web link where the buyer accepts by typing their name. Acceptance snapshots the Quote, converts the hold to `pending`, and triggers the contract document and deposit invoice through rules.
4. Signed contract plus paid deposit moves the Booking to `confirmed`; it then appears on the calendar as firm, syncs to connected external calendars, and starts the pre-event rule chain (advance sheet requests, balance invoice, logistics tasks).
5. After the event date the Booking auto-completes, the balance invoice is chased until paid, and settlement computes commission and talent payout.

Every step is a stage or status change that emits an event, so each client's variations (deposit percentage, who signs first, whether a hold needs approval) are rules and configuration.

**Calendar and availability** — a workspace calendar with lanes per talent and per staff member, day/week/month/agenda views, holds shown distinctly from confirmed, drag to reschedule (with clash re-check), filters by status and tag. Talent availability is editable by staff and by the talent through the portal. iCal feed per talent and per workspace, token-protected (talent who want their bookings in their own calendar subscribe to it).

**Bookings in members' own calendars.** A member connects Google Calendar or Outlook (Settings → Calendar), for every booking or one talent's. OnHold makes a calendar of its own in that account ("OnHold · <workspace>") and keeps it filled: holds and pending bookings as tentative, confirmed ones as busy, with talent, venue and a link back, never the fee. It never touches the member's own events, and disconnecting removes OnHold's calendar. Google is asked only for `calendar.app.created`.

**Connecting talent's personal calendars.** An agency can send each talent a "connect your calendar" link that needs no OnHold account: a busy-only grant from Google or Microsoft, or a private iCal link (Apple and others). Only busy blocks are kept (no titles or details), as unavailability that clash checks use, read every 15 minutes for a year ahead and replaced on each read so the copy never grows. Busy and out-of-office time blocks; tentative time warns; busy time inside a booking OnHold knows (most likely that booking) only warns. Each member of a band can connect their own, and the act is busy when any member is. The solo preset and the talent portal connect directly. Across workspaces (an artist's own workspace and their manager's), each sees the other's dates as busy, with the artist's consent (M7).

**Messaging and follow-ups** — a unified inbox per workspace showing email (via connected Google, Microsoft or any IMAP/SMTP mailbox), WhatsApp and SMS threads, linked to contacts and to the enquiry or booking they concern. A connected mailbox is private to the member who connected it unless shared with the team. Reply from inside the app using templates with merge fields. Each member has a plain-text email signature per workspace, with a workspace default; it is added to inbox, quote, invoice and signing emails (rule emails use the workspace's). To keep storage small, the inbox keeps only mail from people, mail in an existing conversation and mail to or from a contact; attachments are not copied (name and size are kept and the file is read from the mailbox when opened); stored text and HTML are capped. A workspace can send from its own domain once the domain is verified (SPF, DKIM), set up automatically through Domain Connect where the DNS provider supports it, otherwise through guided manual steps with copy buttons and automatic checks. Snooze, assign and resolve. Follow-up sequences are rules: for example, enquiry not replied to in 2 days → reminder task; quote not opened in 3 days → chase email; invoice overdue 1, 7 and 14 days → three escalating templates then a task for a human. Opt-out and bounce handling per channel. Rendered sends are stored, and nothing is sent without a template or a human-written body.

**Documents** — generated from templates (contract, confirmation, advance sheet with travel, day sheet, invoice, quote, rider pack) with the workspace's branding, stored and versioned per booking or talent, optionally sent for e-signature. Uploads (signed PDFs, insurance certificates, passports for travel, stage plots) with expiry dates and a reminder rule for expiring items. A **Document pack** for a booking is a one-click zip of everything a venue or artist needs.

**Importing past PDFs** — bulk upload of old contracts, invoices and other files, attached to a talent, organisation, contact or booking (matched by filename or picked), with kind, date and optional expiry. No AI: the PDF's text layer is extracted so contents are searchable, Tesseract OCR reads scans when installed, and fixed patterns and per-issuer templates suggest fields (invoice number, dates, total and currency, emails) for a person to confirm before saving. Past invoices are imported as historical records (from PDFs, or entered one by one through the API, now; from Xero and QuickBooks history in M6; CSV import still to come) with the PDF attached: they keep their original numbers, count in reports, and are never re-sent. See `PDF-IMPORT.md`.

**Events and projects** (on by default for `buys` and `both` perspectives, available to all) — Events with a line-up of Slots, each slot running its own hold → offer → contracted mini-pipeline against the agent or act; a programme calendar showing events, slots and their fill state; recurring event templates for weekly nights; run sheets and day sheets generated from slot times; event settlement (fees, door split, costs, net) with a statement document. Projects group bookings, events and tasks under a release campaign, tour or season with milestone tasks and a budget, and give labels and tour managers a single page per campaign.

**Travel and tours** (any workspace can switch them on: agents and agencies as much as tour managers):

- **Travel on any booking**, not only tours: flights (number, airports, times), trains, rides and hotels, feeding the advance sheet, a phone-friendly day sheet and the artist's phone view. Live flight-status lookups can come later.
- **"Does it pay?"** on any booking: fee plus buyouts, minus travel, hotels, crew and other costs, minus commission, gives the net, converted across currencies, with a warning when costs eat the fee.
- **Tours as projects**: a self-contained run of dated bookings with routing, crew, tour-wide travel and costs, a budget against actuals, milestone tasks, a day-by-day itinerary and printable day sheets, and a profit and loss per date and for the whole tour.
- **Field-neutral**: the core never assumes music. The same project is a theatre, dance or comedy tour or season, a speaking circuit or book tour, a travelling exhibition, a shoot schedule or production, or a campaign or release run, each with its own words.

## Integrations layer

Every provider implements one of four traits in `core-integrations` — `MailProvider`, `CalendarProvider`, `PaymentProvider`, `AccountingProvider` (plus `MessagingProvider` for WhatsApp/SMS) — so modules code against the trait and a workspace picks the provider (only `MailProvider` exists so far; Stripe is a concrete client until a second payment provider arrives). OAuth tokens and mailbox passwords are encrypted at rest (AES-256-GCM with the `ENCRYPTION_KEYS` keyring; rotate by prepending a new key), refreshed by the worker, and a failing integration shows a red health badge in the UI with the last error and a reconnect button, never a silent failure.

| Provider | Trait | v1 scope | Notes |
| --- | --- | --- | --- |
| Google Workspace / Gmail | Mail | Send as user; receive by polling the Gmail history API (Pub/Sub push later); thread + label mapping; attachments read on demand | One connection per user; shareable with the team. Gmail's mail scopes are "restricted": Google must verify the app (yearly security assessment) before members outside the operator's own domain can connect |
| Google Calendar | Calendar | Write bookings into a calendar OnHold makes in the member's account (holds tentative, confirmed busy, never the fee); read talent's busy times (free/busy) as unavailability | `calendar.app.created` for members, `calendar.freebusy` for talent (no OnHold account needed); both are sensitive scopes, so Google must verify the app (no security assessment) |
| Microsoft 365 (Graph) | Mail + Calendar | Mail via Graph delta queries (subscriptions later); members' bookings written to a calendar OnHold makes, with `Calendars.ReadWrite` (Microsoft has no narrower grant; OnHold writes only to its own calendar); talent's busy times via `Calendars.ReadBasic` (times and "show as" only) | Shared mailbox support |
| Any mailbox (IMAP + SMTP) | Mail | Read INBOX by UID; send by SMTP; servers pre-filled for common providers | Public servers on the usual ports with TLS only |
| Resend | Mail (sender domains) | Verify a workspace's own sending domain (DKIM, and SPF on a `send` subdomain) and send OnHold's mail from it; OnHold's own fallback sender | Up to 5 domains a workspace, one in use; a domain belongs to one workspace and is released after 14 days unverified. Domain Connect for one-click DNS setup (OnHold's template is written but not yet submitted to DNS providers), guided manual fallback; Entri (paid) optional for wider coverage |
| Apple and other calendars | Calendar | Private iCal link read for busy time | For talent connecting a personal calendar |
| Stripe | Payment | Stripe Connect (Standard) per workspace so money settles to the client, not to you; payment links on invoices (card-only Stripe Checkout on the client's connected account); deposit and balance; webhooks for paid/failed/refunded; multi-currency per workspace's Stripe account | Platform fee optional, off by default |
| Xero | Accounting | Push invoices and payments; pull paid status; contact mapping; tax-rate mapping table per workspace; import past invoices as historical records | OAuth 2, tenant id per org |
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
- **Rates**: ECB reference rates fetched into a `fx_rates` table; a booking may pin a manual rate. The rate used is, in order: same currency, a manual rate given at issue, the credited invoice's rate, the booking's rate, then the ECB rate for the issue date. Reporting converts at the rate on the document date.
- **Tax**: a per-workspace table of tax codes (name, rate, jurisdiction, applies-to) seeded with sensible defaults for the workspace's country (UK VAT 20/5/0/exempt; EU VAT with reverse-charge flag; US sales tax left to the accounting system; AU GST). Each line item carries a tax code; invoice totals show tax lines grouped by code. Tax-exempt and reverse-charge wording is template-driven.
- **Invoice compliance**: configurable number format (`INV-{YYYY}-{0000}`), issuer legal name, address and tax id, payer details, issue and due dates, payment terms, bank details per currency, late-fee text. A locked PDF is generated on send. Credit notes reverse invoices rather than editing them.
- **Who invoices whom**: three settings per workspace — agency invoices the buyer and pays talent out; talent invoices the buyer directly and agency invoices talent for commission; or a split (deposit to agency, balance to talent). The model must support all three because agencies in different countries do it differently.
- **Payments**: Stripe card payment links on invoices; manual payments (bank transfer, cash) recorded by hand; partial payments and over-payments allocated across invoices; refunds. Status moves to `paid` only when allocations equal the total.
- **Settlement and payouts**: on booking completion, compute gross fee, agency commission (per talent default, per booking override), expenses, talent net; generate a statement document; record the payout as a Payment with method and reference. Export to accounting.
- **Reporting** (v1 minimum): bookings and revenue by month, by talent, by organisation, in base currency; outstanding and overdue invoices; conversion funnel from enquiry stages; CSV export of everything.

## Talent portal

A deliberately simple part of the web app (`/portal`, loaded only for talent members, in place of the staff frame) on the same API, branded per workspace, for the people the tenant represents. It must feel good on a phone and need no training. For an artist invited by their manager it is a **free viewer**: useful on its own, and a window onto what OnHold could do for them.

- Sign-in by magic link or password; the talent sees only their own talent record(s). One login moves between the free viewer and any workspaces of their own.
- **Dates**: upcoming and past bookings with status, venue, times, fee (if the workspace allows), documents (contract, advance sheet, rider), travel and advance information; a confirm/decline button on pending dates that moves the booking and notifies the agent.
- **Availability**: mark unavailable or tentative ranges; connect their own Google, Microsoft or Apple (iCal) calendar so busy time updates automatically (busy only, no details).
- **Messages**: a thread per booking plus a general thread with the agent; replies land in the staff inbox.
- **Profile and assets**: update bio, photos, tech rider, stage plot, social links, travel details, band members; staff can require approval before changes go live.
- **Money** (switchable per workspace): statements, payout history, their own invoices to the agency if that model is used.
- Everything the talent can see or do is a permission the workspace Owner toggles, so a cautious agency can start with read-only dates.

**Locked features and upgrading** (decided with Solomon, Oct 27, 2026):

- No ads or direct upsell in the free viewer. Paid features are visible but **locked**, with a padlock icon.
- Tapping a locked feature, or the **"Unlock features"** button, opens a read-only **preview of the full app** filled with the artist's own real gigs (or sample data), with a link to upgrade on OnHold's website. It is not a free trial, and nothing is bought in the app.
- Upgrading lets the artist pick the modules and presets they need (artist, label manager, promoter, producer, studio…), combinable in one workspace.
- A **"Run your own bookings"** path starts the artist's own workspace, linked to the manager's: each sees the other's dates as busy, with the artist's consent.
- Agencies get a "hide locked features" setting (off by default) for their white-labelled portal.
- If an artist leaves their manager, they keep their own workspace and lose the manager's view; terms and privacy wording must say so.

## Frontend application

The staff app is a single-page React application that renders itself from the API: entity forms and list columns come from custom-field definitions, stages from the pipeline, actions from permissions. A client's bespoke screens are a directory under `web/tenants/<slug>/` that the app loads when the signed-in workspace's `plugin_slug` matches.

**Structure**

- `packages/api-client`: generated from the OpenAPI spec in CI; the build fails if the spec changed and the client was not regenerated.
- `packages/ui`: design tokens (colour, spacing, type) as CSS variables that a workspace's branding overrides at runtime; shadcn/ui components; a `DataTable` (server-side sort, filter, paginate, column chooser, saved views, bulk actions), `EntityForm` (renders core + custom fields from a schema), `Timeline`, `Calendar`, `Inbox`, `MoneyInput`, `DateTimeTzInput`, `DocumentViewer`, `RuleBuilder`, `TemplateEditor` with live preview.
- `apps/app`: routes per module (`/crm`, `/enquiries`, `/bookings`, `/calendar`, `/inbox`, `/documents`, `/finance`, `/settings`), a global command palette (⌘K: search, create, navigate), keyboard-first lists, optimistic updates with TanStack Query, SSE subscription for live changes.
- The talent portal (`/portal`): a lazily loaded part of `apps/app` with its own frame, a quarter of the surface.

**Extension points the frontend exposes to a tenant directory**: add routes and nav items; add a tab to any entity page; add a widget to the dashboard; override a template's preview; register a custom field renderer; register a bulk action; replace the branding theme. Each is a typed registration function; nothing in `apps/app` imports from `tenants/` directly.

**Quality bar**: works on a 375px phone and a 1440px desktop in any modern browser on any operating system; every list loads under 300ms at p95 with 10,000 rows per table per workspace; WCAG AA contrast; no layout shift on load; empty states that tell the user what to do next; dark mode via tokens.

### Distribution: website and apps

OnHold must work on every operating system: in any browser, and as desktop and mobile apps that work offline.

- **The installable website is the primary way to get "the app".** A web app manifest and a service worker let people Add to Home Screen or install it from the browser, under the client's own name and icon when served from their domain. No store, no cost. It keeps an offline copy only when installed; in an ordinary browser tab it keeps nothing.
- **Native apps** are Tauri 2 shells around the same web app for Windows, macOS, Linux, iOS and Android. The desktop apps are built by CI (unsigned for now); iOS and Android builds from the same shell are still to come. They stay in the repo but are **not published** until it makes financial sense. They sign in by approval on the website (a short code typed at `/link-app`), keep their session in the system keychain, and remove everything they hold on sign-out.
- **Offline**: records are cached on the device (IndexedDB) and kept current from the events feed; changes made offline are queued, folded per record and replayed in order with `Idempotency-Key` and `If-Match`. A conflict is shown on the Sync page (keep mine or use theirs), never silently overwritten. Clash checks run on the device with the server's rules and are checked again on arrival. Email on the device is opt-in per device and capped (two weeks, text only, 5 MB in all).
- **Plan switch**: a per-plan "apps allowed" switch (`APP_PLANS`), enforced by the API, decides which workspaces may use the native apps (default: all, until plans exist). The website, installed or not, stays fully usable on every plan.
- **Store listings**, when published, are one shared OnHold app with OnHold's name and icon; the client's logo and colours show after sign-in.

## Per-client bespoke work

There is only ever one codebase and one app. Each client's workspace stores its setup as data (modules switched on, presets, words, pipelines, templates, branding), and the app reads it when someone signs in. Special features come in three levels, never a fork:

1. **A feature many clients could want** (a tour itinerary, say) is built once as a module, gets a tick box on the setup questionnaire and a line in the price list (both arriving with the questionnaire), and each client switches it on or off. Everyone on it gets every fix.
2. **Something one client wants done their way** (their own fields, stages, templates, rules) is configuration in their workspace, set from Settings with no code.
3. **Truly bespoke logic for one client** (their own commission formula, say) is a small plugin named for that client, plugged into fixed hook points in the core and switched on only for their workspace. This is the paid bespoke work.

Once the questionnaire lands, a client's request from its comments box shows on the Client workspaces page, where OnHold's team decides which level it is.

A new client's "custom app" is the core plus, in order of preference: configuration, frontend tenant directory, backend plugin. Choosing the lowest layer that works is the whole discipline that keeps one deploy serving everyone.

**Onboarding questionnaire** (built, Oct 28, 2026). A plain form where a client ticks what they will use OnHold for (their roles, such as solo artist, agency, manager, label, promoter, venue or tour management, and features), says how big their team is, and leaves comments to request things or ask questions.

- The answers choose combinable presets, modules, words per module and extras (team, commission, advanced), and the app follows: menus, sections, screens and buttons scale with the setup, so a solo artist sees a simple app with no multi-agent assignment, commission splits or team pages, and a full agency sees everything.
- Nothing is locked in: a "Your setup" page lets them re-run or edit it any time. Until it is sent, owners and admins are offered it with a "Set up OnHold" banner.
- A **live price estimate** beside the answers updates as they tick (a base price, plus a price per ticked feature, plus a price for the team size). The price list is data OnHold's team edits, with placeholder numbers until pricing is decided. Nothing is charged from it.
- Comments and the setup show on the Client workspaces page, with "Mark done" for requests.
- **Streamline** (master admin, per client): on top of what the presets give, OnHold's team can hide any menu item, page section, button or optional field for one client. These choices are kept apart from the questionnaire answers, so they survive re-running it, and anything can be unhidden with one click. Hiding never deletes data, and fields that invoices or contracts depend on (amounts, dates, the payer's name) stay required behind the scenes.

| Client asks for | Layer | Example |
| --- | --- | --- |
| Different fields, labels, stages, statuses, templates, reminders, branding (logo, colours, name, custom domain, installable app icon) | Configuration (no code) | "We need a 'tour name' on every booking and a 'rider received' stage" → custom field + pipeline stage, done in Settings |
| A screen, dashboard, report or widget arranged their way | Frontend tenant directory | "A tour grid showing all dates for one artist with travel between them" → `tenants/acme/TourGrid.tsx` registered as a route |
| Logic the rules engine cannot express | Backend plugin crate | "Commission is 15% but 10% on repeat bookings with the same venue within 12 months" → plugin registers a `compute_commission` hook |
| A new integration | Core (shared) | "Sync to Pipedrive" → built as a provider so every tenant can have it |

**Onboarding a new client** (target: under two hours of your time)

1. `onhold admin workspace create --slug acme --name "Acme Agency" --country GB --currency GBP --timezone Europe/London --owner-email owner@acme.example --plan standard` seeds defaults: roles, tax codes and templates; a `--preset` option (for example `--preset agency,manager`, combinable) sets the presets' modules, extras and words now, and from M8 their pipeline, rules and custom fields. The owner can then adjust it in the setup form.
2. Import their spreadsheet with the CSV importer; connect Google or Microsoft; connect Stripe.
3. Adjust configuration with them on a call; most "bespoke" asks end here.
4. If they need a tenant directory or plugin, create it from the templates in `web/tenants/_template` and `crates/plugins/_template`, ship it behind their slug.
5. Hand over, start the monthly fee.

**Operator admin area** (superadmin only): the master admin is inside the same app, not a separate one. OnHold's team sees an "OnHold team" section in the menu:

- **Client workspaces**: every client with status and plan (built). Built with the questionnaire: their setup and answers, their comments and requests with "Mark done", a Streamline panel for hiding parts of their app (see Onboarding questionnaire), and the price list behind the estimate. A **Visit** button goes into a client's workspace to help: it needs a reason, is time-boxed and shows to the client in their audit log (it reuses superadmin impersonation).
- **Theme studio**: each client's full theme, with contrast checks.

Still to come (M8): health, last activity and usage counters; create and suspend workspaces from the browser; failing jobs and integrations across all tenants; feature flags per workspace; export and backups; release notes broadcast.

### Plugin SDK contract

A backend plugin is a crate implementing `onhold_plugin_sdk::Plugin`, compiled into the single `onhold` binary, and activated only for workspaces whose `plugin_slug` matches. The SDK is the only way a plugin touches the system.

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

- One image, one binary with subcommands: `onhold serve` runs the API and worker in one process (the launch setup), and `onhold api` and `onhold worker` run them as separate containers when scale needs it; `onhold migrate` runs migrations as a one-shot step. Static frontends on a CDN (Cloudflare Pages). Managed Postgres with point-in-time recovery. Files in Postgres at first, S3-compatible object storage (Cloudflare R2) once they grow.
- **Launch plan (near-zero cost and operations while there are no clients):** Neon free Postgres, one auto-stopping Fly.io machine running `onhold serve`, Cloudflare Pages for the web app, Resend free tier for mail. Scale by keeping one machine always running (`min_machines_running = 1`), adding machines, or splitting out the worker. Accounts are created at deploy time, not before.
- Environments: `dev` (docker-compose, seeded demo workspace), `staging` (mirrors prod, anonymised copy of data on demand), `prod`.
- CI on every pull request and on `main`: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `sqlx prepare --check`, OpenAPI diff, TypeScript typecheck, frontend unit tests, Playwright smoke suite against a fresh database (desktop and phone sizes), build and unit tests on Windows and macOS as well as Linux, and desktop app builds for all three. Once hosting accounts exist, merge to `main` will deploy to staging and a tagged release to prod with a one-command rollback (not built yet).
- The repository is public for now, so CI minutes are free. Just before the first paying client goes live it goes private, and CI is trimmed to fit the free private minutes: Linux on every push (the server runs on Linux), Windows and macOS on releases only.
- Migrations run before the new version starts and must be backwards-compatible with the previous version for one release (expand/contract pattern) so deploys are zero-downtime.
- Data residency: the stack must be deployable as a second, isolated region (EU and US at minimum) by changing config only; a workspace is pinned to one region at creation.

**Backups and recovery**: continuous WAL archiving plus nightly logical dumps kept 30 days; object storage versioning; a monthly restore drill script that restores to staging and runs the smoke suite; per-workspace export (JSON + CSV + documents zip) on demand, which doubles as the GDPR data-portability export.

**Observability**: structured logs with `workspace_id` and request id on every line; traces for every request and job; dashboards for request latency, job queue depth and age, integration error rate, email bounce rate, per-workspace activity; alerts to your phone for 5xx spikes, queue age over 10 minutes, failed backups, expiring certificates and OAuth tokens. A `/healthz` and `/readyz` on both containers.

**Security**

- RLS on every tenant table; a test that asserts every table with `workspace_id` has a policy.
- OWASP basics enforced in middleware: CSRF on cookie sessions, rate limits per user (per IP in M8), strict CSP, security headers, input size limits, HTML sanitisation of inbound email and user content.
- Secrets from environment or a secret store, never committed; `cargo audit` in CI (`pnpm audit` still to add); dependabot.
- Files are served only through the authenticated API (or private links), with type detected from their bytes and `Content-Security-Policy: sandbox`. Virus scanning (ClamAV in the worker) and signed, expiring URLs once files move to object storage are M8 hardening.
- Per-workspace API keys scoped to permissions; all keys hashed; last-used tracking.
- Password policy, breached-password check (k-anonymity API, M8), session listing and revocation per user, mandatory 2FA for superadmin.

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
- **Integration tests** against a real Postgres (a fresh throwaway database per test with `#[sqlx::test]`) for every API endpoint, including a negative test proving tenant isolation (user in workspace A cannot read, update or delete anything in workspace B by any route).
- **Contract tests** that replay recorded provider responses (Google, Microsoft, Stripe, Xero, QuickBooks, Twilio, WhatsApp) so integrations are tested without live accounts; a separate opt-in live-sandbox suite.
- **End-to-end** Playwright flows: onboard workspace → import CSV → enquiry by form → hold → quote → accept → contract signed → deposit paid → confirmed → appears on calendar and Google Calendar → balance invoice chased → paid → settlement statement generated.
- **Performance**: seed a workspace with 500 talent, 5,000 organisations, 50,000 bookings and 200,000 messages; every list and search p95 under 300ms, calendar month view under 500ms.
- **Accessibility**: axe checks in Playwright; keyboard navigation for every list and form.
- **Storage size**: every change is checked for what it stores in the cloud and on devices, and caps are tested.
- **Reviews**: every PR gets a full ("ultracode") code review once CI is green, and its confirmed findings are fixed before merging.
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
   - **Apps (after M3).** Native Tauri desktop apps (iOS and Android from the same shell still to come) with sign-in by approval on the website, an offline copy and a queue of offline changes, the apps plan switch, branded builds; then the installable website (PWA) reusing the same offline engine. Demonstrable: sign the app in on the website, keep working with no connection, and settle a conflict when someone else changed the same record. See `MILESTONE-APPS.md`.
5. **M4 — Bookings and calendar.** Enquiry pipeline, holds, quotes with web acceptance, bookings with statuses and clash detection, availability, workspace calendar views, iCal feeds, tasks. Demonstrable: enquiry → hold → quote → accept → confirmed booking on the calendar, with the clash test blocking a double booking.
6. **M5 — Documents and money.** Document engine with PDF and e-signature; contract, confirmation and advance-sheet templates; invoices, credit notes, payments, settlement, tax codes, FX, reporting; importing past PDFs and past invoices (`core-pdf`); Stripe Connect with payment links and webhooks (a separate PR). Demonstrable: the full E2E flow through deposit paid and balance chased, in two currencies.
7. **M6 — Messaging and integrations**, in parts: (1) unified email inbox for Gmail, Microsoft and any IMAP/SMTP mailbox, then opt-in recent email on a device; (2) sending from a workspace's own domain (Resend, SPF/DKIM) with Domain Connect and a guided manual fallback; (3) talent connecting their own calendars (busy only); (3b) bookings written into members' own Google or Outlook calendars; alongside, email signatures; (4) Xero and QuickBooks, including past invoices; (5) SMS, WhatsApp and follow-up rules. Demonstrable: an email enquiry arrives in the inbox linked to its contact; a confirmed booking appears in Google Calendar; a paid invoice shows paid in Xero.
   - **Alongside M6 (Oct 28, 2026):** client branding and the theme studio with audited visits, carried to every screen, email, PDF and public page (built); the onboarding questionnaire with setup-driven navigation and a live price estimate (built); travel on any booking with the "does it pay?" check and day sheets, then tours as projects with profit and loss (being built).
8. **M7 — Talent portal (free artist viewer).** Portal app with dates, confirm/decline, availability with connected calendar, riders, travel and advance info, documents, messages, profile, money view, per-workspace permission toggles; padlocked paid features with "Unlock features" opening a preview of the full app and an upgrade link to the website; agency "hide locked features" setting; "run your own bookings" path to the artist's own linked workspace; cross-workspace busy dates with consent. Demonstrable: a talent confirms a pending date from a phone and the agent is notified.
9. **M8 — Operator tooling and hardening.** Admin area, workspace CLI, vertical presets and seeds (combinable in one workspace and addable later, plus simple solo artist, DJ and producer presets and rider and travel templates), tenant export, erasure, retention, backups and restore drill, performance seeding and tuning, security review, runbooks, plugin and tenant-directory templates with one worked example each. Demonstrable: onboard a new client from CLI to first login in under two hours; restore drill passes.

After M8 the core is v1. Bespoke client work starts at M3 if a first customer is waiting — their configuration and frontend directory can be built as modules land. After v1: the full talent version (solo presets as a product, fee splits, royalties and releases).

## Assumptions made and decisions still open

**Assumed** (change these in the spec before starting if wrong):

- Working name **OnHold (written OnHold in the product, onhold in code); it appears only in crate names, the CLI and this document**.
- Rust backend, TypeScript frontend, Postgres, one shared deployment, as discussed.
- Vertical presets at launch (built with the onboarding questionnaire): `agency`, `manager`, `freelance` and `solo` (artist, DJ, producer), combinable; the full preset list and the buyer/seller perspective are in the Vertical presets section.
- Stripe Connect Standard so each client holds their own Stripe account and the money never passes through you.
- E-signature is built in at a simple, evidential level; qualified e-signatures are a later integration.
- Email ingestion uses the client's own mailbox (Google, Microsoft or any IMAP provider); no separate inbound mail domain in v1.
- No AI features in v1; the events bus and outbox are the seams for later, plus a `drafts` table still to add. Reading imported PDFs uses fixed patterns, not AI.

**Decided** (since the first version):

- [x] M1: one login can belong to many workspaces.
- [x] M4: holds lapse automatically after the workspace's hold period (7 days by default, 0 meaning never), with no reason or approval needed to place one.
- [x] M5: the default invoicing model is `agency` (the agency invoices and pays talent out); a workspace can switch to `talent` or `split`.
- [x] M5: built-in contract, confirmation, advance sheet, invoice, credit note and statement templates for every workspace, each overridable. Rider and travel templates come with the presets in M8.
- [x] Apps: native Tauri apps, built but unpublished; the installable website first; one shared store app; a per-plan apps switch.
- [x] Hosting: Fly.io (one auto-stopping machine) with Neon, Cloudflare Pages and Resend at launch.
- [x] Talent side: free artist viewer with padlocked features in M7, combinable presets in M8, full talent version after v1.

**Open** (answer before the relevant milestone):

- [ ] M6: WhatsApp Business verification is per client and takes days; is it a launch requirement or a per-client add-on?
- [ ] M6: submit OnHold's Domain Connect template to DNS providers (after checking its signature format with the Domain Connect checker).
- [ ] M6: Gmail needs Google's app verification (with a yearly security assessment) before members outside the operator's own domain can connect it; when to start it.
- [x] M7: talent fees are off until the agency turns them on (Settings → Talent portal).
- [ ] M7: how cross-workspace busy dates and the artist's consent work in detail, and what an artist keeps when leaving a manager (terms and privacy wording).
- [ ] M7: OnHold's own subscription billing (free viewer, paid plans), separate from clients' Stripe Connect.
- [ ] M8: your pricing tiers, so plan limits (seats, talent count, storage, apps allowed) can be enforced, and the onboarding price list's placeholder numbers replaced.
- [ ] Hosting: EU-only at launch or EU + US regions from day one? And how the API shares the web app's origin (needed for cookies and sign-in redirects): a Cloudflare route sending `/api` to the Fly machine, or the API serving the static app.
- [ ] M5: whether to fetch ECB rates for workspaces outside Europe too.
- [ ] Before the first paying client: make the repository private and trim CI (see Operations); check single-user plans sold outside the App Store against Apple's rules before publishing store apps.
