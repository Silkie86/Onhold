// Keeps the device's copy of the CRM and bookings current and sends changes
// made offline.
//
// - First run: note the newest position in the changes feed, then download
//   every contact, organisation and talent record, every enquiry, booking and
//   task, and the talent's marked-off time from two months back to about a
//   year ahead.
// - Afterwards: read the feed from that position and fetch each record that
//   changed. If the feed has moved past what the server keeps, download
//   everything again.
// - Changes made offline wait in a queue and are sent in order, each with an
//   Idempotency-Key (so a change that reached the server just before the
//   connection dropped is not applied twice) and the version it was made
//   against as If-Match (so it cannot silently overwrite someone else's
//   newer edit; that becomes a conflict for the person to settle).
// - A change that links to a record created offline waits until that record
//   has reached the server and has its real id.

import type { RecordJson } from "@onhold/ui";
import { ApiFailure, RECORD_PATHS, api, ok } from "../api.ts";
import {
  type EntityName,
  OFFLINE_ENTITIES,
  type OutboxItem,
  type OutboxOp,
  type StoredEntity,
  WORK_ENTITIES,
  type WorkEntity,
  allRecords,
  applyChange,
  deleteRecord,
  enqueue,
  getMeta,
  getRecord,
  isLocalId,
  newLocalId,
  outbox,
  putOutbox,
  putRecords,
  rebase,
  removeOutbox,
  replaceAll,
  replaceId,
  rewriteIds,
  setMeta,
  waitsForLocal,
} from "./store.ts";
import { type DressContext, type Stage, blankRecord, dress } from "./work.ts";

export interface SyncStatus {
  /** The last attempt to reach the server worked. */
  online: boolean;
  syncing: boolean;
  /** Changes waiting to be sent. */
  pending: number;
  /** Changes the server would not take: conflicts and refusals. */
  attention: number;
  /** When the copy was last brought up to date. */
  lastSync: string | null;
  /** A full download has finished at least once, so lists can be shown offline. */
  ready: boolean;
}

const TOPICS =
  "contact.*,organisation.*,talent.*,custom_field.*,enquiry.*,booking.*,task.*,availability.*,pipeline.*";
const PAGE = 500;
const EVERY_MS = 20_000;
/**
 * What a full download includes. A device whose copy was made with less
 * (before bookings were kept offline) downloads everything again once.
 */
const DATA_VERSION = 2;
/** Marked-off time is kept from this many days back… */
const AVAILABILITY_BACK_DAYS = 60;
/** …to this many ahead (the calendar API gives at most 400 at a time)… */
const AVAILABILITY_AHEAD_DAYS = 339;
/** …and the window is moved on once a day. */
const AVAILABILITY_STALE_MS = 86_400_000;

const P = (entity: StoredEntity) => `/api/v1/${RECORD_PATHS[entity]}` as "/api/v1/contacts";
const PI = (entity: StoredEntity) => `/api/v1/${RECORD_PATHS[entity]}/{id}` as "/api/v1/contacts/{id}";

/** Not allowed to see these: the copy simply has none. */
const forbidden = (e: unknown) => e instanceof ApiFailure && (e.status === 403 || e.status === 404);

/** True when a request failed for want of a connection, not because the server said no. */
export function isNetworkError(e: unknown): boolean {
  return e instanceof TypeError || (e instanceof DOMException && (e.name === "AbortError" || e.name === "TimeoutError"));
}

export class OfflineSync {
  private status: SyncStatus = { online: true, syncing: false, pending: 0, attention: 0, lastSync: null, ready: false };
  private listeners = new Set<(s: SyncStatus) => void>();
  private timer: ReturnType<typeof setInterval> | null = null;
  private running: Promise<void> | null = null;
  private changed = new Set<() => void>();
  /** Ids that records created offline were given by the server. */
  readonly renamed = new Map<string, string>();

  constructor(readonly ws: string) {}

  get current(): SyncStatus {
    return this.status;
  }

  subscribe(fn: (s: SyncStatus) => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** Called whenever records on the device change, so open pages can refresh. */
  onRecordsChanged(fn: () => void): () => void {
    this.changed.add(fn);
    return () => this.changed.delete(fn);
  }

  private set(patch: Partial<SyncStatus>) {
    this.status = { ...this.status, ...patch };
    for (const fn of this.listeners) fn(this.status);
  }

  private notifyChanged() {
    for (const fn of this.changed) fn();
  }

  private readonly kick = () => void this.syncNow();
  private readonly offline = () => this.set({ online: false });

  async start(): Promise<void> {
    const lastSync = (await getMeta<string>(`${this.ws}:lastSync`)) ?? null;
    this.set({ lastSync, ready: lastSync !== null });
    await this.refreshCounts();
    window.addEventListener("online", this.kick);
    window.addEventListener("offline", this.offline);
    document.addEventListener("visibilitychange", this.kick);
    this.timer = setInterval(this.kick, EVERY_MS);
    void this.syncNow();
  }

  stop() {
    window.removeEventListener("online", this.kick);
    window.removeEventListener("offline", this.offline);
    document.removeEventListener("visibilitychange", this.kick);
    if (this.timer) clearInterval(this.timer);
  }

  /** Sends waiting changes, then fetches what changed. One run at a time. */
  syncNow(): Promise<void> {
    if (typeof document !== "undefined" && document.visibilityState === "hidden") return Promise.resolve();
    this.running ??= this.run().finally(() => {
      this.running = null;
    });
    return this.running;
  }

  private async run() {
    if (!navigator.onLine) {
      this.set({ online: false });
      return;
    }
    this.set({ syncing: true });
    try {
      await this.flush();
      const cursor = await getMeta<number>(`${this.ws}:cursor`);
      const version = await getMeta<number>(`${this.ws}:dataVersion`);
      if (cursor === undefined || version !== DATA_VERSION) await this.downloadAll();
      else await this.pullChanges(cursor);
      const now = new Date().toISOString();
      await setMeta(`${this.ws}:lastSync`, now);
      this.set({ online: true, lastSync: now, ready: true });
    } catch (e) {
      if (isNetworkError(e)) this.set({ online: false });
      else console.warn("sync failed", e);
    } finally {
      await this.refreshCounts();
      this.set({ syncing: false });
    }
  }

  private async refreshCounts() {
    const queue = await outbox(this.ws);
    this.set({
      pending: queue.filter((q) => q.state === "pending").length,
      attention: queue.filter((q) => q.state !== "pending").length,
    });
  }

  // Downloading -------------------------------------------------------------------------

  private async latestPosition(): Promise<number> {
    const page = await ok(api.GET("/api/v1/events", { params: { query: { limit: 1, topics: TOPICS } } }));
    return page.latest;
  }

  async downloadAll(): Promise<void> {
    // The position first: anything that changes during the download is read again from the feed.
    const latest = await this.latestPosition();
    const keep = new Set((await outbox(this.ws)).map((q) => q.id));
    for (const entity of OFFLINE_ENTITIES) {
      const all: RecordJson[] = [];
      for (let offset = 0; ; offset += PAGE) {
        const page = (await ok(
          api.GET(P(entity), { params: { query: { offset, limit: PAGE, sort: "name" } } }),
        )) as unknown as { items: RecordJson[]; total: number };
        all.push(...page.items);
        if (page.items.length < PAGE) break;
      }
      // Records waiting to be sent keep their offline edits.
      await replaceAll(this.ws, entity, all.filter((r) => !keep.has(r.id)), keep);
      await this.downloadSchema(entity);
    }
    for (const entity of ["enquiry", "booking", "task"] as const) {
      let all: RecordJson[] = [];
      try {
        all = await this.downloadWork(entity);
      } catch (e) {
        if (!forbidden(e)) throw e;
      }
      await replaceAll(this.ws, entity, all.filter((r) => !keep.has(r.id)), keep);
    }
    for (const entity of ["enquiry", "booking"] as const) await this.downloadSchema(entity);
    await this.downloadAvailability();
    await this.downloadStages();
    await this.downloadExtras();
    await setMeta(`${this.ws}:cursor`, latest);
    await setMeta(`${this.ws}:dataVersion`, DATA_VERSION);
    this.notifyChanged();
  }

  private async downloadWork(entity: "enquiry" | "booking" | "task"): Promise<RecordJson[]> {
    const all: RecordJson[] = [];
    // Tasks come at most 200 a page; the others take a sort that keeps paging stable.
    const size = entity === "task" ? 200 : PAGE;
    for (let offset = 0; ; offset += size) {
      const query = entity === "task" ? { status: "all", offset, limit: size } : { offset, limit: size, sort: "created_at" };
      const page = (await ok(api.GET(P(entity), { params: { query: query as never } }))) as unknown as {
        items: RecordJson[];
      };
      all.push(...page.items);
      if (page.items.length < size) return all;
    }
  }

  /** The talent's marked-off time in the window the device keeps. */
  private async downloadAvailability() {
    const now = Date.now();
    const from = new Date(now - AVAILABILITY_BACK_DAYS * 86_400_000).toISOString();
    const to = new Date(now + AVAILABILITY_AHEAD_DAYS * 86_400_000).toISOString();
    let items: RecordJson[] = [];
    try {
      const cal = await ok(api.GET("/api/v1/calendar", { params: { query: { from, to } } }));
      items = cal.availability as unknown as RecordJson[];
    } catch (e) {
      if (!forbidden(e)) throw e;
    }
    await replaceAll(this.ws, "availability", items, new Set());
    await setMeta(`${this.ws}:availabilityAt`, now);
  }

  private async downloadStages() {
    try {
      const list = await ok(api.GET("/api/v1/pipelines", { params: { query: { entity: "enquiry" } } }));
      const pipeline = list.find((p) => p.is_default) ?? list[0];
      const stages: Stage[] | null = pipeline ? pipeline.stages.filter((s) => !s.archived) : null;
      await setMeta(`${this.ws}:stages`, stages);
    } catch (e) {
      if (!forbidden(e)) throw e;
    }
  }

  /** The members (whom a task is for) and the workspace's settings. */
  private async downloadExtras() {
    for (const [key, get] of [
      ["members", () => ok(api.GET("/api/v1/members"))],
      ["workspace", () => ok(api.GET("/api/v1/workspace"))],
    ] as const) {
      try {
        await setMeta(`${this.ws}:${key}`, await get());
      } catch (e) {
        if (!forbidden(e)) throw e;
      }
    }
  }

  private async downloadSchema(entity: StoredEntity) {
    try {
      const schema = await ok(api.GET("/api/v1/schema/{entity}", { params: { path: { entity } } }));
      await setMeta(`${this.ws}:schema:${entity}`, schema);
    } catch (e) {
      if (!forbidden(e)) throw e;
    }
  }

  private async pullChanges(cursor: number): Promise<void> {
    let after = cursor;
    const touched = new Map<string, EntityName | "enquiry" | "booking" | "task">();
    let fieldsChanged = false;
    let availabilityChanged = false;
    let stagesChanged = false;
    for (;;) {
      let page;
      try {
        page = await ok(api.GET("/api/v1/events", { params: { query: { after, topics: TOPICS, limit: PAGE } } }));
      } catch (e) {
        if (e instanceof ApiFailure && e.status === 410) {
          await this.downloadAll();
          return;
        }
        throw e;
      }
      for (const ev of page.events) {
        const entity = ev.topic.split(".")[0] ?? "";
        if (entity === "custom_field") fieldsChanged = true;
        else if (entity === "availability") availabilityChanged = true;
        else if (entity === "pipeline") stagesChanged = true;
        else if (ev.object_id && [...OFFLINE_ENTITIES, "enquiry", "booking", "task"].includes(entity)) {
          touched.set(ev.object_id, entity as EntityName);
        }
      }
      after = page.next_after;
      if (!page.has_more) break;
    }
    const waiting = new Set((await outbox(this.ws)).map((q) => q.id));
    for (const [id, entity] of touched) {
      // A record with changes still to send keeps them; a conflict will show if needed.
      if (waiting.has(id)) continue;
      await this.refetch(entity, id);
    }
    if (fieldsChanged) {
      for (const entity of [...OFFLINE_ENTITIES, "enquiry", "booking"] as const) await this.downloadSchema(entity);
    }
    const availabilityAt = (await getMeta<number>(`${this.ws}:availabilityAt`)) ?? 0;
    const availabilityStale = Date.now() - availabilityAt > AVAILABILITY_STALE_MS;
    if (availabilityChanged || availabilityStale) await this.downloadAvailability();
    if (stagesChanged) await this.downloadStages();
    if (availabilityStale) await this.downloadExtras();
    await setMeta(`${this.ws}:cursor`, after);
    if (touched.size || fieldsChanged || availabilityChanged || stagesChanged) this.notifyChanged();
  }

  private async refetch(entity: StoredEntity, id: string) {
    try {
      const r = (await ok(api.GET(PI(entity), { params: { path: { id } } }))) as unknown as RecordJson;
      await putRecords(this.ws, entity, [r]);
    } catch (e) {
      if (e instanceof ApiFailure && (e.status === 404 || e.status === 403)) await deleteRecord(this.ws, entity, id);
      else throw e;
    }
  }

  // Changes made on this device -----------------------------------------------------------

  private async saveQueue(before: OutboxItem[], after: OutboxItem[]) {
    const kept = new Set(after.map((q) => q.seq).filter((s) => s !== undefined));
    for (const q of before) if (q.seq !== undefined && !kept.has(q.seq)) await removeOutbox(q.seq);
    for (const q of after) await putOutbox(q);
    await this.refreshCounts();
  }

  /** Queues a change and applies it to the device's copy straight away. Returns the record as it now reads. */
  async queue(
    entity: StoredEntity,
    op: OutboxOp,
    id: string | null,
    body: Record<string, unknown>,
    version?: number,
    key: string = crypto.randomUUID(),
  ): Promise<RecordJson | null> {
    const recordId = id ?? newLocalId();
    const before = await outbox(this.ws);
    const current = id ? await getRecord(this.ws, entity, id) : undefined;
    const label = String(body.name ?? body.title ?? current?.name ?? current?.title ?? "") || undefined;
    const item: OutboxItem = {
      ws: this.ws,
      entity,
      op,
      id: recordId,
      body,
      label,
      version,
      key,
      createdAt: new Date().toISOString(),
      state: "pending",
    };
    await this.saveQueue(before, enqueue(before, item));

    let result: RecordJson | null = null;
    if (op === "delete") {
      await deleteRecord(this.ws, entity, recordId);
    } else if ((WORK_ENTITIES as string[]).includes(entity)) {
      result = await this.applyWork(entity as WorkEntity, op, recordId, body);
      await putRecords(this.ws, entity, [result]);
    } else {
      const existing = op === "create" ? undefined : await getRecord(this.ws, entity, recordId);
      const now = new Date().toISOString();
      const base: RecordJson = existing ?? {
        id: recordId,
        tags: [],
        emails: [],
        phones: [],
        custom: {},
        version: 0,
        created_at: now,
      };
      result = applyChange({ ...base, updated_at: now }, op, body);
      if (entity === "contact" && typeof result.organisation_id === "string") {
        const org = await getRecord(this.ws, "organisation", result.organisation_id);
        result.organisation_name = org?.name ?? result.organisation_name ?? null;
      }
      if (entity === "contact" && Array.isArray(result.emails)) result.email = result.emails[0] ?? null;
      await putRecords(this.ws, entity, [result]);
    }
    this.notifyChanged();
    void this.syncNow();
    return result;
  }

  /** What a name lookup, the stages and the hold period are, for filling in records changed here. */
  private async dressContext(): Promise<DressContext> {
    const workspace = await getMeta<{ hold_days?: number }>(`${this.ws}:workspace`);
    return {
      name: async (entity, id) => {
        if (!(entity in RECORD_PATHS)) return null;
        const r = await getRecord(this.ws, entity as StoredEntity, id);
        return r ? String(r.name ?? r.title ?? "") || null : null;
      },
      stages: (await getMeta<Stage[] | null>(`${this.ws}:stages`)) ?? undefined,
      holdDays: workspace?.hold_days,
      now: new Date().toISOString(),
    };
  }

  /** A booking, enquiry or task as it reads after a change made here. */
  private async applyWork(entity: WorkEntity, op: OutboxOp, id: string, body: Record<string, unknown>): Promise<RecordJson> {
    const ctx = await this.dressContext();
    const before = op === "create" ? undefined : await getRecord(this.ws, entity, id);
    const base = before ?? blankRecord(entity, id, ctx);
    return dress(entity, applyChange({ ...base, updated_at: ctx.now }, op, body, ctx.now), before, ctx);
  }

  /** True while changes to this record are waiting to be sent. */
  async hasQueued(id: string): Promise<boolean> {
    return (await outbox(this.ws)).some((q) => q.id === id);
  }

  private async flush(): Promise<void> {
    const tried = new Set<number>();
    for (;;) {
      const queue = await outbox(this.ws);
      // A record whose earlier change needs attention keeps its later ones
      // waiting too, so they still apply in order.
      const stuck = new Set(queue.filter((q) => q.state !== "pending").map((q) => q.id));
      const next = queue.find(
        (q) => q.state === "pending" && !stuck.has(q.id) && !tried.has(q.seq ?? -1) && !waitsForLocal(q),
      );
      if (!next) return;
      tried.add(next.seq ?? -1);
      await this.send(next, queue);
    }
  }

  private async send(item: OutboxItem, queue: OutboxItem[]) {
    const headers: Record<string, string> = { "Idempotency-Key": item.key };
    if (item.version !== undefined && item.op !== "create") headers["If-Match"] = `"${item.version}"`;
    try {
      if (item.op === "create") {
        const created = (await ok(
          api.POST(P(item.entity), { headers, body: item.body as never }),
        )) as unknown as RecordJson;
        await deleteRecord(this.ws, item.entity, item.id);
        this.renamed.set(item.id, created.id);
        await setMeta(`${this.ws}:renamed:${item.id}`, created.id);
        const rest = rebase(
          rewriteIds(
            queue.filter((q) => q.seq !== item.seq),
            item.id,
            created.id,
          ),
          item.entity,
          created.id,
          undefined,
          Number(created.version),
        );
        await this.saveQueue(queue, rest);
        await this.rewriteLocalLinks(item.id, created.id);
        await this.keepLocal(item.entity, created, rest);
      } else if (item.op === "update" || item.op === "status") {
        const path = { params: { path: { id: item.id } }, headers, body: item.body as never };
        const updated = (
          item.op === "status"
            ? await ok(api.POST("/api/v1/bookings/{id}/status", path))
            : await ok(api.PATCH(PI(item.entity), path))
        ) as unknown as RecordJson;
        const rest = rebase(
          queue.filter((q) => q.seq !== item.seq),
          item.entity,
          item.id,
          item.version,
          Number(updated.version),
        );
        await this.saveQueue(queue, rest);
        await this.keepLocal(item.entity, updated, rest);
      } else {
        const res = await api.DELETE(PI(item.entity), { params: { path: { id: item.id } }, headers });
        // Already gone elsewhere is as good as deleted.
        if (!res.response.ok && res.response.status !== 404) await ok(Promise.resolve(res));
        await removeOutbox(item.seq as number);
      }
      this.notifyChanged();
    } catch (e) {
      if (isNetworkError(e)) throw e;
      if (!(e instanceof ApiFailure) || e.status >= 500 || e.status === 429) throw e;
      // Signed out, or the session's form token is stale: nothing wrong with
      // the change itself. It waits for the next sign-in.
      if (e.status === 401 || e.code === "csrf_failed") throw e;
      if (e.status === 412) {
        // Someone else changed it first: keep both versions and ask.
        let server: RecordJson | undefined;
        try {
          server = (await ok(api.GET(PI(item.entity), { params: { path: { id: item.id } } }))) as unknown as RecordJson;
        } catch {
          server = undefined;
        }
        await putOutbox({ ...item, state: "conflict", server, error: e.message });
      } else {
        await putOutbox({ ...item, state: "failed", error: e.message });
      }
    } finally {
      await this.refreshCounts();
    }
  }

  /**
   * Stores the server's copy of a record, with the changes still queued for
   * it applied on top, so the device keeps showing them until they are sent.
   */
  private async keepLocal(entity: StoredEntity, server: RecordJson, queue: OutboxItem[]) {
    let r = server;
    for (const q of queue) {
      if (q.entity !== entity || q.id !== server.id || q.op === "delete") continue;
      r = (WORK_ENTITIES as string[]).includes(entity)
        ? await dress(entity as WorkEntity, applyChange(r, q.op, q.body), r, await this.dressContext())
        : applyChange(r, q.op, q.body);
    }
    await putRecords(this.ws, entity, [r]);
  }

  /** Points every record on the device that links to `from` at `to`. */
  private async rewriteLocalLinks(from: string, to: string) {
    for (const entity of [...OFFLINE_ENTITIES, ...WORK_ENTITIES]) {
      const linked = (await allRecords(this.ws, entity)).filter((r) => JSON.stringify(r).includes(from));
      if (linked.length) await putRecords(this.ws, entity, linked.map((r) => replaceId(r, from, to)));
    }
  }

  /** The waiting changes, for the sync page. */
  queued(): Promise<OutboxItem[]> {
    return outbox(this.ws);
  }

  /** Settles a conflict or refusal: send mine over theirs, or drop mine. */
  async resolve(seq: number, choice: "mine" | "theirs"): Promise<void> {
    const item = (await outbox(this.ws)).find((q) => q.seq === seq);
    if (!item) return;
    if (choice === "mine" && item.state === "conflict" && item.server) {
      await putOutbox({ ...item, state: "pending", version: Number(item.server.version), server: undefined, error: undefined });
      const mine = applyChange(item.server, item.op, item.body);
      await putRecords(this.ws, item.entity, [
        (WORK_ENTITIES as string[]).includes(item.entity)
          ? await dress(item.entity as WorkEntity, mine, item.server, await this.dressContext())
          : mine,
      ]);
    } else {
      await removeOutbox(seq);
      if (item.server) await putRecords(this.ws, item.entity, [item.server]);
      else if (isLocalId(item.id)) await deleteRecord(this.ws, item.entity, item.id);
      else if (navigator.onLine) await this.refetch(item.entity, item.id).catch(() => undefined);
    }
    await this.refreshCounts();
    this.notifyChanged();
    void this.syncNow();
  }
}
