// The device's copy of the CRM and the queue of changes made offline, in
// IndexedDB (which every Tauri web view has). One database per device,
// partitioned by workspace; signing out deletes it.

import type { RecordJson } from "@onhold/ui";
import { type DBSchema, type IDBPDatabase, deleteDB, openDB } from "idb";

export type EntityName = "contact" | "organisation" | "talent";
export const OFFLINE_ENTITIES: EntityName[] = ["contact", "organisation", "talent"];

export interface StoredRecord {
  ws: string;
  entity: EntityName;
  id: string;
  data: RecordJson;
}

export type OutboxOp = "create" | "update" | "delete";
export type OutboxState = "pending" | "conflict" | "failed";

export interface OutboxItem {
  seq?: number;
  ws: string;
  entity: EntityName;
  op: OutboxOp;
  /** The record's id; `local-…` until a create reaches the server. */
  id: string;
  /** What to send: the new record, or only the changed fields. */
  body: Record<string, unknown>;
  /** The version the change was made against, sent as `If-Match`. */
  version?: number;
  /** Sent as `Idempotency-Key`, the same on every attempt. */
  key: string;
  createdAt: string;
  state: OutboxState;
  /** Why it was not accepted, in the API's words. */
  error?: string;
  /** The server's copy, when someone else changed the record meanwhile. */
  server?: RecordJson;
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

export async function allRecords(ws: string, entity: EntityName): Promise<RecordJson[]> {
  const rows = await (await db()).getAllFromIndex("records", "by_list", [ws, entity]);
  return rows.map((r) => r.data);
}

export async function getRecord(ws: string, entity: EntityName, id: string): Promise<RecordJson | undefined> {
  return (await (await db()).get("records", [ws, entity, id]))?.data;
}

export async function putRecords(ws: string, entity: EntityName, records: RecordJson[]): Promise<void> {
  const tx = (await db()).transaction("records", "readwrite");
  await Promise.all([...records.map((data) => tx.store.put({ ws, entity, id: data.id, data })), tx.done]);
}

export async function deleteRecord(ws: string, entity: EntityName, id: string): Promise<void> {
  await (await db()).delete("records", [ws, entity, id]);
}

/** Replaces a list's records with `records`, keeping any listed in `keep`. */
export async function replaceAll(ws: string, entity: EntityName, records: RecordJson[], keep: Set<string>): Promise<void> {
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
 * reached the server simply forgets it. Returns the queue as it should be.
 */
export function enqueue(queue: OutboxItem[], next: OutboxItem): OutboxItem[] {
  const i = queue.findIndex(
    (q) => q.entity === next.entity && q.id === next.id && q.ws === next.ws && q.state === "pending",
  );
  if (i === -1) return [...queue, next];
  const prev = queue[i] as OutboxItem;
  const rest = queue.filter((_, j) => j !== i);
  if (next.op === "update") {
    if (prev.op === "delete") return queue;
    return [...rest.slice(0, i), { ...prev, body: mergePatches(prev.body, next.body) }, ...rest.slice(i)];
  }
  if (next.op === "delete") {
    if (prev.op === "create") return rest;
    return [...rest.slice(0, i), { ...prev, op: "delete", body: {} }, ...rest.slice(i)];
  }
  return [...queue, next];
}

/** After a create reaches the server, points everything still queued at its real id. */
export function rewriteIds(queue: OutboxItem[], from: string, to: string): OutboxItem[] {
  return queue.map((q) => ({
    ...q,
    id: q.id === from ? to : q.id,
    body: q.body.organisation_id === from ? { ...q.body, organisation_id: to } : q.body,
  }));
}
