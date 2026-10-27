// The website saved to a phone's home screen: it opens with no connection,
// shows what is saved on the device, and sends changes made offline later.

import { expect, test } from "@playwright/test";

test("the installed website opens and works offline", async ({ page, context }) => {
  const invite = process.env.E2E_INVITE_INSTALLED;
  expect(invite, "global setup creates a workspace").toBeTruthy();
  // Browsers report an installed web app with this media query.
  await context.addInitScript(() => {
    const real = window.matchMedia.bind(window);
    window.matchMedia = (q: string) =>
      q.includes("display-mode: standalone") ? ({ ...real(q), matches: true, media: q } as MediaQueryList) : real(q);
  });

  await page.goto(invite ?? "/");
  await page.getByLabel("Your name").fill("Ines Installed");
  await page.getByLabel("Choose a password").fill("a perfectly fine passphrase");
  await page.getByRole("button", { name: "Accept and continue" }).click();
  await expect(page.getByRole("heading", { name: "Contacts" })).toBeVisible();

  // Installable: a manifest with the app's name and icons, and a service worker.
  const manifest = await (await page.request.get("/manifest.webmanifest")).json();
  expect(manifest).toMatchObject({ name: "OnHold", display: "standalone", start_url: "/" });
  expect(await page.evaluate(() => navigator.serviceWorker.ready.then((r) => !!r.active))).toBe(true);

  await page.getByRole("button", { name: "New contact" }).click();
  await page.getByRole("dialog").getByLabel("Name").fill("Mary Somerville");
  await page.getByRole("dialog").getByLabel("Role").fill("Astronomer");
  await page.getByRole("dialog").getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("heading", { name: "Mary Somerville" })).toBeVisible();
  const recordUrl = page.url();
  await page.goto("/crm/contacts");
  await expect(page.getByRole("link", { name: "Mary Somerville" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Up to date" })).toBeVisible({ timeout: 15_000 });

  // No connection: the page still opens, from the saved app and the device's copy.
  await context.setOffline(true);
  await page.reload();
  await expect(page.getByRole("heading", { name: "Contacts" })).toBeVisible();
  await expect(page.getByText("You are offline.")).toBeVisible();
  await page.getByRole("link", { name: "Mary Somerville" }).click();
  await page.getByLabel("Role").fill("Mathematician");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Saved on this device.")).toBeVisible();

  // Back online, the change is sent.
  await context.setOffline(false);
  await expect(page.getByRole("link", { name: "Up to date" })).toBeVisible({ timeout: 30_000 });
  await page.goto(recordUrl);
  await expect(page.getByLabel("Role")).toHaveValue("Mathematician");
});
