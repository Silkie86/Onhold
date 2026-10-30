import "fake-indexeddb/auto";
import type { RecordJson } from "@onhold/ui";
import { beforeEach, describe, expect, it } from "vitest";
import { localList, localTags, matches, sortRecords } from "./query.ts";
import type { Me } from "../api.ts";
import { keptForSession, keptWorkspaces } from "../session.tsx";
import {
  ORPHANED,
  type OutboxItem,
  allRecords,
  applyPatch,
  claimFor,
  enqueue,
  failOrphans,
  forgetWorkspacesExcept,
  getMeta,
  isLocalId,
  mergePatches,
  newLocalId,
  outbox,
  putOutbox,
  putRecords,
  replaceAll,
  rewriteIds,
  setMeta,
  wipe,
  wipeAllButQueue,
} from "./store.ts";

const rec = (id: string, fields: Partial<RecordJson> = {}): RecordJson =>
  ({ id, name: id, version: 1, ...fields }) as RecordJson;

const item = (op: OutboxItem["op"], id: string, body: Record<string, unknown> = {}): OutboxItem => ({
  ws: "w1",
  entity: "contact",
  op,
  id,
  body,
  key: crypto.randomUUID(),
  createdAt: "2026-10-27T10:00:00Z",
  state: "pending",
});

describe("offline search and lists", () => {
  const people = [
    rec("a", { name: "Zoe Adams", emails: ["zoe@agency.example"], tags: ["vip"], phones: ["+44 20 7946 0000"] }),
    rec("b", { name: "Adam Brook", tags: ["vip", "press"], custom: { region: "North" } }),
    rec("c", { name: null, organisation_id: "o1" }),
    rec("d", { name: "Carl Adamson", tags: ["press"], organisation_id: "o1" }),
  ];

  it("needs every word to appear somewhere", () => {
    expect(matches(people[0] as RecordJson, "zoe agency")).toBe(true);
    expect(matches(people[0] as RecordJson, "zoe press")).toBe(false);
    expect(matches(people[1] as RecordJson, "north")).toBe(true);
  });

  it("matches phone numbers without their punctuation", () => {
    expect(matches(people[0] as RecordJson, "2079460000")).toBe(true);
  });

  it("puts names starting with the search first", () => {
    const ids = sortRecords(people.filter((p) => matches(p, "adam")), null, "adam").map((r) => r.id);
    expect(ids).toEqual(["b", "d", "a"]);
  });

  it("sorts empty values last in both directions", () => {
    expect(sortRecords(people, "name").map((r) => r.id)).toEqual(["b", "d", "a", "c"]);
    expect(sortRecords(people, "-name").map((r) => r.id)).toEqual(["a", "d", "b", "c"]);
  });

  it("filters, pages and counts like the API", () => {
    const page = localList(people, { tag: "VIP", sort: "name", limit: 1 });
    expect(page.total).toBe(2);
    expect(page.items.map((r) => r.id)).toEqual(["b"]);
    expect(localList(people, { organisation_id: "o1" }).total).toBe(2);
    expect(localList(people, { offset: 3 }).items).toHaveLength(1);
  });

  it("counts tags, most used first", () => {
    expect(localTags(people)).toEqual([
      { tag: "press", count: 2 },
      { tag: "vip", count: 2 },
    ]);
  });
});

describe("queuing changes", () => {
  it("applies changed fields, merging custom fields", () => {
    const r = rec("a", { name: "Old", custom: { region: "North", tier: "A" } });
    expect(applyPatch(r, { name: "New", custom: { tier: "B" } })).toMatchObject({
      name: "New",
      custom: { region: "North", tier: "B" },
    });
    expect(mergePatches({ name: "x", custom: { a: 1 } }, { custom: { b: 2 } })).toEqual({
      name: "x",
      custom: { a: 1, b: 2 },
    });
  });

  it("folds repeated edits into one", () => {
    const q = enqueue([item("update", "a", { name: "One" })], item("update", "a", { email: "a@x.example" }));
    expect(q).toHaveLength(1);
    expect(q[0]?.body).toEqual({ name: "One", email: "a@x.example" });
  });

  it("folds edits to a record created offline into its creation", () => {
    const id = newLocalId();
    expect(isLocalId(id)).toBe(true);
    const q = enqueue([item("create", id, { name: "New" })], item("update", id, { name: "Newer" }));
    expect(q).toEqual([expect.objectContaining({ op: "create", body: { name: "Newer" } })]);
  });

  it("forgets a record created and deleted offline", () => {
    const id = newLocalId();
    const other = item("update", "b", { name: "B" });
    expect(enqueue([item("create", id, { name: "New" }), other], item("delete", id))).toEqual([other]);
  });

  it("turns an edit then a delete into a delete, in the same place", () => {
    const first = item("update", "z");
    const q = enqueue([first, item("update", "a", { name: "x" }), item("update", "c")], item("delete", "a"));
    expect(q.map((i) => [i.id, i.op])).toEqual([
      ["z", "update"],
      ["a", "delete"],
      ["c", "update"],
    ]);
    expect(q[1]?.body).toEqual({});
  });

  it("does not fold into a change waiting on a decision", () => {
    const stuck = { ...item("update", "a", { name: "x" }), state: "conflict" as const };
    expect(enqueue([stuck], item("update", "a", { name: "y" }))).toHaveLength(2);
  });

  it("leaves the change being sent, or held by the server, as it is", () => {
    const sending = { ...item("update", "a", { name: "x" }), seq: 7 };
    expect(enqueue([sending], item("update", "a", { email: "a@x.example" }), 7)).toEqual([
      sending,
      expect.objectContaining({ op: "update", body: { email: "a@x.example" } }),
    ]);
    const held = { ...item("update", "a", { name: "x" }), seq: 7, tries: 1 };
    expect(enqueue([held], item("update", "a", { email: "a@x.example" }))).toHaveLength(2);
    // A record whose creation is on its way is deleted after it arrives, not forgotten.
    const id = newLocalId();
    const creating = { ...item("create", id, { name: "New" }), seq: 8 };
    const edit = { ...item("update", id, { name: "Newer" }), seq: 9 };
    expect(enqueue([creating, edit], item("delete", id), 8).map((q) => [q.seq, q.op])).toEqual([
      [8, "create"],
      [undefined, "delete"],
    ]);
  });

  it("queues a change that may have reached the server as it is, after what waits", () => {
    const waiting = { ...item("update", "a", { name: "x" }), seq: 7 };
    const sent = { ...item("update", "a", { email: "a@x.example" }), tries: 0 };
    expect(enqueue([waiting], sent)).toEqual([waiting, sent]);
  });

  it("points queued changes at the server id once a create arrives", () => {
    const q = rewriteIds(
      [item("update", "local-1", { name: "x" }), item("create", "local-2", { organisation_id: "local-1" })],
      "local-1",
      "srv-1",
    );
    expect(q[0]?.id).toBe("srv-1");
    expect(q[1]?.body.organisation_id).toBe("srv-1");
    expect(q[1]?.id).toBe("local-2");
  });
});

describe("the device's copy", () => {
  beforeEach(async () => {
    await wipe();
  });

  it("keeps workspaces apart and replaces lists, keeping what is asked", async () => {
    await putRecords("w1", "contact", [rec("a"), rec("b"), rec("local-c")]);
    await putRecords("w2", "contact", [rec("x")]);
    await replaceAll("w1", "contact", [rec("b", { name: "B2" })], new Set(["local-c"]));
    const ids = (await allRecords("w1", "contact")).map((r) => r.id).sort();
    expect(ids).toEqual(["b", "local-c"]);
    expect((await allRecords("w2", "contact")).map((r) => r.id)).toEqual(["x"]);
  });

  it("keeps the queue and its owner when the session ends elsewhere", async () => {
    await claimFor("u1");
    await putRecords("w1", "contact", [rec("a")]);
    await setMeta("w1:cursor", 5);
    await putOutbox(item("update", "a", { name: "x" }));
    await wipeAllButQueue();
    expect(await allRecords("w1", "contact")).toEqual([]);
    expect(await getMeta("w1:cursor")).toBeUndefined();
    expect(await getMeta("owner")).toBe("u1");
    expect(await outbox("w1")).toHaveLength(1);
  });

  it("clears another person's data, and keeps the same person's", async () => {
    await claimFor("u1");
    await putOutbox(item("update", "a"));
    await claimFor("u1");
    expect(await outbox("w1")).toHaveLength(1);
    await claimFor("u2");
    expect(await outbox("w1")).toEqual([]);
    expect(await getMeta("owner")).toBe("u2");
  });

  it("clears a copy no one claimed, as one written after a sign-out would be", async () => {
    await putRecords("w1", "contact", [rec("a")]);
    await claimFor("u2");
    expect(await allRecords("w1", "contact")).toEqual([]);
    expect(await getMeta("owner")).toBe("u2");
  });

  it("does not send changes queued by no one known", async () => {
    await putOutbox(item("update", "a", { name: "x" }));
    await claimFor("u2");
    expect(await outbox("w1")).toEqual([]);
    await setMeta("w1:mail:keep", true);
    await wipe();
    await setMeta("w1:mail:keep", true);
    await claimFor("u2");
    expect(await getMeta("w1:mail:keep")).toBeUndefined();
  });

  it("forgets the whole copy of a workspace the person may no longer keep, but not their unsent changes", async () => {
    await claimFor("u1");
    for (const ws of ["w1", "w2"]) {
      await putRecords(ws, "contact", [rec(`${ws}-a`)]);
      await putRecords(ws, "booking", [rec(`${ws}-b`)]);
      await setMeta(`${ws}:cursor`, 5);
      await setMeta(`${ws}:workspace`, { hold_days: 7 });
      await setMeta(`${ws}:invoices:all:`, { items: [] });
      await putOutbox({ ...item("update", `${ws}-a`), ws });
    }
    await forgetWorkspacesExcept(["w2"]);
    expect(await allRecords("w1", "contact")).toEqual([]);
    expect(await allRecords("w1", "booking")).toEqual([]);
    expect(await getMeta("w1:cursor")).toBeUndefined();
    expect(await getMeta("w1:workspace")).toBeUndefined();
    expect(await getMeta("w1:invoices:all:")).toBeUndefined();
    expect(await outbox("w1")).toHaveLength(1);
    // The rest stays.
    expect(await allRecords("w2", "contact")).toHaveLength(1);
    expect(await getMeta("w2:workspace")).toEqual({ hold_days: 7 });
    expect(await getMeta("owner")).toBe("u1");
    // None kept at all: nothing stays but who owns the device.
    await forgetWorkspacesExcept([]);
    expect(await allRecords("w2", "booking")).toEqual([]);
    expect(await getMeta("owner")).toBe("u1");
  });
});

describe("the people the device may keep a copy for", () => {
  const w = (id: string, role: string, apps: boolean) =>
    ({ id, slug: id, name: id, status: "active", role, apps_allowed: apps }) as Me["workspaces"][number];

  it("leaves out workspaces where they are talent, and in the apps those without them", () => {
    const list = [w("staff", "agent", true), w("portal", "talent", true), w("noapps", "owner", false)];
    expect(keptWorkspaces(list, true)).toEqual(["staff"]);
    expect(keptWorkspaces(list, false)).toEqual(["staff", "noapps"]);
  });

  it("is not known until the second sign-in step is done, when no workspaces are listed", () => {
    const me = (verified: boolean, workspaces: Me["workspaces"]) =>
      ({ session: { mfa_verified: verified }, workspaces }) as unknown as Me;
    expect(keptForSession(me(false, []))).toBeNull();
    expect(keptForSession(me(true, []))).toEqual([]);
    expect(keptForSession(me(true, [w("staff", "agent", true)]), true)).toEqual(["staff"]);
  });
});

describe("changes linking to a record made offline", () => {
  it("are refused once its creation is no longer queued, and wait while it is", () => {
    const boss = newLocalId();
    const report = { ...item("create", newLocalId(), { name: "R", manager_id: boss }), seq: 2 };
    const step = { ...item("status", boss, { status: "confirmed" }), seq: 3 };
    const creating = { ...item("create", boss, { name: "B" }), seq: 1 };
    expect(failOrphans([creating, report, step])).toEqual([creating, report, step]);
    expect(failOrphans([report, step]).map((q) => [q.seq, q.state, q.error])).toEqual([
      [2, "failed", ORPHANED],
      [3, "failed", ORPHANED],
    ]);
    // A link to a record already on the server is no reason.
    const other = item("update", "srv-1", { organisation_id: "srv-2" });
    expect(failOrphans([other])).toEqual([other]);
  });
});
