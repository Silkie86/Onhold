// The device's copy of the CRM and bookings, and the queue of changes made offline, in
// IndexedDB (which every Tauri web view has). One database per device,
// partitioned by workspace; signing out deletes it.

import type { RecordJson } from "@onhold/ui";
import { type DBSchema, type IDBPDatabase, deleteDB, openDB } from "idb";

export type EntityName = "contact" | "organisation" | "talent";
export const OFFLINE_ENTITIES: EntityName[] = ["contact", "organisation", "talent"];

/** The bookings side: kept on the device too, but not CRM records with names and fields. */
export type WorkEntity = "enquiry" | "booking" | "task" | "availability";
export const WORK_ENTITIES: WorkEntity[] = ["enquiry", "booking", "task", "availability"];

export type StoredEntity = EntityName | WorkEntity;

export interface StoredRecord {
  ws: string;
  entity: StoredEntity;
  id: string;
  data: RecordJson;
}

/** `status` is a booking's move to another status, which has its own endpoint. */
export type OutboxOp = "create" | "update" | "delete" | "status";
export type OutboxState = "pending" | "conflict" | "failed";

export interface OutboxItem {
  seq?: number;
  ws: string;
  entity: StoredEntity;
  op: OutboxOp;
  /** The record's id; `local-…` until a create reaches the server. */
  id: string;
  /** What to send: the new record, or only the changed fields. */
  body: Record<string, unknown>;
  /** The version the change was made against, sent as `If-Match`. */
  version?: number;
  /** The record's name or title when the change was made, to show in the queue. */
  label?: string;
  /** Sent as `Idempotency-Key`, the same on every attempt. */
  key: string;
  createdAt: string;
  state: OutboxState;
  /** Why it was not accepted, in the API's words. */
  error?: string;
  /** The server's copy, when someone else changed the record meanwhile. */
  server?: RecordJson;
  /**
   * How many times the server said it was still working on this change (or
   * gave up waiting); 0 once it was sent and may have arrived. Once set,
   * nothing is folded into it again: its key may belong to exactly this body.
   */
  tries?: number;
  /** Not sent again before this time. */
  retryAt?: string;
}

interface OfflineDb extends DBSchema {
  records: { key: [string, string, string]; value: StoredRecord; indexes: { by_list: [string, string] } };
  outbox: { key: number; value: OutboxItem; indexes: { by_ws: string } };
  meta: { key: string; value: unknown };
}

const DB_NAME = "onhold-offline";
let opened: Promise<IDBPDatabase<OfflineDb>> | null = null;

function db(): Promise<IDBPDatabase<OfflineDb>> {
  opened ??= openDB<OfflineDb>(DB_NAME, 1, {
    upgrade(d) {
      const records = d.createObjectStore("records", { keyPath: ["ws", "entity", "id"] });
      records.createIndex("by_list", ["ws", "entity"]);
      const outbox = d.createObjectStore("outbox", { keyPath: "seq", autoIncrement: true });
      outbox.createIndex("by_ws", "ws");
      d.createObjectStore("meta");
    },
  });
  return opened;
}

export const LOCAL_PREFIX = "local-";
export const isLocalId = (id: string) => id.startsWith(LOCAL_PREFIX);
export const newLocalId = () => `${LOCAL_PREFIX}${crypto.randomUUID()}`;

export async function allRecords(ws: string, entity: StoredEntity): Promise<RecordJson[]> {
  const rows = await (await db()).getAllFromIndex("records", "by_list", [ws, entity]);
  return rows.map((r) => r.data);
}

export async function getRecord(ws: string, entity: StoredEntity, id: string): Promise<RecordJson | undefined> {
  return (await (await db()).get("records", [ws, entity, id]))?.data;
}

export async function putRecords(ws: string, entity: StoredEntity, records: RecordJson[]): Promise<void> {
  const tx = (await db()).transaction("records", "readwrite");
  await Promise.all([...records.map((data) => tx.store.put({ ws, entity, id: data.id, data })), tx.done]);
}

export async function deleteRecord(ws: string, entity: StoredEntity, id: string): Promise<void> {
  await (await db()).delete("records", [ws, entity, id]);
}

/** Replaces a list's records with `records`, keeping any listed in `keep`. */
export async function replaceAll(ws: string, entity: StoredEntity, records: RecordJson[], keep: Set<string>): Promise<void> {
  const d = await db();
  const tx = d.transaction("records", "readwrite");
  const existing = await tx.store.index("by_list").getAllKeys([ws, entity]);
  const incoming = new Set(records.map((r) => r.id));
  for (const key of existing) {
    const id = key[2];
    if (!incoming.has(id) && !keep.has(id)) await tx.store.delete(key);
  }
  for (const data of records) await tx.store.put({ ws, entity, id: data.id, data });
  await tx.done;
}

export async function getMeta<T>(key: string): Promise<T | undefined> {
  return (await (await db()).get("meta", key)) as T | undefined;
}

export async function setMeta(key: string, value: unknown): Promise<void> {
  await (await db()).put("meta", value, key);
}

export async function outbox(ws: string): Promise<OutboxItem[]> {
  const items = await (await db()).getAllFromIndex("outbox", "by_ws", ws);
  return items.sort((a, b) => (a.seq ?? 0) - (b.seq ?? 0));
}

export async function putOutbox(item: OutboxItem): Promise<number> {
  return (await db()).put("outbox", item);
}

export async function removeOutbox(seq: number): Promise<void> {
  await (await db()).delete("outbox", seq);
}

/**
 * Changes a workspace's queue in one transaction: `change` is given the
 * queue as it is now and returns it as it should be. Returns the new queue.
 */
export async function changeOutbox(ws: string, change: (queue: OutboxItem[]) => OutboxItem[]): Promise<OutboxItem[]> {
  const tx = (await db()).transaction("outbox", "readwrite");
  const before = (await tx.store.index("by_ws").getAll(ws)).sort((a, b) => (a.seq ?? 0) - (b.seq ?? 0));
  const after = change(before);
  const kept = new Set(after.map((q) => q.seq));
  for (const q of before) if (!kept.has(q.seq)) await tx.store.delete(q.seq as number);
  const saved: OutboxItem[] = [];
  for (const q of after) saved.push({ ...q, seq: await tx.store.put(q) });
  await tx.done;
  return saved;
}

/**
 * When the session ends without the person signing out here (it expired, or
 * was signed out from another device, perhaps because this one was lost):
 * the copy of the workspace goes, but changes not yet sent stay, marked with
 * whose they are, for that same person's next sign-in.
 */
export async function wipeAllButQueue(): Promise<void> {
  const d = await db();
  const owner = await d.get("meta", "owner");
  const tx = d.transaction(["records", "meta"], "readwrite");
  await tx.objectStore("records").clear();
  await tx.objectStore("meta").clear();
  if (owner !== undefined) await tx.objectStore("meta").put(owner, "owner");
  await tx.done;
}

/** Makes sure the device holds only `userId`'s data, clearing another person's. */
export async function claimFor(userId: string): Promise<void> {
  const owner = await getMeta<string>("owner");
  if (owner !== undefined && owner !== userId) await wipe();
  await setMeta("owner", userId);
}

/** Everything on the device, for signing out. */
export async function wipe(): Promise<void> {
  if (opened) (await opened).close();
  opened = null;
  await deleteDB(DB_NAME);
}

// Queuing changes ---------------------------------------------------------------------------

/** Applies changed fields to a record the way the API would. */
export function applyPatch(record: RecordJson, patch: Record<string, unknown>): RecordJson {
  const { custom, ...rest } = patch;
  return {
    ...record,
    ...rest,
    custom: custom ? { ...(record.custom ?? {}), ...(custom as Record<string, unknown>) } : record.custom,
  };
}

/** Applies a queued change to a record: changed fields, or a booking's new status. */
export function applyChange(record: RecordJson, op: OutboxOp, body: Record<string, unknown>, now = new Date().toISOString()): RecordJson {
  if (op !== "status") return applyPatch(record, body);
  const status = body.status as string;
  return {
    ...record,
    status,
    status_changed_at: now,
    ...(status === "cancelled" && { cancel_reason: (body.reason as string | null | undefined) ?? null }),
    ...(typeof body.override_reason === "string" &&
      body.override_reason.trim() && { clash_override: true, override_reason: body.override_reason }),
  };
}

export function mergePatches(a: Record<string, unknown>, b: Record<string, unknown>): Record<string, unknown> {
  const merged = { ...a, ...b };
  if (a.custom || b.custom) {
    merged.custom = { ...((a.custom as Record<string, unknown>) ?? {}), ...((b.custom as Record<string, unknown>) ?? {}) };
  }
  return merged;
}

/**
 * Adds a change to the queue, folding it into what is already waiting for
 * the same record: edits to a record created offline become part of its
 * creation, repeated edits become one, and deleting a record that never
 * reached the server simply forgets it. A booking's status move stays a step
 * of its own, in order, so edits after it are not folded into what came
 * before. The change being sent (`sending`, its seq) and any that may have
 * reached the server under its key stay exactly as they are; what comes
 * after them is a step of its own, and such a change is never folded into
 * what waits either. Returns the queue as it should be.
 */
export function enqueue(queue: OutboxItem[], next: OutboxItem, sending?: number): OutboxItem[] {
  const same = (q: OutboxItem) => q.entity === next.entity && q.id === next.id && q.ws === next.ws && q.state === "pending";
  const fixed = (q: OutboxItem) => (q.seq !== undefined && q.seq === sending) || q.tries !== undefined;
  // A change that may already have reached the server goes as it is, after what waits.
  if (fixed(next)) return [...queue, next];
  const settled = queue.filter((q) => same(q) && fixed(q));
  const waiting = queue.filter((q) => same(q) && !fixed(q));
  const last = waiting[waiting.length - 1];
  if (next.op === "update") {
    if (!last) return settled.some((q) => q.op === "delete") ? queue : [...queue, next];
    if (last.op === "delete") return queue;
    if (last.op === "status") return [...queue, next];
    return queue.map((q) => (q === last ? { ...q, body: mergePatches(q.body, next.body) } : q));
  }
  if (next.op === "delete") {
    if (settled.length) {
      // What is on its way stays, and the delete follows it in place of what waits.
      const rest = queue.filter((q) => !waiting.includes(q));
      return settled.some((q) => q.op === "delete") ? rest : [...rest, next];
    }
    if (waiting.some((q) => q.op === "create")) return queue.filter((q) => !same(q));
    const first = waiting[0];
    if (!first) return [...queue, next];
    // In the first waiting change's place, against the version it was made on.
    return queue.flatMap((q) => (q === first ? [{ ...q, op: "delete" as const, body: {} }] : same(q) ? [] : [q]));
  }
  return [...queue, next];
}

/** `value` with every string that is exactly `from` changed to `to`, however deep. */
export function replaceId<T>(value: T, from: string, to: string): T {
  if (value === from) return to as T;
  if (Array.isArray(value)) return value.map((v) => replaceId(v, from, to)) as T;
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, replaceId(v, from, to)])) as T;
  }
  return value;
}

/** After a create reaches the server, points everything still queued at its real id. */
export function rewriteIds(queue: OutboxItem[], from: string, to: string): OutboxItem[] {
  return queue.map((q) => ({ ...q, id: q.id === from ? to : q.id, body: replaceId(q.body, from, to) }));
}

const LOCAL_LINK = /"local-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"/;

/** True when fields link to a record created offline that has not reached the server yet. */
export function linksToLocal(body: Record<string, unknown>): boolean {
  return LOCAL_LINK.test(JSON.stringify(body));
}

/** True when a queued change must wait for a record it links to to reach the server. */
export function waitsForLocal(item: OutboxItem): boolean {
  return linksToLocal(item.body);
}

/**
 * After a change to a record is accepted, the changes queued after it for
 * the same record were made on top of it, not on top of anyone else's: they
 * move to the record's new version, so they are not mistaken for conflicts.
 * After a create (`from` undefined), everything queued for the record moves.
 */
export function rebase(queue: OutboxItem[], entity: StoredEntity, id: string, from: number | undefined, to: number): OutboxItem[] {
  return queue.map((q) =>
    q.entity === entity && q.id === id && q.state === "pending" && (from === undefined || q.version === from)
      ? { ...q, version: to }
      : q,
  );
}
