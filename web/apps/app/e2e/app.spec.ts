// The desktop and mobile apps' outcome, in a browser: sign the app in by
// approving it on the website, keep working with no connection, and settle
// the clash when someone else changed the same record meanwhile.

import { type Page, expect, test } from "@playwright/test";

const WEBSITE = process.env.E2E_BASE_URL ?? "http://localhost:5173";

async function setRole(page: Page, role: string) {
  await page.getByLabel("Role").fill(role);
  await page.getByRole("button", { name: "Save" }).click();
}

test("sign in through the website, work offline, settle a conflict", async ({ browser, page: app, context }) => {
  const invite = process.env.E2E_INVITE_APP;
  expect(invite, "global setup creates a workspace").toBeTruthy();

  // The owner joins on the website and adds a contact there.
  const web = await (await browser.newContext()).newPage();
  await web.goto(`${WEBSITE}${invite}`);
  await web.getByLabel("Your name").fill("Ola Owner");
  await web.getByLabel("Choose a password").fill("a perfectly fine passphrase");
  await web.getByRole("button", { name: "Accept and continue" }).click();
  await expect(web.getByRole("heading", { name: "Contacts" })).toBeVisible();
  await web.getByRole("button", { name: "New contact" }).click();
  await web.getByRole("dialog").getByLabel("Name").fill("Ada Lovelace");
  await web.getByRole("dialog").getByLabel("Role").fill("Engineer");
  await web.getByRole("dialog").getByRole("button", { name: "Create" }).click();
  await expect(web.getByRole("heading", { name: "Ada Lovelace" })).toBeVisible();
  const adaUrl = web.url();

  // 1. The app asks for a code, and the website approves it.
  await app.goto("/");
  await app.getByRole("button", { name: "Sign in with your browser" }).click();
  const code = (await app.getByLabel("Sign-in code").textContent())?.trim() ?? "";
  expect(code).toMatch(/^[A-Z]{4}-[A-Z]{4}$/);
  await web.goto(`${WEBSITE}/link-app`);
  await web.getByLabel("Code").fill(code.toLowerCase());
  await web.getByRole("button", { name: "Continue" }).click();
  await web.getByRole("button", { name: "Approve" }).click();
  await expect(web.getByText("is now signed in as")).toBeVisible();

  // The app signs in by itself and downloads the workspace.
  await expect(app.getByRole("heading", { name: "Contacts" })).toBeVisible({ timeout: 15_000 });
  await expect(app.getByRole("link", { name: "Up to date" })).toBeVisible({ timeout: 15_000 });

  // 2. No connection: the list and the record come from the device.
  await context.setOffline(true);
  await app.getByRole("link", { name: "Organisations" }).first().click();
  await app.getByRole("link", { name: "Contacts" }).first().click();
  await expect(app.getByText("You are offline.")).toBeVisible();
  await app.getByRole("link", { name: "Ada Lovelace" }).click();
  await expect(app.getByRole("heading", { name: "Ada Lovelace" })).toBeVisible();
  await setRole(app, "Analyst");
  await expect(app.getByText("Saved on this device.")).toBeVisible();

  // A new contact made offline.
  await app.getByRole("link", { name: "Contacts" }).first().click();
  await app.getByRole("button", { name: "New contact" }).click();
  await app.getByRole("dialog").getByLabel("Name").fill("Grace Hopper");
  await app.getByRole("dialog").getByRole("button", { name: "Create" }).click();
  await expect(app.getByRole("heading", { name: "Grace Hopper" })).toBeVisible();
  await expect(app.getByRole("link", { name: /Offline · 2 waiting/ })).toBeVisible();

  // 3. Meanwhile someone changes the same contact on the website.
  await web.goto(adaUrl);
  await setRole(web, "Mathematician");
  await expect(web.getByText("Saved.")).toBeVisible();

  // 4. Back online: the new contact goes through, the edit needs a decision.
  await context.setOffline(false);
  await expect(app.getByRole("link", { name: "1 needs attention" })).toBeVisible({ timeout: 30_000 });
  await app.getByRole("link", { name: "1 needs attention" }).click();
  await expect(app.getByText("Someone else changed this while you were offline.")).toBeVisible();
  await expect(app.getByRole("cell", { name: "Analyst" })).toBeVisible();
  await expect(app.getByRole("cell", { name: "Mathematician" })).toBeVisible();
  await app.getByRole("button", { name: "Keep mine" }).click();
  await expect(app.getByText("Nothing waiting")).toBeVisible({ timeout: 15_000 });
  await expect(app.getByRole("link", { name: "Up to date" })).toBeVisible({ timeout: 15_000 });

  // The website now has both.
  await web.reload();
  await expect(web.getByLabel("Role")).toHaveValue("Analyst");
  await web.goto(`${WEBSITE}/crm/contacts`);
  await expect(web.getByRole("link", { name: "Grace Hopper" })).toBeVisible();
});
