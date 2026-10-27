# Milestone 3: CRM and frontend shell

**Demonstrable:** import a spreadsheet, find a contact, add a custom field, see it in the form and the table.

It does, twice:

- **In a browser** (`web/apps/app/e2e/demo.spec.ts`, run in CI at 1440 px and at 375 px against the real API and Postgres). A new workspace's owner accepts their invitation, uploads a three-row CSV in the import wizard (the "Company" column is mapped to the organisation automatically), checks it, imports it, finds Priya Patel with ⌘K search, adds a "Dietary needs" field in settings, fills in "vegan" on Priya's record, and sees the new column with "vegan" in the contacts table. On the phone size it also checks that no page scrolls sideways.
- **Through the API and worker** (`crates/core-worker/tests/crm.rs`, `the_m3_demo_import_find_add_a_field_and_see_it`), the same steps without a browser, including the organisations the import creates.

## Built

- **Records:** talent (stage name, legal name, type, bio, base location, default fee, commission, status, band members), organisations (type, addresses, tax number, currency, payment terms) and contacts (name, emails, phones, role, organisation, preferred channel, marketing opt-ins), each with tags, owner, notes and the workspace's custom fields. Every change is versioned, audited and emits an event, so rules and templates (M2) now work on CRM records too (`contact.created` can send a welcome email).
- **Search:** one box across all three (`/search`, ⌘K in the app), forgiving of typos, including searchable custom fields. Lists search, filter by tag, sort by any column or custom field, and page.
- **Duplicates:** before creating, the API and the form say what looks like a match and why (same email, same phone number, same tax number, similar name).
- **Tags** with counts, **bulk actions** (tag, untag, delete) and **saved views** (columns, filters and sort, personal or shared).
- **Activity timeline** per record: changes, who made them, emails sent, and for an organisation or talent its contacts' and members' activity too.
- **CSV import:** upload, suggested column mapping, a dry run listing every problem by spreadsheet row, then a background job with progress. Matches can be skipped, updated or created anyway; contacts can create their organisations; rules only run if asked.
- **One-process mode:** `onhold serve` runs the API and the worker together, for the cheapest hosting while there are no clients.
- **Web app** (`web/apps/app`): sign in with a password, an emailed link, Google or Microsoft; two-step verification; accepting invitations; navigation that becomes a menu on phones; workspace switcher; ⌘K search; light, dark and device themes; contacts, organisations and talent lists and records with timelines; the import wizard; custom field and pipeline settings.
- **UI kit** (`web/packages/ui`): design tokens with dark mode, buttons and inputs, dialogs and menus, `DataTable` (server-side search, sort, tag filter and paging, column chooser, saved views, bulk actions), `EntityForm` (built from `/schema/{entity}`, every custom field type, server issues shown on their fields), `Timeline` and the command palette.
- **API:** 128 operations (40 new), in the OpenAPI document and the regenerated TypeScript client.

## Verified

- 207 Rust tests pass on a real Postgres 16 (`cargo test --workspace`); clippy is clean with `-D warnings`; the offline query data and the generated client are current.
- **CRM API** (12 tests): create, read, update and delete for each record with validation and versions; talent members; contacts linked to organisations; list search, relevance, tag filters, sorting by custom fields and paging; global search; duplicate reasons; timelines; bulk actions; saved views and their privacy; the schema endpoint; import mapping suggestions and dry runs, and refusals for files they cannot read or that are over the size limit; rules and templates on CRM events.
- **Worker** (4 new tests): the demo; a 450-row import split across three jobs with rules skipped unless asked; a rule emailing new contacts with the email showing on their timeline; abandoned uploads deleted.
- **Isolation suite** covers all 40 new routes automatically: another workspace's talent, organisations, contacts, tags, views, imports, timelines, search results and duplicate matches are invisible and untouchable, by session and by API key, including through bulk actions.
- **Database level:** row-level security on every new table; the worker can see only an import's id, status and age (to clean up), never its contents. The migration reverts to exactly the M2 schema.
- **Web:** type-checked, 14 unit tests (Vitest), production build, and the browser demo above, all in CI. Checked by eye in light, dark and phone layouts.

## Skipped, and why

- **Date-offset rules on CRM records** ("30 days after a contact was created"). Rules on CRM events work; date offsets still run for things only. They arrive with the first date field that needs them (bookings, M4).
- **Merging duplicates.** Duplicates are found and shown; merging two records (and their history) is a tool of its own, best built once real data shows which fields people need to choose between.
- **Excel files.** Import takes CSV, which every spreadsheet app exports. XLSX can be added to the same parser.
- **Agents seeing only their own records.** Agents see all CRM records in their workspace. The spec does not say otherwise; restricting it is a permission scope to add when a client asks.
- **Renaming "Talent" per workspace** (a vocabulary table). The words are fixed in English for now.
- **Talent assets and EPKs** (photos, riders, press kits) and **file custom fields** in the form. Both need file storage, which arrives with documents (M5).
- **Per-tenant frontend overrides** (`web/tenants/<slug>/`). The shell is ready for them; the first one comes with the first client that needs one.
- **Branding from the workspace.** The UI kit can apply a workspace's colours and logo (`applyBranding`), but the API does not serve them yet.
- **Offline, desktop and mobile apps.** Solomon chose native apps via Tauri while this milestone was finishing; they come next, after M3 merges. The web app is already a static build with a configurable API address, which is what Tauri packages.
- **Pipelines on CRM records.** Pipelines work for things; which CRM records move through stages is decided with deals and bookings (M4).

## Open

- Hosting: the low-cost starting plan is recorded in `DECISIONS.md`; accounts are created when we first deploy.
