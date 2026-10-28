// The setup form: an owner ticks what they will use OnHold for, sees what
// they will get and an estimate, and the app's menus follow. A solo artist
// gets a small app in their own words; nothing is locked in.

import { expect, test } from "@playwright/test";

test("a solo artist sets up a simple app, then adds invoicing back", async ({ page, isMobile }, info) => {
  const invite = process.env[`E2E_INVITE_SETUP_${info.project.name.toUpperCase()}`];
  expect(invite, "global setup creates a workspace").toBeTruthy();

  await page.goto(invite ?? "/");
  await page.getByLabel("Your name").fill("Sam Solo");
  await page.getByLabel("Choose a password").fill("a perfectly fine passphrase");
  await page.getByRole("button", { name: "Accept and continue" }).click();
  await expect(page.getByRole("heading", { name: "Contacts" })).toBeVisible();

  const menu = page.getByRole("navigation", { name: "Main" });
  const openMenu = async () => {
    if (isMobile) await page.getByRole("button", { name: "Open menu" }).click();
  };
  const closeMenu = async () => {
    if (isMobile) await page.getByRole("button", { name: "Close menu" }).last().click();
  };

  // Until the form is sent, everything shows and the app offers the form.
  await openMenu();
  await expect(menu.getByRole("link", { name: "Pipelines", exact: true })).toBeVisible();
  await closeMenu();
  await page.getByRole("link", { name: "Set up OnHold" }).click();
  await expect(page.getByRole("heading", { name: "Your setup" })).toBeVisible();

  // Each choice explains itself behind an info button.
  const soloInfo = page.getByRole("button", { name: "More about Artist, DJ or producer running their own bookings" });
  await soloInfo.click();
  await expect(soloInfo).toHaveAttribute("aria-expanded", "true");
  await expect(page.getByText(/For an artist, DJ or producer running their own bookings\. A simple app/)).toBeVisible();

  // It starts from what the workspace has now, so saving as is changes nothing.
  await expect(page.getByRole("checkbox", { name: "Shape it to how I work: custom fields, pipelines and imports" })).toBeChecked();
  await page.getByRole("checkbox", { name: "Shape it to how I work: custom fields, pipelines and imports" }).uncheck();
  await page.getByRole("checkbox", { name: "Take commission and pay talent their share" }).uncheck();
  await page.getByRole("checkbox", { name: "Artist, DJ or producer running their own bookings" }).check();
  // A role ticks what it usually needs; it can be unticked.
  await expect(page.getByRole("checkbox", { name: "Send quotes and invoices, and get paid" })).toBeChecked();
  await page.getByRole("checkbox", { name: "Read and answer email inside OnHold" }).uncheck();
  await page.getByRole("checkbox", { name: "Send quotes and invoices, and get paid" }).uncheck();
  await page.getByRole("checkbox", { name: "DJs" }).check();
  await page.getByRole("radio", { name: "Just me" }).check();
  const summary = page.getByRole("heading", { name: "Your app" }).locator("..");
  await expect(summary.getByText("Requests and gigs")).toBeVisible();
  await expect(summary.getByText("Estimated price")).toBeVisible();
  await page.getByLabel("Anything else?").fill("Can I keep my rider in here?");
  await page.getByRole("button", { name: "Set up my app" }).click();
  await expect(page.getByText(/^Saved\./)).toBeVisible();

  // The menu now holds only what they use, in their words.
  await openMenu();
  await expect(menu.getByRole("link", { name: "Gigs", exact: true })).toBeVisible();
  await expect(menu.getByRole("link", { name: "My acts", exact: true })).toBeVisible();
  for (const gone of ["Inbox", "Invoices", "Pipelines", "Custom fields", "Email accounts"]) {
    await expect(menu.getByRole("link", { name: gone, exact: true })).toHaveCount(0);
  }
  await closeMenu();
  await expect(page.getByRole("link", { name: "Set up OnHold" })).toHaveCount(0);
  await expect(page.getByText("Can I keep my rider in here?")).toBeVisible();

  // Nothing is locked in: tick invoicing again and it comes back.
  await page.getByRole("checkbox", { name: "Send quotes and invoices, and get paid" }).check();
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByText(/^Saved\./)).toBeVisible();
  await openMenu();
  await expect(menu.getByRole("link", { name: "Invoices", exact: true })).toBeVisible();
});
