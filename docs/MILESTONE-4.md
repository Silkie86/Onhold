# Milestone 4: Bookings and calendar

**Demonstrable:** enquiry → hold → quote → accept → confirmed booking on the calendar, with the clash test blocking a double booking.

It does, twice:

- **In a browser** (`web/apps/app/e2e/bookings.spec.ts`, run in CI at 1440 px and at 375 px against the real API and Postgres). A new workspace's owner adds an act, logs an enquiry for it, places a hold from the enquiry, writes a quote and sends it. The buyer opens the link in a fresh page without signing in and accepts it by typing their name. The hold has moved to pending; the owner confirms it and it shows on the June 2031 calendar. Trying to book the same act at an overlapping time shows "This would double-book someone", and saving it as confirmed stops at the override question; backing out leaves only the one confirmed booking.
- **Through the API** (`crates/core-api/tests/bookings.rs`), the same pieces in more depth, including two confirmations racing for one slot (exactly one wins) and the database refusing a double booking written past the API.

## Built

- **Enquiries** with talent, organisation, contact, venue, dates, budget, owner, tags, notes and custom fields. They move through a pipeline (New, Checking availability, Quoted, Won, Lost) made with the workspace's first enquiry and editable like any other pipeline. Sending a quote moves the enquiry to Quoted; acceptance moves it to Won; it never moves backwards by itself.
- **Holds and bookings.** A booking starts as a hold (or is entered as pending or confirmed), with talent, times, an all-day option, a running order, fee and deposit, and is linked to its enquiry, whose people and place it takes by default. Statuses move along fixed arrows: hold → pending → confirmed → completed, with cancelled and expired on the side and a lapsed hold able to be held again.
- **Clash detection.** Holds and pending bookings never block each other: they are pencil marks, and the check lists them as warnings. A confirmed booking or a talent's unavailable time blocks confirming another booking for the same talent; an owner or admin can confirm anyway by giving a reason, which is kept on the booking and in the audit log. A Postgres exclusion constraint backs the check so a race cannot slip past it.
- **Hold expiry.** New holds lapse after the workspace's hold period (7 days by default, 0 for never, set in Calendar settings). The worker expires them, withdrawing any quote still open; it also completes confirmed bookings once they end and expires quotes past their last day.
- **Availability.** Unavailable (blocks), tentative (warns) and available-only time per talent, edited on the talent's page and shown hatched on the calendar.
- **Quotes** hang off a booking: lines (prefilled from the booking's fee or the talent's default fee), validity (14 days by default), a message and terms, numbered Q-0001 per workspace. Sending emails the buyer a secret link and withdraws any other open quote for the booking. Revising makes Q-0001-R2. The buyer's page shows the quote in the workspace's name, records when it is opened, and accepts (with their name, time, address and a snapshot of exactly what they accepted) or declines (with an optional reason). An accepted quote cannot be changed, even by a direct database write.
- **Tasks** with due dates, assignees and a link to a booking, enquiry or CRM record; a list of everyone's or your own; done and reopened. Rules can create tasks (`create_task`, with a templated title) and can now fire on booking dates ("a week before the booking starts").
- **Calendar**: month view, a week view with a lane per talent, and an agenda; filter to one talent; holds dashed, confirmed solid, unavailable time hatched; drag a booking to another day to move it (the clash check still applies).
- **iCal links**: private subscription links for the whole workspace or one talent, showing holds, pending and confirmed bookings from 90 days ago to two years ahead, never fees. Each shows when it was last read and can be revoked.
- **Quote email** as a system template a workspace can override, like the invitation.
- **Search and timelines** cover enquiries and bookings; a booking's timeline includes its quotes, an enquiry's its bookings.
- **API:** 172 operations (40 new), in the OpenAPI document and the regenerated TypeScript client.

## Verified

- 268 Rust tests pass on a real Postgres 16 (`cargo test --workspace`); clippy is clean with `-D warnings`; the offline query data and the generated client are current.
- **Bookings API** (9 tests): enquiries in the default pipeline, validation, versions and filters; clashes with confirmed bookings and with unavailable and tentative time; overrides only for admins and only with a reason; the race between two confirmations and the database constraint; status arrows and rescheduling; quotes sent, viewed, accepted and frozen; revised, withdrawn and declined; cancelling a booking withdrawing its quotes; tasks; the calendar window and talent lanes; iCal content and revocation; the hold period setting.
- **Worker** (3 new tests): holds lapsing (and their quotes withdrawn), finished bookings completing, quotes expiring, each done once and again only when due again; the quote email with its link; rules making tasks on `booking.confirmed` and on a booking's start date.
- **Isolation suite** covers all 40 new routes automatically: another workspace's enquiries, bookings, availability, quotes, tasks and calendar links are invisible and untouchable, by session and by API key. Public routes are listed with their reason (the token names one quote or feed).
- **Database level:** row-level security on every new table; the worker reads only the columns its sweeps need (no titles, fees or quote lines). The migration reverts to exactly the M3 schema.
- **Web:** type-checked, 28 unit tests (Vitest, including the calendar date arithmetic), production build, and the browser demo above, all in CI.

## Skipped, and why

- **Offline copies of bookings in the apps and installed website.** Done straight after, in its own PR: see "Bookings offline" in `MILESTONE-APPS.md`.
- **Two-way calendar sync** (Google, Microsoft). That is M6; the iCal links give a read-only view now.
- **Drag between talent lanes, and resizing.** Dragging moves a booking to another day; changing who is booked or how long is done on the booking.
- **Deposits and invoices from accepted quotes.** M5. The accepted snapshot is what the invoice will be built from.
- **Hold approval or required reasons** (the spec's open question). Holds lapse automatically after a configurable period; nothing else gates them.

## Open

- The spec's M4 question (hold expiry) is answered by its assumption: automatic, 7 days by default, configurable per workspace. Easy to change if Solomon prefers otherwise.
