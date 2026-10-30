// @vitest-environment node
// The sync engine against a small in-memory API. Node's own fetch and
// AbortController, so requests can be cancelled the way a web view does.

import "fake-indexeddb/auto";
import type { RecordJson } from "@onhold/ui";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ORPHANED, ORPHANED_MAYBE_THERE, type OutboxItem, allRecords, getMeta, outbox, putOutbox, putRecords, setMeta, wipe } from "./store.ts";
import { KEY_EXPIRED, OfflineSync } from "./sync.ts";

interface Seen {
  method: string;
  path: string;
  ws: string | null;
  ifMatch: string | null;
  body: unknown;
}

const server = vi.hoisted(() => ({
  /** The workspace the rest of the app has open. */
  current: "w1",
  seen: [] as Seen[],
  handle: (async () => new Response(null, { status: 500 })) as (req: Request) => Promise<Response>,
}));

vi.mock("../api.ts", async (original) => {
  const real = await original<typeof import("../api.ts")>();
  const { createApiClient } = await import("@onhold/api-client");
  const fetch = async (input: Request) => {
    const url = new URL(input.url);
    const text = await input.clone().text();
    server.seen.push({
      method: input.method,
      path: url.pathname,
      ws: input.headers.get("X-Workspace-Id"),
      ifMatch: input.headers.get("If-Match"),
      body: text ? JSON.parse(text) : undefined,
    });
    return server.handle(input);
  };
  return {
    ...real,
    api: createApiClient({ baseUrl: "http://api.test", workspaceId: () => server.current, fetch: fetch as typeof globalThis.fetch }),
  };
});

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const refuse = (status: number, code: string) => json(status, { error: { code, message: code.replace(/_/g, " ") } });

/** A promise opened from outside, and given up on if the request is cancelled. */
function gate() {
  let open = () => {};
  const opened = new Promise<void>((resolve) => {
    open = resolve;
  });
  return {
    open,
    wait: (req: Request) =>
      new Promise<void>((resolve, reject) => {
        void opened.then(resolve);
        req.signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
      }),
  };
}

/** Contacts on the server, and what it answers. */
let contacts: Map<string, RecordJson>;
let made = 0;
/** What each Idempotency-Key was first sent with, and the answer it got. */
let keys: Map<string, { body: string; res: Response }>;

/** Like the API: a key sent again gets the first answer, and only with the same body. */
async function api(req: Request): Promise<Response> {
  const key = req.headers.get("Idempotency-Key");
  if (!key) return answer(req);
  const body = await req.clone().text();
  const first = keys.get(key);
  if (first) return first.body === body ? first.res.clone() : refuse(422, "idempotency_key_reused");
  const res = await answer(req);
  if (res.ok) keys.set(key, { body, res: res.clone() });
  return res;
}

async function answer(req: Request): Promise<Response> {
  const path = new URL(req.url).pathname;
  const query = new URL(req.url).searchParams;
  if (path === "/api/v1/events") return json(200, { events: [], next_after: 0, has_more: false, latest: 0 });
  if (path === "/api/v1/contacts" && req.method === "GET") {
    const sorted = [...contacts.values()].sort((a, b) => String(a.name).localeCompare(String(b.name)));
    const offset = Number(query.get("offset") ?? 0);
    const limit = Number(query.get("limit") ?? 50);
    return json(200, { items: sorted.slice(offset, offset + limit), total: sorted.length });
  }
  if (path === "/api/v1/organisations" || path === "/api/v1/talent") return json(200, { items: [], total: 0 });
  if (path === "/api/v1/contacts" && req.method === "POST") {
    const r = { ...((await req.json()) as RecordJson), id: `srv-${++made}`, version: 1 };
    contacts.set(r.id, r);
    return json(201, r);
  }
  const id = /^\/api\/v1\/contacts\/(.+)$/.exec(path)?.[1];
  if (id && req.method === "PATCH") {
    const r = contacts.get(id);
    if (!r) return refuse(404, "not_found");
    if (req.headers.get("If-Match") !== `"${r.version}"`) return refuse(412, "version_mismatch");
    const next = { ...r, ...((await req.json()) as RecordJson), version: Number(r.version) + 1 };
    contacts.set(id, next);
    return json(200, next);
  }
  return refuse(403, "forbidden");
}

const queued = (ws: string, fields: Partial<OutboxItem>): OutboxItem => ({
  ws,
  entity: "contact",
  op: "create",
  id: `local-${crypto.randomUUID()}`,
  body: { name: "New" },
  key: crypto.randomUUID(),
  createdAt: "2026-10-27T10:00:00Z",
  state: "pending",
  ...fields,
});

const posts = () => server.seen.filter((s) => s.method === "POST").length;

beforeEach(async () => {
  await wipe();
  vi.stubGlobal("navigator", { onLine: true });
  vi.stubGlobal("window", new EventTarget());
  vi.stubGlobal("document", Object.assign(new EventTarget(), { visibilityState: "visible" }));
  server.current = "w1";
  server.seen = [];
  server.handle = api;
  contacts = new Map();
  keys = new Map();
  made = 0;
  // Downloaded already: a run sends the queue and reads the (empty) feed.
  for (const ws of ["w1", "w2"]) {
    await setMeta(`${ws}:cursor`, 0);
    await setMeta(`${ws}:dataVersion`, 2);
    await setMeta(`${ws}:availabilityAt`, Date.now());
  }
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("changes made while one is on its way", () => {
  it("sends an edit made while the record's earlier edit was being sent", async () => {
    contacts.set("c1", { id: "c1", name: "Ada", version: 1 } as RecordJson);
    await putRecords("w1", "contact", [{ id: "c1", name: "Ada", version: 1 } as RecordJson]);
    const slow = gate();
    server.handle = async (req) => {
      if (req.method === "PATCH" && server.seen.filter((s) => s.method === "PATCH").length === 1) await slow.wait(req);
      return api(req);
    };
    const engine = new OfflineSync("w1");
    await engine.queue("contact", "update", "c1", { phone: "0207" }, 1);
    await vi.waitFor(() => expect(server.seen.some((s) => s.method === "PATCH")).toBe(true));
    await engine.queue("contact", "update", "c1", { email: "ada@example.com" }, 1);
    slow.open();
    await engine.syncNow();

    expect(contacts.get("c1")).toMatchObject({ phone: "0207", email: "ada@example.com", version: 3 });
    const patches = server.seen.filter((s) => s.method === "PATCH");
    expect(patches.map((p) => [p.body, p.ifMatch])).toEqual([
      [{ phone: "0207" }, '"1"'],
      [{ email: "ada@example.com" }, '"2"'],
    ]);
    expect(await outbox("w1")).toEqual([]);
    expect((await allRecords("w1", "contact"))[0]).toMatchObject({ phone: "0207", email: "ada@example.com" });
  });

  it("sends an edit made while the record's creation was being sent, to its new id", async () => {
    const slow = gate();
    server.handle = async (req) => {
      if (req.method === "POST") await slow.wait(req);
      return api(req);
    };
    const engine = new OfflineSync("w1");
    const local = await engine.queue("contact", "create", null, { name: "Grace" });
    await vi.waitFor(() => expect(posts()).toBe(1));
    await engine.queue("contact", "update", local?.id as string, { role: "Admiral" }, 0);
    slow.open();
    await engine.syncNow();

    expect(contacts.get("srv-1")).toMatchObject({ name: "Grace", role: "Admiral", version: 2 });
    expect(await outbox("w1")).toEqual([]);
    expect(await allRecords("w1", "contact")).toEqual([expect.objectContaining({ id: "srv-1", role: "Admiral" })]);
  });
});

describe("a change that may have reached the server", () => {
  it.each([
    ["the connection drops", false],
    ["the engine stops", true],
  ])("is sent again as it was when %s after it arrived, with later edits after it", async (_, stops) => {
    contacts.set("c1", { id: "c1", name: "Ada", version: 1 } as RecordJson);
    await putRecords("w1", "contact", [{ id: "c1", name: "Ada", version: 1 } as RecordJson]);
    const slow = gate();
    server.handle = async (req) => {
      if (req.method !== "PATCH" || server.seen.filter((s) => s.method === "PATCH").length > 1) return api(req);
      // The server takes it, but the answer never comes back.
      await api(req);
      if (!stops) throw new TypeError("network connection was lost");
      await slow.wait(req);
      return api(req);
    };
    let engine = new OfflineSync("w1");
    await engine.queue("contact", "update", "c1", { phone: "0207" }, 1);
    const run = engine.syncNow();
    await vi.waitFor(() => expect(contacts.get("c1")?.version).toBe(2));
    if (stops) {
      // Say a workspace switch: this engine stops, and a new one carries on.
      engine.stop();
      engine = new OfflineSync("w1");
    }
    await run;
    vi.stubGlobal("navigator", { onLine: false });
    await engine.queue("contact", "update", "c1", { email: "ada@example.com" }, 1);
    vi.stubGlobal("navigator", { onLine: true });
    await engine.syncNow();

    const patches = server.seen.filter((s) => s.method === "PATCH");
    expect(patches.map((p) => [p.body, p.ifMatch])).toEqual([
      [{ phone: "0207" }, '"1"'],
      [{ phone: "0207" }, '"1"'],
      [{ email: "ada@example.com" }, '"2"'],
    ]);
    expect(contacts.get("c1")).toMatchObject({ phone: "0207", email: "ada@example.com", version: 3 });
    expect(await outbox("w1")).toEqual([]);
  });

  it("is not folded into when it was sent straight away and its answer was lost", async () => {
    contacts.set("c1", { id: "c1", name: "Ada", phone: "0207", version: 2 } as RecordJson);
    keys.set("k1", { body: JSON.stringify({ phone: "0207" }), res: json(200, contacts.get("c1")) });
    const engine = new OfflineSync("w1");
    vi.stubGlobal("navigator", { onLine: false });
    // As the app does when its own attempt with key k1 got no answer.
    await engine.queue("contact", "update", "c1", { phone: "0207" }, 1, "k1", true);
    await engine.queue("contact", "update", "c1", { email: "ada@example.com" }, 1);
    vi.stubGlobal("navigator", { onLine: true });
    await engine.syncNow();

    const patches = server.seen.filter((s) => s.method === "PATCH");
    expect(patches.map((p) => p.body)).toEqual([{ phone: "0207" }, { email: "ada@example.com" }]);
    expect(contacts.get("c1")).toMatchObject({ email: "ada@example.com", version: 3 });
    expect(await outbox("w1")).toEqual([]);
  });
});

describe("switching workspace", () => {
  it("sends each workspace's changes to that workspace, whatever the app has open", async () => {
    await putOutbox(queued("w1", { body: { name: "A" } }));
    await putOutbox(queued("w1", { body: { name: "B" } }));
    const engine = new OfflineSync("w1");
    server.current = "w2";
    await engine.syncNow();
    expect(server.seen.length).toBeGreaterThan(2);
    expect(server.seen.every((s) => s.ws === "w1")).toBe(true);
  });

  it("stops a run in progress when the engine stops", async () => {
    await putOutbox(queued("w1", { body: { name: "A" } }));
    await putOutbox(queued("w1", { body: { name: "B" } }));
    const slow = gate();
    server.handle = async (req) => {
      await slow.wait(req);
      return api(req);
    };
    const engine = new OfflineSync("w1");
    const run = engine.syncNow();
    await vi.waitFor(() => expect(posts()).toBe(1));
    engine.stop();
    slow.open();
    await run;
    expect(server.seen).toHaveLength(1);
    // Not known to have arrived: both stay, to be sent with the same keys next time.
    expect((await outbox("w1")).map((q) => q.state)).toEqual(["pending", "pending"]);
  });
});

describe("a change the server is still working on", () => {
  it.each([
    [409, "request_in_progress"],
    [408, "request_timeout"],
  ])("stays queued after %i and is tried again later", async (status, code) => {
    await putOutbox(queued("w1", {}));
    await putOutbox(queued("w1", { body: { name: "Other" } }));
    server.handle = async (req) =>
      req.method === "POST" && server.seen.filter((s) => s.method === "POST").length === 1 ? refuse(status, code) : api(req);
    const engine = new OfflineSync("w1");
    await engine.syncNow();

    const [held, ...rest] = await outbox("w1");
    expect(held).toMatchObject({ state: "pending", tries: 1 });
    expect(Date.parse(held?.retryAt as string)).toBeGreaterThan(Date.now());
    // The rest of the queue carried on.
    expect(rest).toEqual([]);
    // Not again before its time.
    await engine.syncNow();
    expect(posts()).toBe(2);
  });

  it("sends a refused change again when asked", async () => {
    const seq = await putOutbox(queued("w1", { state: "failed", error: "nope" }));
    const engine = new OfflineSync("w1");
    await engine.retry(seq);
    expect(posts()).toBe(1);
    expect(await outbox("w1")).toEqual([]);
  });
});

describe("a full download", () => {
  it("keeps a record that moved to an earlier page while the list was read", async () => {
    const names = Array.from({ length: 600 }, (_, i) => `c${String(i).padStart(3, "0")}`);
    for (const n of names) contacts.set(n, { id: n, name: n, version: 1 } as RecordJson);
    await putRecords("w1", "contact", [...contacts.values()]);
    let firstPage = true;
    server.handle = async (req) => {
      const res = await api(req);
      const url = new URL(req.url);
      if (url.pathname === "/api/v1/contacts" && url.searchParams.get("offset") === "0" && firstPage) {
        // Someone deletes the first contact as the first page arrives.
        firstPage = false;
        contacts.delete("c000");
      }
      return res;
    };
    await new OfflineSync("w1").downloadAll();
    const ids = new Set((await allRecords("w1", "contact")).map((r) => r.id));
    expect(ids.has("c500")).toBe(true);
    expect(ids.has("c000")).toBe(false);
    expect(ids.size).toBe(599);
  });
});

describe("another window of the app", () => {
  it("does not fold its edit into the change this window is sending, which would lose it", async () => {
    contacts.set("c1", { id: "c1", name: "Ada", version: 1 } as RecordJson);
    await putRecords("w1", "contact", [{ id: "c1", name: "Ada", version: 1 } as RecordJson]);
    const slow = gate();
    server.handle = async (req) => {
      if (req.method === "PATCH" && server.seen.filter((s) => s.method === "PATCH").length === 1) await slow.wait(req);
      return api(req);
    };
    const one = new OfflineSync("w1");
    const two = new OfflineSync("w1");
    await one.queue("contact", "update", "c1", { name: "Ada L" }, 1);
    await vi.waitFor(() => expect(server.seen.some((s) => s.method === "PATCH")).toBe(true));
    // The second window makes its edit while the first one's is on its way.
    vi.stubGlobal("navigator", { onLine: false });
    await two.queue("contact", "update", "c1", { phone: "0207" }, 1);
    vi.stubGlobal("navigator", { onLine: true });
    slow.open();
    await one.syncNow();
    await two.syncNow();

    expect(contacts.get("c1")).toMatchObject({ name: "Ada L", phone: "0207", version: 3 });
    expect(await outbox("w1")).toEqual([]);
  });
});

describe("a creation whose key the server may have forgotten", () => {
  const daysAgo = (days: number) => new Date(Date.now() - days * 86_400_000).toISOString();

  it("is not sent again on its own, but is when the person asks", async () => {
    await putOutbox(queued("w1", { tries: 0, sentAt: daysAgo(2) }));
    const engine = new OfflineSync("w1");
    await engine.syncNow();
    expect(posts()).toBe(0);
    const [item] = await outbox("w1");
    expect(item).toMatchObject({ state: "failed", error: KEY_EXPIRED });
    expect(engine.current.attention).toBe(1);

    await engine.retry(item?.seq as number);
    expect(posts()).toBe(1);
    expect(await outbox("w1")).toEqual([]);
  });

  it("goes again as usual when it was queued long ago but first sent just now", async () => {
    await putOutbox(queued("w1", { createdAt: daysAgo(3) }));
    server.handle = async (req) => {
      const res = await api(req);
      // The server takes it, but the answer is lost the first time.
      if (req.method === "POST" && posts() === 1) throw new TypeError("network connection was lost");
      return res;
    };
    const engine = new OfflineSync("w1");
    await engine.syncNow();
    expect(await outbox("w1")).toEqual([expect.objectContaining({ state: "pending", tries: 0 })]);
    await engine.syncNow();
    expect(posts()).toBe(2);
    expect(contacts.size).toBe(1);
    expect(await outbox("w1")).toEqual([]);
  });
});

describe("a record made offline that is discarded", () => {
  it("takes the changes linking to it out of the queue's wait, as refused", async () => {
    vi.stubGlobal("navigator", { onLine: false });
    const engine = new OfflineSync("w1");
    const boss = await engine.queue("contact", "create", null, { name: "Boss" });
    await engine.queue("contact", "create", null, { name: "Report", manager_id: boss?.id });
    vi.stubGlobal("navigator", { onLine: true });
    server.handle = async (req) => (req.method === "POST" ? refuse(422, "invalid") : api(req));
    await engine.syncNow();
    const [refused, report] = await outbox("w1");
    expect(refused?.state).toBe("failed");
    expect(report?.state).toBe("pending");

    await engine.resolve(refused?.seq as number, "theirs");
    expect(await outbox("w1")).toEqual([expect.objectContaining({ seq: report?.seq, state: "failed", error: ORPHANED })]);
    expect(engine.current).toMatchObject({ pending: 0, attention: 1 });
    // Trying it again cannot help: it stays refused.
    await engine.retry(report?.seq as number);
    expect((await outbox("w1"))[0]?.state).toBe("failed");
    await engine.resolve(report?.seq as number, "theirs");
    expect(await outbox("w1")).toEqual([]);
  });

  it("does the same when the record is deleted before it was sent", async () => {
    vi.stubGlobal("navigator", { onLine: false });
    const engine = new OfflineSync("w1");
    const boss = await engine.queue("contact", "create", null, { name: "Boss" });
    await engine.queue("contact", "create", null, { name: "Report", manager_id: boss?.id });
    await engine.queue("contact", "delete", boss?.id as string, {});
    expect(await outbox("w1")).toEqual([expect.objectContaining({ op: "create", state: "failed", error: ORPHANED })]);
  });
});

describe("reading what changed", () => {
  const events = (ids: string[], nextAfter: number, more: boolean) =>
    json(200, {
      events: ids.map((id) => ({ topic: "contact.updated", object_id: id })),
      next_after: nextAfter,
      has_more: more,
      latest: 999,
    });
  const reads = () => server.seen.filter((s) => s.method === "GET" && /^\/api\/v1\/contacts\/.+/.test(s.path)).length;
  const readOne = (req: Request) => {
    const id = /^\/api\/v1\/contacts\/(.+)$/.exec(new URL(req.url).pathname)?.[1];
    return id && req.method === "GET" ? json(200, contacts.get(id) ?? { id, name: id, version: 1 }) : null;
  };

  it("downloads everything again when too many records changed to fetch one by one", async () => {
    const ids = Array.from({ length: 300 }, (_, i) => `c${i}`);
    for (const id of ids) contacts.set(id, { id, name: id, version: 1 } as RecordJson);
    server.handle = async (req) => {
      const url = new URL(req.url);
      if (url.pathname === "/api/v1/events" && url.searchParams.get("limit") !== "1") return events(ids, 300, false);
      if (url.pathname === "/api/v1/events") return events([], 999, false);
      return readOne(req) ?? api(req);
    };
    await new OfflineSync("w1").syncNow();
    expect(reads()).toBe(0);
    expect(await allRecords("w1", "contact")).toHaveLength(300);
    expect(await getMeta("w1:cursor")).toBe(999);
  });

  it("notes its place after each page of the feed, so a run cut short carries on from there", async () => {
    server.handle = async (req) => {
      const url = new URL(req.url);
      if (url.pathname === "/api/v1/events") {
        if (url.searchParams.get("after") === "0") return events(["c1", "c2", "c3"], 3, true);
        throw new TypeError("network connection was lost");
      }
      return readOne(req) ?? api(req);
    };
    const engine = new OfflineSync("w1");
    await engine.syncNow();
    expect(reads()).toBe(3);
    expect(await getMeta("w1:cursor")).toBe(3);
    expect(await allRecords("w1", "contact")).toHaveLength(3);

    server.seen = [];
    server.handle = async (req) => (new URL(req.url).pathname === "/api/v1/events" ? events([], 3, false) : api(req));
    await engine.syncNow();
    expect(server.seen.find((s) => s.path === "/api/v1/events")).toBeDefined();
    expect(reads()).toBe(0);
  });
});

describe("what the feed says changed besides records", () => {
  type Ev = [topic: string, seq: number, id?: string];
  /** The feed in pages: each `after` gets its events, the next position, whether there is more and the newest position then. */
  type FeedPage = { events: Ev[]; next: number; more: boolean; latest: number } | "drop";
  let feed: Map<string, FeedPage>;
  const count = (re: RegExp) => server.seen.filter((s) => s.method === "GET" && re.test(s.path)).length;
  const handle = async (req: Request) => {
    const url = new URL(req.url);
    if (url.pathname === "/api/v1/events") {
      const page = feed.get(url.searchParams.get("after") ?? "");
      if (page === "drop") throw new TypeError("network connection was lost");
      const { events = [], next = 0, more = false, latest = 0 } = page ?? {};
      return json(200, {
        events: events.map(([topic, seq, id]) => ({ topic, seq, object_id: id ?? null })),
        next_after: next,
        has_more: more,
        latest,
      });
    }
    const id = /^\/api\/v1\/contacts\/(.+)$/.exec(url.pathname)?.[1];
    if (id && req.method === "GET") return json(200, { id, name: id, version: 1 });
    return api(req);
  };
  beforeEach(() => {
    server.handle = handle;
  });

  it("reads them once after the last page, and a record already read after its change only once", async () => {
    feed = new Map<string, FeedPage>([
      ["0", { events: [["contact.updated", 1, "c1"], ["custom_field.created", 2], ["availability.created", 3]], next: 3, more: true, latest: 10 }],
      ["3", { events: [["contact.updated", 4, "c1"], ["availability.updated", 5], ["contact.updated", 11, "c2"]], next: 11, more: false, latest: 11 }],
    ]);
    await new OfflineSync("w1").syncNow();
    expect(count(/^\/api\/v1\/contacts\/c1$/)).toBe(1);
    expect(count(/^\/api\/v1\/contacts\/c2$/)).toBe(1);
    expect(count(/^\/api\/v1\/schema\/contact$/)).toBe(1);
    expect(count(/^\/api\/v1\/calendar$/)).toBe(1);
    expect(await getMeta("w1:cursor")).toBe(11);
  });

  it("also reads the members and settings once a day, even when marked-off time changed", async () => {
    await setMeta("w1:availabilityAt", 0);
    feed = new Map<string, FeedPage>([["0", { events: [["availability.created", 1]], next: 1, more: false, latest: 1 }]]);
    await new OfflineSync("w1").syncNow();
    expect(count(/^\/api\/v1\/calendar$/)).toBe(1);
    expect(count(/^\/api\/v1\/members$/)).toBe(1);
    expect(count(/^\/api\/v1\/workspace$/)).toBe(1);
  });

  it("remembers them when a run is cut short after noting its place", async () => {
    feed = new Map<string, FeedPage>([
      ["0", { events: [["custom_field.created", 1]], next: 1, more: true, latest: 1 }],
      ["1", "drop"],
    ]);
    const engine = new OfflineSync("w1");
    await engine.syncNow();
    expect(await getMeta("w1:cursor")).toBe(1);
    expect(count(/^\/api\/v1\/schema\//)).toBe(0);

    feed = new Map<string, FeedPage>([["1", { events: [], next: 1, more: false, latest: 1 }]]);
    await engine.syncNow();
    expect(count(/^\/api\/v1\/schema\/contact$/)).toBe(1);
    expect(await getMeta("w1:pullPending")).toBeUndefined();
  });

  it("reads marked-off time in smaller windows when the server says one holds too much", async () => {
    await setMeta("w1:availabilityAt", 0);
    feed = new Map();
    let windows = 0;
    server.handle = async (req) => {
      const url = new URL(req.url);
      if (url.pathname !== "/api/v1/calendar") return handle(req);
      const days = (Date.parse(url.searchParams.get("to") ?? "") - Date.parse(url.searchParams.get("from") ?? "")) / 86_400_000;
      if (days > 120) return refuse(400, "too_many_entries");
      windows++;
      // One marked off for the whole year comes in every window.
      return json(200, { bookings: [], availability: [{ id: "all-year", source: "manual" }, { id: `w${windows}`, source: "manual" }] });
    };
    await new OfflineSync("w1").syncNow();
    const ids = (await allRecords("w1", "availability")).map((a) => a.id).sort();
    expect(windows).toBeGreaterThan(1);
    expect(ids).toEqual(["all-year", ...Array.from({ length: windows }, (_, i) => `w${i + 1}`)].sort());
  });
});

describe("settling a change", () => {
  it("keeps mine when the server's copy could not be read when the conflict was found", async () => {
    contacts.set("c1", { id: "c1", name: "Ada", version: 2 } as RecordJson);
    const seq = await putOutbox(queued("w1", { op: "update", id: "c1", body: { phone: "0207" }, version: 1, state: "conflict" }));
    server.handle = async (req) => {
      const path = new URL(req.url).pathname;
      if (req.method === "GET" && path === "/api/v1/contacts/c1") return json(200, contacts.get("c1"));
      return api(req);
    };
    const engine = new OfflineSync("w1");
    await engine.resolve(seq, "mine");
    await engine.syncNow();
    expect(contacts.get("c1")).toMatchObject({ phone: "0207", version: 3 });
    expect(await outbox("w1")).toEqual([]);
  });

  it("keeps mine waiting when the server's copy still cannot be read", async () => {
    const seq = await putOutbox(queued("w1", { op: "update", id: "c1", body: { phone: "0207" }, version: 1, state: "conflict" }));
    vi.stubGlobal("navigator", { onLine: false });
    await new OfflineSync("w1").resolve(seq, "mine");
    expect(await outbox("w1")).toEqual([expect.objectContaining({ seq, state: "pending", body: { phone: "0207" } })]);
  });

  it("says what to do with changes linking to a creation discarded because it may be there already", async () => {
    const boss = `local-${crypto.randomUUID()}`;
    const seq = await putOutbox(queued("w1", { id: boss, state: "failed", error: KEY_EXPIRED }));
    await putOutbox(queued("w1", { body: { name: "Report", manager_id: boss } }));
    vi.stubGlobal("navigator", { onLine: false });
    await new OfflineSync("w1").resolve(seq, "theirs");
    expect(await outbox("w1")).toEqual([expect.objectContaining({ state: "failed", error: ORPHANED_MAYBE_THERE })]);
  });
});

describe("a change made to a record whose creation has reached the server", () => {
  it("goes to its real id, even when another window sent the creation", async () => {
    const local = `local-${crypto.randomUUID()}`;
    await setMeta(`w1:renamed:${local}`, "srv-9");
    vi.stubGlobal("navigator", { onLine: false });
    const engine = new OfflineSync("w1");
    await engine.queue("contact", "create", null, { name: "Report", manager_id: local });
    await engine.queue("contact", "update", local, { name: "Boss" }, 1);
    const [report, boss] = await outbox("w1");
    expect(report?.body).toEqual({ name: "Report", manager_id: "srv-9" });
    expect(report?.state).toBe("pending");
    expect(boss?.id).toBe("srv-9");
  });
});

describe("an engine stopped while it starts", () => {
  it("leaves nothing listening", async () => {
    const listen = vi.spyOn(window, "addEventListener");
    const engine = new OfflineSync("w1");
    const started = engine.start();
    engine.stop();
    await started;
    expect(listen).not.toHaveBeenCalled();
  });
});
