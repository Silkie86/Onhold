import { defineConfig, devices } from "@playwright/test";

// The demo flow against a real API and database. Locally, start `onhold
// serve` and `pnpm dev` yourself (or let this start them); in CI the
// workflow migrates a fresh database first. `ONHOLD_BIN` is the built
// binary; the API's PUBLIC_BASE_URL must be this app's address.
const bin = process.env.ONHOLD_BIN ?? "../../../target/debug/onhold";
const baseURL = process.env.E2E_BASE_URL ?? "http://localhost:5173";

export default defineConfig({
  testDir: "e2e",
  globalSetup: "./e2e/setup.ts",
  timeout: 60_000,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: { baseURL, trace: "retain-on-failure", screenshot: "only-on-failure" },
  projects: [
    { name: "desktop", use: { ...devices["Desktop Chrome"], viewport: { width: 1440, height: 900 } } },
    { name: "phone", use: { ...devices["Pixel 7"], viewport: { width: 375, height: 812 } } },
  ],
  webServer: [
    {
      command: `${bin} serve`,
      url: "http://localhost:8080/readyz",
      reuseExistingServer: !process.env.CI,
      env: { PUBLIC_BASE_URL: baseURL, COOKIE_SECURE: "false" },
    },
    { command: "pnpm exec vite --port 5173 --strictPort", url: baseURL, reuseExistingServer: !process.env.CI },
  ],
});
