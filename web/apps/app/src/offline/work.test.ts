import type { RecordJson } from "@onhold/ui";
import { describe, expect, it } from "vitest";
import {
  type OutboxItem,
  applyChange,
  enqueue,
  linksToLocal,
  newLocalId,
  rebase,
  replaceId,
  rewriteIds,
  waitsForLocal,
} from "./store.ts";
import {
  type DressContext,
  blankRecord,
  clashMessage,
  dress,
  localBookings,
  localCalendar,
  localClashes,
  localEnquiries,
  localTasks,
} from "./work.ts";

const r = (id: string, fields: Record<string, unknown> = {}) => ({ id, version: 1, ...fields }) as RecordJson;
const ada = { id: "t1", name: "Ada" };
const bob = { id: "t2", name: "Bob" };

const booking = (id: string, status: string, start: string, end: string, talent = [ada], fields = {}) =>
  r(id, { title: `Gig ${id}`, status, starts_at: start, ends_at: end, talent, ...fields });

const item = (op: OutboxItem["op"], id: string, body: Record<string, unknown> = {}, version?: number): OutboxItem => ({
  ws: "w1",
  entity: "booking",
  op,
  id,
  body,
  version,
  key: crypto.randomUUID(),
  createdAt: "2026-10-27T10:00:00Z",
  state: "pending",
});

describe("bookings on the device", () => {
  const all = [
    booking("a", "hold", "2031-06-01T19:00:00Z", "2031-06-01T23:00:00Z", [ada], { venue_text: "The Jazz Café" }),
    booking("b", "confirmed", "2031-06-01T20:00:00Z", "2031-06-01T22:00:00Z", [bob]),
    booking("c", "cancelled", "2031-06-01T20:00:00Z", "2031-06-01T22:00:00Z", [ada]),
    booking("d", "confirmed", "2031-05-01T19:00:00Z", "2031-05-01T23:00:00Z", [ada, bob], { enquiry_id: "e1" }),
  ];

  it("lists by status, time, talent and enquiry, soonest first", () => {
    expect(localBookings(all, { status: "hold,confirmed" }).items.map((b) => b.id)).toEqual(["d", "a", "b"]);
    expect(localBookings(all, { sort: "-starts_at" }).items.map((b) => b.id)).toEqual(["b", "c", "a", "d"]);
    expect(localBookings(all, { from: "2031-05-15T00:00:00Z" }).total).toBe(3);
    expect(localBookings(all, { talent_id: "t2" }).items.map((b) => b.id)).toEqual(["d", "b"]);
    expect(localBookings(all, { enquiry_id: "e1" }).items.map((b) => b.id)).toEqual(["d"]);
    expect(localBookings(all, { q: "jazz ada" }).items.map((b) => b.id)).toEqual(["a"]);
  });

  it("shows live bookings and marked time in the calendar's window", () => {
    const off = [r("x", { talent_id: "t1", kind: "unavailable", starts_at: "2031-06-02T00:00:00Z", ends_at: "2031-06-03T00:00:00Z" })];
    const june = localCalendar(all, off, { from: "2031-06-01T00:00:00Z", to: "2031-07-01T00:00:00Z" });
    expect(june.bookings.map((b) => b.id)).toEqual(["a", "b"]);
    expect(june.availability).toHaveLength(1);
    const bobs = localCalendar(all, off, { from: "2031-06-01T00:00:00Z", to: "2031-07-01T00:00:00Z", talent_id: "t2" });
    expect(bobs.bookings.map((b) => b.id)).toEqual(["b"]);
    expect(bobs.availability).toEqual([]);
  });

  it("finds clashes as the server does: confirmed and unavailable block, the rest warn", () => {
    const off = [
      r("x", { talent_id: "t1", talent_name: "Ada", kind: "unavailable", starts_at: "2031-06-01T21:00:00Z", ends_at: "2031-06-02T00:00:00Z" }),
      r("y", { talent_id: "t1", talent_name: "Ada", kind: "available", starts_at: "2031-06-01T00:00:00Z", ends_at: "2031-06-02T00:00:00Z" }),
    ];
    const q = { talent_ids: ["t1", "t2"], starts_at: "2031-06-01T18:00:00Z", ends_at: "2031-06-01T21:30:00Z" };
    const report = localClashes(all, off, q);
    expect(report.blocking).toBe(true);
    expect(report.clashes.map((c) => [c.booking_id ?? c.availability_id, c.talent_name, c.blocking])).toEqual([
      ["b", "Bob", true],
      ["x", "Ada", true],
      ["a", "Ada", false],
    ]);
    expect(clashMessage(report)).toBe(
      "Bob is already booked for “Gig b”; Ada is unavailable then. Choose another time, or an admin can confirm anyway with a reason.",
    );
    // A booking does not clash with itself; touching ends do not overlap.
    const self = localClashes(all, [], { talent_ids: ["t1"], starts_at: "2031-06-01T19:00:00Z", ends_at: "2031-06-01T23:00:00Z", except_id: "a" });
    expect(self.clashes).toEqual([]);
    expect(localClashes(all, [], { talent_ids: ["t1"], starts_at: "2031-06-01T23:00:00Z", ends_at: "2031-06-02T01:00:00Z" }).clashes).toEqual([]);
  });

  it("lists enquiries newest first and tasks as the server orders them", () => {
    const enquiries = [
      r("e1", { title: "Summer ball", stage: "new", created_at: "2031-01-01T00:00:00Z" }),
      r("e2", { title: "Wedding", stage: "quoted", created_at: "2031-02-01T00:00:00Z" }),
    ];
    expect(localEnquiries(enquiries, {}).items.map((e) => e.id)).toEqual(["e2", "e1"]);
    expect(localEnquiries(enquiries, { q: "ball" }).items.map((e) => e.id)).toEqual(["e1"]);
    const tasks = [
      r("t-late", { status: "open", due_at: "2031-01-02T00:00:00Z", created_at: "2031-01-01T00:00:00Z", assignee_id: "u1" }),
      r("t-soon", { status: "open", due_at: "2031-01-01T00:00:00Z", created_at: "2031-01-01T00:00:00Z" }),
      r("t-none", { status: "open", due_at: null, created_at: "2031-01-03T00:00:00Z", related_id: "b1" }),
      r("t-done", { status: "done", done_at: "2031-01-05T00:00:00Z", created_at: "2031-01-01T00:00:00Z" }),
    ];
    expect(localTasks(tasks, {}).items.map((t) => t.id)).toEqual(["t-soon", "t-late", "t-none"]);
    expect(localTasks(tasks, { status: "all" }).items.map((t) => t.id)).toEqual(["t-soon", "t-late", "t-none", "t-done"]);
    expect(localTasks(tasks, { assignee_id: "u1" }).total).toBe(1);
    expect(localTasks(tasks, { related_id: "b1", status: "all" }).items.map((t) => t.id)).toEqual(["t-none"]);
  });
});

describe("records changed offline", () => {
  const names: Record<string, string> = { "talent:t1": "Ada", "organisation:o1": "Ronnie's", "booking:b1": "Gig" };
  const ctx: DressContext = {
    name: async (entity, id) => names[`${entity}:${id}`] ?? null,
    stages: [
      { key: "new", name: "New", outcome: "open" },
      { key: "lost", name: "Lost", outcome: "lost" },
    ],
    holdDays: 7,
    now: "2031-01-01T12:00:00.000Z",
  };

  it("fills in a new hold as the server would", async () => {
    const body = { title: "Gig", talent_ids: ["t1"], venue_id: "o1", starts_at: "x", ends_at: "y" };
    const b = await dress("booking", applyChange(blankRecord("booking", "local-1", ctx), "create", body), undefined, ctx);
    expect(b).toMatchObject({
      status: "hold",
      talent: [{ id: "t1", name: "Ada" }],
      venue_name: "Ronnie's",
      hold_expires_at: "2031-01-08T12:00:00.000Z",
      version: 0,
    });
    expect(b.talent_ids).toBeUndefined();
  });

  it("names the stage an enquiry moves to, and a task's record", async () => {
    const before = r("e1", { stage: "new", stage_name: "New" });
    const moved = await dress("enquiry", applyChange(before, "update", { stage: "lost" }), before, ctx);
    expect(moved).toMatchObject({ stage_name: "Lost", stage_outcome: "lost", stage_entered_at: ctx.now });
    const task = await dress(
      "task",
      applyChange(blankRecord("task", "local-2", ctx), "create", { title: "Call", related_entity: "booking", related_id: "b1" }),
      undefined,
      ctx,
    );
    expect(task).toMatchObject({ status: "open", related_name: "Gig" });
    const done = await dress("task", applyChange(task, "update", { status: "done" }), task, ctx);
    expect(done.done_at).toBe(ctx.now);
  });

  it("applies a status move, with its reasons", () => {
    const b = r("b1", { status: "hold" });
    expect(applyChange(b, "status", { status: "cancelled", reason: "Rained off" }, ctx.now)).toMatchObject({
      status: "cancelled",
      cancel_reason: "Rained off",
      status_changed_at: ctx.now,
    });
    expect(applyChange(b, "status", { status: "confirmed", override_reason: "Agreed with Ada" })).toMatchObject({
      status: "confirmed",
      clash_override: true,
    });
  });
});

describe("queuing booking changes", () => {
  it("keeps a status move as its own step, in order", () => {
    let q = enqueue([], item("update", "b1", { title: "One" }, 3));
    q = enqueue(q, item("status", "b1", { status: "confirmed" }, 3));
    q = enqueue(q, item("update", "b1", { notes: "x" }, 3));
    q = enqueue(q, item("update", "b1", { venue_text: "y" }, 3));
    expect(q.map((i) => [i.op, i.body])).toEqual([
      ["update", { title: "One" }],
      ["status", { status: "confirmed" }],
      ["update", { notes: "x", venue_text: "y" }],
    ]);
    // Deleting replaces them all, in the first one's place.
    const deleted = enqueue([item("update", "z"), ...q], item("delete", "b1", {}, 3));
    expect(deleted.map((i) => [i.id, i.op])).toEqual([
      ["z", "update"],
      ["b1", "delete"],
    ]);
  });

  it("forgets everything about a booking made and deleted offline", () => {
    const id = newLocalId();
    let q = enqueue([], item("create", id, { title: "New" }));
    q = enqueue(q, item("status", id, { status: "confirmed" }, 0));
    expect(enqueue(q, item("delete", id))).toEqual([]);
  });

  it("moves later changes to the version an accepted change made", () => {
    const q = [item("status", "b1", {}, 3), item("update", "b1", {}, 3), item("update", "b2", {}, 3)];
    expect(rebase(q, "booking", "b1", 3, 4).map((i) => i.version)).toEqual([4, 4, 3]);
    expect(rebase([item("status", "b1", {}, 0)], "booking", "b1", undefined, 1)[0]?.version).toBe(1);
  });

  it("waits for records it links to, then points at their real ids", () => {
    const enquiry = newLocalId();
    const talent = newLocalId();
    const waiting = item("create", newLocalId(), { enquiry_id: enquiry, talent_ids: ["t1", talent], title: "local-ish" });
    expect(waitsForLocal(waiting)).toBe(true);
    expect(linksToLocal({ title: "local-ish" })).toBe(false);
    const [moved] = rewriteIds(rewriteIds([waiting], enquiry, "e-real"), talent, "t-real");
    expect(moved?.body).toEqual({ enquiry_id: "e-real", talent_ids: ["t1", "t-real"], title: "local-ish" });
    expect(waitsForLocal(moved as OutboxItem)).toBe(false);
    expect(replaceId({ talent: [{ id: talent, name: "New" }] }, talent, "t-real")).toEqual({ talent: [{ id: "t-real", name: "New" }] });
  });
});
