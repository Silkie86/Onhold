// A workspace's own look: an owner sets its name, logo and colour in
// Settings → Branding, and the app takes them on. On a phone, the invoice
// list and the calendar's title fit the screen.

import { expect, test } from "@playwright/test";

// A 120×40 red PNG.
const LOGO = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAHgAAAAoCAIAAAC6iKlyAAAAZUlEQVR4nO3QAQkAIADAMOMYwv4YyxYKd/AAZ2PPpQuN5wefBBp0K9CgW4EG3Qo06FagQbcCDboVaNCtQINuBRp0K9CgW4EG3Qo06FagQbcCDboVaNCtQINuBRp0K9CgW4EG3eoAX5f6PJCbc3wAAAAASUVORK5CYII=",
  "base64",
);

test("set the name, logo and colour, and see them in the app", async ({ page, isMobile }, info) => {
  const invite = process.env[`E2E_INVITE_BRANDING_${info.project.name.toUpperCase()}`];
  expect(invite, "global setup creates a workspace").toBeTruthy();

  await page.goto(invite ?? "/");
  await page.getByLabel("Your name").fill("Erin Owner");
  await page.getByLabel("Choose a password").fill("a perfectly fine passphrase");
  await page.getByRole("button", { name: "Accept and continue" }).click();
  await expect(page.getByRole("heading", { name: "Contacts" })).toBeVisible();

  const go = async (name: string) => {
    if (isMobile) await page.getByRole("button", { name: "Open menu" }).click();
    await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name, exact: true }).click();
  };

  await go("Branding");
  await expect(page.getByRole("heading", { name: "Branding" })).toBeVisible();
  await page.getByLabel("Business name").fill("Harbour Lights Agency");
  await page.locator("#brand-logo").setInputFiles({ name: "logo.png", mimeType: "image/png", buffer: LOGO });
  await expect(page.getByRole("img", { name: "Your logo" })).toBeVisible();
  await page.getByLabel("Brand colour", { exact: true }).fill("#C0392B");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText(/^Saved\./)).toBeVisible();

  // The app takes the colour, and keeps it after a reload.
  const primary = () => page.evaluate(() => document.documentElement.style.getPropertyValue("--primary"));
  await expect.poll(primary).toBe("#c0392b");
  await page.reload();
  await expect.poll(primary).toBe("#c0392b");
  if (isMobile) await page.getByRole("button", { name: "Open menu" }).click();
  await expect(page.getByText("Harbour Lights Agency").filter({ visible: true }).first()).toBeVisible();

  // Bad colours are refused before saving.
  if (isMobile) await page.getByRole("button", { name: "Close menu" }).last().click();
  await page.getByLabel("Brand colour", { exact: true }).fill("red");
  await expect(page.getByText("Use a hex colour such as #2747c7.")).toBeVisible();
  await expect(page.getByRole("button", { name: "Save" })).toBeDisabled();

  // Nothing on these pages is wider than the screen, and the calendar's title shows whole.
  for (const path of ["/finance/invoices", "/calendar?date=2031-11-01"]) {
    await page.goto(path);
    await page.waitForLoadState("networkidle");
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow, path).toBeLessThanOrEqual(0);
  }
  const title = page.getByText("November 2031", { exact: true });
  await expect(title).toBeVisible();
  expect(await title.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});
