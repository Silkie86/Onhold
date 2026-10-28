# Milestone 6: Integrations

**Demonstrable:** an email enquiry arrives in the inbox linked to its contact; a confirmed booking appears in Google Calendar; a paid invoice shows paid in Xero.

M6 comes in several PRs. This page grows with each.

1. **Email inbox**: Gmail, Outlook and Microsoft 365, and any other mailbox by IMAP and SMTP.
2. **Own-domain sending**: verified sender domains (SPF, DKIM), with automatic DNS setup through Domain Connect and a guided manual fallback.
3. **Talent's own calendars**: busy times read from the calendars talent connect through a link (Google, Microsoft or a private iCal link), counted by clash checks. Pushing bookings into Google and Microsoft calendars follows in its own PR.
4. Xero and QuickBooks, including importing past invoices.
5. SMS, WhatsApp and follow-up rules.

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
- One member connects Google (asking only for `calendar.freebusy`), another Outlook (`Calendars.ReadBasic`, asking for times and "show as" only), a third pastes an iCal link. A page that is not a calendar is refused while they are there.
- The worker reads a year ahead. Busy, out-of-office and tentative times arrive; free time does not. They show in the talent's availability, named by whose calendar they come from, and a booking over them is a blocking clash. They cannot be edited or removed in OnHold.
- When a calendar changes, the next read follows it, and stretches that did not change are not rewritten.
- A withdrawn Google grant turns the calendar "needs connecting again"; its last busy times stay. Connecting again through the link revives the same calendar.
- Viewers can see the calendars but not send links; another workspace sees nothing. Disconnecting forgets the busy times and asks Google to forget the grant. A revoked or expired link stops working, even part way through connecting. Removing the talent disconnects their calendars.

`e2e/bookings.spec.ts` gets a link from a talent's page and opens it as the talent.

### Built

- **Talent page → Their own calendars.** "Get a link to send" makes a link that works for two weeks, for as many people as need it (each member of an act can connect their own; up to 12 calendars per talent). Each calendar shows whose it is, when it was last read and whether it needs connecting again, with "read now" and "disconnect". Links can be stopped.
- **The talent's page** (`/connect-calendar/<link>`), in the agency's branding: their name if they are one of several, then Google, Microsoft, or a private iCal link (Apple, Proton, Fastmail; `webcal://` is accepted).
- **Busy only.** Google is asked through its free/busy query; Microsoft for each event's start, end and "show as"; an iCal file is read for times, with repeating events counted out (`rrule`), moved and cancelled occurrences, whole days in the calendar's zone, free and cancelled events left out. Titles, places and people are never stored. Tentative time warns; busy and out-of-office time blocks, like dates marked unavailable.
- **Reading.** Every 15 minutes (`CALENDAR_POLL_SECONDS`), from yesterday to a year ahead, Google in 60-day windows. Failures are retried quietly at first, then shown, waiting 5 minutes doubling to 6 hours; a withdrawn grant or a link that stopped working waits for the talent to connect again.
- **Small.** Busy times are a copy of the provider's answer, replaced as it changes, so a calendar holds only a year's stretches (at most 3,000) and never grows. Nothing about it is kept on devices.
- **Configuration.** The same Google and Microsoft apps as the inbox, with the redirect URI `<PUBLIC_BASE_URL>/connect-calendar` added, and Google's Calendar API switched on.

### Not yet

- Bookings and holds written into members' and talent's Google and Microsoft calendars (the next M6 PR).
- Google's `calendar.freebusy` is a sensitive scope: until Google verifies the app (no security assessment needed), people outside the operator's own domain see an "unverified app" warning.
- Push notifications from Google and Microsoft instead of reading on a timer.

## Email inbox: not yet

- Push notifications from Gmail and Graph instead of polling.
- IMAP reads INBOX only; replies sent from another mail program show in OnHold only when the other side answers.
- Gmail's read and send permissions are "restricted" scopes: before members outside the operator's own Google Workspace can connect Gmail, Google has to verify the app, which includes a yearly security assessment. Until then Gmail works for test users and the operator's own domain; Outlook and IMAP have no such step.
