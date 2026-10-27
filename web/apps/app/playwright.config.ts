import { defineConfig, devices } from "@playwright/test";

// The demo flow against a real API and database. Locally, start `onhold
// serve` and `pnpm dev` yourself (or let this start them); in CI the
// workflow migrates a fresh database first. `ONHOLD_BIN` is the built
// binary; the API's PUBLIC_BASE_URL must be this app's address.
//
// The `app` project runs the desktop and mobile apps' code in a browser: a
// build with VITE_APP_MODE=app on its own origin, talking to the API across
// origins with a bearer token, as the Tauri apps do.
const bin = process.env.ONHOLD_BIN ?? "../../../target/debug/onhold";
const baseURL = process.env.E2E_BASE_URL ?? "http://localhost:5173";
const appURL = "http://localhost:5174";
// The website as installed ("Add to Home Screen"): a production build, so
// its service worker exists.
const installedURL = "http://localhost:5175";
const apiURL = "http://localhost:8080";

export default defineConfig({
  testDir: "e2e",
  globalSetup: "./e2e/setup.ts",
  timeout: 60_000,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: { baseURL, trace: "retain-on-failure", screenshot: "only-on-failure" },
  projects: [
    {
      name: "desktop",
      testIgnore: ["app.spec.ts", "installed*.spec.ts"],
      use: { ...devices["Desktop Chrome"], viewport: { width: 1440, height: 900 } },
    },
    { name: "phone", testIgnore: ["app.spec.ts", "installed*.spec.ts"], use: { ...devices["Pixel 7"], viewport: { width: 375, height: 812 } } },
    {
      name: "app",
      testMatch: "app.spec.ts",
      use: { ...devices["Desktop Chrome"], viewport: { width: 1280, height: 820 }, baseURL: appURL },
    },
    {
      name: "installed",
      testMatch: "installed*.spec.ts",
      use: { ...devices["Pixel 7"], viewport: { width: 375, height: 812 }, baseURL: installedURL },
    },
  ],
  webServer: [
    {
      command: `${bin} serve`,
      url: `${apiURL}/readyz`,
      reuseExistingServer: !process.env.CI,
      env: { PUBLIC_BASE_URL: baseURL, COOKIE_SECURE: "false", CORS_ORIGINS: appURL },
    },
    { command: "pnpm exec vite --port 5173 --strictPort", url: baseURL, reuseExistingServer: !process.env.CI },
    // A production build, as the apps ship it (and without the dev server's
    // reload-on-reconnect, which would get in the way of going offline).
    {
      command: "pnpm exec vite build --outDir dist-app && pnpm exec vite preview --outDir dist-app --port 5174 --strictPort",
      url: appURL,
      reuseExistingServer: !process.env.CI,
      env: { VITE_APP_MODE: "app", VITE_API_URL: apiURL },
      timeout: 180_000,
    },
    {
      command: "pnpm exec vite build --outDir dist-web && pnpm exec vite preview --outDir dist-web --port 5175 --strictPort",
      url: installedURL,
      reuseExistingServer: !process.env.CI,
      timeout: 180_000,
    },
  ],
});
