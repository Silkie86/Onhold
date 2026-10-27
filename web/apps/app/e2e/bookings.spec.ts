// M4's demonstrable outcome, in a browser: an enquiry becomes a hold, the
// hold gets a quote, the buyer accepts it from the emailed link, the booking
// is confirmed and shows on the calendar, and a double booking is stopped.

import { type Locator, expect, test } from "@playwright/test";

test("enquiry to hold to quote to accepted to confirmed, on the calendar, with no double booking", async ({ page, context, isMobile }, info) => {
  const invite = process.env[`E2E_INVITE_BOOKINGS_${info.project.name.toUpperCase()}`];
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
  const pickTalent = async (within: Locator) => {
    await within.getByLabel("Talent").fill("Midnight");
    await within.getByRole("button", { name: "The Midnight Owls" }).click();
  };

  // Some talent to book.
  await go("Talent");
  await page.getByRole("button", { name: "New talent" }).click();
  await page.getByRole("dialog").getByRole("textbox", { name: /^Name/ }).fill("The Midnight Owls");
  await page.getByRole("dialog").getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("heading", { name: "The Midnight Owls" })).toBeVisible();

  // 1. An enquiry comes in.
  await go("Enquiries");
  await page.getByRole("button", { name: "New enquiry" }).click();
  const dialog = page.getByRole("dialog");
  await pickTalent(dialog);
  await dialog.getByLabel("Title").fill("Summer ball");
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("heading", { name: "Summer ball" })).toBeVisible();
  await expect(page.getByLabel("Stage")).toHaveValue("new");

  // 2. A hold is placed for it.
  await page.getByRole("button", { name: "Place a hold" }).click();
  const hold = page.getByRole("dialog", { name: "Place a hold" });
  await expect(hold.getByLabel("Title")).toHaveValue("Summer ball");
  await expect(hold.getByText("The Midnight Owls")).toBeVisible();
  await hold.getByLabel("Starts").fill("2031-06-01T19:00");
  await hold.getByLabel("Ends").fill("2031-06-01T23:00");
  await hold.getByRole("button", { name: "Place hold" }).click();
  await page.getByRole("link", { name: /Summer ball.*Hold/s }).click();
  await expect(page.getByRole("heading", { name: "Summer ball" })).toBeVisible();
  await expect(page.getByText("Hold lapses")).toBeVisible();
  const bookingUrl = page.url();

  // 3. A quote is written and sent.
  await page.getByRole("button", { name: "New quote" }).click();
  await page.getByRole("button", { name: "Add a line" }).click();
  await page.getByLabel("Line 1 description").fill("Evening performance");
  await page.getByLabel("Line 1 price").fill("2000");
  await expect(page.getByText("Total £2,000.00")).toBeVisible();
  await page.getByRole("button", { name: "Save quote" }).click();
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await page.getByLabel("Send to").fill("buyer@example.test");
  await page.getByRole("button", { name: "Send quote" }).click();
  const link = await page.getByLabel("Link to the quote").inputValue();
  expect(link).toContain("/quote/");

  // 4. The buyer opens the link and accepts, without signing in.
  const buyer = await context.browser()!.newPage();
  await buyer.goto(link);
  await expect(buyer.getByRole("heading", { name: "Quote Q-0001" })).toBeVisible();
  await expect(buyer.getByText("£2,000.00").first()).toBeVisible();
  await buyer.getByRole("button", { name: "Accept quote" }).click();
  await buyer.getByLabel("Your full name").fill("Bea Booker");
  await buyer.getByRole("button", { name: "Accept", exact: true }).click();
  await expect(buyer.getByText(/Accepted by Bea Booker/)).toBeVisible();
  await buyer.close();

  // 5. The hold moved to pending; the agent confirms it.
  await page.goto(bookingUrl);
  await expect(page.getByText("Pending", { exact: true }).first()).toBeVisible();
  await expect(page.getByText(/accepted by Bea Booker/)).toBeVisible();
  await page.getByRole("button", { name: "Confirm" }).click();
  await expect(page.getByText("Confirmed", { exact: true }).first()).toBeVisible();

  // 6. It shows on the calendar.
  await page.goto("/calendar?date=2031-06-01");
  await expect(page.getByText("June 2031")).toBeVisible();
  await expect(page.getByRole("link", { name: /Summer ball/ })).toBeVisible();

  // 7. Booking the same act at the same time is stopped.
  await page.getByRole("button", { name: "Place a hold" }).click();
  const clash = page.getByRole("dialog", { name: "Place a hold" });
  await clash.getByLabel("Title").fill("Corporate dinner");
  await pickTalent(clash);
  await clash.getByLabel("Starts").fill("2031-06-01T20:00");
  await clash.getByLabel("Ends").fill("2031-06-01T22:00");
  await expect(clash.getByText("This would double-book someone.")).toBeVisible();
  await clash.getByLabel("As").selectOption("confirmed");
  await page.getByRole("dialog", { name: "New booking" }).getByRole("button", { name: "Save booking" }).click();
  // An owner may override, but only by giving a reason; going back leaves it unbooked.
  await expect(page.getByRole("dialog", { name: "Confirm despite the clash?" })).toBeVisible();
  await page.getByRole("button", { name: "Back" }).click();
  await page.keyboard.press("Escape");
  await page.goto("/bookings?status=confirmed");
  await expect(page.getByRole("link", { name: /Summer ball/ })).toBeVisible();
  await expect(page.getByRole("link", { name: /Corporate dinner/ })).toHaveCount(0);
});
