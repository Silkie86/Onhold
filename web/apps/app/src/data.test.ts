// @vitest-environment node
// How pages' reads keep the device's copy, against a small in-memory API.

import "fake-indexeddb/auto";
import type { RecordJson } from "@onhold/ui";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fetchList, fetchRecord, fetchWorkspace } from "./data.ts";
import { allRecords, getMeta, getRecord, putOutbox, putRecords, wipe } from "./offline/store.ts";
import { OfflineSync } from "./offline/sync.ts";

const server = vi.hoisted(() => ({
  /** The workspace the rest of the app has open. */
  current: "w1" as string | undefined,
  contacts: [] as RecordJson[],
}));

vi.mock("./api.ts", async (original) => {
  const real = await original<typeof import("./api.ts")>();
  const { createApiClient } = await import("@onhold/api-client");
  const json = (body: unknown) => new Response(JSON.stringify(body), { headers: { "content-type": "application/json" } });
  const fetch = async (req: Request) => {
    const path = new URL(req.url).pathname;
    // Like the API: what it answers is for the workspace the request names.
    const ws = req.headers.get("X-Workspace-Id");
    if (path === "/api/v1/workspace") return json({ id: ws, name: `Workspace ${ws}`, hold_days: ws === "w1" ? 7 : 14 });
    if (path === "/api/v1/contacts") return json({ items: server.contacts, total: server.contacts.length });
    const id = /^\/api\/v1\/contacts\/(.+)$/.exec(path)?.[1];
    const found = server.contacts.find((c) => c.id === id);
    return found ? json(found) : new Response(null, { status: 404 });
  };
  return {
    ...real,
    currentWorkspace: () => server.current,
    api: createApiClient({ baseUrl: "http://api.test", workspaceId: () => server.current, fetch: fetch as typeof globalThis.fetch }),
  };
});

const contact = (id: string, name: string) => ({ id, name, version: 1 }) as RecordJson;

beforeEach(async () => {
  await wipe();
  vi.stubGlobal("navigator", { onLine: true });
  server.current = "w1";
  server.contacts = [];
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("just after switching workspace", () => {
  it("keeps nothing of the new workspace in the old one's copy", async () => {
    // The app has switched to w2; its engine is still w1's for a moment.
    const old = new OfflineSync("w1");
    server.current = "w2";
    server.contacts = [contact("b1", "Of w2")];

    const ws = await fetchWorkspace(old);
    expect(ws.data).toMatchObject({ id: "w2" });
    await fetchList(old, "contact", {});
    expect(await getMeta("w1:workspace")).toBeUndefined();
    expect(await allRecords("w1", "contact")).toEqual([]);
  });

  it("keeps the workspace's own answers once the engine is its own", async () => {
    server.contacts = [contact("a1", "Of w1")];
    const sync = new OfflineSync("w1");
    await fetchWorkspace(sync);
    await fetchList(sync, "contact", {});
    expect(await getMeta("w1:workspace")).toMatchObject({ id: "w1", hold_days: 7 });
    expect(await allRecords("w1", "contact")).toEqual([contact("a1", "Of w1")]);
  });
});

describe("a record with changes still to send", () => {
  it("keeps the device's copy, with the changes, when a list or the record is read online", async () => {
    const sync = new OfflineSync("w1");
    await putRecords("w1", "contact", [contact("a1", "Mine")]);
    await putOutbox({
      ws: "w1",
      entity: "contact",
      op: "update",
      id: "a1",
      body: { name: "Mine" },
      version: 1,
      key: "k1",
      createdAt: "2026-10-27T10:00:00Z",
      state: "conflict",
    });
    server.contacts = [contact("a1", "Theirs"), contact("a2", "Other")];

    await fetchList(sync, "contact", {});
    expect((await getRecord("w1", "contact", "a1"))?.name).toBe("Mine");
    expect((await getRecord("w1", "contact", "a2"))?.name).toBe("Other");
    // Held for a decision, it is read from the server, but the device keeps its copy.
    await fetchRecord(sync, "contact", "a1");
    expect((await getRecord("w1", "contact", "a1"))?.name).toBe("Mine");
  });
});
