// How pages read and change records. In the browser this is the API,
// as before. In the desktop and mobile apps the API is tried first and the
// device's copy answers when there is no connection; changes made offline
// are queued and sent later (see `offline/sync.ts`).

import type { RecordJson } from "@onhold/ui";
import { createContext, useContext, useEffect, useState } from "react";
import { ApiFailure, type EntitySchema, ENTITIES, RECORD_PATHS, api, ok } from "./api.ts";
import type { Schemas } from "@onhold/api-client";
import { type ListParams, type Page, localList, localTags, matches, sortRecords } from "./offline/query.ts";
import {
  type BookingParams,
  type EnquiryParams,
  type Stage,
  type TaskParams,
  clashMessage,
  localBookings,
  localCalendar,
  localClashes,
  localEnquiries,
  localTasks,
} from "./offline/work.ts";
import {
  type EntityName,
  type OutboxOp,
  type StoredEntity,
  allRecords,
  deleteRecord as forgetRecord,
  getMeta,
  getRecord,
  isLocalId,
  linksToLocal,
  putRecords,
  setMeta,
} from "./offline/store.ts";
import type { KeptDetail, ThreadParams } from "./offline/mail.ts";
import { type OfflineSync, type SyncStatus, isNetworkError } from "./offline/sync.ts";

export const OfflineContext = createContext<OfflineSync | null>(null);

/** The app's sync engine; null in the browser. */
export function useOffline(): OfflineSync | null {
  return useContext(OfflineContext);
}

export function useSyncStatus(): SyncStatus | null {
  const sync = useOffline();
  const [status, setStatus] = useState(sync?.current ?? null);
  useEffect(() => {
    if (!sync) return;
    setStatus(sync.current);
    return sync.subscribe(setStatus);
  }, [sync]);
  return status;
}

/** Data, and whether it came from the device rather than the server. */
export interface Fetched<T> {
  data: T;
  offline: boolean;
}

/** Thrown when the app is offline and the device has no copy to show. */
export class NotOnDevice extends Error {
  constructor() {
    super("You are offline, and this is not saved on this device yet.");
  }
}

const TIMEOUT_MS = 10_000;

function withTimeout<T>(p: Promise<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new DOMException("timed out", "TimeoutError")), TIMEOUT_MS);
    p.then(
      (v) => {
        clearTimeout(t);
        resolve(v);
      },
      (e: unknown) => {
        clearTimeout(t);
        reject(e);
      },
    );
  });
}

/** The server's answer, or the device's when the server cannot be reached. */
async function serverOrDevice<T>(
  sync: OfflineSync | null,
  server: () => Promise<T>,
  device: () => Promise<T | undefined>,
  remember?: (data: T) => Promise<void>,
): Promise<Fetched<T>> {
  if (!sync) return { data: await server(), offline: false };
  if (navigator.onLine) {
    try {
      const data = await withTimeout(server());
      if (remember) await remember(data).catch(() => undefined);
      return { data, offline: false };
    } catch (e) {
      if (!isNetworkError(e)) throw e;
    }
  }
  const data = await device();
  if (data === undefined) throw new NotOnDevice();
  return { data, offline: true };
}

const P = (entity: StoredEntity) => `/api/v1/${RECORD_PATHS[entity]}` as "/api/v1/contacts";
const PI = (entity: StoredEntity) => `/api/v1/${RECORD_PATHS[entity]}/{id}` as "/api/v1/contacts/{id}";

export function fetchSchema(sync: OfflineSync | null, entity: string): Promise<Fetched<EntitySchema>> {
  return serverOrDevice(
    sync,
    () => ok(api.GET("/api/v1/schema/{entity}", { params: { path: { entity } } })),
    () => getMeta<EntitySchema>(`${sync?.ws}:schema:${entity}`),
    (schema) => setMeta(`${sync?.ws}:schema:${entity}`, schema),
  );
}

export function fetchList(sync: OfflineSync | null, entity: EntityName, params: ListParams): Promise<Fetched<Page>> {
  return serverOrDevice(
    sync,
    async () =>
      (await ok(
        api.GET(P(entity), {
          params: {
            query: {
              q: params.q || undefined,
              sort: params.sort ?? undefined,
              tag: params.tag ?? undefined,
              organisation_id: params.organisation_id,
              offset: params.offset,
              limit: params.limit,
            },
          },
        }),
      )) as unknown as Page,
    async () => (sync?.current.ready ? localList(await allRecords(sync.ws, entity), params) : undefined),
    // What the server just showed is the freshest copy there is.
    async (page) => {
      if (sync) await putRecords(sync.ws, entity, page.items);
    },
  );
}

export async function fetchRecord(sync: OfflineSync | null, entity: StoredEntity, id: string): Promise<Fetched<RecordJson>> {
  if (sync && (isLocalId(id) || (await sync.hasQueued(id)))) {
    // Changes are still waiting to be sent: the device's copy is the one to show.
    const local = await getRecord(sync.ws, entity, id);
    if (local) return { data: local, offline: !navigator.onLine };
  }
  return serverOrDevice(
    sync,
    async () => (await ok(api.GET(PI(entity), { params: { path: { id } } }))) as unknown as RecordJson,
    () => (sync ? getRecord(sync.ws, entity, id) : Promise.resolve(undefined)),
    async (r) => {
      if (sync) await putRecords(sync.ws, entity, [r]);
    },
  );
}

export function fetchTags(sync: OfflineSync | null, entity: EntityName) {
  return serverOrDevice(
    sync,
    () => ok(api.GET("/api/v1/tags", { params: { query: { entity } } })),
    async () => (sync?.current.ready ? localTags(await allRecords(sync.ws, entity)) : undefined),
  );
}

export interface Hit {
  id: string;
  entity: EntityName;
  name: string;
  detail?: string | null;
}

export async function searchEverything(sync: OfflineSync | null, q: string): Promise<Hit[]> {
  const found = await serverOrDevice(
    sync,
    async () =>
      (await ok(api.GET("/api/v1/search", { params: { query: { q, limit: 12 } } }))).map((h) => ({
        id: h.id,
        entity: h.entity as EntityName,
        name: h.name,
        detail: h.detail,
      })),
    async () => {
      if (!sync?.current.ready) return undefined;
      const hits: Hit[] = [];
      for (const entity of ["contact", "organisation", "talent"] as EntityName[]) {
        const list = sortRecords((await allRecords(sync.ws, entity)).filter((r) => matches(r, q)), null, q).slice(0, 4);
        for (const r of list) {
          const detail = (r.organisation_name ?? r.email ?? r.talent_type ?? r.org_type ?? null) as string | null;
          hits.push({ id: r.id, entity, name: String(r.name ?? ""), detail });
        }
      }
      return hits;
    },
  );
  return found.data;
}

export function fetchWorkspace(sync: OfflineSync | null): Promise<Fetched<Schemas["WorkspaceView"]>> {
  return serverOrDevice(
    sync,
    () => ok(api.GET("/api/v1/workspace")),
    () => getMeta<Schemas["WorkspaceView"]>(`${sync?.ws}:workspace`),
    (w) => setMeta(`${sync?.ws}:workspace`, w),
  );
}

// Email ------------------------------------------------------------------------------------

/** Conversations: from the server, or offline from the recent email the device keeps, if it keeps any. */
export function fetchThreads(sync: OfflineSync | null, params: ThreadParams): Promise<Fetched<Schemas["ThreadPage"]>> {
  return serverOrDevice(
    sync,
    () => ok(api.GET("/api/v1/inbox/threads", { params: { query: params } })),
    async () => sync?.mail.list(params),
  );
}

export function fetchThread(sync: OfflineSync | null, id: string): Promise<Fetched<KeptDetail>> {
  return serverOrDevice<KeptDetail>(
    sync,
    () => ok(api.GET("/api/v1/inbox/threads/{id}", { params: { path: { id } } })),
    async () => sync?.mail.read(id),
    async (d) => {
      if (sync) await sync.mail.remember(d);
    },
  );
}

/**
 * Sends a reply, or with no connection queues it on the device to go when
 * there is one. A reply to a conversation that already has one waiting
 * queues behind it, so they arrive in order.
 */
export async function sendReply(sync: OfflineSync | null, threadId: string, body: string, label: string): Promise<{ queued: boolean }> {
  const send = (headers: Record<string, string>) =>
    ok(api.POST("/api/v1/inbox/threads/{id}/reply", { params: { path: { id: threadId } }, headers, body: { body } }));
  if (!sync) {
    await send({});
    return { queued: false };
  }
  const key = crypto.randomUUID();
  let sent = false;
  if (navigator.onLine && !(await sync.hasQueued(threadId))) {
    sent = true;
    try {
      await withTimeout(send({ "Idempotency-Key": key }));
      return { queued: false };
    } catch (e) {
      if (!isNetworkError(e)) throw e;
      // It may have arrived: it goes again with the same key, which the server recognises.
    }
  }
  await sync.queueReply(threadId, { body }, label, key, sent);
  return { queued: true };
}

// Changes -----------------------------------------------------------------------------------

/** The outcome of a change: the record as it now reads, and whether it is waiting to be sent. */
export interface Saved {
  record: RecordJson | null;
  queued: boolean;
}

/** True when a change to this record would wait on the device rather than go straight to the server. */
async function willQueue(sync: OfflineSync | null, id: string | null): Promise<boolean> {
  if (!sync) return false;
  return !navigator.onLine || (id !== null && (isLocalId(id) || (await sync.hasQueued(id))));
}

async function sendOrQueue(
  sync: OfflineSync | null,
  entity: StoredEntity,
  op: OutboxOp,
  id: string | null,
  body: Record<string, unknown>,
  version: number | undefined,
  send: (headers: Record<string, string>) => Promise<RecordJson | null>,
): Promise<Saved> {
  if (!sync) return { record: await send({}), queued: false };
  const key = crypto.randomUUID();
  let sent = false;
  // A link to a record made offline is sent once that record has its real id.
  if (!(await willQueue(sync, id)) && !linksToLocal(body)) {
    sent = true;
    try {
      const record = await withTimeout(send({ "Idempotency-Key": key }));
      if (record) await putRecords(sync.ws, entity, [record]);
      else if (id) await forgetRecord(sync.ws, entity, id);
      return { record, queued: false };
    } catch (e) {
      if (!isNetworkError(e)) throw e;
      // It may or may not have arrived: the same key makes sending it again
      // safe, as long as nothing is folded into it meanwhile.
    }
  }
  return { record: await sync.queue(entity, op, id, body, version, key, sent), queued: true };
}

export function createRecord(sync: OfflineSync | null, entity: StoredEntity, body: Record<string, unknown>): Promise<Saved> {
  return sendOrQueue(sync, entity, "create", null, body, undefined, async (headers) =>
    (await ok(api.POST(P(entity), { headers, body: body as never }))) as unknown as RecordJson,
  );
}

export function updateRecord(
  sync: OfflineSync | null,
  entity: StoredEntity,
  id: string,
  body: Record<string, unknown>,
  version: number,
): Promise<Saved> {
  return sendOrQueue(sync, entity, "update", id, body, version, async (headers) =>
    (await ok(
      api.PATCH(PI(entity), {
        params: { path: { id } },
        headers: { ...headers, "If-Match": `"${version}"` },
        body: body as never,
      }),
    )) as unknown as RecordJson,
  );
}

export function deleteRecord(sync: OfflineSync | null, entity: StoredEntity, id: string, version?: number): Promise<Saved> {
  return sendOrQueue(sync, entity, "delete", id, {}, version, async (headers) => {
    await ok(api.DELETE(PI(entity), { params: { path: { id } }, headers }));
    return null;
  });
}

/** The duplicate check before creating; skipped offline, where only the server could answer. */
export async function findDuplicates(
  sync: OfflineSync | null,
  entity: EntityName,
  query: Record<string, unknown>,
): Promise<{ id: string; name: string; reasons: string[] }[]> {
  try {
    return await ok(api.POST(`/api/v1/${ENTITIES[entity].path}/duplicates` as "/api/v1/contacts/duplicates", { body: query as never }));
  } catch (e) {
    if (sync && isNetworkError(e)) return [];
    throw e;
  }
}

// Enquiries, bookings, tasks and the calendar ---------------------------------------------------

type EnquiryPage = Schemas["EnquiryPage"];
type BookingPage = Schemas["BookingPage"];
type TaskPage = Schemas["TaskPage"];
type CalendarView = Schemas["CalendarView"];
type ClashReport = Schemas["ClashReport"];
type Booking = Schemas["BookingView"];

const ready = (sync: OfflineSync | null): sync is OfflineSync => !!sync?.current.ready;

/**
 * The server's answer, or the device's offline; only the device's for
 * records made offline and not sent yet, which the server knows nothing of.
 */
async function readWork<T>(
  sync: OfflineSync | null,
  ids: (string | null | undefined)[],
  server: () => Promise<T>,
  device: () => Promise<T | undefined>,
  remember?: (data: T) => Promise<void>,
): Promise<Fetched<T>> {
  if (sync && ids.some((id) => id && isLocalId(id))) {
    const data = await device();
    if (data !== undefined) return { data, offline: !navigator.onLine };
  }
  return serverOrDevice(sync, server, device, remember);
}
const remember = (sync: OfflineSync | null, entity: StoredEntity) => async (page: { items: unknown[] }) => {
  if (sync) await putRecords(sync.ws, entity, page.items as RecordJson[]);
};

export function fetchEnquiries(sync: OfflineSync | null, p: EnquiryParams): Promise<Fetched<EnquiryPage>> {
  return serverOrDevice(
    sync,
    () => ok(api.GET("/api/v1/enquiries", { params: { query: { q: p.q || undefined, stage: p.stage, limit: p.limit } } })),
    async () => (ready(sync) ? (localEnquiries(await allRecords(sync.ws, "enquiry"), p) as unknown as EnquiryPage) : undefined),
    remember(sync, "enquiry"),
  );
}

export function fetchBookings(sync: OfflineSync | null, p: BookingParams): Promise<Fetched<BookingPage>> {
  return readWork(
    sync,
    [p.talent_id, p.enquiry_id],
    () =>
      ok(
        api.GET("/api/v1/bookings", {
          params: {
            query: {
              q: p.q || undefined,
              status: p.status || undefined,
              from: p.from,
              to: p.to,
              talent_id: p.talent_id,
              enquiry_id: p.enquiry_id,
              sort: p.sort,
              limit: p.limit,
            },
          },
        }),
      ),
    async () => (ready(sync) ? (localBookings(await allRecords(sync.ws, "booking"), p) as unknown as BookingPage) : undefined),
    remember(sync, "booking"),
  );
}

/** Tasks; `mine` lists the signed-in person's (`userId`). */
export function fetchTasks(
  sync: OfflineSync | null,
  p: Omit<TaskParams, "assignee_id"> & { mine?: boolean },
  userId: string,
): Promise<Fetched<TaskPage>> {
  return readWork(
    sync,
    [p.related_id],
    () =>
      ok(
        api.GET("/api/v1/tasks", {
          params: { query: { status: p.status, assignee: p.mine ? "me" : undefined, related_id: p.related_id, limit: p.limit } },
        }),
      ),
    async () =>
      ready(sync)
        ? (localTasks(await allRecords(sync.ws, "task"), { ...p, assignee_id: p.mine ? userId : undefined }) as unknown as TaskPage)
        : undefined,
    remember(sync, "task"),
  );
}

export function fetchCalendar(
  sync: OfflineSync | null,
  p: { from: string; to: string; talent_id?: string },
): Promise<Fetched<CalendarView>> {
  return readWork(
    sync,
    [p.talent_id],
    () => ok(api.GET("/api/v1/calendar", { params: { query: p } })),
    async () =>
      ready(sync)
        ? (localCalendar(
            await allRecords(sync.ws, "booking"),
            await allRecords(sync.ws, "availability"),
            p,
          ) as unknown as CalendarView)
        : undefined,
    async (cal) => {
      if (sync) await putRecords(sync.ws, "booking", cal.bookings as unknown as RecordJson[]);
    },
  );
}

export interface ClashQuery {
  talent_ids: string[];
  starts_at: string;
  ends_at: string;
  except_id?: string | null;
}

async function deviceClashes(sync: OfflineSync, q: ClashQuery): Promise<ClashReport> {
  return localClashes(
    await allRecords(sync.ws, "booking"),
    await allRecords(sync.ws, "availability"),
    q,
  ) as unknown as ClashReport;
}

/** What a booking of these talent at this time would clash with. */
export async function checkClashes(sync: OfflineSync | null, q: ClashQuery): Promise<Fetched<ClashReport>> {
  // The server knows nothing of records not sent yet.
  if (sync && [...q.talent_ids, q.except_id ?? ""].some(isLocalId)) {
    return { data: await deviceClashes(sync, q), offline: !navigator.onLine };
  }
  return serverOrDevice(
    sync,
    () => ok(api.POST("/api/v1/bookings/clash-check", { body: { ...q, except_id: q.except_id ?? null } })),
    async () => (ready(sync) ? deviceClashes(sync, q) : undefined),
  );
}

/**
 * Confirming (or keeping confirmed) while the change waits on the device:
 * refuses a blocking clash the device can see, as the server would, unless
 * an override reason is given. The server checks again when it is sent.
 */
async function checkFirm(sync: OfflineSync | null, id: string | null, q: ClashQuery, overrideReason: unknown) {
  if (!sync || !(await willQueue(sync, id))) return;
  if (typeof overrideReason === "string" && overrideReason.trim()) return;
  const report = await deviceClashes(sync, q);
  if (report.blocking) throw new ApiFailure(409, "booking_clash", clashMessage(report as never), []);
}

const talentIds = (b: Pick<Booking, "talent">) => b.talent.map((t) => t.id);

export async function createBooking(sync: OfflineSync | null, body: Record<string, unknown>): Promise<Saved> {
  if (body.status === "confirmed") {
    await checkFirm(
      sync,
      null,
      { talent_ids: (body.talent_ids as string[]) ?? [], starts_at: String(body.starts_at), ends_at: String(body.ends_at) },
      body.override_reason,
    );
  }
  return createRecord(sync, "booking", body);
}

export async function updateBooking(sync: OfflineSync | null, b: Booking, body: Record<string, unknown>): Promise<Saved> {
  const moves = "starts_at" in body || "ends_at" in body || "talent_ids" in body;
  if (b.status === "confirmed" && moves) {
    await checkFirm(
      sync,
      b.id,
      {
        talent_ids: (body.talent_ids as string[] | undefined) ?? talentIds(b),
        starts_at: String(body.starts_at ?? b.starts_at),
        ends_at: String(body.ends_at ?? b.ends_at),
        except_id: b.id,
      },
      body.override_reason,
    );
  }
  return updateRecord(sync, "booking", b.id, body, b.version);
}

export interface StatusChange {
  status: Schemas["BookingStatus"];
  reason?: string | null;
  override_reason?: string | null;
}

/** Moves a booking to another status, or queues the move. */
export async function changeStatus(sync: OfflineSync | null, b: Booking, change: StatusChange): Promise<Saved> {
  const body = { status: change.status, reason: change.reason ?? null, override_reason: change.override_reason ?? null };
  if (change.status === "confirmed") {
    await checkFirm(
      sync,
      b.id,
      { talent_ids: talentIds(b), starts_at: b.starts_at, ends_at: b.ends_at, except_id: b.id },
      change.override_reason,
    );
  }
  return sendOrQueue(sync, "booking", "status", b.id, body, b.version, async (headers) =>
    (await ok(
      api.POST("/api/v1/bookings/{id}/status", {
        params: { path: { id: b.id } },
        headers: { ...headers, "If-Match": `"${b.version}"` },
        body,
      }),
    )) as unknown as RecordJson,
  );
}

/** The enquiry pipeline's stages; null before the workspace has one. */
export function fetchStages(sync: OfflineSync | null): Promise<Fetched<Stage[] | null>> {
  return serverOrDevice(
    sync,
    async () => {
      const list = await ok(api.GET("/api/v1/pipelines", { params: { query: { entity: "enquiry" } } }));
      const pipeline = list.find((p) => p.is_default) ?? list[0];
      return pipeline ? pipeline.stages.filter((s) => !s.archived) : null;
    },
    () => getMeta<Stage[] | null>(`${sync?.ws}:stages`),
    (stages) => setMeta(`${sync?.ws}:stages`, stages),
  );
}

export function fetchMembers(sync: OfflineSync | null): Promise<Fetched<Schemas["MemberView"][]>> {
  return serverOrDevice(
    sync,
    () => ok(api.GET("/api/v1/members")),
    () => getMeta<Schemas["MemberView"][]>(`${sync?.ws}:members`),
    (m) => setMeta(`${sync?.ws}:members`, m),
  );
}

/** A booking's quotes; offline, as they were when last seen here. */
export function fetchQuotes(sync: OfflineSync | null, bookingId: string): Promise<Fetched<Schemas["QuotePage"]>> {
  const none = { items: [], total: 0 };
  if (isLocalId(bookingId)) return Promise.resolve({ data: none, offline: !navigator.onLine });
  return serverOrDevice(
    sync,
    () => ok(api.GET("/api/v1/bookings/{id}/quotes", { params: { path: { id: bookingId } } })),
    async () => (await getMeta<Schemas["QuotePage"]>(`${sync?.ws}:quotes:${bookingId}`)) ?? (ready(sync) ? none : undefined),
    (q) => setMeta(`${sync?.ws}:quotes:${bookingId}`, q),
  );
}

/**
 * A server read kept on the device as last seen (invoices, payments,
 * documents): offline, that copy is shown read-only; with none, it is
 * `NotOnDevice`.
 */
export function fetchSaved<T>(sync: OfflineSync | null, key: string, server: () => Promise<T>): Promise<Fetched<T>> {
  return serverOrDevice(
    sync,
    server,
    () => getMeta<T>(`${sync?.ws}:${key}`),
    (data) => setMeta(`${sync?.ws}:${key}`, data),
  );
}
