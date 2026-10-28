// The talent portal, M7's demonstrable: an agent invites a talent from their
// record; the talent signs up from the invitation, sees their own pending
// date on their phone, confirms it, and the agent gets a task saying so.

import { createHash, randomBytes } from "node:crypto";
import { execFileSync } from "node:child_process";
import { expect, test } from "@playwright/test";

test("a talent confirms a pending date and the agent is told", async ({ page, browser, isMobile }, info) => {
  const invite = process.env[`E2E_INVITE_PORTAL_${info.project.name.toUpperCase()}`];
  expect(invite, "global setup creates a workspace").toBeTruthy();

  await page.goto(invite ?? "/");
  await page.getByLabel("Your name").fill("Avery Agent");
  await page.getByLabel("Choose a password").fill("a perfectly fine passphrase");
  await page.getByRole("button", { name: "Accept and continue" }).click();
  await expect(page.getByRole("heading", { name: "Contacts" })).toBeVisible();

  // A talent with a pending date in Manchester.
  const talentId = await page.evaluate(async () => {
    const me = await (await fetch("/api/v1/auth/me")).json();
    const headers = {
      "Content-Type": "application/json",
      "X-CSRF-Token": me.session.csrf_token,
      "X-Workspace-Id": me.workspaces[0].id,
    };
    const talent = await (await fetch("/api/v1/talent", { method: "POST", headers, body: JSON.stringify({ name: "Nova Reyes" }) })).json();
    await fetch("/api/v1/bookings", {
      method: "POST",
      headers,
      body: JSON.stringify({
        title: "Warehouse Project",
        talent_ids: [talent.id],
        status: "pending",
        timezone: "Europe/London",
        venue_text: "Depot Mayfield, Manchester",
        starts_at: "2031-09-20T21:00:00Z",
        ends_at: "2031-09-20T23:30:00Z",
        schedule: [{ label: "Soundcheck", at: "2031-09-20T17:00:00Z" }],
        fee: { minor: 250000, currency: "GBP" },
      }),
    });
    return talent.id as string;
  });

  // Invited from their record.
  const email = `nova-${info.project.name}-${Date.now().toString(36)}@artist.test`;
  await page.goto(`/crm/talent/${talentId}`);
  const card = page.getByRole("region", { name: "Their portal" });
  await card.getByRole("button", { name: "Invite" }).click();
  await card.getByLabel("Their email").fill(email);
  await card.getByRole("button", { name: "Send invitation" }).click();
  await expect(card.getByText(`Invitation sent to ${email}`)).toBeVisible();

  // The link is in the email; the test sets a token it knows instead.
  const token = randomBytes(24).toString("base64url");
  const hash = createHash("sha256").update(token).digest("hex");
  execFileSync(
    "psql",
    [
      process.env.DATABASE_URL ?? "postgres://onhold:onhold@localhost:5432/onhold",
      "-v", "ON_ERROR_STOP=1",
      "-c", `UPDATE invitations SET token_hash = decode('${hash}', 'hex') WHERE email = '${email}' AND accepted_at IS NULL AND revoked_at IS NULL`,
    ],
    { encoding: "utf8" },
  );

  // The talent, on their own device.
  const talentContext = await browser.newContext(info.project.use);
  const phone = await talentContext.newPage();
  await phone.goto(`/invitations/accept?token=${token}`);
  await expect(phone.getByText(/invited you to see your dates/)).toBeVisible();
  await phone.getByLabel("Your name").fill("Nova Reyes");
  await phone.getByLabel("Choose a password").fill("another perfectly fine passphrase");
  await phone.getByRole("button", { name: "Accept and continue" }).click();
  await expect(phone.getByRole("heading", { name: "Your dates" })).toBeVisible();
  await expect(phone.getByText("One date is waiting for your answer.")).toBeVisible();
  // Fees stay hidden until the agency shares them.
  await expect(phone.getByText("£2,500.00")).toHaveCount(0);

  await phone.getByRole("link", { name: /Warehouse Project/ }).click();
  await expect(phone.getByRole("heading", { name: "Warehouse Project" })).toBeVisible();
  await expect(phone.getByText("Depot Mayfield, Manchester")).toBeVisible();
  await expect(phone.getByText("Soundcheck")).toBeVisible();
  await phone.getByRole("button", { name: "Confirm" }).click();
  await expect(phone.getByText(/You confirmed this date/)).toBeVisible();
  if (isMobile) {
    const overflow = await phone.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow).toBeLessThanOrEqual(0);
  }
  // The staff app does not open for them.
  await phone.goto("/bookings");
  await expect(phone.getByRole("heading", { name: "Your dates" })).toBeVisible();

  // They write to the agency; the agent answers from the inbox.
  await phone.getByRole("link", { name: "Messages" }).first().click();
  await expect(phone.getByRole("heading", { name: "Messages" })).toBeVisible();
  await phone.getByLabel("Your message").fill("Can I bring a plus one?");
  await phone.getByRole("button", { name: "Send" }).click();
  await expect(phone.getByRole("list", { name: "Conversation" }).getByText("Can I bring a plus one?")).toBeVisible();
  await page.goto("/inbox?channel=portal");
  await page.getByText("Can I bring a plus one?").first().click();
  await expect(page.getByRole("heading", { name: "Portal messages with Nova Reyes" })).toBeVisible();
  await page.getByLabel("Reply").fill("Yes, you're both on the list.");
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.getByText("Yes, you're both on the list.").first()).toBeVisible();
  await phone.reload();
  await expect(phone.getByRole("list", { name: "Conversation" }).getByText("Yes, you're both on the list.")).toBeVisible();
  await phone.screenshot({ path: info.outputPath("messages.png"), fullPage: true });

  // And keep their own bio.
  await phone.goto("/portal/profile");
  await phone.getByLabel("Based in").fill("Manchester");
  await phone.getByRole("button", { name: "Save" }).click();
  await expect(phone.getByText(/Saved/)).toBeVisible();
  await phone.goto("/portal");

  // Paid extras show padlocked, with a preview built from their own dates.
  await phone.getByRole("link", { name: "Unlock features" }).first().click();
  await expect(phone.getByRole("heading", { name: "Unlock the full app" })).toBeVisible();
  const board = phone.getByLabel("Your bookings board");
  await expect(board.getByText("Warehouse Project")).toBeVisible();
  await expect(phone.getByRole("list", { name: "Locked features" }).getByRole("listitem")).toHaveCount(6);
  await expect(phone.getByText(/Nothing is sold inside this app/)).toBeVisible();
  if (isMobile) {
    const overflow = await phone.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow).toBeLessThanOrEqual(0);
  }
  await phone.screenshot({ path: info.outputPath("unlock.png"), fullPage: true });
  await talentContext.close();

  // The agency can hide them.
  await page.goto("/settings/portal");
  await page.getByText("Hide locked features").click();
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText(/Saved/)).toBeVisible();

  // The agent's task.
  await page.goto("/tasks");
  await expect(page.getByText("Nova Reyes confirmed Warehouse Project")).toBeVisible();
});
