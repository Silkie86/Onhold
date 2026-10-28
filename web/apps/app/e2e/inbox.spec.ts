// M6's inbox in a browser: with no mailbox connected yet, a member writes to
// someone through OnHold, replies, resolves the conversation and finds where
// to connect their own mailbox. Reading Gmail and Outlook is covered against
// stand-in providers in the worker's tests.

import { expect, test } from "@playwright/test";

test("write, reply and resolve in the inbox, then find where to connect a mailbox", async ({ page, isMobile }, info) => {
  const invite = process.env[`E2E_INVITE_INBOX_${info.project.name.toUpperCase()}`];
  expect(invite, "global setup creates a workspace").toBeTruthy();

  await page.goto(invite ?? "/");
  await page.getByLabel("Your name").fill("Ana Owner");
  await page.getByLabel("Choose a password").fill("a perfectly fine passphrase");
  await page.getByRole("button", { name: "Accept and continue" }).click();
  await expect(page.getByRole("heading", { name: "Contacts" })).toBeVisible();

  const go = async (name: string) => {
    if (isMobile) await page.getByRole("button", { name: "Open menu" }).click();
    await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name, exact: true }).click();
  };

  // Nothing connected: the inbox says how to start.
  await go("Inbox");
  await expect(page.getByRole("heading", { name: "Inbox" })).toBeVisible();
  await expect(page.getByText("Connect your email")).toBeVisible();

  // A message through OnHold's own sender.
  await page.getByRole("button", { name: "New message" }).click();
  const compose = page.getByRole("dialog", { name: "New message" });
  await expect(compose.getByLabel("From")).toHaveValue("");
  await compose.getByLabel("To").fill("bea@grand.example");
  await compose.getByLabel("Subject").fill("Dates for June");
  await compose.getByLabel("Message").fill("Hi Bea,\n\nWe have 12 and 19 June.");
  await compose.getByRole("button", { name: "Send" }).click();
  await expect(page.getByRole("heading", { name: "Dates for June" })).toBeVisible();
  const conversation = page.getByRole("region", { name: "Conversation", exact: true });
  // Message bodies show in a frame where nothing in them can run.
  const message = (n: number) => conversation.locator('iframe[title="Message"]').nth(n).contentFrame();
  await expect(message(0).getByText("We have 12 and 19 June.")).toBeVisible();
  await expect(conversation.getByText("Sends through OnHold")).toBeVisible();

  // A follow-up in the same conversation.
  await conversation.getByLabel("Reply").fill("Or 26 June, if that suits better.");
  await conversation.getByRole("button", { name: "Send", exact: true }).click();
  await expect(message(1).getByText("Or 26 June, if that suits better.")).toBeVisible();

  // Resolved, it leaves the open list and is found under Resolved.
  await conversation.getByRole("button", { name: "Resolve" }).click();
  await expect(conversation.getByText("Resolved", { exact: true })).toBeVisible();
  if (isMobile) await conversation.getByRole("button", { name: "Back to conversations" }).click();
  await page.getByRole("tab", { name: "resolved" }).click();
  await expect(page.getByRole("region", { name: "Conversations", exact: true }).getByText("Dates for June")).toBeVisible();

  // Connecting a mailbox: any provider works by IMAP, with common servers filled in.
  await go("Email accounts");
  await expect(page.getByRole("heading", { name: "Email accounts" })).toBeVisible();
  await page.getByRole("button", { name: "Another provider (IMAP)" }).click();
  await page.getByLabel("Provider").selectOption("Fastmail");
  await expect(page.getByLabel("Incoming mail (IMAP) server")).toHaveValue("imap.fastmail.com");
  await expect(page.getByLabel("Outgoing mail (SMTP) port")).toHaveValue("465");
});
