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

  // The app takes the colour, keeps it after a reload, and uses a lighter shade in dark mode.
  const primary = () =>
    page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--primary").trim());
  await expect.poll(primary).toBe("#c0392b");
  await page.reload();
  await expect.poll(primary).toBe("#c0392b");
  await page.emulateMedia({ colorScheme: "dark" });
  await expect.poll(primary).not.toBe("#c0392b");
  await page.emulateMedia({ colorScheme: "light" });
  if (isMobile) await page.getByRole("button", { name: "Open menu" }).click();
  await expect(page.getByText("Harbour Lights Agency").filter({ visible: true }).first()).toBeVisible();
  if (isMobile) await page.getByRole("button", { name: "Close menu" }).last().click();

  // Bad colours are refused before saving.
  await page.getByLabel("Brand colour", { exact: true }).fill("red");
  await expect(page.getByText("Use a hex colour such as #2747c7.")).toBeVisible();
  await expect(page.getByRole("button", { name: "Save" })).toBeDisabled();

  // Back to OnHold's own blue: the brand colour goes.
  await page.getByRole("button", { name: "Use OnHold blue" }).click();
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText(/^Saved\./)).toBeVisible();
  await expect.poll(() => page.evaluate(() => document.getElementById("onhold-brand") === null)).toBe(true);

  // An invoice with a long payer name still fits the screen.
  await go("Invoices");
  await page.getByRole("button", { name: "New invoice" }).click();
  const dialog = page.getByRole("dialog", { name: "New invoice" });
  await dialog.getByLabel("Or a name").fill("The Grand Harbourside Hotel and Conference Centre Ltd");
  await dialog.getByLabel("Line 1 description").fill("Evening performance");
  await dialog.getByLabel("Line 1 price").fill("123456.78");
  await dialog.getByRole("button", { name: "Save draft" }).click();
  await expect(page.getByRole("heading", { name: "Invoice (draft)" })).toBeVisible();
  await page.goto("/finance/invoices");
  await expect(page.getByText(/Grand Harbourside/).filter({ visible: true }).first()).toBeVisible();
  const overflow = () => page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(await overflow()).toBeLessThanOrEqual(0);

  // The calendar's title shows whole, inside the screen, in every view.
  for (const [view, text] of [["", "November 2031"], ["week", /2031/], ["agenda", /2031/]] as const) {
    await page.goto(`/calendar?date=2031-11-05${view ? `&view=${view}` : ""}`);
    const title = page.locator("p[aria-live=polite]").filter({ hasText: text });
    await expect(title).toBeVisible();
    const box = await title.boundingBox();
    const width = page.viewportSize()?.width ?? 0;
    expect(box && box.x >= 0 && box.x + box.width <= width, view || "month").toBe(true);
    expect(await title.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  }
  expect(await overflow()).toBeLessThanOrEqual(0);
});
