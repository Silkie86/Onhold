# Desktop and mobile apps (after M3)

**Asked for:** OnHold as a desktop app for Windows, macOS and Linux and a mobile app for iOS and Android, all working offline (Solomon, overriding the spec's "no native apps in v1"). He chose Tauri. The website must stay fully usable on phones, because the apps may be a paid-plan feature. Each client should be able to have the app under its own name and icon.

**Demonstrable:** sign the app in by approving it on the website, keep working with no connection, and settle the clash when someone else changed the same record meanwhile.

It does, in a browser on every CI run (`web/apps/app/e2e/app.spec.ts`, Playwright project `app`): the app shows a code, the owner types it at `/link-app` on the website and approves, the app signs itself in and downloads the workspace. The test then cuts the connection. The contacts list and Ada Lovelace's record come from the device, her role is changed to "Analyst" ("Saved on this device") and a new contact, Grace Hopper, is created. Meanwhile the website changes Ada's role to "Mathematician". Back online, Grace is sent, Ada's edit shows on the Sync page with both values side by side, "Keep mine" is chosen, and the website then shows "Analyst" and lists Grace.

This runs the app's own code in Chromium, built with `VITE_APP_MODE=app` on its own origin and talking to the API across origins with a bearer token, as the Tauri apps do. The desktop installers themselves are built in CI on Linux, macOS and Windows.

## Built

- **Signing in through the website** (`/auth/app/start`, `/token`, `/lookup`, `/decide`): an eight-letter code (no vowels, so no words), valid for ten minutes and collectable once. The website's `/link-app` page shows which device is asking and from where before the person approves or refuses. App sessions last 90 days from last use, show in the session list as "app" with the device's name, and can be revoked there like any other.
- **Plan switch:** `APP_PLANS` lists the plans that include the apps (unset: all). Each workspace in `/auth/me` says `apps_allowed`, and the app explains when a workspace's plan does not include it.
- **Offline copy** of contacts, organisations and talent, plus the workspace's field definitions, downloaded on first sign-in and kept current from the changes feed (`/events`); a full re-download when the feed has moved on too far. Lists, search (one box and ⌘K), tags and record pages answer from the device when the server cannot be reached; a note says so.
- **Changes made offline** are queued, folded per record, and sent in order when the connection returns, with idempotency keys (so a change that may have arrived is never applied twice) and `If-Match` versions. Records created offline get their real id when they arrive, and anything that pointed at them is updated.
- **Sync page** with status, "Sync now", changes still waiting, conflicts (field by field: yours and what is now on the server; keep mine or use theirs), and refusals with the server's reason. The top bar shows Up to date, Offline · N waiting, or N needs attention.
- **Desktop shell** (`web/apps/app/src-tauri`): Tauri 2, a 2.7 MB `.deb` on Linux. The session token is kept in the system keychain (with a private file as fallback). Sign-in pages open in the person's own browser. The CSP allows only the app itself and https servers (http on localhost for development).
- **Branded builds:** `node scripts/brand.mjs brands/<client>.json` builds the app with a client's name, identifier, icon (SVG or PNG) and server address. The name shows on the sign-in screen, the window and the device name, and each brand keeps its own keychain entry.
- **CI:** unsigned installers for Linux (`.deb`, `.rpm`, AppImage), macOS (`.app`, `.dmg`) and Windows (`.msi`, setup `.exe`), plus the example brand, uploaded as build artifacts.

## Verified

- **API** (`crates/core-api/tests/apps.rs`, 5 tests): the full sign-in flow; refused, expired, unknown and already-collected sign-ins; a token cannot be used as a cookie or the other way round; the plan switch; CORS preflights for the Tauri origins and none for other sites, with `ETag` exposed. The new migration applies and reverts cleanly (`crates/core-db/tests/migrations.rs`).
- **Web** unit tests for the offline layer (`src/offline/offline.test.ts`, 16 tests): search and sorting match the API's rules, folding queued changes, re-pointing ids, keeping workspaces apart on the device, and what is kept or wiped when a session ends or a different person signs in.
- The browser test above, and the existing demo at desktop and phone sizes, pass together in CI.
- The test turned up three bugs, now fixed. A signed-out app froze on its first page, because the router's `<Navigate>` kept re-navigating; there is now a navigate-once `GoTo`. The app returned to sign-in right after being approved, because the cached "signed out" answer was used before the fresh one arrived. Pages offline waited for a connection forever, because React Query pauses queries offline by default; in the apps they now run and answer from the device.

## Skipped, and why

- **iOS and Android builds** come next, from the same shell (`tauri ios init`, `tauri android init`). They need Xcode on a Mac runner and the Android SDK, and publishing needs Apple and Google developer accounts. How clients get branded phone apps is Solomon's decision first (see the thread).
- **Signing and auto-update.** Unsigned installers warn when installed. Signing needs an Apple Developer account ($99 a year) and a Windows code-signing certificate, and updates need a place to publish them. All are worth setting up once there is a client to ship to.
- **Offline for everything else.** Settings, imports, saved views, timelines and talent members need a connection, and the app says so. Bookings, documents and money will join the offline copy as those milestones land.
- **Branding inside the website** (logo, colours, the website's own name). The spec puts branding on the workspace record. The branded apps cover name and icon now, and in-app theming arrives with the workspace branding settings.
- **Installable website (next).** Solomon would rather start with a website people can "save" to their home screen or desktop (a progressive web app) than ship store apps. It works on every system with no store and no cost. It reuses this offline layer and shows the client's name and icon on their own address. The native apps stay here, built by CI but unpublished, until they make financial sense.
