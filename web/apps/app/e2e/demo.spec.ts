// M3's demonstrable outcome, in a browser: import a spreadsheet, find a
// contact, add a custom field, and see it in the form and the table.

import { expect, test } from "@playwright/test";

const CSV = `First name,Last name,Email,Phone,Company,Job title
Priya,Patel,priya@harbour.example,+44 7700 900111,Harbour Arts,Programmer
Jo,Smith,jo@blueroom.example,,Blue Room,Head booker
Marcus,Lee,marcus@blueroom.example,07700 900222,Blue Room,
`;

test("import, find, add a field, see it in the form and the table", async ({ page, isMobile }, info) => {
  const invite = process.env[`E2E_INVITE_${info.project.name.toUpperCase()}`];
  expect(invite, "global setup creates a workspace").toBeTruthy();

  // The owner accepts their invitation and lands in the app.
  await page.goto(invite ?? "/");
  await page.getByLabel("Your name").fill("Erin Owner");
  await page.getByLabel("Choose a password").fill("a perfectly fine passphrase");
  await page.getByRole("button", { name: "Accept and continue" }).click();
  await expect(page.getByRole("heading", { name: "Contacts" })).toBeVisible();

  const go = async (name: string) => {
    if (isMobile) await page.getByRole("button", { name: "Open menu" }).click();
    await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name }).click();
  };

  // 1. Import a spreadsheet.
  await go("Imports");
  await page.getByRole("button", { name: "Import a spreadsheet" }).first().click();
  await page.getByLabel("What does the spreadsheet hold?").selectOption("contact");
  await page.getByLabel("CSV file").setInputFiles({ name: "people.csv", mimeType: "text/csv", buffer: Buffer.from(CSV) });
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByLabel("Field for Company")).toHaveValue("organisation");
  await page.getByRole("button", { name: "Check" }).click();
  await expect(page.getByText("New records")).toBeVisible();
  await page.getByRole("button", { name: "Import 3 records" }).click();
  await expect(page.getByText("Created", { exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByRole("link", { name: "See contacts" }).click();
  await expect(page.getByRole("link", { name: "Priya Patel" })).toBeVisible();

  // 2. Find a contact with search.
  await page.getByRole("button", { name: /Search…/ }).click();
  await page.getByRole("combobox", { name: "Search and go" }).fill("priya");
  await page.getByRole("option", { name: /Priya Patel/ }).click();
  await expect(page.getByRole("heading", { name: "Priya Patel" })).toBeVisible();
  await expect(page.getByText("Harbour Arts")).toBeVisible();

  // 3. Add a custom field.
  await go("Custom fields");
  await page.getByRole("button", { name: "Add field" }).click();
  await page.getByLabel("Label").fill("Dietary needs");
  await page.getByRole("dialog").getByRole("button", { name: "Add field" }).click();
  await expect(page.getByText("Dietary needs")).toBeVisible();

  // 4. See it in the form…
  await go("Contacts");
  await page.getByRole("link", { name: "Priya Patel" }).click();
  await expect(page.getByRole("heading", { name: "Priya Patel" })).toBeVisible();
  await page.getByLabel("Dietary needs").fill("vegan");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Saved.")).toBeVisible();
  await expect(page.getByText(/Dietary needs: empty → vegan/)).toBeVisible();

  // …and in the table.
  await go("Contacts");
  await expect(page.getByRole("columnheader", { name: "Dietary needs" })).toBeVisible();
  await expect(page.getByRole("cell", { name: "vegan" })).toBeVisible();
  // Nothing on the page is wider than the screen.
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(0);
});
