# Milestone 6: Integrations

**Demonstrable:** an email enquiry arrives in the inbox linked to its contact; a confirmed booking appears in Google Calendar; a paid invoice shows paid in Xero.

M6 comes in several PRs. This page grows with each.

1. **Email inbox**: Gmail, Outlook and Microsoft 365, and any other mailbox by IMAP and SMTP.
2. **Own-domain sending**: verified sender domains (SPF, DKIM), with automatic DNS setup through Domain Connect and a guided manual fallback.
3. **Talent's own calendars**: busy times read from the calendars talent connect through a link (Google, Microsoft or a private iCal link), counted by clash checks.
3b. **Bookings in members' own calendars**: a member connects Google Calendar or Outlook and OnHold keeps the bookings in a calendar of its own there.
4. **The workspace's books**: Xero and QuickBooks Online, sent what OnHold issues and read back for what was paid, with past invoices imported.
5. **Texts and follow-ups**: the workspace's own SMS (Twilio) and WhatsApp (Meta's Cloud API) numbers in the inbox, rules that text, and ready-made follow-ups.

Alongside: **email signatures** for members and the workspace.

## Email inbox

The first part of the demonstrable outcome runs end to end in `crates/core-worker/tests/inbox.rs`, against stand-ins for Google, Gmail, Microsoft login, Microsoft Graph and an IMAP/SMTP server that answer with recorded responses (`crates/core-worker/tests/fixtures/mail`):

- A member connects their Gmail. The worker reads the last 30 days; spam and newsletters stay out, and Bea's enquiry arrives linked to Bea's contact, with its HTML cleaned and its attachment listed (opened straight from Gmail).
- The reply goes out from the member's Gmail in the same Gmail thread. Bea's answer and her out-of-office join the conversation; the copy Gmail filed under Sent is recognised, not added twice.
- Another member does not see the private mailbox until it is shared; another workspace sees nothing.
- The conversation becomes an enquiry for Bea, is snoozed and comes back on its own.
- When the member withdraws OnHold's access in Google, the mailbox asks to be connected again, and replies meanwhile go out through OnHold's own sender. Connecting again carries on where it stopped; disconnecting revokes the grant at Google and keeps the conversations.
- The same with Outlook (Graph delta queries and `sendMail`) and with IMAP and SMTP, and a conversation started through OnHold's own sender that an answer later joins from a connected mailbox.

`e2e/inbox.spec.ts` covers the screens in a browser, on desktop and phone.

### Built

- **Connecting mailboxes** (Settings → Email accounts).
  - Gmail and Google Workspace, and Outlook and Microsoft 365, by signing in with the provider. These reuse the sign-in apps (`OIDC_GOOGLE_*`, `OIDC_MICROSOFT_*`), so nothing new is configured beyond the redirect URI and mail permissions. A Microsoft 365 shared mailbox (such as bookings@) is connected through a member's own account.
  - Any other provider by IMAP and SMTP, with servers filled in for common providers. Both servers are tried before the mailbox is saved.
  - Grants and passwords are sealed with the workspace keyring. Access tokens are refreshed as needed; a grant that stops working marks the mailbox "needs connecting again" rather than retrying for ever, and other failures back off from 2 minutes to 6 hours.
- **Reading.** The worker reads each mailbox every two minutes (`MAIL_POLL_SECONDS`), and on demand ("Check now"). Messages are parsed once, threaded, linked to the contact (and their organisation) whose address sent them. Gmail and Outlook list what changed and OnHold downloads only messages it does not hold yet; IMAP messages are read one at a time, so a slow server or a large message never stops the ones before it being read. When a thread starts part way through a Gmail conversation (a first read goes newest first), the rest of the conversation is read too. Gmail mail moved out of Spam or Promotions into the inbox is read in. A message OnHold cannot take is logged and passed over rather than holding the mailbox back. A read that stops part way (a deploy, an error) is picked up again within ten minutes. Mail is only ever filed into a thread of the mailbox it came from (or one written through OnHold's own sender), never into another member's private thread. Connecting a mailbox again after disconnecting it carries on with the same connection, and closing a workspace disconnects its mailboxes. Disconnecting a Google mailbox revokes OnHold's access only when no other workspace still uses that account.
- **Keeping it small.** OnHold keeps only what the business needs: mail from people, what belongs to a conversation already in the inbox, and mail to or from a contact. Mailing lists, stray auto-replies and the mailbox's other sent mail stay in the mailbox. Attachments are not copied: OnHold keeps their name and size and reads them from the mailbox when someone opens one (so they cannot be opened once the mailbox is disconnected). Pictures placed inside a message are left out. A message's text is kept up to 100,000 characters and its cleaned HTML up to 256 KB; longer HTML is dropped and the text shown instead. Nothing from the inbox is kept on devices unless the person asks (below).
- **The inbox** (`/inbox`). Open, snoozed and resolved conversations; filters for mailbox, unread and not linked; search over subjects and people. A conversation can be resolved, snoozed, assigned, marked unread, linked to a contact, enquiry or booking, or turned into an enquiry (adding the sender as a contact). Contacts, organisations, enquiries and bookings show their conversations.
- **Writing.** Replies and new messages are written as plain text (or from a template) and sent by the worker from the conversation's mailbox, threaded with `In-Reply-To` and `References`. Without a mailbox they go through OnHold's own sender in the workspace's name, to one person and without copies, answering to the member (or to the conversation's mailbox while it needs connecting again). New messages can choose OnHold's sender explicitly. A reply goes to the address the sender asked answers to go to (`Reply-To`), such as a client writing through a booking site. A message handed to the provider is never sent twice: if sending was interrupted it is marked failed with a note to check the Sent folder, and the mailbox's own copy, when read back, marks it sent. Outlook takes attachments up to 2 MB in all (Graph's request limit), other mailboxes 20 MB. If a Gmail conversation was deleted, a reply starts a new one. Workspace wording for quote, invoice, signature and invitation emails is refused when saved if it uses the link anywhere it would be removed.
- **Email on this device** (Settings → Email accounts, and the Sync page; apps and the installed website only). Off unless the person turns it on, separately on each device. It keeps the last two weeks of conversations they can see as text only: no HTML, no attachments (their names stay, and they open once online), the latest 20 messages of each, quoted earlier messages left out, each message up to 20,000 characters, and never more than 5 MB on the device in all (every workspace together), newest first; what would not fit is not downloaded. The setting shows how much room it takes. The list is checked every half hour and on first switching on; in between the changes feed names the conversations with news and only those are read again. Conversations that fall out of the two weeks, or out of what the person may see, are deleted from the device, and so is all the email of a workspace the person leaves; turning it off deletes everything kept, and so does the session ending (the choice itself stays), including signing out in an ordinary browser tab on the same computer. Offline, the inbox and conversations open from the device, and replies wait with the other offline changes, addressed to whoever the writer saw they would go to (so someone writing in meanwhile does not become the recipient), sent in order with an `Idempotency-Key` once back online (so a reply whose answer was lost is not sent twice). Other changes to a conversation need a connection.
- **API.** `/api/v1/inbox/mailboxes` and `/api/v1/inbox/threads`, with new permissions `inbox:read` (every role), `inbox:write` (all but viewers) and `integration:manage` (owners and admins).
- **Quote emails** now get their link put in after the template is rendered, as invitations have since M2, so a workspace's own quote template cannot reshape or leak it.

## Sending from your own domain

Runs end to end in `crates/core-worker/tests/domains.rs`, against stand-ins for Resend's domain API, a DNS-over-HTTPS resolver and a DNS provider with Domain Connect:

- An owner adds `acme.test`. Resend's DKIM and SPF records show with copy buttons, as most DNS providers want the names.
- The first check finds the domain's DNS provider through Domain Connect, so "Set up automatically" appears, and finds no DMARC policy, so a harmless one is suggested. The setup link is signed and names the template's values.
- Until the records are found, email still goes out from the platform's address. Once they are, the first verified domain is used straight away, and invitations go out from `bookings@acme.test`. Turning it off goes back to the platform's address.
- Another workspace cannot add the same domain; agents cannot see the page; removing the domain removes it at Resend too and frees it.
- A domain whose records are not found for 14 days is removed; one the provider no longer knows is removed at once if it was never verified.

`e2e/inbox.spec.ts` checks the page on a server without Resend set up.

### Built

- **Settings → Email accounts → Your own domain** (owners and admins). Up to 5 domains a workspace, one sent from at a time. Mail services' own domains (gmail.com and the like) and the platform's own domain are refused. The sending address's part before the @ is chosen per domain (`hello` unless changed).
- **Records.** Resend gives a DKIM key and SPF with a bounce MX on a `send` subdomain, so the domain's own SPF record is left alone. A DMARC record is suggested only when neither the domain nor a domain it is under has one.
- **Automatic setup.** When `DOMAIN_CONNECT_*` is set and the domain's DNS provider has taken in OnHold's template (`docs/domain-connect/`), "Set up automatically" sends the owner to their provider with a signed link; the provider sends them back and OnHold checks straight away. Discovery follows the domain's `_domainconnect` record to public https addresses only.
- **Checks.** The worker asks Resend about a pending domain often at first (every minute, slowing to every six hours) and about a verified one daily, and asks Resend to start or retry its own verification when it has stopped; "Check now" asks it to look straight away. Records that go missing after verification turn the domain "Records not found", and email falls back to the platform's address; Resend's "temporary failure" leaves a verified domain verified. A domain whose records have not been found for 14 days (since they were last found, or since it was added) is released here and at Resend, so nobody can hold a domain they do not own; so is a never-verified domain Resend no longer knows. A failed check does not fail the job: the next one is booked first. Closing a workspace releases its domains. Released domains stay as rows marked removed, since the app never deletes, and the domain is free to be added again.
- **What uses it.** Invitations, quote, invoice and signature emails, rule emails, and inbox messages sent without a connected mailbox. The display name stays the workspace's (or "member at workspace"); replies still go where they did. Mail from a connected mailbox always comes from that mailbox.
- **Small.** Each domain is one row with its handful of records; nothing is kept on devices.
- **Configuration.** `RESEND_API_KEY` (and `RESEND_REGION`, default `eu-west-1`), with `SMTP_URL` pointing at Resend's SMTP server for the same account, since only the account that verified a domain may send from it.

### Not yet

- Resend's webhooks instead of checking on a timer.
- OnHold's Domain Connect template is written but not yet submitted; until providers take it in, every domain is set up by hand.

## Talent's own calendars

Runs end to end in `crates/core-worker/tests/talent_calendars.rs`, against stand-ins for Google (OAuth and free/busy), Microsoft (OAuth and the calendar view) and a published iCal file:

- On an act's page the agency gets a link and sends it on. Opening it needs no OnHold account; it names the agency and the act, and says that only busy times are seen.
- One member connects Google (asking only for `calendar.freebusy`, never for what the app was given before), another Outlook (`Calendars.ReadBasic`, asking for times and "show as" only), a third pastes an iCal link. A page that is not a calendar is refused while they are there; pasting the same iCal link again renews that calendar. A grant that carries more than busy times (mailbox access given to the same app, say) is refused.
- The worker reads a year ahead. Busy, out-of-office and tentative times arrive; free time does not. They show in the talent's availability, named by whose calendar they come from, and a booking over them is a blocking clash, unless they lie within that booking (most likely the booking itself, written into the calendar), when they only warn. They cannot be edited or removed in OnHold.
- When a calendar changes, the next read follows it, and stretches that did not change are not rewritten.
- A withdrawn Google grant turns the calendar "needs connecting again", and its busy times stop counting. Connecting again through the link revives the same calendar.
- Viewers can see the calendars but not send links; another workspace sees nothing. Disconnecting forgets the grant and the busy times, and asks Google to forget the grant unless another connection (a mailbox, or the same calendar for another talent) still uses the account. A revoked or expired link stops working, even part way through connecting. Removing the talent disconnects their calendars, and the worker then revokes their grants.

`e2e/bookings.spec.ts` gets a link from a talent's page and opens it as the talent.

### Built

- **Talent page → Their own calendars.** "Get a link to send" makes a link that works for two weeks, for as many people as need it (each member of an act can connect their own; up to 12 calendars per talent). Each calendar shows whose it is, when it was last read and whether it needs connecting again, with "read now" and "disconnect". Links can be stopped.
- **The talent's page** (`/connect-calendar/<link>`), in the agency's branding: their name if they are one of several, then Google, Microsoft, or a private iCal link (Apple, Proton, Fastmail; `webcal://` is accepted).
- **Busy only.** Google is asked through its free/busy query; Microsoft for each event's start, end and "show as"; an iCal file is read for times, with repeating events counted out (`rrule`), moved and cancelled occurrences, whole days in the calendar's zone (else the workspace's), free and cancelled events left out. Titles, places and people are never stored. Tentative time warns; busy and out-of-office time blocks, like dates marked unavailable.
- **Reading.** Every 15 minutes (`CALENDAR_POLL_SECONDS`), from the start of yesterday to a year ahead in whole days, Google in 60-day windows. Failures are retried quietly at first, then shown, waiting 5 minutes doubling to 6 hours; a withdrawn grant or a link that stopped working waits for the talent to connect again, its busy times dropped meanwhile.
- **Safe to read.** iCal files are at most 8 MB and 20,000 events; repeating events are counted out from just before the window, rules repeating by the second or minute count once, and a file needing more than 2 million steps or 50,000 stretches is refused. Files are read two at a time across the server, off the request threads, for at most 20 seconds. A link stops working after 200 uses.
- **Small.** Busy times are a copy of the provider's answer, replaced as it changes, so a calendar holds only a year's stretches (at most 3,000) and never grows. A disconnected calendar keeps no grant. Busy times are not kept on devices: the calendar shows them when online, and offline clash checks go without them.
- **Configuration.** The same Google and Microsoft apps as the inbox, with the redirect URI `<PUBLIC_BASE_URL>/connect-calendar` added, and Google's Calendar API switched on.

### Not yet

- Google's `calendar.freebusy` is a sensitive scope: until Google verifies the app (no security assessment needed), people outside the operator's own domain see an "unverified app" warning.
- Push notifications from Google and Microsoft instead of reading on a timer.

## Bookings in members' own calendars

Runs end to end in `crates/core-worker/tests/calendar_push.rs`, against stand-ins for Google (OAuth and the Calendar API) and Microsoft (OAuth and Graph):

- The owner connects Google Calendar from Settings → Calendar, asking only for `calendar.app.created` (OnHold may make calendars and change only those). OnHold makes "OnHold · <workspace>" in the account and fills it: holds and pending bookings tentative, confirmed ones busy, with the talent, venue and a link back, never the fee. Bookings from a month back to two years ahead go in; older ones stay as they were.
- A booking that changes is written straight away; one cancelled or deleted is removed. A daily comparison writes nothing when nothing changed. If the owner deletes OnHold's calendar in Google, it is made again and filled.
- An agent connects Outlook for one talent only ("OnHold · <workspace> · <talent>"). Each member sees and manages only their own connections; another workspace sees nothing.
- A grant that also carries the mailbox is refused. An answer lost after Google added an event does not lead to a second copy; an event the owner already deleted counts as removed (Google answers 410). If OnHold loses track of its Outlook calendar, the one there is taken over rather than refused as a duplicate name.
- A withdrawn grant turns the connection "needs connecting again"; connecting again revives it and catches up. A member leaving has their calendars removed from their accounts and the grants forgotten; disconnecting does the same, and asks Google to forget the grant unless another connection still uses the account.

### Built

- **Settings → Calendar → Your own calendar.** Connect Google Calendar or Outlook, for every booking or one talent's (up to 5 per member); each shows its account, whether it is working, when it was last brought up to date, with "update now", "connect again" and "disconnect".
- **Writing.** The event dispatcher queues a write of the one booking for every connected calendar when a booking changes; every connection is also compared in full each day, up to 200 calls a run with the rest following 30 seconds later. What was written is remembered as a hash, so unchanged bookings cost no calls. One write runs at a time per connection, holding a lease it renews at each step and checks before recording anything, so a write that loses its connection (disconnected, connected again) stops; a change that arrives meanwhile is picked up straight after. Failures wait 5 minutes doubling to 6 hours; Google's rate limits (429, or 403 with a rate reason) wait as told.
- **No doubles.** Each booking's event has an id derived from the connection and booking (Google) or a transaction id (Outlook), so trying again after a lost answer writes over the event rather than adding another. An event already deleted in the calendar (404, or Google's 410) counts as removed. If Outlook already has a calendar of OnHold's name (a clean-up that did not finish), it is taken over. A booking missing from a full read is looked up on its own before its event is removed.
- **Disconnecting.** The worker removes OnHold's calendar, then forgets the grant and revokes it unless another connection uses the account; while the provider is unavailable it tries again every 10 minutes for up to 3 days, keeping the grant until then.
- **Only what its owner may see.** A connection writes only while its member can read bookings; a member who leaves or loses that permission has theirs disconnected.
- **Small.** OnHold keeps one small row per written booking (the event's id and a hash), removed with the event, or once the booking is more than a month past (its event stays in the calendar as a record); nothing is kept on devices. A disconnected calendar keeps no grant once cleaned up.
- **Configuration.** The same Google and Microsoft apps as the inbox, with the redirect URI `<PUBLIC_BASE_URL>/settings/calendar` added. Microsoft has no narrower grant than `Calendars.ReadWrite`; OnHold writes only to the calendar it made.
- Talent who want their bookings in their own calendar subscribe to the talent's iCal link (Settings → Calendar → Calendar links).

### Not yet

- Writing into a talent's own calendar through the connect link (the link reads busy times only).
- `calendar.app.created` is a sensitive scope: until Google verifies the app, people outside the operator's own domain see an "unverified app" warning.

## The workspace's books: Xero and QuickBooks

Runs end to end in `crates/core-worker/tests/accounting.rs`, against stand-ins for Xero (OAuth, connections and the accounting API) and QuickBooks Online (OAuth and Intuit's API):

- The owner connects Xero from Settings → Accounting; the organisation they picked is the one connected. Nothing is sent until they choose the revenue account sales go to, the bank account payments go into, and which tax rate each of OnHold's tax codes is. Another workspace sees nothing of it.
- An issued invoice is sent to the contact the books already have of that name (or a new one), with its number, dates, lines and tax, and the totals match. A payment recorded in OnHold is sent against it and taken back when voided; one whose answer was lost is asked for again with the same key and amount, so it is there once and can still be taken back. Paid in Xero, it is read back and shows paid in OnHold, and that payment is not sent back.
- A credit note is sent and set against its invoice; voided, it is taken off again and voided. An invoice voided after it was sent is voided there (one voided there already is fine); one voided before it was sent is never sent.
- A contact the books refuse is kept with the reason (Settings → Accounting lists it) and the rest go on; it is not tried again on every change, and once put right, "send now" gets it there. A sync cut off part way is followed by another once its lease runs out. An answer lost after Xero added an invoice is tried again with the same idempotency key, so it is there once.
- Importing brings in the invoices from before the start date (not voided ones) as past invoices, with what they still owe, matched to the organisation of the same name; they are never sent back, and importing again adds nothing.
- A withdrawn grant waits to be connected again; connecting the same books again picks up where it was, sending nothing twice. Other books cannot be connected meanwhile. Disconnecting forgets the grant; for Xero it ends only the connection to that organisation, and not while another workspace still uses it.
- QuickBooks: the company comes from Intuit's redirect; a US company's tax is worked out by QuickBooks, so lines say only taxable or not; a customer made inactive there is found and made active again; payments go into the chosen account with their reference; an invoice deleted there before it is voided here needs nothing; settings warn when QuickBooks applies credits itself; voids, paid in QuickBooks, importing and disconnecting as above (Intuit revokes with JSON).

### Built

- **Settings → Accounting** (owners, admins and finance). Connect Xero or QuickBooks, choose where things go and from which day issued invoices are sent (the day of connecting, by default), see what was refused and why, "send now", import past invoices, connect again, disconnect. One set of books per workspace.
- **Sending.** The event dispatcher asks for a sync when an invoice is issued or voided or a payment changes, and every connection is compared every 3 hours and on "send now". A sync sends, in order: invoices and credit notes, credit notes set against their invoices, payments; then reads back what was paid (every 3 hours, and on "send now"); then a page of past invoices when importing. At most 50 calls a run, the rest following a minute later (a read-back cut short goes on from where it stopped). New work comes before refused work. Payments are capped at what the books still say is owed, and credits at what is left of the credit there, so nothing is paid twice. Xero needs the account payments go into; without one, payments are not sent (and not counted as refused).
- **Read back.** An invoice paid (or credited) in the books by more than OnHold has recorded gets a payment for the difference, "Paid in Xero" (or QuickBooks), capped at what OnHold still shows as owing; imported invoices are read back too. An invoice with a payment from OnHold still on its way there, or being taken back, waits for that first. Nothing read back is sent again.
- **No doubles.** Every create carries a key made from the connection, the item and how many times it was refused (Xero's `Idempotency-Key`, Intuit's `requestid`), so a lost answer is not added twice, and a refused item put right is a new request (its customer too). A payment or credit is noted with its amount before it is asked for, and until its answer is in it is asked for again just so. One sync runs at a time per connection, holding a lease it checks before recording anything; a change that arrives while a sync runs gets another straight after, and while it holds the lease the next sync is due when the lease runs out, so a sync cut off part way is always followed by another. Refresh tokens are used once, so refreshing happens under the connection's row lock.
- **Failures.** A refusal (validation) is kept per item and tried again once after each full sync (every 3 hours), when it changes, or on "send now". The provider being down or limiting waits 5 minutes doubling to 6 hours (rate limits as told), shown from the third failure in a row. A withdrawn grant, books disconnected on the provider's side (Xero's and Intuit's 403s), or a grant this server can no longer read waits for connecting again. Already voided or deleted there counts as done (Intuit's code 610, Xero's status read back).
- **What is sent.** Workspace-issued invoices and credit notes on or after the start date (drafts and talent-issued self-billing never), the payer as a contact (organisation or talent by link, else by name), lines net of tax with their tax rate, and payments with their date and reference. Xero takes each line against the chosen revenue account; QuickBooks against the chosen service item.
- **Disconnecting.** The worker ends OnHold's access, trying again every 10 minutes for up to 3 days while the provider is unavailable, then forgets the grant. Intuit's grant (for that company only) is revoked; for Xero only the connection to that organisation is ended, since revoking a Xero grant ends the person's connections to every organisation. Books another workspace still uses are left connected (a grant made for other books by mistake, too). Connecting the same books again calls off a revoke still waiting. What is in the books stays there, and the record of what was sent is kept, so reconnecting sends nothing twice. Closing a workspace disconnects its books.
- **Small.** One row per connection, and one small row per item sent (its id in the books and what was sent last); no new index on payments (finding taken-back payments starts from what was sent); past invoices are imported as totals with their lines in the notes (up to ten). Nothing is kept on devices.
- **Configuration.** `XERO_CLIENT_ID`/`XERO_CLIENT_SECRET` and `QUICKBOOKS_CLIENT_ID`/`QUICKBOOKS_CLIENT_SECRET` (`QUICKBOOKS_SANDBOX=true` for Intuit's sandbox), each app with the redirect URI `<PUBLIC_BASE_URL>/settings/accounting`.

### Not yet

- Refunds are not sent.
- Changes made to a sent invoice in the books are not read back, only what was paid.
- QuickBooks outside the US needs each tax code mapped to one of the company's tax codes; Intuit gives production keys only after its app assessment.

## Texts: SMS and WhatsApp

Runs end to end in `crates/core-worker/tests/messaging.rs`, against stand-ins for Twilio and Meta's WhatsApp Cloud API:

- An owner connects the workspace's Twilio number (account SID, auth token, number) and its WhatsApp number (phone number ID, a system user's access token, the app secret) in Settings → Text messages; each is checked with the provider first. The page shows the address to give each provider for what comes in (and Meta's verify token).
- A text sent in, correctly signed, becomes a conversation in the inbox with that number, linked to the contact who has it (numbers are matched in international form, with the workspace's country for local ones). Unsigned or wrongly signed requests are refused. Replies are written in the inbox and sent by the worker; a failed delivery shows "Not sent" with the provider's reason.
- STOP (and the usual words like it) stops anything more going to that number until START. Twilio's own "unsubscribed" refusal is recorded the same way.
- WhatsApp: free text only within 24 hours of their last message; outside it, only a template Meta approved (the template's subject names it, with its language; each body line fills one value).
- Rules can text (`send_sms`, `send_whatsapp`) the contact of what they are about, or a contact field, only if that contact agreed to texts (`sms_opt_in`, `whatsapp_opt_in`); the rule's log says who was left out and why.

### Built

- **Settings → Text messages** (owners and admins connect; the inbox module): numbers, their provider addresses, disconnect, and the SMS and WhatsApp templates.
- **Inbox**: SMS and WhatsApp conversations alongside email, a channel filter, a text reply box (counting texts past 160 characters), the WhatsApp 24-hour notice with a template picker, and "New text" to a contact or a number.
- **Follow-ups** (Settings → Follow-ups): ready-made chasers switched on in one click, each ordinary rules and templates underneath, editable like any other. An enquiry still new after 2 days makes a task; a quote not opened 3 days after sending is chased by email; an unpaid invoice gets emails 1, 7 and 14 days after it was due and a task to call at 14; a confirmed booking texts its contact the day before. Switching one off deletes its rules and keeps its (perhaps reworded) templates; switching on again adds only what is missing. Rules can now count from an enquiry's dates (`created_at`, `stage_entered_at`, `starts_on`; open enquiries only) and a quote's (`sent_at`, `valid_until`; sent and undecided only).
- **API:** `GET /inbox/numbers`, `POST /inbox/numbers/sms`, `POST /inbox/numbers/whatsapp`, `DELETE /inbox/numbers/{id}`, `POST /inbox/texts`, `GET /follow-ups`, `PUT /follow-ups/{key}`; the providers post to `/hooks/messaging/{token}`.
- **Small.** One row per number and one per opted-out number; texts are inbox messages. Pictures and files sent in are noted, not kept. Nothing new on devices beyond the inbox's own copy.
- **Configuration.** Nothing on the server: each workspace brings its own Twilio and Meta accounts. `MESSAGING_HOOK_BASE` overrides the public address providers post to (it must end with `/hooks/messaging/`).

### Not yet

- Sending a WhatsApp template from Meta's list: templates are entered by name in OnHold.
- MMS and WhatsApp media out; in, they are noted only.
- Meta's business verification is needed before a WhatsApp number can message more than a small daily number of people; that is done by each client in Meta Business Manager.

## Email signatures

Covered by `crates/core-api/tests/tenancy.rs` (who may set which) and the end-to-end mail tests in `crates/core-worker/tests` (`bookings.rs`, `crm.rs`, `inbox.rs`):

- Each member sets their own signature in Settings → Email → Signature; owners and admins also set the workspace's. Plain text, up to 20 lines and 2,000 characters; mail apps make addresses and links in it clickable themselves.
- Inbox replies and new messages are signed with the sender's signature, or the workspace's when they have none. The API can leave it off with `"signature": false`.
- Quote, invoice and signing emails carry the signature of the member who sent them (under the message, above the branded footer). Emails sent by rules carry the workspace's.
- In the plain text the signature follows the usual `-- ` line; in the HTML it sits in a muted block. It is stored with the message, so the thread shows what was sent.
- **Small.** Two short text columns (`memberships.email_signature`, `workspaces.email_signature`); nothing on devices. The audit log records that a member's changed, not its text.

## Email inbox: not yet

- Push notifications from Gmail and Graph instead of polling.
- IMAP reads INBOX only; replies sent from another mail program show in OnHold only when the other side answers.
- Gmail's read and send permissions are "restricted" scopes: before members outside the operator's own Google Workspace can connect Gmail, Google has to verify the app, which includes a yearly security assessment. Until then Gmail works for test users and the operator's own domain; Outlook and IMAP have no such step.
