# Milestone 7: Talent portal (free artist viewer)

**Demonstrable:** a talent confirms a pending date from a phone and the agent is notified.

M7 comes in several PRs. This page grows with each.

1. **The portal itself**: inviting a talent from their record, and their own dates, answers, availability, calendar, travel, documents and statements, with the agency's switches.
2. Locked paid features with "Unlock features", a preview of the full app, and the agency's "hide locked features" switch.
3. Messages between the talent and their agent, and profile edits.
4. Busy elsewhere: a talent with more than one agency (or their own workspace) lets each see when they are booked in the others, with their consent.
5. Plans: the setup page's price estimate says each talent invited to the portal costs nothing extra and never counts towards the team (the portal feature itself keeps its price), and links to the plans on OnHold's website (the price list's upgrade link). Nothing is charged in the app.

## The portal

Runs end to end in `web/apps/app/e2e/portal.spec.ts` on desktop and phone: an agent invites Nova Reyes from her record; she signs up from the invitation on her phone, sees her pending date (without its fee), opens it, confirms, and the agent finds the task "Nova Reyes confirmed Warehouse Project". `crates/core-api/tests/portal.rs` covers the API.

### Built

- **Inviting** (a talent's page → Their portal). Anyone who can edit talent can invite one person per talent record by email; the invitation email and page say what the portal is. Accepting opens that record for them. A waiting invitation can be sent again or cancelled; removing access ends it, and a talent member with no record left leaves the workspace. Talent access cannot be given from the members page, and leaving the workspace ends it too.
- **What talent see** (Settings → Talent portal, owners and admins): confirm or decline dates, pencilled dates, fees, travel, documents, their own availability, statements. Fees and statements start off.
- **The portal** (`/portal`), phone first, in the workspace's colours and logo, shown instead of the staff app to talent members (the staff app's addresses send them to it):
  - **Dates**: coming up and the last two years, with status, venue, the venue's clock, and the fee when shared; a count of dates waiting for an answer.
  - **A date**: times, running order, who it is for, travel (flights, trains, rides and hotels, never their cost), and documents of talent-facing kinds (contract, confirmation, advance sheet, rider, tech spec, stage plot).
  - **Confirm or decline**, with a note. The agent gets a task (the enquiry's owner, or whoever made the booking), rules get `booking.talent_answered`, and the booking's status stays the agency's. Moving the date's time clears the answer. Holds, pending and confirmed dates can be answered until they end.
  - **Availability**: their own unavailable and tentative times (removable by them; the agency's entries stay the agency's), busy times from their connected calendars, and "Connect a calendar", the same link an agency sends (Google, Outlook or an iCal link, busy only). Listed a month back to three years ahead.
  - **Documents**: on their dates from three months back, and on their own record (rider, press kit, insurance, passport, visa).
  - **Money**: final settlements, with fee, commission, expenses, net, what was paid out and the statement PDF.
- **API**: `/api/v1/portal…` for talent (`portal:use`), `/api/v1/workspace/portal` and `/api/v1/talent/{id}/portal` for staff.
- **What talent never see**: the talent role holds only `portal:use` (no staff endpoint, not even the workspace's rules or templates). On a booking with several acts, the shared fee is never shown and travel shows only items naming one of their acts. While fees are hidden, so are contracts and confirmations, which carry them. A record's portal opens only through the invitation (not by editing the record), not for a colleague whose role has no portal, and deleting the record ends it.

- **Messages** (Messages tab, switch "Messages"): one conversation per talent record, which sits in the agency's inbox as a Portal conversation beside email and texts (unread, assign, snooze, link as usual). The talent writes from their page; staff answer from the inbox or with "Write to them" on the talent's record, and the talent gets an email (`system.portal_message`) with a link back. The tab shows how many are unread.
- **Profile** (account menu, switch "Edit their profile"): the talent keeps their bio and where they are based on their record; the change is recorded as theirs (`talent.updated`, `by_talent`).
- **Busy elsewhere** (Availability, "Share when I'm booked or pencilled in with my other agencies", off until the talent switches it on): someone who is talent in more than one workspace lets each see when they are held, pending or confirmed in the others. Clash checks list it as "busy elsewhere", a warning that never blocks, with the times only: never the title, client, fee or which workspace. Closed workspaces never count. The switch is the person's own and covers all their workspaces (`user.busy_sharing_changed`).

### Not yet

- No email or push to the agent beyond the task; a rule on `booking.talent_answered` can send one.
- The portal keeps nothing on the device for offline use; dates need a connection.
- Starting their own workspace from the portal: upgrades happen on OnHold's website (the price list's upgrade link).
