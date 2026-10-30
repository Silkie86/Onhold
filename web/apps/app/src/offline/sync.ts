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
// - When the person keeps recent email on the device, the feed also names
//   the conversations with news, and those are read again (see `mail.ts`).
//   Replies written offline wait in the same queue.

import type { RecordJson } from "@onhold/ui";
import { ApiFailure, RECORD_PATHS, api, ok } from "../api.ts";
import {
  type EntityName,
  OFFLINE_ENTITIES,
  ORPHANED,
  ORPHANED_MAYBE_THERE,
  type OutboxItem,
  type OutboxOp,
  type QueuedEntity,
  type StoredEntity,
  WORK_ENTITIES,
  type WorkEntity,
  allRecords,
  applyChange,
  changeOutbox,
  deleteRecord,
  enqueue,
  failOrphans,
  getMeta,
  getRecord,
  isLocalId,
  localLinks,
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
import { MailCopy } from "./mail.ts";
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

/** A list as downloaded; not `complete` when it kept moving and records may be missing. */
interface Downloaded {
  items: RecordJson[];
  complete: boolean;
}

const TOPICS =
  "contact.*,organisation.*,talent.*,custom_field.*,enquiry.*,booking.*,task.*,availability.*,pipeline.*";
/** Also read when the device keeps email: which conversations had news. */
const MAIL_TOPICS = `${TOPICS},thread.*,message.*`;
const PAGE = 500;
const EVERY_MS = 20_000;
/**
 * What a full download includes. A device whose copy was made with less
 * (before bookings were kept offline) downloads everything again once.
 */
const DATA_VERSION = 2;
/** How many times a list that keeps moving under a full download is read again. */
const DOWNLOAD_TRIES = 3;
/** When the server is still on a change (or gave up waiting), it is tried again after this, doubling each time… */
const RETRY_MS = 30_000;
/** …up to this. */
const RETRY_MAX_MS = 15 * 60_000;
/** Marked-off time is kept from this many days back… */
const AVAILABILITY_BACK_DAYS = 60;
/** …to this many ahead (the calendar API gives at most 400 at a time)… */
const AVAILABILITY_AHEAD_DAYS = 339;
/** …and the window is moved on once a day. */
const AVAILABILITY_STALE_MS = 86_400_000;
/** A window the server says holds too much at once is read in halves, down to a day. */
const AVAILABILITY_SPLIT_MIN_MS = 86_400_000;
/**
 * The server recognises an Idempotency-Key for a day. A creation or reply
 * that may have arrived, first sent longer ago than this, is not sent again
 * blindly, as it would then simply be made a second time; the person checks.
 */
export const KEY_KEPT_MS = 20 * 3_600_000;
export const KEY_EXPIRED =
  "This may have reached OnHold when the connection dropped, too long ago for OnHold to recognise it now. Check it is not there already before trying again.";
/** More records changed than this since the last run (an import, say), and everything is downloaded again instead. */
const REFETCH_MAX = 250;
/** How many changed records are read at once. */
const REFETCH_AT_ONCE = 6;

/** What the feed said changed besides records, not read again yet. */
interface PullPending {
  fields: boolean;
  availability: boolean;
  stages: boolean;
}

/** Engines running on this page, so signing out can stop them all. */
const engines = new Set<OfflineSync>();

/** Stops every sync on this page, before the device's copy is deleted. */
export function stopEverySync(): void {
  for (const engine of engines) engine.stop();
}

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
  /** Aborted by `stop()`: a run in progress ends, and its requests with it. */
  private readonly stopping = new AbortController();
  /** The queued change on its way to the server, which new changes must not fold into. */
  private sending: number | undefined;
  /** Ids that records created offline were given by the server. */
  readonly renamed = new Map<string, string>();
  /** Recent email kept on the device, when the person asks for it. */
  readonly mail: MailCopy;
  /** Conversations the feed says had news, to read again when email is kept. */
  private readonly mailTouched = new Set<string>();

  constructor(readonly ws: string) {
    this.mail = new MailCopy(ws, () => this.req);
  }

  /**
   * Every request names this engine's workspace, whatever the app has
   * switched to since, and stops when the engine does.
   */
  private get req() {
    return { headers: { "X-Workspace-Id": this.ws }, signal: this.stopping.signal };
  }

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
    if (this.stopping.signal.aborted) return;
    engines.add(this);
    const lastSync = (await getMeta<string>(`${this.ws}:lastSync`)) ?? null;
    this.set({ lastSync, ready: lastSync !== null });
    await this.refreshCounts();
    // Stopped while reading those: nothing is left listening.
    if (this.stopping.signal.aborted) return;
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
    this.stopping.abort();
    engines.delete(this);
  }

  /** Sends waiting changes, then fetches what changed. One run at a time. */
  syncNow(): Promise<void> {
    if (this.stopping.signal.aborted) return Promise.resolve();
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
      await this.refreshMail();
    } catch (e) {
      if (this.stopping.signal.aborted) return;
      if (isNetworkError(e)) this.set({ online: false });
      else console.warn("sync failed", e);
    } finally {
      await this.refreshCounts();
      this.set({ syncing: false });
    }
  }

  /** Brings the email kept on the device up to date; a failure here leaves the rest alone. */
  private async refreshMail() {
    const before = [...this.mailTouched];
    const touched = new Set(before);
    try {
      if (await this.mail.refresh(touched)) this.notifyChanged();
    } catch (e) {
      if (isNetworkError(e) || this.stopping.signal.aborted) throw e;
      console.warn("email sync failed", e);
    } finally {
      // What was dealt with; the rest is read on the next run.
      for (const id of before) if (!touched.has(id)) this.mailTouched.delete(id);
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
    const page = await ok(api.GET("/api/v1/events", { ...this.req, params: { query: { limit: 1, topics: TOPICS } } }));
    return page.latest;
  }

  async downloadAll(): Promise<void> {
    // The position first: anything that changes during the download is read again from the feed.
    const latest = await this.latestPosition();
    const keep = new Set((await outbox(this.ws)).map((q) => q.id));
    for (const entity of OFFLINE_ENTITIES) {
      const list = await this.readAll(PAGE, async (offset, limit) => {
        const page = (await ok(
          api.GET(P(entity), { ...this.req, params: { query: { offset, limit, sort: "name" } } }),
        )) as unknown as { items: RecordJson[] };
        return page.items;
      });
      await this.keepList(entity, list, keep);
      await this.downloadSchema(entity);
    }
    for (const entity of ["enquiry", "booking", "task"] as const) {
      let list: Downloaded = { items: [], complete: true };
      try {
        list = await this.downloadWork(entity);
      } catch (e) {
        if (!forbidden(e)) throw e;
      }
      await this.keepList(entity, list, keep);
    }
    for (const entity of ["enquiry", "booking"] as const) await this.downloadSchema(entity);
    await this.downloadAvailability();
    await this.downloadStages();
    await this.downloadExtras();
    await setMeta(`${this.ws}:cursor`, latest);
    await setMeta(`${this.ws}:pullPending`, undefined);
    await setMeta(`${this.ws}:dataVersion`, DATA_VERSION);
    this.notifyChanged();
  }

  /**
   * Stores a downloaded list in place of the device's. Records waiting to be
   * sent keep their offline edits. If the list kept moving while it was read,
   * nothing is removed: a record not seen may simply have been skipped.
   */
  private async keepList(entity: StoredEntity, list: Downloaded, keep: Set<string>) {
    const items = list.items.filter((r) => !keep.has(r.id));
    if (list.complete) await replaceAll(this.ws, entity, items, keep);
    else await putRecords(this.ws, entity, items);
  }

  /**
   * Every record of a list the API pages by offset. A record deleted, added
   * or renamed ahead of the page being read moves the rest along, so one
   * could be skipped without a trace in the feed. Each page after the first
   * therefore starts one back, on the last record already read; if that is
   * not where it was, the list is read again from the start.
   */
  private async readAll(size: number, read: (offset: number, limit: number) => Promise<RecordJson[]>): Promise<Downloaded> {
    let all: RecordJson[] = [];
    for (let attempt = 0; attempt < DOWNLOAD_TRIES; attempt++) {
      all = await read(0, size);
      let full = all.length === size;
      let moved = false;
      while (full && !moved) {
        this.stopping.signal.throwIfAborted();
        const page = await read(all.length - 1, size);
        moved = page[0]?.id !== all[all.length - 1]?.id;
        if (!moved) all.push(...page.slice(1));
        full = page.length === size;
      }
      if (!moved) return { items: all, complete: true };
    }
    return { items: all, complete: false };
  }

  private downloadWork(entity: "enquiry" | "booking" | "task"): Promise<Downloaded> {
    // Tasks come at most 200 a page; the others take a sort that keeps paging stable.
    const size = entity === "task" ? 200 : PAGE;
    return this.readAll(size, async (offset, limit) => {
      const query = entity === "task" ? { status: "all", offset, limit } : { offset, limit, sort: "created_at" };
      const page = (await ok(api.GET(P(entity), { ...this.req, params: { query: query as never } }))) as unknown as {
        items: RecordJson[];
      };
      return page.items;
    });
  }

  /** The talent's marked-off time in the window the device keeps. */
  private async downloadAvailability() {
    const now = Date.now();
    let items: RecordJson[] = [];
    try {
      const all = await this.readAvailability(now - AVAILABILITY_BACK_DAYS * 86_400_000, now + AVAILABILITY_AHEAD_DAYS * 86_400_000);
      // One marked off across the line between two windows comes in both.
      items = [...new Map(all.map((a) => [a.id, a])).values()];
    } catch (e) {
      if (!forbidden(e)) throw e;
    }
    await replaceAll(this.ws, "availability", items, new Set());
    await setMeta(`${this.ws}:availabilityAt`, now);
  }

  /** Marked-off time between two moments; in halves when there is more than the server gives at once. */
  private async readAvailability(from: number, to: number): Promise<RecordJson[]> {
    try {
      const query = { from: new Date(from).toISOString(), to: new Date(to).toISOString() };
      const cal = await ok(api.GET("/api/v1/calendar", { ...this.req, params: { query } }));
      // Busy times read from talent's own calendars stay on the server: they
      // change on every read, and offline clash checks go without them.
      return cal.availability.filter((a) => a.source !== "external") as unknown as RecordJson[];
    } catch (e) {
      if (!(e instanceof ApiFailure && e.status === 400) || to - from <= AVAILABILITY_SPLIT_MIN_MS) throw e;
      const mid = from + Math.floor((to - from) / 2);
      return [...(await this.readAvailability(from, mid)), ...(await this.readAvailability(mid, to))];
    }
  }

  private async downloadStages() {
    try {
      const list = await ok(api.GET("/api/v1/pipelines", { ...this.req, params: { query: { entity: "enquiry" } } }));
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
      ["members", () => ok(api.GET("/api/v1/members", this.req))],
      ["workspace", () => ok(api.GET("/api/v1/workspace", this.req))],
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
      const schema = await ok(api.GET("/api/v1/schema/{entity}", { ...this.req, params: { path: { entity } } }));
      await setMeta(`${this.ws}:schema:${entity}`, schema);
    } catch (e) {
      if (!forbidden(e)) throw e;
    }
  }

  /**
   * Reads the feed a page at a time, fetching what each page names and then
   * noting the position, so a run stopped part way carries on from there.
   * Field, availability and stage changes are read once, after the last
   * page; until then they are noted on the device, so a run stopped part way
   * still reads them next time. When more records changed than is worth
   * fetching one by one (an import, or undoing one), everything is
   * downloaded again instead.
   */
  private async pullChanges(cursor: number): Promise<void> {
    let after = cursor;
    const topics = (await this.mail.enabled()) ? MAIL_TOPICS : TOPICS;
    const waiting = new Set((await outbox(this.ws)).map((q) => q.id));
    const pendingKey = `${this.ws}:pullPending`;
    const pending: PullPending = { fields: false, availability: false, stages: false, ...(await getMeta<PullPending>(pendingKey)) };
    // Before anything is downloaded below, which would make it look fresh.
    const availabilityAt = (await getMeta<number>(`${this.ws}:availabilityAt`)) ?? 0;
    const availabilityStale = Date.now() - availabilityAt > AVAILABILITY_STALE_MS;
    /** Records fetched this run, with the feed's newest position before they were read: what they already show. */
    const fetchedAt = new Map<string, number>();
    let changed = false;
    try {
      for (;;) {
        let page;
        try {
          page = await ok(api.GET("/api/v1/events", { ...this.req, params: { query: { after, topics, limit: PAGE } } }));
        } catch (e) {
          if (e instanceof ApiFailure && e.status === 410) {
            await this.downloadAll();
            return;
          }
          throw e;
        }
        const touched = new Map<string, EntityName | "enquiry" | "booking" | "task">();
        const flagged = { ...pending };
        for (const ev of page.events) {
          const entity = ev.topic.split(".")[0] ?? "";
          if ((entity === "thread" || entity === "message") && ev.object_id) this.mailTouched.add(ev.object_id);
          else if (entity === "custom_field") pending.fields = true;
          else if (entity === "availability") pending.availability = true;
          else if (entity === "pipeline") pending.stages = true;
          else if (ev.object_id && [...OFFLINE_ENTITIES, "enquiry", "booking", "task"].includes(entity)) {
            // A record with changes still to send keeps them; a conflict will show if needed.
            if (waiting.has(ev.object_id)) continue;
            // Already read this run, after this change.
            if (ev.seq <= (fetchedAt.get(ev.object_id) ?? -1)) continue;
            touched.set(ev.object_id, entity as EntityName);
          }
        }
        if (fetchedAt.size + [...touched.keys()].filter((id) => !fetchedAt.has(id)).length > REFETCH_MAX) {
          await this.downloadAll();
          return;
        }
        const ids = [...touched];
        for (let i = 0; i < ids.length; i += REFETCH_AT_ONCE) {
          await Promise.all(ids.slice(i, i + REFETCH_AT_ONCE).map(([id, entity]) => this.refetch(entity, id)));
        }
        for (const id of touched.keys()) fetchedAt.set(id, page.latest);
        changed ||= touched.size > 0;
        after = page.next_after;
        if (pending.fields !== flagged.fields || pending.availability !== flagged.availability || pending.stages !== flagged.stages) {
          await setMeta(pendingKey, pending);
        }
        await setMeta(`${this.ws}:cursor`, after);
        if (!page.has_more) break;
      }
      if (pending.fields) {
        for (const entity of [...OFFLINE_ENTITIES, "enquiry", "booking"] as const) await this.downloadSchema(entity);
      }
      if (pending.availability || availabilityStale) await this.downloadAvailability();
      if (pending.stages) await this.downloadStages();
      if (availabilityStale) await this.downloadExtras();
      if (pending.fields || pending.availability || pending.stages) {
        changed = true;
        await setMeta(pendingKey, undefined);
      }
    } finally {
      if (changed) this.notifyChanged();
    }
  }

  private async refetch(entity: StoredEntity, id: string) {
    try {
      const r = (await ok(api.GET(PI(entity), { ...this.req, params: { path: { id } } }))) as unknown as RecordJson;
      await putRecords(this.ws, entity, [r]);
    } catch (e) {
      if (e instanceof ApiFailure && (e.status === 404 || e.status === 403)) await deleteRecord(this.ws, entity, id);
      else throw e;
    }
  }

  // Changes made on this device -----------------------------------------------------------

  /**
   * Changes the queue as it is now, in one transaction, so a change queued
   * while another is being sent is never lost. Returns the new queue.
   */
  private async saveQueue(change: (queue: OutboxItem[]) => OutboxItem[]): Promise<OutboxItem[]> {
    const queue = await changeOutbox(this.ws, change);
    await this.refreshCounts();
    return queue;
  }

  /** The id the server gave a record made offline, once its creation has reached the server. */
  private async realId(id: string): Promise<string | undefined> {
    if (!isLocalId(id)) return undefined;
    const real = this.renamed.get(id) ?? (await getMeta<string>(`${this.ws}:renamed:${id}`));
    if (real) this.renamed.set(id, real);
    return real;
  }

  /** Queues a change and applies it to the device's copy straight away. Returns the record as it now reads. */
  async queue(
    entity: StoredEntity,
    op: OutboxOp,
    id: string | null,
    body: Record<string, unknown>,
    version?: number,
    key: string = crypto.randomUUID(),
    /** It was sent once already and may have arrived, so it must be sent again exactly as it is. */
    sent = false,
  ): Promise<RecordJson | null> {
    // A page may still be showing a record made offline that has just reached
    // the server (sent here, or from another window): the change, and what it
    // links to, go to its real id.
    const recordId = id ? ((await this.realId(id)) ?? id) : newLocalId();
    for (const local of localLinks(body)) {
      const real = await this.realId(local);
      if (real) body = replaceId(body, local, real);
    }
    const current = id ? await getRecord(this.ws, entity, recordId) : undefined;
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
      ...(sent && { tries: 0, sentAt: new Date().toISOString() }),
    };
    await this.saveQueue((queue) => {
      const next = enqueue(queue, item, this.sending);
      // Deleting a record made here and never sent: what links to it can never go.
      return op === "delete" && isLocalId(recordId) ? failOrphans(next) : next;
    });

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

  /**
   * Queues a reply to a conversation, to be sent when the device is back
   * online. `sent` when it was sent once already and may have arrived: it
   * then goes again with the same key, which the server recognises.
   */
  async queueReply(threadId: string, body: { body: string; to?: string[] }, label: string, key: string, sent = false): Promise<void> {
    const item: OutboxItem = {
      ws: this.ws,
      entity: "reply",
      op: "create",
      id: threadId,
      body,
      label,
      key,
      createdAt: new Date().toISOString(),
      state: "pending",
      ...(sent && { tries: 0, sentAt: new Date().toISOString() }),
    };
    await this.saveQueue((queue) => enqueue(queue, item, this.sending));
    void this.syncNow();
  }

  /** True while changes to this record are waiting to be sent. */
  async hasQueued(id: string): Promise<boolean> {
    return (await outbox(this.ws)).some((q) => q.id === id);
  }

  private async flush(): Promise<void> {
    const tried = new Set<number>();
    for (;;) {
      this.stopping.signal.throwIfAborted();
      const queue = await outbox(this.ws);
      const now = Date.now();
      // A record whose earlier change needs attention, or is waiting to be
      // tried again, keeps its later ones waiting too, so they still apply in order.
      const held = (q: OutboxItem) => q.state !== "pending" || (q.retryAt !== undefined && Date.parse(q.retryAt) > now);
      const stuck = new Set(queue.filter(held).map((q) => q.id));
      const next = queue.find((q) => !stuck.has(q.id) && !tried.has(q.seq ?? -1) && !waitsForLocal(q));
      if (!next) return;
      tried.add(next.seq ?? -1);
      // From here a change to the same record queues behind this one instead of folding into it.
      this.sending = next.seq;
      try {
        // Read again, with anything folded into it before that, and marked as
        // on its way in the queue itself, so another window of the app does
        // not fold its changes into it either.
        const at = new Date().toISOString();
        let item: OutboxItem | undefined;
        await this.saveQueue((queue) =>
          queue.map((q) => {
            if (q.seq !== next.seq || q.state !== "pending") return q;
            if (q.op === "create" && q.sentAt && Date.parse(at) - Date.parse(q.sentAt) > KEY_KEPT_MS) {
              return { ...q, state: "failed" as const, error: KEY_EXPIRED };
            }
            item = q;
            return { ...q, tries: q.tries ?? 0, sentAt: q.sentAt ?? at };
          }),
        );
        if (item) await this.send(item);
      } finally {
        this.sending = undefined;
      }
    }
  }

  private async send(item: OutboxItem) {
    const headers: Record<string, string> = { ...this.req.headers, "Idempotency-Key": item.key };
    if (item.version !== undefined && item.op !== "create") headers["If-Match"] = `"${item.version}"`;
    const { signal } = this.req;
    try {
      // What was queued meanwhile is in the queue as it is now, not as it was when this was sent.
      if (item.entity === "reply") {
        await ok(
          api.POST("/api/v1/inbox/threads/{id}/reply", {
            params: { path: { id: item.id } },
            headers,
            signal,
            body: item.body as never,
          }),
        );
        await removeOutbox(item.seq as number);
        // Read again with the feed's news, once the rest of the queue has gone.
        this.mailTouched.add(item.id);
      } else if (item.op === "create") {
        const created = (await ok(
          api.POST(P(item.entity), { headers, signal, body: item.body as never }),
        )) as unknown as RecordJson;
        await deleteRecord(this.ws, item.entity, item.id);
        this.renamed.set(item.id, created.id);
        await setMeta(`${this.ws}:renamed:${item.id}`, created.id);
        const rest = await this.saveQueue((queue) =>
          rebase(
            rewriteIds(
              queue.filter((q) => q.seq !== item.seq),
              item.id,
              created.id,
            ),
            item.entity,
            created.id,
            undefined,
            Number(created.version),
          ),
        );
        await this.rewriteLocalLinks(item.id, created.id);
        await this.keepLocal(item.entity, created, rest);
      } else if (item.op === "update" || item.op === "status") {
        const path = { params: { path: { id: item.id } }, headers, signal, body: item.body as never };
        const updated = (
          item.op === "status"
            ? await ok(api.POST("/api/v1/bookings/{id}/status", path))
            : await ok(api.PATCH(PI(item.entity), path))
        ) as unknown as RecordJson;
        const rest = await this.saveQueue((queue) =>
          rebase(
            queue.filter((q) => q.seq !== item.seq),
            item.entity,
            item.id,
            item.version,
            Number(updated.version),
          ),
        );
        await this.keepLocal(item.entity, updated, rest);
      } else {
        // A record that never reached the server (its creation was discarded) has nothing to delete there.
        if (!isLocalId(item.id)) {
          const res = await api.DELETE(PI(item.entity), { params: { path: { id: item.id } }, headers, signal });
          // Already gone elsewhere is as good as deleted.
          if (!res.response.ok && res.response.status !== 404) await ok(Promise.resolve(res));
        }
        await removeOutbox(item.seq as number);
      }
      this.notifyChanged();
    } catch (e) {
      if (isNetworkError(e) || (e instanceof ApiFailure && e.status >= 500)) {
        // It may have arrived (the connection dropped, or the engine stopped,
        // after the server took it), so its key may now belong to this body:
        // later changes queue behind it instead of folding into it.
        await changeOutbox(this.ws, (queue) =>
          queue.map((q) => (q.seq === item.seq ? { ...q, tries: q.tries ?? 0 } : q)),
        );
        throw e;
      }
      if (!(e instanceof ApiFailure)) throw e;
      // Too many requests; or signed out, or the session's form token is
      // stale: nothing wrong with the change itself, and it did not arrive.
      // It waits, as it was, for the next run or sign-in.
      if (e.status === 429 || e.status === 401 || e.code === "csrf_failed") {
        await changeOutbox(this.ws, (queue) => queue.map((q) => (q.seq === item.seq ? item : q)));
        throw e;
      }
      if (e.status === 408 || (e.status === 409 && e.code === "request_in_progress")) {
        // The server gave up waiting, or is still working on it under this
        // key: nothing wrong with the change. It is tried again later, less
        // often each time, and the rest of the queue carries on meanwhile.
        const tries = (item.tries ?? 0) + 1;
        const wait = Math.min(RETRY_MS * 2 ** (tries - 1), RETRY_MAX_MS);
        await putOutbox({
          ...item,
          tries,
          sentAt: item.sentAt ?? new Date().toISOString(),
          retryAt: new Date(Date.now() + wait).toISOString(),
          error: e.message,
        });
      } else if (e.status === 412 && item.entity !== "reply") {
        const entity = item.entity;
        // Someone else changed it first: keep both versions and ask.
        let server: RecordJson | undefined;
        try {
          server = (await ok(
            api.GET(PI(entity), { ...this.req, params: { path: { id: item.id } } }),
          )) as unknown as RecordJson;
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

  /** Settles a conflict or refusal: send mine over theirs, or drop mine (see also `retry`). */
  async resolve(seq: number, choice: "mine" | "theirs"): Promise<void> {
    const item = (await outbox(this.ws)).find((q) => q.seq === seq);
    if (!item) return;
    if (item.entity === "reply") {
      // A reply has no copy to go back to: dropping it is all there is.
      if (choice === "theirs") await removeOutbox(seq);
    } else if (choice === "mine") {
      // Only a conflict has a "mine" to keep; nothing else is ever dropped for it.
      if (item.state !== "conflict") return;
      const entity = item.entity;
      // The server's copy could not be read when the conflict was found: read it now.
      const server =
        item.server ??
        (navigator.onLine
          ? ((await ok(api.GET(PI(entity), { ...this.req, params: { path: { id: item.id } } })).catch(
              () => undefined,
            )) as unknown as RecordJson | undefined)
          : undefined);
      if (server) {
        await putOutbox({ ...item, state: "pending", version: Number(server.version), server: undefined, error: undefined });
        const mine = applyChange(server, item.op, item.body);
        await putRecords(this.ws, entity, [
          (WORK_ENTITIES as string[]).includes(entity) ? await dress(entity as WorkEntity, mine, server, await this.dressContext()) : mine,
        ]);
      } else {
        // Still not to be had: it is sent again, and if it is still refused the conflict comes back, with their copy.
        await putOutbox({ ...item, state: "pending", error: undefined });
      }
    } else {
      // Dropping the creation of a record made here leaves what links to it
      // nothing to link to; if it may be on the server already, they say so.
      const error = item.error === KEY_EXPIRED ? ORPHANED_MAYBE_THERE : ORPHANED;
      await this.saveQueue((queue) => failOrphans(queue.filter((q) => q.seq !== seq), error));
      if (item.server) await putRecords(this.ws, item.entity, [item.server]);
      else if (isLocalId(item.id)) await deleteRecord(this.ws, item.entity, item.id);
      else if (navigator.onLine) await this.refetch(item.entity, item.id).catch(() => undefined);
    }
    await this.refreshCounts();
    this.notifyChanged();
    void this.syncNow();
  }

  /** Sends a refused change again as it is, now. */
  async retry(seq: number): Promise<void> {
    const item = (await outbox(this.ws)).find((q) => q.seq === seq);
    if (!item || item.state !== "failed") return;
    // Asked for, so it goes even if it may have arrived long ago; but not if
    // what it links to is gone, as it would only wait for ever.
    await this.saveQueue((queue) =>
      failOrphans(
        queue.map((q) => (q.seq === seq ? { ...q, state: "pending", error: undefined, retryAt: undefined, sentAt: undefined } : q)),
      ),
    );
    await this.syncNow();
  }
}
