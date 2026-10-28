// Recent email kept on the device, when the person turns it on (it is off
// unless they do). It keeps the conversations of the last two weeks that the
// person can see, as text only: no HTML, no attachments, quoted earlier
// messages left out, and no more than a few megabytes in all, newest first.
//
// - When it is turned on, and every half hour after, the list of recent
//   conversations is read and each one new or changed since is fetched.
// - In between, the changes feed names the conversations that had news, and
//   only those are fetched again.
// - Conversations that fall out of the two weeks, or out of what the person
//   may see, are deleted from the device.
//
// Replies written offline are queued with the other changes (see `sync.ts`).

import type { Schemas } from "@onhold/api-client";
import { ApiFailure, api, ok } from "../api.ts";
import {
  type StoredThread,
  allMail,
  clearMail,
  deleteMail,
  getMail,
  getMeta,
  keepMailKey,
  putMail,
  setMeta,
} from "./store.ts";

export type ThreadView = Schemas["ThreadView"];
export type ThreadDetail = Schemas["ThreadDetail"];

/** A conversation as kept on the device. */
export interface KeptDetail extends ThreadDetail {
  /** Earlier messages not kept on the device. */
  left_out?: number;
  /** Some text was shortened or quoted text left out. */
  shortened?: boolean;
}

/** How far back conversations are kept, by their latest message. */
export const MAIL_DAYS = 14;
/** The most room the email on a device may take. */
export const MAIL_MAX_BYTES = 5 * 1024 * 1024;
/** The latest messages kept of each conversation… */
const MESSAGES_PER_THREAD = 20;
/** …each up to this much text. */
const MESSAGE_CHARS = 20_000;
/** The most conversations listed. */
const MAX_THREADS = 300;
/** Conversations fetched in one go; the rest follow on the next run. */
const READS_PER_RUN = 40;
/** How often the whole list is checked, for conversations the feed did not name. */
const LIST_EVERY_MS = 30 * 60_000;
const PAGE = 100;

const DAY_MS = 86_400_000;
const listedKey = (ws: string) => `${ws}:mail:listedAt`;

/** Roughly how many bytes a value takes once stored. */
export function bytesOf(value: unknown): number {
  return new TextEncoder().encode(JSON.stringify(value)).length;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} bytes`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

const QUOTE_HEADER = /^(On .+ wrote:|Le .+ a écrit\s?:|Am .+ schrieb .+:|El .+ escribió:)$/;
const ORIGINAL = /^-{2,}\s*(Original Message|Forwarded message)\s*-{2,}$/i;

/**
 * A message's text without the earlier messages quoted below it, which the
 * device keeps separately anyway: the trailing `>` lines and the "On …
 * wrote:" line before them, or everything from an Outlook-style
 * "-----Original Message-----" or "From: … / Sent: …" header down. Text
 * that is nothing but a quote is left as it is.
 */
export function withoutQuote(text: string): string {
  const lines = text.split(/\r?\n/);
  let end = lines.length;
  for (let i = 0; i < lines.length; i++) {
    const line = (lines[i] ?? "").trim();
    const next = (lines[i + 1] ?? "").trim();
    if (ORIGINAL.test(line) || (/^From:\s/.test(line) && /^(Sent|Date):\s/.test(next))) {
      end = i;
      break;
    }
  }
  // Trailing quoted lines (and blank ones between them).
  let cut = end;
  while (cut > 0 && /^\s*(>|$)/.test(lines[cut - 1] ?? "")) cut--;
  const quoted = lines.slice(cut, end).some((l) => l.trimStart().startsWith(">"));
  if (quoted) {
    end = cut;
    // The "On … wrote:" line, which may be wrapped over two.
    const last = (lines[end - 1] ?? "").trim();
    const both = `${(lines[end - 2] ?? "").trim()} ${last}`;
    if (QUOTE_HEADER.test(last)) end -= 1;
    else if (/wrote:$/.test(last) && QUOTE_HEADER.test(both)) end -= 2;
  }
  const kept = lines.slice(0, end).join("\n").trimEnd();
  return kept || text;
}

/** A conversation trimmed to what the device keeps: text only, the latest messages. */
export function trimDetail(detail: ThreadDetail): KeptDetail {
  const recent = detail.messages.slice(-MESSAGES_PER_THREAD);
  let shortened = false as boolean;
  const messages = recent.map((m) => {
    let text = withoutQuote(m.text);
    if (text.length > MESSAGE_CHARS) text = `${text.slice(0, MESSAGE_CHARS)}…`;
    if (text !== m.text) shortened = true;
    // Attachments' names stay, so the person knows they are there; they open once online.
    return { ...m, text, html: null };
  });
  const kept: KeptDetail = { ...detail, messages };
  const left_out = detail.messages.length - recent.length;
  if (left_out > 0) kept.left_out = left_out;
  if (shortened) kept.shortened = true;
  return kept;
}

function storedThread(ws: string, detail: ThreadDetail): StoredThread {
  const kept = trimDetail(detail);
  return {
    ws,
    id: detail.thread.id,
    detail: kept,
    at: detail.thread.last_message_at,
    version: detail.thread.version,
    bytes: bytesOf(kept),
  };
}

/** True while a reply in it is still on its way, so it is read again until it is settled. */
function unsettled(t: StoredThread): boolean {
  return (t.detail as KeptDetail).messages.some((m) => m.status === "queued" || m.status === "sending");
}

/**
 * Which kept conversations stay: those in the window, newest first, as many
 * as fit. Returns the ids of the rest.
 */
export function overflow(threads: StoredThread[], now: number): string[] {
  const cutoff = now - MAIL_DAYS * DAY_MS;
  const sorted = [...threads].sort((a, b) => Date.parse(b.at) - Date.parse(a.at));
  const drop: string[] = [];
  let used = 0;
  for (const t of sorted) {
    if (Date.parse(t.at) < cutoff || used + t.bytes > MAIL_MAX_BYTES) {
      drop.push(t.id);
      continue;
    }
    used += t.bytes;
  }
  return drop;
}

export interface ThreadParams {
  status?: string;
  integration_id?: string;
  contact_id?: string;
  organisation_id?: string;
  enquiry_id?: string;
  booking_id?: string;
  assigned_to?: string;
  unlinked?: boolean;
  unread?: boolean;
  q?: string;
  offset?: number;
  limit?: number;
}

/** The conversations kept on the device that a list would show, the way the API filters them. */
export function localThreads(threads: ThreadView[], p: ThreadParams): { items: ThreadView[]; total: number } {
  const q = p.q?.trim().toLowerCase();
  const found = threads
    .filter(
      (t) =>
        (!p.status || t.status === p.status) &&
        (!p.integration_id || t.integration_id === p.integration_id) &&
        (!p.contact_id || t.contact_id === p.contact_id) &&
        (!p.organisation_id || t.organisation_id === p.organisation_id) &&
        (!p.enquiry_id || t.enquiry_id === p.enquiry_id) &&
        (!p.booking_id || t.booking_id === p.booking_id) &&
        (!p.assigned_to || t.assigned_to === p.assigned_to) &&
        (!p.unlinked || (!t.contact_id && !t.enquiry_id && !t.booking_id)) &&
        (!p.unread || t.unread) &&
        (!q || [t.subject ?? "", ...t.participants].join(" ").toLowerCase().includes(q)),
    )
    .sort((a, b) => Date.parse(b.last_message_at) - Date.parse(a.last_message_at) || b.id.localeCompare(a.id));
  const offset = p.offset ?? 0;
  const limit = Math.min(Math.max(p.limit ?? 50, 1), 200);
  return { items: found.slice(offset, offset + limit), total: found.length };
}

type Req = () => { headers: Record<string, string>; signal: AbortSignal };

const gone = (e: unknown) => e instanceof ApiFailure && (e.status === 403 || e.status === 404);

/** One workspace's email on this device. */
export class MailCopy {
  constructor(
    readonly ws: string,
    private readonly req: Req,
  ) {}

  async enabled(): Promise<boolean> {
    return (await getMeta<boolean>(keepMailKey(this.ws))) === true;
  }

  /** Turns keeping email on or off for this device; off deletes what was kept. */
  async setEnabled(on: boolean): Promise<void> {
    await setMeta(keepMailKey(this.ws), on);
    await setMeta(listedKey(this.ws), 0);
    if (!on) await clearMail(this.ws);
  }

  /** How much is kept. */
  async usage(): Promise<{ bytes: number; threads: number }> {
    const all = await allMail(this.ws);
    return { bytes: all.reduce((n, t) => n + t.bytes, 0), threads: all.length };
  }

  async read(id: string): Promise<KeptDetail | undefined> {
    if (!(await this.enabled())) return undefined;
    return (await getMail(this.ws, id))?.detail as KeptDetail | undefined;
  }

  /** The kept conversations a list would show; undefined when nothing is kept. */
  async list(p: ThreadParams): Promise<{ items: ThreadView[]; total: number } | undefined> {
    if (!(await this.enabled()) || !(await getMeta<number>(listedKey(this.ws)))) return undefined;
    const all = await allMail(this.ws);
    return localThreads(
      all.map((t) => (t.detail as KeptDetail).thread),
      p,
    );
  }

  /** Keeps a conversation the person just opened, when it is one the device keeps. */
  async remember(detail: ThreadDetail, now = Date.now()): Promise<void> {
    if (!(await this.enabled())) return;
    if (Date.parse(detail.thread.last_message_at) < now - MAIL_DAYS * DAY_MS) return;
    await putMail(storedThread(this.ws, detail));
    await this.fit(now);
  }

  /** Reads one conversation again, after a reply to it went. */
  async reread(id: string): Promise<void> {
    if (await this.enabled()) await this.refresh(new Set([id]));
  }

  /**
   * Brings the kept email up to date: the whole list when it is due, and
   * the conversations in `touched` (named by the changes feed). Returns
   * true when anything changed.
   */
  async refresh(touched: Set<string>, now = Date.now()): Promise<boolean> {
    if (!(await this.enabled())) return false;
    const stored = new Map((await allMail(this.ws)).map((t) => [t.id, t]));
    const cutoff = now - MAIL_DAYS * DAY_MS;
    const due = now - ((await getMeta<number>(listedKey(this.ws))) ?? 0) > LIST_EVERY_MS;
    const toRead = new Set<string>();
    let changed = false;
    if (due) {
      const listed = await this.recent(cutoff);
      if (listed === null) {
        // No longer allowed to read the inbox.
        await clearMail(this.ws);
        await setMeta(listedKey(this.ws), now);
        return stored.size > 0;
      }
      const seen = new Set(listed.map((t) => t.id));
      const missing = [...stored.keys()].filter((id) => !seen.has(id));
      if (missing.length) {
        await deleteMail(this.ws, missing);
        for (const id of missing) stored.delete(id);
        changed = true;
      }
      for (const t of listed) {
        const s = stored.get(t.id);
        if (!s || s.version !== t.version) toRead.add(t.id);
      }
    }
    for (const id of touched) toRead.add(id);
    for (const s of stored.values()) if (unsettled(s)) toRead.add(s.id);

    let reads = 0;
    for (const id of toRead) {
      if (reads === READS_PER_RUN) break;
      reads++;
      this.req().signal.throwIfAborted();
      try {
        const detail = await ok(api.GET("/api/v1/inbox/threads/{id}", { ...this.req(), params: { path: { id } } }));
        if (Date.parse(detail.thread.last_message_at) >= cutoff) {
          await putMail(storedThread(this.ws, detail));
          changed = true;
        } else if (stored.has(id)) {
          await deleteMail(this.ws, [id]);
          changed = true;
        }
      } catch (e) {
        if (!gone(e)) throw e;
        if (stored.has(id)) {
          await deleteMail(this.ws, [id]);
          changed = true;
        }
      }
    }
    // Listed again on the next run until every conversation has been read.
    if (due && reads === toRead.size) await setMeta(listedKey(this.ws), now);
    return (await this.fit(now)) || changed;
  }

  /** Lets go of what is too old, or does not fit. */
  private async fit(now: number): Promise<boolean> {
    const drop = overflow(await allMail(this.ws), now);
    if (drop.length) await deleteMail(this.ws, drop);
    return drop.length > 0;
  }

  /** The conversations with news in the window, newest first; null when the inbox is not this person's to read. */
  private async recent(cutoff: number): Promise<ThreadView[] | null> {
    const found: ThreadView[] = [];
    for (let offset = 0; found.length < MAX_THREADS; offset += PAGE) {
      let page;
      try {
        page = await ok(api.GET("/api/v1/inbox/threads", { ...this.req(), params: { query: { offset, limit: PAGE } } }));
      } catch (e) {
        if (e instanceof ApiFailure && e.status === 403) return null;
        throw e;
      }
      const recent = page.items.filter((t) => Date.parse(t.last_message_at) >= cutoff);
      found.push(...recent);
      if (recent.length < page.items.length || page.items.length < PAGE) break;
    }
    return found.slice(0, MAX_THREADS);
  }
}
