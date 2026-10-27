# Milestone 1: Tenancy and auth

**Demonstrable:** the isolation test suite passes; two workspaces cannot see each other.

## Built

- **Schema** (one reversible migration): users, workspaces, memberships, per-workspace role permissions, invitations, API keys, sessions, magic links, OIDC identities, recovery codes, audit log, outbox and idempotency keys. Every table has row-level security; the API runs as `onhold_app`, which cannot bypass it, delete anything, or touch `is_superadmin`.
- **Workspaces:** slug, name, plan, status (trial, active, suspended, closed), perspective, country, currency, time zone, locale, enabled modules, branding, billing contact, plugin slug, and an optional "staff must use 2FA" switch. Settings changes are versioned (`ETag` / `If-Match`). Closing keeps the data and locks everyone out.
- **Roles and permissions:** owner, admin, agent, finance, viewer, talent, each with a default permission set stored per workspace. Services check one permission per operation. Owners are protected: only owners grant or remove the owner role, and the last owner cannot leave.
- **Sign-in:** email and password (Argon2id, lockout after 10 failures), magic links, Google and Microsoft (OIDC with PKCE), TOTP two-factor with recovery codes, session list and revoke, password change that signs out other sessions.
- **Invitations:** by email and role, valid 7 days, accepted by creating an account or by an existing signed-in user with the same address. One login can be in many workspaces.
- **API keys:** scoped to a subset of their creator's permissions, optional expiry, last-used time, revocable.
- **Audit log:** every change with before/after, actor (user, API key or system), impersonator, IP, user agent and request id. Paged API and CSV export (safe to open in Excel).
- **Operators:** superadmins (TOTP required) list and create workspaces and impersonate one for up to an hour with a stated reason, all of it in the customer's audit log. CLI: `onhold admin workspace create`, `onhold admin superadmin grant|revoke`, `onhold admin generate-key`.
- **API:** 38 operations under `/api/v1`, all in the OpenAPI 3.1 document at `/api/openapi.json` (also `onhold openapi`). Writes accept `Idempotency-Key` for safe retries.
- **TypeScript client:** `web/packages/api-client`, generated from the committed spec; CI fails if either is stale.
- **CI:** the new tests, plus a job that regenerates the TypeScript types and type-checks them. Windows and macOS builds stay green.

## Verified

- 115 tests pass on a real Postgres 16 (`cargo test --workspace`); clippy is clean with `-D warnings`.
- **Isolation suite** (`crates/core-api/tests/isolation.rs`), driven by the OpenAPI document so every new workspace route is covered automatically:
  - a member of workspace A, pointing every workspace route at B, gets `403 not_a_member`; A's API key with every permission gets `403 workspace_mismatch`;
  - B's member, invitation and API key ids used from inside A are `404` on every route that takes an id;
  - no list or read in A contains B's id, slug, emails or key ids;
  - afterwards B is unchanged: same name, version, members, open invitations and keys;
  - a route without `X-Workspace-Id` fails the build unless it is on an explained allowlist.
- **Database level** (`rls.rs`): every table has RLS; the app role is not superuser, not `BYPASSRLS`, owns nothing and cannot delete; with no context it sees zero rows in every table; with A in context it sees only A and cannot write into B; it cannot make anyone superadmin.
- **Sign-in** against a mock identity provider signing real RS256 tokens: verified Google emails link, unverified ones and every Microsoft email do not, invitations work through OIDC, linking needs a signed-in user, wrong nonce and forged state are refused, redirects never leave the site.
- Manual run of the real binary: `migrate`, `admin workspace create`, accepting the printed invitation over HTTP, `/auth/me`, `admin superadmin grant`.

## Skipped, and why

- **Sending email.** Invitation and magic-link emails are queued in the outbox with their links; the worker delivers them in M2. Until then the CLI and admin API return the owner's invitation link to pass on by hand.
- **Breached-password check and per-IP rate limiting.** Need an external list and an edge proxy; M8 hardening. Account lockout and per-session 2FA attempt limits are in.
- **Admin screens.** The admin API exists; the UI arrives with the frontend shell (M3) and the admin area (M8).
- **Re-encrypting secrets after a key rotation.** Old keys keep decrypting; a re-encrypt command comes with M8's operator tooling.
- **Talent memberships.** The role exists; assigning it waits for talent records (M7).

## Open

- Hosting (Fly.io vs Hetzner + Coolify, EU only vs EU + US) still decides staging and production deploys.
- Desktop, mobile and offline: the choice is with Solomon. M1 already includes the server-side pieces any of the options needs (idempotent writes, row versions); M2 adds a changes-since feed.
