// Enquiries, bookings, tasks and marked-off time on the device, for when the
// app is offline: the lists and calendar read from the copy, a clash check
// done the way the server does it, and records changed offline filled in
// (talent names, the stage's name, when a hold lapses) the way the server
// would show them.

import type { RecordJson } from "@onhold/ui";
import type { WorkEntity } from "./store.ts";

type R = RecordJson;
type Named = { id: string; name: string };

const LIVE = ["hold", "pending", "confirmed"];

function words(q: string | undefined): string[] {
  return (q ?? "").toLowerCase().split(/\s+/).filter(Boolean);
}

function text(r: R): string {
  const talent = Array.isArray(r.talent) ? (r.talent as Named[]).map((t) => t.name) : [];
  return [
    r.title,
    r.venue_text,
    r.venue_name,
    r.organisation_name,
    r.contact_name,
    r.notes,
    r.related_name,
    ...talent,
    ...(Array.isArray(r.tags) ? r.tags : []),
  ]
    .filter((v) => typeof v === "string" && v)
    .join(" ")
    .toLowerCase();
}

/** Every search word appears somewhere in the record. */
export function matchesWork(r: R, q: string | undefined): boolean {
  const t = text(r);
  return words(q).every((w) => t.includes(w));
}

const time = (v: unknown) => (typeof v === "string" ? new Date(v).getTime() : Number.NaN);
const byTitle = (a: R, b: R) => String(a.title ?? "").localeCompare(String(b.title ?? "")) || String(a.id).localeCompare(String(b.id));

function page<T>(items: T[], offset = 0, limit = 50): { items: T[]; total: number } {
  return { items: items.slice(offset, offset + Math.min(Math.max(limit, 1), 500)), total: items.length };
}

// Lists ---------------------------------------------------------------------------------

export interface EnquiryParams {
  q?: string;
  stage?: string;
  limit?: number;
}

/** Newest first, as the API lists them. */
export function localEnquiries(all: R[], p: EnquiryParams): { items: R[]; total: number } {
  const found = all
    .filter((e) => matchesWork(e, p.q) && (!p.stage || e.stage === p.stage))
    .sort((a, b) => time(b.created_at) - time(a.created_at) || byTitle(a, b));
  return page(found, 0, p.limit);
}

export interface BookingParams {
  q?: string;
  /** Comma-separated statuses; empty for all. */
  status?: string;
  /** Bookings ending after this… */
  from?: string;
  /** …and starting before this. */
  to?: string;
  talent_id?: string;
  enquiry_id?: string;
  /** `starts_at` (default) or `-starts_at`. */
  sort?: string;
  limit?: number;
}

export function localBookings(all: R[], p: BookingParams): { items: R[]; total: number } {
  const statuses = (p.status ?? "").split(",").map((s) => s.trim()).filter(Boolean);
  const from = p.from ? time(p.from) : null;
  const to = p.to ? time(p.to) : null;
  const desc = p.sort === "-starts_at";
  const found = all
    .filter(
      (b) =>
        (statuses.length === 0 || statuses.includes(String(b.status))) &&
        (from === null || time(b.ends_at) > from) &&
        (to === null || time(b.starts_at) < to) &&
        (!p.talent_id || (b.talent as Named[] | undefined)?.some((t) => t.id === p.talent_id)) &&
        (!p.enquiry_id || b.enquiry_id === p.enquiry_id) &&
        matchesWork(b, p.q),
    )
    .sort((a, b) => (desc ? -1 : 1) * (time(a.starts_at) - time(b.starts_at)) || byTitle(a, b));
  return page(found, 0, p.limit);
}

export interface TaskParams {
  status?: "open" | "done" | "all";
  /** A member's id; offline, "me" is the signed-in person's id. */
  assignee_id?: string;
  related_id?: string;
  limit?: number;
}

/** Open first, soonest due first; then done, most recently done first. */
export function localTasks(all: R[], p: TaskParams): { items: R[]; total: number } {
  const status = p.status ?? "open";
  const due = (t: R) => (t.status === "open" && t.due_at ? time(t.due_at) : Number.POSITIVE_INFINITY);
  const found = all
    .filter(
      (t) =>
        (status === "all" || t.status === status) &&
        (!p.assignee_id || t.assignee_id === p.assignee_id) &&
        (!p.related_id || t.related_id === p.related_id),
    )
    .sort(
      (a, b) =>
        Number(a.status === "done") - Number(b.status === "done") ||
        due(a) - due(b) ||
        (time(b.done_at) || 0) - (time(a.done_at) || 0) ||
        time(b.created_at) - time(a.created_at) ||
        String(a.id).localeCompare(String(b.id)),
    );
  return page(found, 0, p.limit);
}

const overlaps = (r: R, from: number, to: number) => time(r.starts_at) < to && time(r.ends_at) > from;

/** What the calendar shows between `from` and `to`: live bookings and marked time, soonest first. */
export function localCalendar(
  bookings: R[],
  availability: R[],
  p: { from: string; to: string; talent_id?: string },
): { bookings: R[]; availability: R[] } {
  const from = time(p.from);
  const to = time(p.to);
  const forTalent = (ids: string[]) => !p.talent_id || ids.includes(p.talent_id);
  return {
    bookings: bookings
      .filter((b) => LIVE.includes(String(b.status)) && overlaps(b, from, to))
      .filter((b) => forTalent(((b.talent as Named[] | undefined) ?? []).map((t) => t.id)))
      .sort((a, b) => time(a.starts_at) - time(b.starts_at) || byTitle(a, b)),
    availability: availability
      .filter((a) => overlaps(a, from, to) && forTalent([String(a.talent_id)]))
      .sort((a, b) => time(a.starts_at) - time(b.starts_at)),
  };
}

export interface Clash {
  kind: "booking" | "availability";
  blocking: boolean;
  talent_id: string;
  talent_name: string;
  booking_id: string | null;
  availability_id: string | null;
  title: string | null;
  status: string;
  starts_at: string;
  ends_at: string;
}

/**
 * The clash check of `POST /bookings/clash-check`, on the device's copy:
 * live bookings of the same talent overlapping the time (confirmed ones
 * block), and unavailable (blocks) or tentative (warns) time.
 */
export function localClashes(
  bookings: R[],
  availability: R[],
  q: { talent_ids: string[]; starts_at: string; ends_at: string; except_id?: string | null },
): { blocking: boolean; clashes: Clash[] } {
  const from = time(q.starts_at);
  const to = time(q.ends_at);
  const clashes: Clash[] = [];
  for (const b of bookings) {
    if (b.id === q.except_id || !LIVE.includes(String(b.status)) || !overlaps(b, from, to)) continue;
    for (const t of (b.talent as Named[] | undefined) ?? []) {
      if (!q.talent_ids.includes(t.id)) continue;
      clashes.push({
        kind: "booking",
        blocking: b.status === "confirmed",
        talent_id: t.id,
        talent_name: t.name,
        booking_id: b.id,
        availability_id: null,
        title: String(b.title ?? ""),
        status: String(b.status),
        starts_at: String(b.starts_at),
        ends_at: String(b.ends_at),
      });
    }
  }
  for (const a of availability) {
    if (!q.talent_ids.includes(String(a.talent_id)) || !["unavailable", "tentative"].includes(String(a.kind))) continue;
    if (!overlaps(a, from, to)) continue;
    clashes.push({
      kind: "availability",
      blocking: a.kind === "unavailable",
      talent_id: String(a.talent_id),
      talent_name: String(a.talent_name ?? ""),
      booking_id: null,
      availability_id: a.id,
      title: (a.note as string | null) ?? null,
      status: String(a.kind),
      starts_at: String(a.starts_at),
      ends_at: String(a.ends_at),
    });
  }
  clashes.sort((a, b) => Number(a.blocking === false) - Number(b.blocking === false) || time(a.starts_at) - time(b.starts_at));
  return { blocking: clashes.some((c) => c.blocking), clashes };
}

// Filling in records changed offline ----------------------------------------------------------

export interface Stage {
  key: string;
  name: string;
  outcome: string;
}

export interface DressContext {
  /** A linked record's name (or title), from the device. */
  name: (entity: string, id: string) => Promise<string | null>;
  /** The enquiry pipeline's stages, when known. */
  stages?: Stage[];
  /** The workspace's hold period, when known. */
  holdDays?: number;
  now: string;
}

/** A new record as the server would start it, before the fields sent are applied. */
export function blankRecord(entity: WorkEntity, id: string, ctx: DressContext): R {
  const common = { id, version: 0, created_at: ctx.now, updated_at: ctx.now, tags: [], custom: {} };
  switch (entity) {
    case "enquiry":
      return { ...common, source: "manual", talent: [], stage: ctx.stages?.[0]?.key ?? null, stage_entered_at: ctx.now } as R;
    case "booking":
      return {
        ...common,
        status: "hold",
        status_changed_at: ctx.now,
        talent: [],
        timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
        all_day: false,
        schedule: [],
        clash_override: false,
      } as R;
    case "task":
      return { ...common, status: "open", origin: "manual" } as R;
    default:
      return common as R;
  }
}

/**
 * Fills in what the server would derive from the fields sent: names for
 * linked records, the stage's name and outcome, when a task was done, and
 * when a new hold lapses.
 */
export async function dress(entity: WorkEntity, r: R, before: R | undefined, ctx: DressContext): Promise<R> {
  const out: R = { ...r };
  if (Array.isArray(out.talent_ids)) {
    const ids = out.talent_ids as string[];
    out.talent = await Promise.all(ids.map(async (id) => ({ id, name: (await ctx.name("talent", id)) ?? "…" })));
  }
  delete out.talent_ids;
  for (const [key, linked] of [
    ["organisation_id", "organisation"],
    ["contact_id", "contact"],
    ["venue_id", "organisation"],
  ] as const) {
    const id = out[key];
    if (id === before?.[key] && before) continue;
    const nameKey = key.replace(/_id$/, "_name");
    out[nameKey] = typeof id === "string" ? ((await ctx.name(linked, id)) ?? out[nameKey] ?? null) : null;
  }
  if (entity === "enquiry" && out.stage !== before?.stage) {
    const stage = ctx.stages?.find((s) => s.key === out.stage);
    out.stage_name = stage?.name ?? out.stage_name ?? null;
    out.stage_outcome = stage?.outcome ?? out.stage_outcome ?? null;
    out.stage_entered_at = ctx.now;
  }
  if (entity === "task") {
    if (typeof out.related_id === "string" && out.related_id !== before?.related_id && out.related_entity) {
      out.related_name = await ctx.name(String(out.related_entity), out.related_id);
    }
    if (out.status !== before?.status) out.done_at = out.status === "done" ? ctx.now : null;
  }
  if (entity === "booking" && !before) {
    out.status ??= "hold";
    if (out.status === "hold" && out.hold_expires_at === undefined && ctx.holdDays !== undefined) {
      out.hold_expires_at =
        ctx.holdDays > 0 ? new Date(new Date(ctx.now).getTime() + ctx.holdDays * 86_400_000).toISOString() : null;
    }
    if (typeof out.override_reason === "string" && out.override_reason.trim() && out.status === "confirmed") {
      out.clash_override = true;
    }
  }
  return out;
}

/** The server's words for a blocking clash, so a refusal made on the device reads the same. */
export function clashMessage(report: { clashes: Clash[] }): string {
  const parts = report.clashes
    .filter((c) => c.blocking)
    .map((c) =>
      c.kind === "booking"
        ? `${c.talent_name} is already booked for “${c.title ?? "another booking"}”`
        : `${c.talent_name} is unavailable then`,
    );
  return parts.length
    ? `${parts.join("; ")}. Choose another time, or an admin can confirm anyway with a reason.`
    : "this would double-book someone";
}
