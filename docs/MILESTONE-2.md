# Milestone 2: Engines

**Demonstrable:** a rule on `thing.created` sends a templated email through the queue with retries.

It does: `crates/core-worker/tests/worker.rs`, `a_rule_on_thing_created_sends_a_templated_email_with_retries`. A workspace saves a `welcome` email template and a rule "when a thing is created, if it has a client email, send `welcome` to it". A thing is created through the API with the mail server set to refuse the first two attempts. The worker publishes the event, runs the rule, and queues the email; the job fails twice (logged with the server's "421 try again later"), succeeds on the third attempt, and the rendered email ("Welcome aboard, Spring campaign", HTML and text, from "acme agency") is stored in the message log. A second thing with no client email is logged by the rule as skipped, with the failed condition as the reason. All of it is visible through `/messages`, `/rules/{id}/log` and `/jobs/{id}`.

## Built

- **Custom fields** (`/custom-fields`): text, long text, number, money, date, datetime, boolean, select, multi-select, url, email, phone, file and relation, with required, default, validation (regex, min/max, length, options, currency), visibility, searchable, group, position and help text. Values are validated on every write, all problems reported at once. Archiving hides a field and keeps its data and key.
- **Pipelines** (`/pipelines`): ordered stages with colour, SLA days, won/lost outcome, required fields to enter and allowed next stages. Editing a pipeline matches stages by key; removed stages are archived, not deleted. A pipeline in use cannot be deleted.
- **Things** (`/things`): the stand-in entity, with name, due date, pipeline stage and custom fields. Every change emits `thing.created`, `thing.updated`, `thing.stage_changed` or `thing.deleted`.
- **Templates** (`/templates`): email, SMS, WhatsApp, notification and partial templates in minijinja, sandboxed, checked on save with the line of any error. Filters for money (`£12,500.50`), dates and datetimes in the workspace's timezone, and addresses; `{% include "signature" %}` for partials. Live preview of a saved or unsaved template against a real record or sample data, with warnings for variables the record lacks. `GET /templates/variables` documents the whole context, custom fields included.
- **Rules** (`/rules`): `WHEN` an event, a date offset (`due_at` or any date field, plus or minus minutes) or a cron schedule in the workspace's timezone, `IF` nested all/any/not comparisons (13 operators, money compared in minor units, `changed` for updates), `THEN` send an email (to an address, an email field or every member with a role), set a field, move a stage, or call a webhook. Every reference is checked on save. Guards: once per record by default, dry-run mode, a depth limit on rules triggering rules, and a per-workspace kill switch (`PUT /automations`). `POST /rules/{id}/dry-run` explains each comparison and lists what the rule would do; `GET /rules/{id}/log` shows what it did and why it did not.
- **Event feed** (`/events`, `/events/stream`): every workspace event in order with a position, filterable by topic patterns, by cursor or live over server-sent events. This is the changes feed the offline desktop and mobile apps will sync from.
- **Webhooks** (`/webhooks`): endpoints subscribe to topics or patterns; deliveries are signed (`OnHold-Signature`, HMAC-SHA256), retried, logged with status and response excerpt, and can be replayed or tested with a ping. Private and internal addresses are refused.
- **Jobs** (`/jobs`, `/admin/jobs`): the workspace's own queue with each attempt's log, retry for dead jobs, and an operator view of queue depth across workspaces. `/messages` lists every email sent.
- **Worker** (`onhold worker`): publishes the outbox, runs jobs (8 at a time by default) with exponential backoff and dead-lettering, schedules date and cron rules, returns jobs whose worker died, and deletes old events (30 days), finished jobs (7 days) and stored idempotent responses (24 hours). Woken by database notifications, with polling as the fallback; several workers can run at once.
- **Email:** invitation and sign-in emails are now actually sent (M1 queued them), through any SMTP server. The invitation wording can be overridden per workspace. Links are never stored readable. Mailpit is in the development stack.
- **API:** 88 operations (50 new), in the OpenAPI document and the regenerated TypeScript client.

## Verified

- 178 tests pass on a real Postgres 16 (`cargo test --workspace`); clippy is clean with `-D warnings`; `cargo audit` is clean.
- **Worker** (11 tests): the demo above; invitation and sign-in emails arrive and their links work, while no readable link is left in the outbox, the queue or the message log; a permanent refusal goes dead after one attempt and can be retried from the API; temporary failures stop after 10 attempts with one failure logged; webhooks reach a real local HTTP server, fail once with 503, succeed on retry, and verify with the endpoint's secret; two rules that undo each other stop at the depth limit (7 runs each, 8 refused); a paused workspace runs nothing and says so; date-offset and cron rules fire from the scheduler exactly once; a job abandoned by a stopped worker is picked up again; cleanup prunes old events and expires old cursors; the running worker reacts to new work and shuts down cleanly.
- **API** (9 new tests): field validation and versioning; things against their fields and pipeline (allowed moves, required fields, every issue reported); stage archiving; template syntax errors with line numbers, preview with real money formatting, HTML escaping and missing-variable warnings; rule checks on save and dry-run explanations; the event feed's order, paging, filtering and expiry; the live stream; webhook secrets shown once and stored encrypted; private webhook addresses refused.
- **Isolation suite** now covers all 50 new routes automatically (it walks the OpenAPI document): a second workspace's fields, pipelines, templates, rules, things, webhooks, events and jobs are invisible and untouchable, by session and by API key.
- **Database level:** the worker role is not superuser, cannot bypass row-level security, and gets "permission denied" on every table except the queue, and even there on the columns it does not need. No privileges are granted by default on future tables. The migration reverts to exactly the M1 schema.

## Skipped, and why

- **SMS, WhatsApp, in-app notifications, tasks, documents and plugin actions as rule actions.** Their templates can be written and previewed now; sending needs the providers (M6), tasks and notifications need the shell (M3), documents arrive in M5, plugins later. Each will be one more action type.
- **Conditions on parent records** (`organisation.country`). There are no parents yet; the condition paths are ready for them.
- **Translated month names.** Dates format in the workspace's timezone with English month names until the first non-English client.
- **A rules form builder and the other screens.** The API is complete; the UI arrives with the frontend shell (M3).
- **Per-workspace sending domains.** One platform sender for now; own-domain email and DNS automation are planned for M6.

## Open

- Hosting (Fly.io vs Hetzner + Coolify, EU only vs EU + US) still decides staging, production and the real SMTP provider.
- Desktop, mobile and offline: waiting on Solomon's choice. The server side is ready for any of the options: idempotent writes and row versions (M1) and now the changes feed with live updates.
