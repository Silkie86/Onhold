// @vitest-environment node
// Recent email kept on the device, and replies written offline, against a
// small in-memory inbox API.

import "fake-indexeddb/auto";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  MAIL_MAX_BYTES,
  type ThreadDetail,
  type ThreadView,
  localThreads,
  overflow,
  trimDetail,
  withoutQuote,
} from "./mail.ts";
import { allMail, getMeta, keepMailKey, outbox, setMeta, wipe, wipeAllButQueue } from "./store.ts";
import { OfflineSync } from "./sync.ts";

interface Seen {
  method: string;
  path: string;
  key: string | null;
  body: unknown;
}

const server = vi.hoisted(() => ({
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
      key: input.headers.get("Idempotency-Key"),
      body: text ? JSON.parse(text) : undefined,
    });
    return server.handle(input);
  };
  return {
    ...real,
    api: createApiClient({ baseUrl: "http://api.test", workspaceId: () => "w1", fetch: fetch as typeof globalThis.fetch }),
  };
});

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const refuse = (status: number, code: string) => json(status, { error: { code, message: code.replace(/_/g, " ") } });

const DAY = 86_400_000;
const ago = (days: number) => new Date(Date.now() - days * DAY).toISOString();

function thread(id: string, fields: Partial<ThreadView> = {}): ThreadView {
  return {
    id,
    channel: "email",
    integration_id: null,
    subject: `About ${id}`,
    contact_id: null,
    organisation_id: null,
    enquiry_id: null,
    booking_id: null,
    status: "open",
    snoozed_until: null,
    assigned_to: null,
    unread: false,
    message_count: 1,
    participants: ["ada@example.com"],
    preview: "Hello",
    last_message_at: ago(1),
    last_inbound_at: null,
    last_outbound_at: null,
    created_at: ago(30),
    version: 1,
    ...fields,
  };
}

function message(id: string, text: string, fields: Partial<ThreadDetail["messages"][number]> = {}): ThreadDetail["messages"][number] {
  return {
    id,
    direction: "inbound",
    from_address: "ada@example.com",
    from_name: "Ada",
    to: ["me@example.com"],
    cc: [],
    subject: null,
    text,
    html: `<p>${text}</p>`,
    sent_at: ago(1),
    status: "received",
    error: null,
    automatic: false,
    bulk: false,
    author_id: null,
    template_key: null,
    files: [],
    ...fields,
  };
}

/** The inbox on the server. */
let threads: Map<string, ThreadDetail>;
let events: { seq: number; topic: string; object_id: string }[];
let keys: Map<string, Response>;
let replyStatus: number;

async function api(req: Request): Promise<Response> {
  const url = new URL(req.url);
  const path = url.pathname;
  if (path === "/api/v1/events") {
    const after = Number(url.searchParams.get("after") ?? 0);
    const page = events.filter((e) => e.seq > after);
    const latest = events.at(-1)?.seq ?? 0;
    return json(200, {
      events: page.map((e) => ({ ...e, id: e.seq, occurred_at: ago(0), data: {} })),
      next_after: latest,
      has_more: false,
      latest,
    });
  }
  if (path === "/api/v1/inbox/threads" && req.method === "GET") {
    const sorted = [...threads.values()]
      .map((d) => d.thread)
      .sort((a, b) => Date.parse(b.last_message_at) - Date.parse(a.last_message_at));
    const offset = Number(url.searchParams.get("offset") ?? 0);
    const limit = Number(url.searchParams.get("limit") ?? 50);
    return json(200, { items: sorted.slice(offset, offset + limit), total: sorted.length });
  }
  const reply = /^\/api\/v1\/inbox\/threads\/([^/]+)\/reply$/.exec(path)?.[1];
  if (reply && req.method === "POST") {
    const key = req.headers.get("Idempotency-Key");
    const first = key ? keys.get(key) : undefined;
    if (first) return first.clone();
    const d = threads.get(reply);
    if (!d || replyStatus !== 200) return refuse(replyStatus === 200 ? 404 : replyStatus, "not_found");
    const { body } = (await req.json()) as { body: string };
    d.messages.push(message(`m${d.messages.length + 1}`, body, { direction: "outbound", status: "queued" }));
    d.thread = { ...d.thread, version: d.thread.version + 1, message_count: d.messages.length };
    const res = json(200, d);
    if (key) keys.set(key, res.clone());
    return res;
  }
  const id = /^\/api\/v1\/inbox\/threads\/([^/]+)$/.exec(path)?.[1];
  if (id && req.method === "GET") {
    const d = threads.get(id);
    return d ? json(200, d) : refuse(404, "not_found");
  }
  // The CRM side has nothing in these tests.
  if (req.method === "GET") return json(200, { items: [], total: 0 });
  return refuse(403, "forbidden");
}

function put(id: string, fields: Partial<ThreadView> = {}, messages = [message(`${id}-1`, "Hello")]) {
  threads.set(id, { thread: thread(id, fields), messages, mailbox: null });
}

const reads = (id?: string) =>
  server.seen.filter((s) => s.method === "GET" && (id ? s.path === `/api/v1/inbox/threads/${id}` : /\/inbox\/threads\/[^/]+$/.test(s.path))).length;
const listings = () => server.seen.filter((s) => s.method === "GET" && s.path === "/api/v1/inbox/threads").length;

beforeEach(async () => {
  await wipe();
  vi.stubGlobal("navigator", { onLine: true });
  vi.stubGlobal("window", new EventTarget());
  vi.stubGlobal("document", Object.assign(new EventTarget(), { visibilityState: "visible" }));
  server.seen = [];
  server.handle = api;
  threads = new Map();
  events = [];
  keys = new Map();
  replyStatus = 200;
  await setMeta("w1:cursor", 0);
  await setMeta("w1:dataVersion", 2);
  await setMeta("w1:availabilityAt", Date.now());
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("what a message keeps", () => {
  it("leaves out the quoted earlier messages", () => {
    expect(withoutQuote("Yes, Friday works.\n\nOn Mon, 27 Oct 2026 at 10:00, Ada <ada@example.com> wrote:\n> Can you do Friday?\n> Thanks")).toBe(
      "Yes, Friday works.",
    );
    expect(withoutQuote("Sounds good\nOn Mon, 27 Oct 2026, Ada Lovelace\n<ada@example.com> wrote:\n> earlier")).toBe("Sounds good");
    expect(withoutQuote("Thanks\n\n-----Original Message-----\nFrom: Ada\nHi")).toBe("Thanks");
    expect(withoutQuote("Thanks\n\nFrom: Ada Lovelace\nSent: Monday\nTo: me\n\nHi")).toBe("Thanks");
    // Nothing but a quote: kept as it is.
    expect(withoutQuote("> only a quote")).toBe("> only a quote");
    // Quoted lines with an answer after them are part of the answer.
    expect(withoutQuote("> Friday?\nYes, Friday.")).toBe("> Friday?\nYes, Friday.");
  });

  it("keeps the latest messages as text, without HTML", () => {
    const messages = Array.from({ length: 25 }, (_, i) => message(`m${i}`, i === 24 ? "x".repeat(30_000) : `Message ${i}`));
    const kept = trimDetail({ thread: thread("t1"), messages, mailbox: null });
    expect(kept.messages).toHaveLength(20);
    expect(kept.messages[0]?.id).toBe("m5");
    expect(kept.left_out).toBe(5);
    expect(kept.messages.every((m) => m.html === null)).toBe(true);
    expect(kept.messages.at(-1)?.text.length).toBe(20_001);
    expect(kept.shortened).toBe(true);
  });

  it("lets go of conversations older than two weeks, and the oldest when over the limit", () => {
    const now = Date.now();
    const big = MAIL_MAX_BYTES / 2 - 10;
    const t = (id: string, days: number, bytes: number) => ({ ws: "w1", id, detail: {}, at: ago(days), version: 1, bytes });
    expect(overflow([t("old", 15, 10), t("a", 1, big), t("b", 3, big), t("c", 2, big), t("new", 0, 10)], now).sort()).toEqual(["b", "old"]);
  });

  it("filters kept conversations the way the API does", () => {
    const list = [
      thread("a", { last_message_at: ago(1), unread: true, subject: "Gig in Leeds" }),
      thread("b", { last_message_at: ago(2), contact_id: "c1", status: "resolved" }),
      thread("c", { last_message_at: ago(0), participants: ["grace@example.com"] }),
    ];
    expect(localThreads(list, {}).items.map((t) => t.id)).toEqual(["c", "a", "b"]);
    expect(localThreads(list, { status: "open" }).items.map((t) => t.id)).toEqual(["c", "a"]);
    expect(localThreads(list, { unread: true }).items.map((t) => t.id)).toEqual(["a"]);
    expect(localThreads(list, { unlinked: true }).total).toBe(2);
    expect(localThreads(list, { contact_id: "c1" }).items.map((t) => t.id)).toEqual(["b"]);
    expect(localThreads(list, { q: "LEEDS" }).items.map((t) => t.id)).toEqual(["a"]);
    expect(localThreads(list, { q: "grace" }).items.map((t) => t.id)).toEqual(["c"]);
  });
});

describe("keeping recent email", () => {
  it("keeps nothing and reads no email unless turned on", async () => {
    put("t1");
    const engine = new OfflineSync("w1");
    await engine.syncNow();
    expect(listings()).toBe(0);
    expect(await allMail("w1")).toEqual([]);
    expect(await engine.mail.list({})).toBeUndefined();
  });

  it("keeps the last two weeks of conversations once turned on", async () => {
    put("t1", { last_message_at: ago(1) });
    put("t2", { last_message_at: ago(3) });
    put("old", { last_message_at: ago(20) });
    const engine = new OfflineSync("w1");
    await engine.mail.setEnabled(true);
    await engine.syncNow();

    const kept = await allMail("w1");
    expect(kept.map((t) => t.id).sort()).toEqual(["t1", "t2"]);
    expect(reads("old")).toBe(0);
    const usage = await engine.mail.usage();
    expect(usage.threads).toBe(2);
    expect(usage.bytes).toBeGreaterThan(0);
    expect((await engine.mail.read("t1"))?.messages[0]).toMatchObject({ text: "Hello", html: null });
    expect((await engine.mail.list({}))?.items.map((t) => t.id)).toEqual(["t1", "t2"]);
  });

  it("reads again only the conversations the feed names, between listings", async () => {
    put("t1");
    put("t2");
    const engine = new OfflineSync("w1");
    await engine.mail.setEnabled(true);
    await engine.syncNow();
    server.seen = [];

    const d = threads.get("t2") as ThreadDetail;
    d.messages.push(message("t2-2", "Any news?"));
    d.thread = { ...d.thread, version: 2, message_count: 2 };
    events.push({ seq: 1, topic: "message.received", object_id: "t2" });
    await engine.syncNow();

    expect(listings()).toBe(0);
    expect(reads("t1")).toBe(0);
    expect(reads("t2")).toBe(1);
    expect((await engine.mail.read("t2"))?.messages).toHaveLength(2);
  });

  it("lets go of a conversation the person can no longer see", async () => {
    put("t1");
    put("t2");
    const engine = new OfflineSync("w1");
    await engine.mail.setEnabled(true);
    await engine.syncNow();

    threads.delete("t2");
    await setMeta("w1:mail:listedAt", 1);
    await engine.syncNow();
    expect((await allMail("w1")).map((t) => t.id)).toEqual(["t1"]);
  });

  it("deletes everything kept when turned off", async () => {
    put("t1");
    const engine = new OfflineSync("w1");
    await engine.mail.setEnabled(true);
    await engine.syncNow();
    expect(await allMail("w1")).toHaveLength(1);

    await engine.mail.setEnabled(false);
    expect(await allMail("w1")).toEqual([]);
    expect(await engine.mail.read("t1")).toBeUndefined();
  });

  it("goes when the session ends, but the choice and unsent replies stay", async () => {
    put("t1");
    const engine = new OfflineSync("w1");
    await engine.mail.setEnabled(true);
    await engine.syncNow();
    vi.stubGlobal("navigator", { onLine: false });
    await engine.queueReply("t1", { body: "On my way" }, "Reply: About t1", "k1");

    await wipeAllButQueue();
    expect(await allMail("w1")).toEqual([]);
    expect(await getMeta(keepMailKey("w1"))).toBe(true);
    expect(await outbox("w1")).toHaveLength(1);
  });
});

describe("replies written offline", () => {
  it("wait on the device and go once online, with their key", async () => {
    put("t1");
    const engine = new OfflineSync("w1");
    await engine.mail.setEnabled(true);
    vi.stubGlobal("navigator", { onLine: false });
    await engine.queueReply("t1", { body: "Friday works" }, "Reply: About t1", "k1");
    await engine.queueReply("t1", { body: "And Saturday" }, "Reply: About t1", "k2");
    await engine.syncNow();
    expect(server.seen.filter((s) => s.method === "POST")).toEqual([]);
    expect(engine.current.pending).toBe(2);

    vi.stubGlobal("navigator", { onLine: true });
    await engine.syncNow();
    const posts = server.seen.filter((s) => s.method === "POST");
    expect(posts.map((p) => [p.path, p.key, p.body])).toEqual([
      ["/api/v1/inbox/threads/t1/reply", "k1", { body: "Friday works" }],
      ["/api/v1/inbox/threads/t1/reply", "k2", { body: "And Saturday" }],
    ]);
    expect(await outbox("w1")).toEqual([]);
    // The kept copy shows them.
    expect((await engine.mail.read("t1"))?.messages.map((m) => m.text)).toEqual(["Hello", "Friday works", "And Saturday"]);
  });

  it("is not sent twice when the first attempt arrived but its answer was lost", async () => {
    put("t1");
    const engine = new OfflineSync("w1");
    // Sent once straight away; the answer never came back.
    await api(new Request("http://api.test/api/v1/inbox/threads/t1/reply", {
      method: "POST",
      headers: { "Idempotency-Key": "k1", "content-type": "application/json" },
      body: JSON.stringify({ body: "Friday works" }),
    }));
    await engine.queueReply("t1", { body: "Friday works" }, "Reply: About t1", "k1", true);
    await engine.syncNow();
    expect(threads.get("t1")?.messages).toHaveLength(2);
    expect(await outbox("w1")).toEqual([]);
  });

  it("waits for a decision when the server will not take it", async () => {
    put("t1");
    replyStatus = 404;
    const engine = new OfflineSync("w1");
    await engine.queueReply("t1", { body: "Friday works" }, "Reply: About t1", "k1");
    await engine.syncNow();
    const [failed] = await outbox("w1");
    expect(failed).toMatchObject({ entity: "reply", state: "failed" });

    await engine.resolve(failed?.seq as number, "theirs");
    expect(await outbox("w1")).toEqual([]);
  });
});
