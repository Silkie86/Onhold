import { addDays, dayKey, daysBetween, formatWhen, monthGrid, nextStatuses, onDay, parseDayKey, shiftByDays, startOfWeek } from "./work.ts";
import { lineAmount, lineDrafts, lineInputs } from "./pages/quotes.tsx";
import { validateCalendarSearch, viewRange } from "./pages/calendar.tsx";

test("a month view is six whole weeks starting on a Monday", () => {
  const days = monthGrid(new Date(2031, 5, 15));
  expect(days).toHaveLength(42);
  expect(days[0]!.getDay()).toBe(1);
  expect(dayKey(days[0]!)).toBe("2031-05-26");
  expect(days.some((d) => dayKey(d) === "2031-06-30")).toBe(true);
});

test("weeks start on Monday, even from a Sunday", () => {
  expect(dayKey(startOfWeek(new Date(2031, 5, 1)))).toBe("2031-05-26");
  expect(dayKey(startOfWeek(new Date(2031, 5, 2)))).toBe("2031-06-02");
});

test("day keys round-trip and anything else means today", () => {
  expect(dayKey(parseDayKey("2031-02-28"))).toBe("2031-02-28");
  expect(dayKey(parseDayKey("junk"))).toBe(dayKey(new Date()));
  expect(dayKey(parseDayKey(undefined))).toBe(dayKey(new Date()));
});

test("a booking shows on each day it touches, and not on the day it ends at midnight", () => {
  const starts = new Date(2031, 5, 1, 22).toISOString();
  const ends = new Date(2031, 5, 2, 2).toISOString();
  expect(onDay(starts, ends, new Date(2031, 5, 1))).toBe(true);
  expect(onDay(starts, ends, new Date(2031, 5, 2))).toBe(true);
  expect(onDay(starts, ends, new Date(2031, 5, 3))).toBe(false);
  const allDay = [new Date(2031, 5, 1).toISOString(), new Date(2031, 5, 2).toISOString()] as const;
  expect(onDay(...allDay, new Date(2031, 5, 2))).toBe(false);
});

test("dragging to another day keeps the times of day", () => {
  const starts = new Date(2031, 5, 1, 19, 30);
  const ends = new Date(2031, 5, 1, 23);
  const by = daysBetween(starts, new Date(2031, 5, 4));
  expect(by).toBe(3);
  const moved = shiftByDays(starts.toISOString(), ends.toISOString(), by);
  expect(new Date(moved.starts_at).getHours()).toBe(19);
  expect(new Date(moved.starts_at).getMinutes()).toBe(30);
  expect(dayKey(new Date(moved.ends_at))).toBe("2031-06-04");
  expect(daysBetween(addDays(starts, 5), starts)).toBe(-5);
});

test("booking times read naturally", () => {
  const s = new Date(2031, 5, 1, 19).toISOString();
  const e = new Date(2031, 5, 1, 23).toISOString();
  expect(formatWhen(s, e, false, "en-GB")).toBe("Sun, 1 Jun 2031, 19:00–23:00");
  const d1 = new Date(2031, 5, 1).toISOString();
  expect(formatWhen(d1, new Date(2031, 5, 2).toISOString(), true, "en-GB")).toBe("Sun, 1 Jun 2031");
  expect(formatWhen(d1, new Date(2031, 5, 4).toISOString(), true, "en-GB")).toBe("Sun, 1 Jun 2031 – Tue, 3 Jun 2031");
});

test("status buttons follow the server's arrows", () => {
  expect(nextStatuses("hold").map((s) => s.status)).toEqual(["confirmed", "pending", "cancelled"]);
  expect(nextStatuses("confirmed").map((s) => s.status)).toEqual(["completed", "cancelled"]);
  expect(nextStatuses("expired").map((s) => s.status)).toEqual(["hold"]);
  expect(nextStatuses("cancelled")).toEqual([]);
  expect(nextStatuses("completed")).toEqual([]);
});

test("quote lines total as the server does", () => {
  expect(lineAmount(2, 15_000)).toBe(30_000);
  expect(lineAmount(1.5, 333)).toBe(500);
});

test("saving a quote's lines keeps the talent each one is for", () => {
  const talent = "0190a3c4-0000-7000-8000-000000000001";
  const saved = lineInputs(
    lineDrafts([
      { description: "Fee: The Midnight Owls", quantity: 1, unit_minor: 200_000, amount_minor: 200_000, talent_id: talent },
      { description: "Travel", quantity: 2, unit_minor: 15_000, amount_minor: 30_000 },
    ]),
  );
  expect(saved).toEqual([
    { description: "Fee: The Midnight Owls", quantity: 1, unit_minor: 200_000, talent_id: talent },
    { description: "Travel", quantity: 2, unit_minor: 15_000, talent_id: null },
  ]);
});

test("calendar addresses and ranges", () => {
  expect(validateCalendarSearch({ view: "week", date: "2031-06-01", talent: "", junk: 1 })).toEqual({
    view: "week",
    date: "2031-06-01",
    talent: undefined,
  });
  expect(validateCalendarSearch({ view: "year" }).view).toBeUndefined();
  const week = viewRange("week", new Date(2031, 5, 4));
  expect(dayKey(week.from)).toBe("2031-06-02");
  expect(dayKey(week.to)).toBe("2031-06-09");
  expect(week.days).toHaveLength(7);
});
