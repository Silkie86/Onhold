// How pages read and change CRM records. In the browser this is the API,
// as before. In the desktop and mobile apps the API is tried first and the
// device's copy answers when there is no connection; changes made offline
// are queued and sent later (see `offline/sync.ts`).

import type { RecordJson } from "@onhold/ui";
import { createContext, useContext, useEffect, useState } from "react";
import { type EntitySchema, ENTITIES, api, ok } from "./api.ts";
import type { Schemas } from "@onhold/api-client";
import { type ListParams, type Page, localList, localTags, matches, sortRecords } from "./offline/query.ts";
import {
  type EntityName,
  allRecords,
  deleteRecord as forgetRecord,
  getMeta,
  getRecord,
  isLocalId,
  putRecords,
  setMeta,
} from "./offline/store.ts";
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

const P = (entity: EntityName) => `/api/v1/${ENTITIES[entity].path}` as "/api/v1/contacts";
const PI = (entity: EntityName) => `/api/v1/${ENTITIES[entity].path}/{id}` as "/api/v1/contacts/{id}";

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

export async function fetchRecord(sync: OfflineSync | null, entity: EntityName, id: string): Promise<Fetched<RecordJson>> {
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

// Changes -----------------------------------------------------------------------------------

/** The outcome of a change: the record as it now reads, and whether it is waiting to be sent. */
export interface Saved {
  record: RecordJson | null;
  queued: boolean;
}

async function sendOrQueue(
  sync: OfflineSync | null,
  entity: EntityName,
  op: "create" | "update" | "delete",
  id: string | null,
  body: Record<string, unknown>,
  version: number | undefined,
  send: (headers: Record<string, string>) => Promise<RecordJson | null>,
): Promise<Saved> {
  if (!sync) return { record: await send({}), queued: false };
  const key = crypto.randomUUID();
  const mustQueue = !navigator.onLine || (id !== null && (isLocalId(id) || (await sync.hasQueued(id))));
  if (!mustQueue) {
    try {
      const record = await withTimeout(send({ "Idempotency-Key": key }));
      if (record) await putRecords(sync.ws, entity, [record]);
      else if (id) await forgetRecord(sync.ws, entity, id);
      return { record, queued: false };
    } catch (e) {
      if (!isNetworkError(e)) throw e;
      // It may or may not have arrived: the same key makes sending it again safe.
    }
  }
  return { record: await sync.queue(entity, op, id, body, version, key), queued: true };
}

export function createRecord(sync: OfflineSync | null, entity: EntityName, body: Record<string, unknown>): Promise<Saved> {
  return sendOrQueue(sync, entity, "create", null, body, undefined, async (headers) =>
    (await ok(api.POST(P(entity), { headers, body: body as never }))) as unknown as RecordJson,
  );
}

export function updateRecord(
  sync: OfflineSync | null,
  entity: EntityName,
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

export function deleteRecord(sync: OfflineSync | null, entity: EntityName, id: string, version?: number): Promise<Saved> {
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
