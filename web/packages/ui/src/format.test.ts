import { contactSchema } from "./fixtures.ts";
import { fieldValue, formatMoney, formatValue, parseMoneyInput } from "./format.ts";

test("money goes to minor units by the currency's digits and back", () => {
  expect(parseMoneyInput("1,200.50", "GBP")).toBe(120050);
  expect(parseMoneyInput("1200", "JPY")).toBe(1200);
  expect(parseMoneyInput("12.5abc", "GBP")).toBeNull();
  expect(parseMoneyInput("", "GBP")).toBeNull();
  expect(formatMoney({ minor: 120050, currency: "GBP" }, "en-GB")).toBe("£1,200.50");
});

test("values are read from core and custom fields and shown as text", () => {
  const row = { id: "1", name: "Jo", custom: { genre: "jazz" } };
  expect(fieldValue(row, "name")).toBe("Jo");
  expect(fieldValue(row, "custom.genre")).toBe("jazz");
  const genre = contactSchema.fields.find((f) => f.key === "custom.genre");
  expect(formatValue(genre, "jazz")).toBe("Jazz");
  expect(formatValue(undefined, true)).toBe("Yes");
  expect(formatValue(undefined, ["a", "b"])).toBe("a, b");
  expect(formatValue(undefined, null)).toBe("");
});
