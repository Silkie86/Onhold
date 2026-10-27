// Bookings on the installed website with no connection: the calendar and a
// booking open from the device, a hold is confirmed, and a new enquiry gets a
// hold (with the clash check done on the device) and a task. Back online, all
// of it reaches the server in order.

import { type Locator, expect, test } from "@playwright/test";

test("bookings, enquiries, tasks and the calendar work offline", async ({ page, context }) => {
  const invite = process.env.E2E_INVITE_INSTALLED_BOOKINGS;
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

  const go = async (name: string) => {
    await page.getByRole("button", { name: "Open menu" }).click();
    await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name, exact: true }).click();
  };
  const pickTalent = async (within: Locator) => {
    await within.getByLabel("Talent").fill("Nina");
    await within.getByRole("button", { name: "Nina Simone" }).click();
  };

  // Online: some talent, and a hold for them.
  await go("Talent");
  await page.getByRole("button", { name: "New talent" }).click();
  await page.getByRole("dialog").getByRole("textbox", { name: /^Name/ }).fill("Nina Simone");
  await page.getByRole("dialog").getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("heading", { name: "Nina Simone" })).toBeVisible();
  await go("Bookings");
  await page.getByRole("button", { name: "Place a hold" }).click();
  const first = page.getByRole("dialog", { name: "Place a hold" });
  await first.getByLabel("Title").fill("Village Gate");
  await pickTalent(first);
  await first.getByLabel("Starts").fill("2031-06-01T19:00");
  await first.getByLabel("Ends").fill("2031-06-01T23:00");
  await first.getByRole("button", { name: "Place hold" }).click();
  await expect(page.getByRole("heading", { name: "Village Gate" })).toBeVisible();
  await page.goto("/calendar?date=2031-06-01&view=agenda");
  await expect(page.getByRole("link", { name: /Village Gate/ })).toBeVisible();
  await expect(page.getByRole("link", { name: "Up to date" })).toBeVisible({ timeout: 15_000 });

  // No connection: the calendar and the booking come from the device.
  await context.setOffline(true);
  await page.reload();
  await expect(page.getByText("You are offline.")).toBeVisible();
  await page.getByRole("link", { name: /Village Gate/ }).click();
  await expect(page.getByRole("heading", { name: "Village Gate" })).toBeVisible();
  await expect(page.getByText("Activity shows when you are online.")).toBeVisible();
  await page.getByRole("button", { name: "Confirm" }).click();
  await expect(page.getByText("Confirmed", { exact: true }).first()).toBeVisible();

  // A new enquiry, a hold for it that the device sees clashing, and a task.
  await go("Enquiries");
  await page.getByRole("button", { name: "New enquiry" }).click();
  const dialog = page.getByRole("dialog");
  await pickTalent(dialog);
  await dialog.getByLabel("Title").fill("Summer festival");
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("heading", { name: "Summer festival" })).toBeVisible();
  await page.getByRole("button", { name: "Place a hold" }).click();
  const hold = page.getByRole("dialog", { name: "Place a hold" });
  await hold.getByLabel("Starts").fill("2031-06-01T20:00");
  await hold.getByLabel("Ends").fill("2031-06-01T22:00");
  await expect(hold.getByText("This would double-book someone.")).toBeVisible();
  // Confirming over it is refused on the device as the server would…
  await hold.getByLabel("As").selectOption("confirmed");
  await page.getByRole("dialog", { name: "New booking" }).getByRole("button", { name: "Save booking" }).click();
  await expect(page.getByRole("dialog", { name: "Confirm despite the clash?" })).toBeVisible();
  await page.getByRole("button", { name: "Back" }).click();
  // …but a hold is only pencilled in.
  await page.getByRole("dialog", { name: "New booking" }).getByLabel("As").selectOption("hold");
  await page.getByRole("dialog", { name: "Place a hold" }).getByRole("button", { name: "Place hold" }).click();
  await expect(page.getByRole("link", { name: /Summer festival.*Hold/s })).toBeVisible();
  await page.getByRole("button", { name: "Add task" }).click();
  await page.getByLabel("What needs doing").fill("Send the contract");
  await page.getByRole("dialog", { name: "New task" }).getByRole("button", { name: "Add task" }).click();
  await expect(page.getByText("Send the contract")).toBeVisible();
  await expect(page.getByRole("link", { name: /Offline · 4 waiting/ })).toBeVisible();

  // Back online: everything is sent, the new records with their real ids.
  await context.setOffline(false);
  await expect(page.getByRole("link", { name: "Up to date" })).toBeVisible({ timeout: 30_000 });
  await page.goto("/bookings");
  await expect(page.getByRole("link", { name: /Village Gate.*Confirmed/s })).toBeVisible();
  await expect(page.getByRole("link", { name: /Summer festival.*Hold/s })).toBeVisible();
  await page.goto("/tasks");
  await expect(page.getByText("Send the contract")).toBeVisible();
  await expect(page.getByRole("link", { name: "Summer festival" })).toBeVisible();
});
