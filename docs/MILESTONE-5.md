# Milestone 5: Documents and money

**Demonstrable:** the full flow through deposit paid and balance chased, in two currencies.

It runs end to end in `crates/core-worker/tests/finance.rs` against the real API, worker and Postgres:

- Two bookings for the same act, one priced in pounds and one in euros, in a workspace whose base currency is pounds. Each gets a 25% deposit invoice with 20% VAT. The euro invoice is converted at the stored ECB rate, and the rate and its source are kept on the invoice.
- Each invoice is sent. The email carries the PDF, and the payer's link opens the invoice without signing in.
- Each deposit is paid. A rule on `booking.deposit_paid` confirms the booking and generates its confirmation document.
- The balance invoices fall overdue. The worker marks them overdue, and a rule on "one day after the due date, while anything is outstanding" creates one chase task per invoice. It does not repeat on the next run. The ageing report shows both balances, and the cash report shows both deposits.

Card payments through Stripe Connect came in a second PR: the payer's invoice page offers "Pay by card", and Stripe's webhooks record the payment against the invoice. See [Card payments](#card-payments-stripe-connect) below.

## Built

- **Files.**
  - Uploads up to 25 MB by default (`MAX_UPLOAD_MB`). Each file's type is checked from its bytes, not its name.
  - Files are stored in the database by default, or in a directory or S3/R2 (`STORAGE`).
  - Files are served with a sandboxing CSP and as downloads.
  - Files not attached to anything are swept after a day.
- **Document engine.**
  - Templates are HTML with minijinja, rendered inside the workspace's branding (logo and colours) and turned into PDF.
  - PDFs come from headless Chromium when it is installed. Otherwise a built-in plain PDF writer is used, so documents work on every platform with no extra install.
  - Each document is rendered straight away, with a background job as a fallback that retries if that fails. The screen waits while it renders.
  - Each document keeps its versions, and uploaded files can become new versions.
- **Templates.** There are built-in contract, confirmation, advance sheet, invoice, credit note and statement templates. A workspace can override any of them, like the emails. Booking packs zip a booking's current documents.
- **E-signature.**
  - Signers default to the booking's contact. Each signer gets an emailed private link, and the email template can be overridden.
  - The signer reads the PDF, types their name and ticks agree, or declines.
  - Each request for signatures is its own round. Once everyone in the round has signed, a sealed version is made with a signing page (names, times, IP addresses) and `document.signed` fires. Sealing runs as a job, so a failure never loses a signature, and a PDF that cannot take the signing page (password-protected or unreadable) is refused before anyone is asked.
  - Signed documents are frozen.
- **Invoices and credit notes.**
  - An invoice is made from a booking (deposit, balance or full, from the accepted quote) or written by hand.
  - Lines carry tax codes. A draft can be edited, then is issued, sent, part paid, paid, overdue, credited or void.
  - Numbers are gapless per issuer and kind, in a format the workspace sets (for example `INV-{YYYY}-{0000}`), and are given at issue.
  - Issued invoices cannot be changed, even by a direct database write.
  - A credit note credits all or part of an invoice. A refund pays money back against a payment.
- **Payments.** Payments are recorded with a method and a date, then allocated across invoices (several at once). Allocations can be removed and payments voided.
- **Settlement.**
  - Gross comes from the talent's lines of the accepted quote. Commission and expenses are taken off to give what is owed to the talent.
  - A settlement is finalised, which makes a statement document, then paid out (part or full). It can be reopened before it is paid.
  - Three invoicing models, set per workspace:
    - `agency` (the default): the agency invoices the buyer and pays the talent.
    - `talent`: invoices are issued in the talent's name, and finalising raises a commission invoice to the talent.
    - `split`: the agency invoices the deposit and the talent the balance.
  - Numbering is kept separately for each issuer.
- **Tax codes** (standard, reduced, zero, exempt, reverse charge, or the workspace's own) and finance settings: base currency, numbering, payment terms, bank details and invoice footer.
- **FX.**
  - The worker fetches ECB reference rates hourly when stale (`FX_RATES=false` turns this off) and backfills up to 90 days after a gap.
  - Every invoice in a foreign currency stores the rate used and its source.
  - The rate comes from, in order: same currency, a manual rate given at issue, the credited invoice's rate, the booking's rate, then the ECB rate for the issue date.
- **Reports.** Revenue by month, payer and talent in the base currency, outstanding invoices, ageing buckets, cash received, and the enquiry-to-paid funnel. Each table downloads as CSV.
- **Bulk import of past PDFs.**
  - Upload hundreds at once. The worker reads each PDF with the `core-pdf` parser from PR #9 (no AI) and suggests fields: invoice number, dates, amounts, currency, VAT, payer, talent, contact and booking.
  - It suggests matching records and flags duplicates.
  - Confirming attaches the PDF to the chosen records and can keep it as a historical invoice: paid or outstanding, with no new number. Its own reference is kept, and a duplicate reference is refused.
  - All stored PDFs are full-text searchable from Documents.
- **Automations.**
  - New rule actions: `generate_document`, `create_invoice` (deposit, balance or full, optionally issued) and `set_booking_status`.
  - Rules can fire on invoice issue and due dates and on document expiry dates.
  - New triggers include `invoice.*`, `payment.*`, `document.*` and `booking.deposit_paid`.
- **Email.** Invoices go out with the PDF attached. Invoice and signature emails are system templates a workspace can override. Private links are redacted in the outbound copy kept in the database.
- **Web.**
  - New screens: Invoices, an invoice's page, Payments, Reports, Invoicing settings, Documents, a document's page, and Import past PDFs.
  - A booking's page gains Money (invoices and settlements) and Documents panels.
  - The public invoice and signing pages use the workspace's branding.
  - Lists and pages already seen keep a last-seen copy on the device and open offline. Making or changing anything needs a connection.
- **API:** 240 operations (68 new), in the OpenAPI document and the regenerated TypeScript client.

## Verified

- All Rust tests pass on a real Postgres 16 (`cargo test --workspace`). Clippy is clean with `-D warnings`. The offline query data and the generated client are current.
- **Finance API** (`crates/core-api/tests/finance.rs`):
  - Tax codes and numbering.
  - The invoice lifecycle: issue, part payment, refusals, credit notes, refunds, voids, the database freeze and CSV.
  - Historical invoices and duplicate references.
  - Settlement through payout, with a second payout refused.
- **Worker:**
  - The two-currency flow above.
  - Signers each emailed their own link, with the links redacted in the stored copy.
  - Rates not fetched when turned off.
  - Documents generated, rendered, signed, sealed, uploaded and packed (`documents.rs`).
  - Past PDFs read, matched, confirmed as historical invoices, searched and de-duplicated.
- **Isolation suite** covers every new route: another workspace's invoices, payments, documents, files and imports are invisible and untouchable, by session and by API key. The public invoice and signing routes are listed with their reason (the token names one invoice or one signer).
- **Database level:**
  - Row-level security on every new table.
  - The worker reads only the invoice and file columns its sweeps need, never amounts, lines or file contents.
  - The migration reverts to exactly the M4 schema.
- **Web:** type-checked, unit tests and production build, in CI.

## Reviewed

An ultracode review (six reviewers, each finding checked by a skeptic) confirmed 24 distinct issues before merge. All are fixed in this PR, with tests:

- **Money.**
  - A past invoice keeps what was paid before the import (`paid_before_minor`).
  - Credit notes no longer clog the overdue sweep.
  - A fully credited deposit no longer blocks a new one or comes off the balance.
  - Reopening a settlement removes its draft commission invoice, or refuses while one is issued.
  - New numbers skip ones used by imported invoices.
  - Reports only add up invoices issued in today's base currency.
- **Documents.**
  - Signing rounds stand alone.
  - Simultaneous signatures still seal.
  - A signer's link never serves a copy sealed from another round.
  - Sealing failures are retried.
  - Invoice and settlement PDFs cannot be relinked, deleted or regenerated.
- **Files and worker.**
  - Uploads over 2 MB work.
  - The unattached-file sweep deletes the row before the bytes.
  - An uploaded logo is kept.
  - A rule action that fails leaves nothing behind.
  - Exchange-rate gaps of a few days are filled.
  - PDF imports no longer get stuck on duplicate or racing files.
- **Web.**
  - Import cards pick up what was read.
  - "Keep as past invoice" follows the kind.
  - Settings keep every bank account and the address region.
  - Line prices stay with their lines and their currency.
  - The bank label shows on the payer's page.
  - App downloads keep their file names.

## Card payments (Stripe Connect)

Each workspace connects its own Stripe account (Standard accounts, through Stripe's OAuth), so money settles to the client directly and Stripe handles their payouts, disputes and tax forms. OnHold can take a cut of each payment (`STRIPE_APPLICATION_FEE_BPS`, default 0).

- **Connecting.** Settings → Card payments sends an owner or admin to Stripe and back. The round trip carries a sealed, 30-minute state naming the workspace and the person, so a code can only be finished by whoever started it, in the same workspace. An account connected to one workspace is refused by another; once disconnected there, it can be connected elsewhere. Disconnecting here, or switching to another account, first closes that account's open payment links in Stripe and then disconnects it there too. Disconnecting from Stripe's side arrives as a webhook and does the same. Each workspace keeps the accounts it has had, so a payment or refund Stripe reports later on a disconnected account is still recorded.
- **Paying.** The payer's invoice page shows "Pay … by card" while the invoice owes money, the account can take payments and the currency is one Stripe counts the same way (not HUF, ISK, TWD, UGX or the three-decimal currencies). It opens a card-only Stripe Checkout session on the client's account for what is still owed. Cards settle at once, so no slow bank payment can be pending while the link offers another. Pressing again reuses the open session while the amount is the same. A changed amount closes the old session first, and if the payer already paid it, no second link is made. Stripe is called outside any database transaction, so a slow Stripe holds no lock on the invoice. The currency check compares Stripe's decimal places with OnHold's, so a currency such as MGA (two decimals in ISO 4217, none at Stripe) is paid by bank transfer instead.
- **Webhooks** (`POST /api/v1/webhooks/stripe`). The `Stripe-Signature` is checked against `STRIPE_WEBHOOK_SECRET` with Stripe's 5-minute tolerance. Each event is kept once (Stripe retries), and a `stripe.event` job processes it:
  - `checkout.session.completed` and `checkout.session.async_payment_succeeded` record a card payment (method `stripe`, keyed by its PaymentIntent so it is recorded once), allocated to the invoice up to what it owes. Anything beyond that, if the invoice was paid another way meanwhile, stays unallocated for the client to refund.
  - A payer who paid in their own currency (Adaptive Pricing) is recorded in the invoice's currency, from Stripe's conversion details.
  - `charge.refunded` records what Stripe refunded beyond the Stripe refunds already recorded (refunds recorded by hand do not count), taking it off the invoice so it owes again. A refund processed before its payment waits and is retried.
  - `account.updated` keeps whether the account can take payments, ignoring one older than the state kept. `account.application.deauthorized` disconnects, unless it predates the account being connected again.
  - An event Stripe sends again while it still waits to be processed is queued again. Test-mode events for a live account (or the other way round) and events for accounts the workspace never had change nothing.
- **Web.** Settings → Card payments (connect, state, disconnect), and the Pay button on the payer's page, which then checks until the payment shows.
- **Verified** in `crates/core-worker/tests/stripe.rs` against a stand-in for Stripe answering with recorded responses (`tests/fixtures/stripe`): the connect round trip and its refusals, the session's account header, amount, fee and metadata, the session reused and replaced, refused signatures (none, wrong key, too old), retried and repeated events, the payment and a partial refund, account changes and both kinds of disconnect. Three more tests cover the review fixes below. The isolation suite covers the new routes.
- **Reviewed.** An ultracode review (five reviewers, each finding checked by a skeptic) confirmed 28 findings, about a dozen distinct issues once duplicates were merged, all fixed here with tests. The issues were: payments lost after a disconnect or account switch; links left open at Stripe; a disconnected account locked to its old workspace; async payment methods allowing a second payment; MGA charged at 100 times the amount; refunds matched against manual ones; refunds arriving before their payment; late account events; dead jobs never retried; Pay holding the invoice lock during Stripe calls; a failed expiry allowing a double payment; and the payer page polling forever.

## Skipped, and why

- **A browser walk-through of the money flow.** The flow is proven through the API and worker above. A Playwright run of the payer's page would need Stripe's hosted Checkout, which cannot run in CI without a live test account; it can come with the first real deployment.
- **A screen for editing templates.** Workspaces override the built-in templates as HTML through the API (`/api/v1/templates`, which already has a preview). A visual editor can come with the white-label branding work.
- **Credit control beyond the first chase.** Rules can chase on any offset, so more reminders are configuration. Automatic statements and late fees are not built.

## Open

- Whether to fetch ECB rates for workspaces outside Europe too. They are fetched once for everyone and cost nothing.
- Whether bookings confirm on deposit or on signature. This is a rule each workspace sets, not code. The demo uses deposit.
