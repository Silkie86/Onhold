import { describe, expect, it } from "vitest";
import { estimate, price, setupOf } from "./setup.ts";

describe("setup", () => {
  it("shows everything until the workspace is known", () => {
    const s = setupOf(undefined);
    expect(s.known).toBe(false);
    expect(s.has("finance")).toBe(true);
    expect(s.hasExtra("team")).toBe(true);
    expect(s.word("crm", "talent", true)).toBe("Talent");
  });

  it("follows the workspace's modules, extras and words", () => {
    const s = setupOf({
      enabled_modules: ["crm", "bookings"],
      extras: [],
      vocabulary: { bookings: { booking: { one: "Gig", many: "Gigs" } } },
      setup_at: "2026-10-28T10:00:00Z",
    });
    expect(s.done).toBe(true);
    expect(s.has("finance")).toBe(false);
    expect(s.hasExtra("commission")).toBe(false);
    expect(s.word("bookings", "booking", true)).toBe("Gigs");
    expect(s.word("bookings", "enquiry")).toBe("Enquiry");
  });

  it("keeps showing everything from a device copy saved before setups", () => {
    const s = setupOf({ enabled_modules: ["crm", "bookings", "messaging"] });
    expect(s.has("finance")).toBe(true);
    expect(s.hasExtra("commission")).toBe(true);
    expect(s.has("documents")).toBe(false);
  });

  it("adds up an estimate", () => {
    const list = { currency: "GBP", base: 1500, features: { email: 500, invoicing: 800 }, team: { small: 2000 } };
    expect(estimate(list, { features: ["email", "invoicing", "bookings"], team: "small" })).toBe(4800);
    expect(estimate(list, { features: [], team: null })).toBe(1500);
    expect(price(4800, "GBP")).toContain("48");
    expect(price(4850, "GBP")).toContain("48.50");
    expect(price(4800, "JPY")).toContain("4,800");
  });
});
