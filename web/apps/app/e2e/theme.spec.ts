// OnHold's team sets a client's full theme in the theme studio: the whole app
// previews it, it is saved for everyone, and the client's own Branding page
// keeps working beside it.

import { execFileSync } from "node:child_process";
import { createHmac } from "node:crypto";
import { expect, test } from "@playwright/test";

/** The current six-digit code for a base32 secret (RFC 6238, SHA-1, 30 s). */
function totp(secret: string, at = Date.now()): string {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = "";
  for (const ch of secret.replace(/=+$/, "")) bits += alphabet.indexOf(ch).toString(2).padStart(5, "0");
  const key = Buffer.from((bits.match(/.{8}/g) ?? []).map((b) => Number.parseInt(b, 2)));
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(Math.floor(at / 1000 / 30)));
  const h = createHmac("sha1", key).update(counter).digest();
  const o = (h[h.length - 1] ?? 0) & 0xf;
  return String((h.readUInt32BE(o) & 0x7fffffff) % 1_000_000).padStart(6, "0");
}

test("OnHold's team sets a client's full theme", async ({ page, isMobile }, info) => {
  const invite = process.env[`E2E_INVITE_THEME_${info.project.name.toUpperCase()}`];
  expect(invite, "global setup creates a workspace").toBeTruthy();
  await page.goto(invite ?? "/");
  await page.getByLabel("Your name").fill("Sol Operator");
  await page.getByLabel("Choose a password").fill("a perfectly fine passphrase");
  await page.getByRole("button", { name: "Accept and continue" }).click();
  await expect(page.getByRole("heading", { name: "Contacts" })).toBeVisible();

  const go = async (name: string) => {
    if (isMobile) await page.getByRole("button", { name: "Open menu" }).click();
    await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name, exact: true }).click();
  };

  // Not yet one of OnHold's team: the studio says so.
  await page.goto("/settings/theme");
  await expect(page.getByText(/Only OnHold's team sets the full theme/)).toBeVisible();

  // Made one, with two-factor sign-in on.
  const email = await page.evaluate(async () => (await (await fetch("/api/v1/auth/me")).json()).user.email as string);
  execFileSync(process.env.ONHOLD_BIN ?? "../../../target/debug/onhold", ["admin", "superadmin", "grant", "--email", email], { encoding: "utf8" });
  const secret = await page.evaluate(async () => {
    const me = await (await fetch("/api/v1/auth/me")).json();
    const headers = { "Content-Type": "application/json", "X-CSRF-Token": me.session.csrf_token };
    return (await (await fetch("/api/v1/auth/totp/setup", { method: "POST", headers })).json()).secret as string;
  });
  const enabled = await page.evaluate(
    async (code) => {
      const me = await (await fetch("/api/v1/auth/me")).json();
      const headers = { "Content-Type": "application/json", "X-CSRF-Token": me.session.csrf_token };
      return (await fetch("/api/v1/auth/totp/enable", { method: "POST", headers, body: JSON.stringify({ code }) })).status;
    },
    totp(secret),
  );
  expect(enabled).toBe(200);
  await page.reload();

  // The studio, from OnHold's own menu.
  await go("Theme studio");
  await expect(page.getByRole("heading", { name: "Theme studio" })).toBeVisible();
  await page.getByLabel("Typeface").selectOption("serif");
  await page.getByLabel("Page background hex, light mode").fill("#fff8ee");
  await page.getByLabel("Buttons and links hex, light mode").fill("#7b2d26");
  const token = (t: string) => page.evaluate((t) => getComputedStyle(document.documentElement).getPropertyValue(`--${t}`).trim(), t);
  await expect.poll(() => token("background")).toBe("#fff8ee");
  await expect.poll(() => token("font-sans")).toContain("Charter");

  // A bad pairing is flagged before saving.
  await page.getByLabel("Text on buttons hex, light mode").fill("#7b2d27");
  await expect(page.getByText("needs 4.5").first()).toBeVisible();
  await page.getByRole("button", { name: "Clear Text on buttons" }).click();

  // Dark mode has its own colours.
  await page.getByRole("tab", { name: "dark" }).click();
  await page.getByLabel("Page background hex, dark mode").fill("#1a1410");
  await expect.poll(() => token("background")).toBe("#1a1410");
  await page.getByRole("tab", { name: "light" }).click();

  await page.getByRole("button", { name: "Save theme" }).click();
  await expect(page.getByText(/^Saved\./)).toBeVisible();

  // Saved for everyone, after a reload too; and the client's Branding page still saves.
  await page.reload();
  await expect.poll(() => token("background")).toBe("#fff8ee");
  await go("Branding");
  await page.getByLabel("Brand colour", { exact: true }).fill("#336699");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText(/^Saved\./)).toBeVisible();
  await expect.poll(() => token("background")).toBe("#fff8ee");

  // OnHold's team sees every client workspace.
  await page.goto("/operator");
  await expect(page.getByRole("heading", { name: "Client workspaces" })).toBeVisible();
  // Earlier runs against the same database left workspaces of the same name.
  await expect(page.getByText(`E2E theme-${info.project.name}`).first()).toBeVisible();
});
