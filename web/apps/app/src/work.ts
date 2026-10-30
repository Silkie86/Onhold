// Types and plain helpers shared by the enquiry, booking, calendar, quote and
// task pages: status names and arrows, how a booking's time reads, and the
// date arithmetic the calendar is drawn with.

import type { Schemas } from "@onhold/api-client";

export type Enquiry = Schemas["EnquiryView"];
export type Booking = Schemas["BookingView"];
export type BookingStatus = Schemas["BookingStatus"];
export type Quote = Schemas["QuoteView"];
export type Task = Schemas["TaskView"];
export type Availability = Schemas["AvailabilityView"];
export type ClashReport = Schemas["ClashReport"];
export type Named = Schemas["Named"];

export const STATUS_LABEL: Record<BookingStatus, string> = {
  hold: "Hold",
  pending: "Pending",
  confirmed: "Confirmed",
  cancelled: "Cancelled",
  completed: "Completed",
  expired: "Expired",
};

/** How each status looks on a chip or card. Holds are dashed: pencilled in, not inked. */
export const STATUS_STYLE: Record<BookingStatus, string> = {
  hold: "border-dashed border-warning bg-warning/10 text-warning",
  pending: "border-info bg-info/10 text-info",
  confirmed: "border-success bg-success text-success-foreground",
  cancelled: "border-muted bg-muted text-muted-foreground line-through",
  completed: "border-muted bg-muted text-foreground",
  expired: "border-dashed border-muted text-muted-foreground line-through",
};

/** The moves a booking in `status` can make, as the server allows them. */
export function nextStatuses(status: BookingStatus): { status: BookingStatus; label: string }[] {
  switch (status) {
    case "hold":
      return [
        { status: "confirmed", label: "Confirm" },
        { status: "pending", label: "Mark pending" },
        { status: "cancelled", label: "Release hold" },
      ];
    case "pending":
      return [
        { status: "confirmed", label: "Confirm" },
        { status: "cancelled", label: "Cancel" },
      ];
    case "confirmed":
      return [
        { status: "completed", label: "Mark completed" },
        { status: "cancelled", label: "Cancel" },
      ];
    case "expired":
      return [{ status: "hold", label: "Hold again" }];
    default:
      return [];
  }
}

/** Owners and admins may confirm over a clash, with a reason. */
export function canOverride(role: string): boolean {
  return role === "owner" || role === "admin";
}

/** "Sat, 1 Jun 2031, 19:00–23:00", or the days alone for an all-day booking. */
export function formatWhen(startsAt: string, endsAt: string, allDay: boolean, locale?: string): string {
  const s = new Date(startsAt);
  const e = new Date(endsAt);
  const day = new Intl.DateTimeFormat(locale, { weekday: "short", day: "numeric", month: "short", year: "numeric" });
  const time = new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit" });
  if (allDay) {
    // An all-day booking ends at the start of the day after its last.
    const last = addDays(e, -1);
    return sameDay(s, last) || last < s ? day.format(s) : `${day.format(s)} – ${day.format(last)}`;
  }
  if (sameDay(s, e)) return `${day.format(s)}, ${time.format(s)}–${time.format(e)}`;
  return `${day.format(s)}, ${time.format(s)} – ${day.format(e)}, ${time.format(e)}`;
}

// Dates ---------------------------------------------------------------------------------

export function addDays(d: Date, days: number): Date {
  const out = new Date(d);
  out.setDate(out.getDate() + days);
  return out;
}

export function startOfDay(d: Date): Date {
  const out = new Date(d);
  out.setHours(0, 0, 0, 0);
  return out;
}

export function sameDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

/** Monday of `d`'s week. */
export function startOfWeek(d: Date): Date {
  const day = startOfDay(d);
  return addDays(day, -((day.getDay() + 6) % 7));
}

/** The six weeks of days a month view shows, Monday first. */
export function monthGrid(month: Date): Date[] {
  const first = new Date(month.getFullYear(), month.getMonth(), 1);
  const start = startOfWeek(first);
  return Array.from({ length: 42 }, (_, i) => addDays(start, i));
}

/** `YYYY-MM-DD` in local time, for addresses and keys. */
export function dayKey(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** The local day a `YYYY-MM-DD` key names; today for anything else. */
export function parseDayKey(key: string | undefined): Date {
  const m = key && /^(\d{4})-(\d{2})-(\d{2})$/.exec(key);
  if (!m) return startOfDay(new Date());
  return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
}

/** Whether something from `startsAt` to `endsAt` touches the local day `day`. */
export function onDay(startsAt: string, endsAt: string, day: Date): boolean {
  const from = startOfDay(day).getTime();
  const to = addDays(startOfDay(day), 1).getTime();
  return new Date(startsAt).getTime() < to && new Date(endsAt).getTime() > from;
}

/** The same booking moved by whole days, keeping its times of day. */
export function shiftByDays(startsAt: string, endsAt: string, days: number): { starts_at: string; ends_at: string } {
  return {
    starts_at: addDays(new Date(startsAt), days).toISOString(),
    ends_at: addDays(new Date(endsAt), days).toISOString(),
  };
}

/** Whole local days from `a` to `b`. */
export function daysBetween(a: Date, b: Date): number {
  return Math.round((startOfDay(b).getTime() - startOfDay(a).getTime()) / 86_400_000);
}

/** `datetime-local` text for an instant, in local time. */
export function toLocalInput(iso: string | null | undefined): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${dayKey(d)}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** An instant from `datetime-local` text; null when blank or unreadable. */
export function fromLocalInput(text: string): string | null {
  if (!text) return null;
  const d = new Date(text);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

/** `datetime-local` text for an instant as the clock reads in `zone`. */
export function toZonedInput(iso: string | null | undefined, zone: string): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  try {
    const parts = Object.fromEntries(
      new Intl.DateTimeFormat("en-GB", {
        timeZone: zone,
        hourCycle: "h23",
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
      })
        .formatToParts(d)
        .map((p) => [p.type, p.value]),
    );
    return `${parts.year}-${parts.month}-${parts.day}T${parts.hour}:${parts.minute}`;
  } catch {
    return toLocalInput(iso);
  }
}

/** The instant a `datetime-local` text means on a clock in `zone`; null when blank or unreadable. */
export function fromZonedInput(text: string, zone: string): string | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(text);
  if (!m) return null;
  const wanted = Date.UTC(+m[1]!, +m[2]! - 1, +m[3]!, +m[4]!, +m[5]!);
  // Guess, see how the zone reads the guess, and correct by the difference;
  // a second round settles the hours around a clock change.
  let at = wanted;
  for (let i = 0; i < 2; i++) {
    const read = toZonedInput(new Date(at).toISOString(), zone);
    const r = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(read);
    if (!r) return null;
    at += wanted - Date.UTC(+r[1]!, +r[2]! - 1, +r[3]!, +r[4]!, +r[5]!);
  }
  return new Date(at).toISOString();
}

/** "07:10" (or "Sat 14 Jun, 07:10") as the clock reads in `zone`. */
export function zonedTime(iso: string, zone: string, withDay = false, locale?: string): string {
  try {
    return new Intl.DateTimeFormat(locale, {
      timeZone: zone,
      ...(withDay && { weekday: "short", day: "numeric", month: "short" }),
      hour: "2-digit",
      minute: "2-digit",
    }).format(new Date(iso));
  } catch {
    return new Date(iso).toLocaleString(locale);
  }
}

/** Every IANA zone the browser knows, for a picker. */
export function timeZones(): string[] {
  try {
    return Intl.supportedValuesOf("timeZone");
  } catch {
    return ["UTC", "Europe/London"];
  }
}
