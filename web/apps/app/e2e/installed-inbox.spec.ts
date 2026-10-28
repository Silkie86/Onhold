// Recent email kept on a phone, when the person turns it on: the inbox opens
// with no connection, a conversation reads as text, and a reply written
// offline waits on the device and goes once the phone is back online.

import { expect, test } from "@playwright/test";

test("email kept on the device can be read and answered offline", async ({ page, context }) => {
  const invite = process.env.E2E_INVITE_INSTALLED_INBOX;
  expect(invite, "global setup creates a workspace").toBeTruthy();
  await context.addInitScript(() => {
    const real = window.matchMedia.bind(window);
    window.matchMedia = (q: string) =>
      q.includes("display-mode: standalone") ? ({ ...real(q), matches: true, media: q } as MediaQueryList) : real(q);
  });

  await page.goto(invite ?? "/");
  await page.getByLabel("Your name").fill("Ola Offline");
  await page.getByLabel("Choose a password").fill("a perfectly fine passphrase");
  await page.getByRole("button", { name: "Accept and continue" }).click();
  await expect(page.getByRole("heading", { name: "Contacts" })).toBeVisible();

  const go = async (name: string) => {
    await page.getByRole("button", { name: "Open menu" }).click();
    await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name, exact: true }).click();
  };

  // A conversation to keep.
  await go("Inbox");
  await page.getByRole("button", { name: "New message" }).click();
  const compose = page.getByRole("dialog", { name: "New message" });
  await compose.getByLabel("To").fill("bea@grand.example");
  await compose.getByLabel("Subject").fill("Load-in times");
  await compose.getByLabel("Message").fill("Hi Bea, can we load in at 4pm?");
  await compose.getByRole("button", { name: "Send" }).click();
  await expect(page.getByRole("heading", { name: "Load-in times" })).toBeVisible();

  // Off unless turned on; then it shows how much room it takes.
  await go("Email accounts");
  const keep = page.getByLabel(/Keep recent email on this device/);
  await expect(keep).not.toBeChecked();
  await keep.check();
  await expect(page.getByText(/Using \d+ (bytes|KB) for 1 conversation\./)).toBeVisible({ timeout: 30_000 });

  // No connection: the app opens again, and the inbox from the device.
  await context.setOffline(true);
  await page.reload();
  await go("Inbox");
  await expect(page.getByText("Offline: the last two weeks of email kept on this device.")).toBeVisible();
  await page.getByRole("region", { name: "Conversations", exact: true }).getByText("Load-in times").click();
  const conversation = page.getByRole("region", { name: "Conversation", exact: true });
  await expect(conversation.getByText("Offline: the copy kept on this device, as text only.")).toBeVisible();
  await expect(conversation.getByText("Hi Bea, can we load in at 4pm?")).toBeVisible();

  // A reply waits on the device…
  await conversation.getByLabel("Reply").fill("Or 5pm if that is easier.");
  await conversation.getByRole("button", { name: "Send", exact: true }).click();
  await expect(conversation.getByText("Saved on this device. It sends when you are back online.")).toBeVisible();
  await expect(conversation.getByText("Waiting for a connection")).toBeVisible();

  // …and goes once back online.
  await context.setOffline(false);
  await expect(conversation.getByText("Waiting for a connection")).toBeHidden({ timeout: 30_000 });
  await page.reload();
  await expect(
    page.getByRole("region", { name: "Conversation", exact: true }).locator('iframe[title="Message"]').nth(1).contentFrame().getByText("Or 5pm if that is easier."),
  ).toBeVisible({ timeout: 15_000 });
});
