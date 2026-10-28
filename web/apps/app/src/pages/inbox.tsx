// The inbox: conversations read from the team's connected mailboxes (and
// messages written here), and texts on the workspace's SMS and WhatsApp
// numbers, linked to the contacts, enquiries and bookings they are about.
// Replies go out from the mailbox the conversation is in, or through
// OnHold's own sender when there is none; texts go from the number.

import type { Schemas } from "@onhold/api-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import {
  Alert,
  Badge,
  Button,
  Card,
  Checkbox,
  Dialog,
  EmptyState,
  Input,
  Label,
  Menu,
  MenuContent,
  MenuItem,
  MenuTrigger,
  NativeSelect,
  Spinner,
  Textarea,
  cn,
  relativeTime,
} from "@onhold/ui";
import { AlarmClock, ArrowLeft, CheckCheck, Mail, MailOpen, MessageSquare, Paperclip, PenSquare, RotateCcw, Send, Settings, Sparkles } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import {
  NotOnDevice,
  fetchBookings,
  fetchEnquiries,
  fetchMembers,
  fetchThread,
  fetchThreads,
  sendReply,
  useOffline,
} from "../data.ts";
import { type KeptDetail, replyRecipients } from "../offline/mail.ts";
import { isNetworkError } from "../offline/sync.ts";
import { useSession } from "../session.tsx";
import { saveDownload } from "./finance.tsx";
import { RecordPicker } from "./pickers.tsx";

export type ThreadView = Schemas["ThreadView"];
export type ThreadDetail = Schemas["ThreadDetail"];
type MessageView = Schemas["MessageView"];
type Mailbox = Schemas["IntegrationView"];

function failure(e: unknown): string {
  if (isNetworkError(e)) return "You are offline. This needs a connection; replies can still be written and go when you are back online.";
  return e instanceof ApiFailure || e instanceof Error ? e.message : "Something went wrong.";
}

/** Offline, with no email kept on this device: how to change that. */
function NotKept({ className }: { className?: string }) {
  return (
    <Alert tone="info" className={className}>
      You are offline, and this email is not kept on this device.{" "}
      <Link to="/settings/email" search={{}} className="underline">
        Keep recent email on this device
      </Link>{" "}
      to read and answer it without a connection.
    </Alert>
  );
}

const STATUSES = ["open", "snoozed", "resolved"] as const;
type Status = (typeof STATUSES)[number];

const CHANNELS = ["email", "sms", "whatsapp"] as const;
type Channel = (typeof CHANNELS)[number];
const CHANNEL_NAMES: Record<string, string> = { email: "Email", sms: "SMS", whatsapp: "WhatsApp" };

/** How long after their last message WhatsApp lets a business write freely. */
const WHATSAPP_WINDOW_MS = 24 * 60 * 60 * 1000;

export interface InboxSearch {
  status?: Status;
  channel?: Channel;
  mailbox?: string;
  unlinked?: boolean;
  unread?: boolean;
  q?: string;
  thread?: string;
}

const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);
const yes = (v: unknown) => (v === true || v === "true" ? true : undefined);

export function validateInboxSearch(s: Record<string, unknown>): InboxSearch {
  return {
    status: STATUSES.includes(s.status as Status) ? (s.status as Status) : undefined,
    channel: CHANNELS.includes(s.channel as Channel) ? (s.channel as Channel) : undefined,
    mailbox: str(s.mailbox),
    unlinked: yes(s.unlinked),
    unread: yes(s.unread),
    q: str(s.q),
    thread: str(s.thread),
  };
}

/** Whether the signed-in member can reply, link and change conversations. */
function useCanWrite(): boolean {
  return useSession().workspace.role !== "viewer";
}

/** Whether the signed-in member's role may make enquiries (finance may not). */
function useCanMakeEnquiry(): boolean {
  const role = useSession().workspace.role;
  return role === "owner" || role === "admin" || role === "agent";
}

export function useMailSetup() {
  return useQuery({ queryKey: ["mailboxes"], queryFn: () => ok(api.GET("/api/v1/inbox/mailboxes")) });
}

/** The workspace's SMS and WhatsApp numbers. */
export function useNumbers() {
  return useQuery({ queryKey: ["numbers"], queryFn: async () => (await ok(api.GET("/api/v1/inbox/numbers"))).channels });
}

/** A conversation's heading: its subject, or for a text, which kind. */
function heading(t: ThreadView): string {
  if (t.channel !== "email") return `${CHANNEL_NAMES[t.channel] ?? "Text"} with ${who(t)}`;
  return t.subject ?? "(no subject)";
}

function useMembers() {
  const sync = useOffline();
  return useQuery({ queryKey: ["members"], queryFn: async () => (await fetchMembers(sync)).data });
}

/** Who a conversation is with, for the list. */
function who(t: ThreadView): string {
  if (t.participants.length === 0) return "(nobody)";
  return t.participants.length > 2 ? `${t.participants.slice(0, 2).join(", ")} +${t.participants.length - 2}` : t.participants.join(", ");
}

export function InboxPage() {
  const sync = useOffline();
  const search = useSearch({ from: "/app/inbox" });
  const navigate = useNavigate({ from: "/inbox" });
  const setup = useMailSetup();
  const canWrite = useCanWrite();
  const [q, setQ] = useState(search.q ?? "");
  const numbers = useNumbers();
  const [composing, setComposing] = useState(false);
  const [texting, setTexting] = useState(false);
  const status = search.status ?? "open";
  const threads = useQuery({
    queryKey: ["threads", status, search.channel, search.mailbox, search.unlinked, search.unread, search.q],
    queryFn: () =>
      fetchThreads(sync, {
        status,
        channel: search.channel,
        integration_id: search.mailbox,
        unlinked: search.unlinked,
        unread: search.unread,
        q: search.q,
        limit: 100,
      }),
    refetchInterval: 60_000,
  });
  const set = (patch: Partial<InboxSearch>) => void navigate({ search: (s) => ({ ...s, ...patch }) });
  const needsAttention = setup.data?.mailboxes.filter((m) => m.status !== "active") ?? [];

  return (
    <div className="flex h-[calc(100dvh-3.5rem)] min-h-0 flex-col">
      <div className="flex flex-wrap items-center gap-2 border-b p-3">
        <h1 className="mr-2 text-xl font-semibold">Inbox</h1>
        <div role="tablist" aria-label="Status" className="flex rounded-md border p-0.5 text-sm">
          {STATUSES.map((s) => (
            <button
              key={s}
              type="button"
              role="tab"
              aria-selected={status === s}
              className={cn("rounded px-2.5 py-1 capitalize", status === s && "bg-secondary font-medium")}
              onClick={() => set({ status: s === "open" ? undefined : s, thread: undefined })}
            >
              {s}
            </button>
          ))}
        </div>
        <form
          className="flex min-w-40 flex-1"
          onSubmit={(e) => {
            e.preventDefault();
            set({ q: q.trim() || undefined });
          }}
        >
          <Input aria-label="Search conversations" placeholder="Search subject or people…" value={q} onChange={(e) => setQ(e.target.value)} />
        </form>
        {((numbers.data?.length ?? 0) > 0 || search.channel) && (
          <NativeSelect aria-label="Channel" className="w-auto" value={search.channel ?? ""} onChange={(e) => set({ channel: (e.target.value || undefined) as Channel | undefined })}>
            <option value="">Email and texts</option>
            {CHANNELS.map((c) => (
              <option key={c} value={c}>
                {CHANNEL_NAMES[c]}
              </option>
            ))}
          </NativeSelect>
        )}
        {(setup.data?.mailboxes.length ?? 0) > 1 && (
          <NativeSelect aria-label="Mailbox" className="w-auto" value={search.mailbox ?? ""} onChange={(e) => set({ mailbox: e.target.value || undefined })}>
            <option value="">All mailboxes</option>
            {setup.data?.mailboxes.map((m) => (
              <option key={m.id} value={m.id}>
                {m.display_name ?? m.mailbox}
              </option>
            ))}
          </NativeSelect>
        )}
        <label className="flex items-center gap-1.5 text-sm">
          <Checkbox checked={search.unread ?? false} onChange={(e) => set({ unread: e.target.checked || undefined })} /> Unread
        </label>
        <label className="flex items-center gap-1.5 text-sm">
          <Checkbox checked={search.unlinked ?? false} onChange={(e) => set({ unlinked: e.target.checked || undefined })} /> Not linked
        </label>
        {canWrite && (
          <Button size="sm" onClick={() => setComposing(true)}>
            <PenSquare /> New message
          </Button>
        )}
        {canWrite && (numbers.data?.length ?? 0) > 0 && (
          <Button size="sm" variant="outline" onClick={() => setTexting(true)}>
            <MessageSquare /> New text
          </Button>
        )}
      </div>
      {needsAttention.length > 0 && (
        <Alert tone="info" className="m-3 mb-0">
          {needsAttention.map((m) => m.mailbox).join(", ")} {needsAttention.length === 1 ? "needs" : "need"} attention.{" "}
          <Link to="/settings/email" search={{}} className="underline">
            Check email accounts
          </Link>
        </Alert>
      )}
      <div className="flex min-h-0 flex-1">
        <section
          aria-label="Conversations"
          className={cn("min-h-0 w-full overflow-y-auto border-r md:w-96 md:shrink-0", search.thread && "hidden md:block")}
        >
          {threads.data?.offline && (
            <p className="border-b bg-muted px-3 py-2 text-xs text-muted-foreground">
              Offline: the last two weeks of email kept on this device.
            </p>
          )}
          {threads.isPending ? (
            <div className="flex justify-center p-6">
              <Spinner />
            </div>
          ) : threads.isError ? (
            threads.error instanceof NotOnDevice ? (
              <NotKept className="m-3" />
            ) : (
              <Alert className="m-3">{threads.error.message}</Alert>
            )
          ) : threads.data.data.items.length === 0 ? (
            <EmptyInbox setup={setup.data} filtered={Boolean(search.q || search.unlinked || search.unread || search.mailbox || search.channel) || status !== "open"} />
          ) : (
            <ul className="divide-y">
              {threads.data.data.items.map((t) => (
                <li key={t.id}>
                  <button
                    type="button"
                    onClick={() => set({ thread: t.id })}
                    className={cn(
                      "flex w-full flex-col gap-0.5 px-3 py-2.5 text-left text-sm hover:bg-accent",
                      search.thread === t.id && "bg-accent",
                    )}
                  >
                    <span className="flex items-center gap-2">
                      {t.unread && <span className="size-2 shrink-0 rounded-full bg-primary" aria-label="Unread" />}
                      <span className={cn("min-w-0 flex-1 truncate", t.unread && "font-semibold")}>{who(t)}</span>
                      <span className="shrink-0 text-xs text-muted-foreground">{relativeTime(t.last_message_at)}</span>
                    </span>
                    <span className={cn("flex items-center gap-1.5 truncate", t.unread && "font-medium")}>
                      {t.channel !== "email" && <Badge className="border text-xs">{CHANNEL_NAMES[t.channel]}</Badge>}
                      <span className="truncate">{t.channel === "email" ? (t.subject ?? "(no subject)") : t.preview}</span>
                    </span>
                    {t.channel === "email" && <span className="truncate text-xs text-muted-foreground">{t.preview}</span>}
                    {!t.contact_id && !t.enquiry_id && !t.booking_id && (
                      <span className="text-xs text-warning">Not linked yet</span>
                    )}
                  </button>
                </li>
              ))}
            </ul>
          )}
          {threads.data && threads.data.data.total > threads.data.data.items.length && (
            <p className="p-3 text-xs text-muted-foreground">
              Showing the latest {threads.data.data.items.length} of {threads.data.data.total}. Search to find older ones.
            </p>
          )}
        </section>
        <section aria-label="Conversation" className={cn("min-h-0 flex-1 overflow-y-auto", !search.thread && "hidden md:block")}>
          {search.thread ? (
            <ThreadPane key={search.thread} id={search.thread} onBack={() => set({ thread: undefined })} />
          ) : (
            <p className="p-6 text-sm text-muted-foreground">Choose a conversation.</p>
          )}
        </section>
      </div>
      {composing && setup.data && (
        <ComposeDialog
          mailboxes={setup.data.mailboxes}
          onClose={() => setComposing(false)}
          onSent={(d) => {
            setComposing(false);
            set({ thread: d.thread.id, status: undefined });
          }}
        />
      )}
      {texting && numbers.data && (
        <TextDialog
          numbers={numbers.data}
          onClose={() => setTexting(false)}
          onSent={(d) => {
            setTexting(false);
            set({ thread: d.thread.id, status: undefined });
          }}
        />
      )}
    </div>
  );
}

function EmptyInbox({ setup, filtered }: { setup?: Schemas["MailSetup"]; filtered: boolean }) {
  if (filtered) return <p className="p-6 text-sm text-muted-foreground">No conversations here.</p>;
  if (setup && setup.mailboxes.length === 0) {
    return (
      <EmptyState title="Connect your email">
        <div className="flex flex-col items-center gap-3">
          <Mail className="size-6" />
          <p>Connect Gmail, Outlook or any other mailbox and enquiries arrive here, linked to the people and bookings they are about.</p>
          <Link to="/settings/email" search={{}}>
            <Button>
              <Settings /> Connect a mailbox
            </Button>
          </Link>
        </div>
      </EmptyState>
    );
  }
  return <p className="p-6 text-sm text-muted-foreground">Nothing waiting. New mail shows up here within a couple of minutes.</p>;
}

// One conversation ---------------------------------------------------------------------

function useThread(id: string) {
  const sync = useOffline();
  return useQuery({
    queryKey: ["thread", id],
    queryFn: () => fetchThread(sync, id),
    // Replies are sent in the background: look again while one is on its way.
    refetchInterval: (q) =>
      q.state.data?.data.messages.some((m) => m.status === "queued" || m.status === "sending") ? 3000 : 60_000,
  });
}

/** Replies written to this conversation on this device that are waiting for a connection. */
function useWaitingReplies(id: string) {
  const sync = useOffline();
  const qc = useQueryClient();
  const waiting = useQuery({
    queryKey: ["outbox", sync?.ws],
    queryFn: () => sync?.queued() ?? Promise.resolve([]),
    enabled: !!sync,
    select: (items) => items.filter((i) => i.entity === "reply" && i.id === id),
  });
  // The queue changes as the device works through it; a reply that went shows in the conversation.
  useEffect(() => {
    if (!sync) return;
    let pending = sync.current.pending;
    return sync.subscribe((s) => {
      void qc.invalidateQueries({ queryKey: ["outbox", sync.ws] });
      if (s.pending < pending) void qc.invalidateQueries({ queryKey: ["thread", id] });
      pending = s.pending;
    });
  }, [sync, qc, id]);
  return waiting.data ?? [];
}

function snoozeTimes(): { label: string; at: Date }[] {
  const now = new Date();
  const later = new Date(now.getTime() + 3 * 3600_000);
  const tomorrow = new Date(now);
  tomorrow.setDate(now.getDate() + 1);
  tomorrow.setHours(9, 0, 0, 0);
  const nextWeek = new Date(now);
  nextWeek.setDate(now.getDate() + ((8 - now.getDay()) % 7 || 7));
  nextWeek.setHours(9, 0, 0, 0);
  return [
    { label: "In 3 hours", at: later },
    { label: "Tomorrow morning", at: tomorrow },
    { label: "Next Monday", at: nextWeek },
  ];
}

export function ThreadPane({ id, onBack }: { id: string; onBack?: () => void }) {
  const qc = useQueryClient();
  const detail = useThread(id);
  const members = useMembers();
  const canWrite = useCanWrite();
  const canMakeEnquiry = useCanMakeEnquiry();
  const [error, setError] = useState<string | null>(null);
  const [making, setMaking] = useState(false);
  const marked = useRef(false);

  const waiting = useWaitingReplies(id);
  const patch = async (body: Schemas["ThreadPatch"]) => {
    setError(null);
    try {
      const d = await ok(api.PATCH("/api/v1/inbox/threads/{id}", { params: { path: { id } }, body }));
      qc.setQueryData(["thread", id], { data: d, offline: false });
      void qc.invalidateQueries({ queryKey: ["threads"] });
    } catch (e) {
      setError(failure(e));
    }
  };

  // Opening a conversation marks it read (once online, if it was opened offline).
  useEffect(() => {
    if (detail.data?.data.thread.unread && !detail.data.offline && !marked.current) {
      marked.current = true;
      void patch({ unread: false });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detail.data?.data.thread.unread, detail.data?.offline]);

  if (detail.isPending) {
    return (
      <div className="flex justify-center p-6">
        <Spinner />
      </div>
    );
  }
  if (detail.isError) {
    return detail.error instanceof NotOnDevice ? <NotKept className="m-4" /> : <Alert className="m-4">{detail.error.message}</Alert>;
  }
  const { thread, messages, mailbox, left_out, shortened }: KeptDetail = detail.data.data;
  const offline = detail.data.offline;

  return (
    <article className="mx-auto flex max-w-4xl flex-col gap-4 p-4">
      <header className="flex flex-col gap-3">
        <div className="flex items-start gap-2">
          {onBack && (
            <Button variant="ghost" size="icon" className="md:hidden" aria-label="Back to conversations" onClick={onBack}>
              <ArrowLeft />
            </Button>
          )}
          <h2 className="min-w-0 flex-1 text-lg font-semibold break-words">{heading(thread)}</h2>
          {thread.status === "snoozed" && thread.snoozed_until && (
            <Badge className="border">Back {new Date(thread.snoozed_until).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}</Badge>
          )}
          {thread.status === "resolved" && <Badge className="border">Resolved</Badge>}
        </div>
        {canWrite && (
          <div className="flex flex-wrap items-center gap-2">
            {thread.status === "resolved" ? (
              <Button size="sm" variant="outline" onClick={() => patch({ status: "open" })}>
                <RotateCcw /> Reopen
              </Button>
            ) : (
              <Button size="sm" variant="outline" onClick={() => patch({ status: "resolved" })}>
                <CheckCheck /> Resolve
              </Button>
            )}
            <Menu>
              <MenuTrigger asChild>
                <Button size="sm" variant="outline">
                  <AlarmClock /> Snooze
                </Button>
              </MenuTrigger>
              <MenuContent>
                {snoozeTimes().map((t) => (
                  <MenuItem key={t.label} onSelect={() => patch({ status: "snoozed", snoozed_until: t.at.toISOString() })}>
                    {t.label}
                  </MenuItem>
                ))}
              </MenuContent>
            </Menu>
            <Button size="sm" variant="outline" onClick={() => patch({ unread: true })}>
              <MailOpen /> Mark unread
            </Button>
            <NativeSelect
              aria-label="Assigned to"
              className="h-8 w-auto text-sm"
              value={thread.assigned_to ?? ""}
              onChange={(e) => patch({ assigned_to: e.target.value || null })}
            >
              <option value="">Not assigned</option>
              {members.data?.map((m) => (
                <option key={m.user_id} value={m.user_id}>
                  {m.name}
                </option>
              ))}
            </NativeSelect>
            {canMakeEnquiry && !thread.enquiry_id && !thread.booking_id && (
              <Button size="sm" onClick={() => setMaking(true)}>
                <Sparkles /> Make an enquiry
              </Button>
            )}
          </div>
        )}
        <Links thread={thread} canWrite={canWrite} onLink={patch} />
        {offline && (
          <Alert tone="info">
            Offline: the copy kept on this device, as text only.
            {shortened && " Earlier messages quoted in replies are left out."} Attachments open once you are back online.
          </Alert>
        )}
        {error && <Alert>{error}</Alert>}
      </header>
      {offline && left_out ? (
        <p className="text-xs text-muted-foreground">
          {left_out} earlier {left_out === 1 ? "message is" : "messages are"} not kept on this device.
        </p>
      ) : null}
      <ol className="flex flex-col gap-3">
        {messages.map((m) => (
          <li key={m.id}>
            <MessageCard threadId={thread.id} message={m} offline={offline} />
          </li>
        ))}
        {waiting.map((w) => (
          <li key={w.seq}>
            <Card className="flex flex-col gap-2 border-primary/30 bg-primary/5 p-4 text-sm">
              <div className="flex flex-wrap items-baseline gap-2">
                <span className="font-medium">You</span>
                {Array.isArray(w.body.to) && <span className="text-muted-foreground">to {(w.body.to as string[]).join(", ")}</span>}
                <Badge className="border">{w.state === "pending" ? "Waiting for a connection" : "Not sent"}</Badge>
                <span className="ml-auto text-xs text-muted-foreground">{relativeTime(w.createdAt)}</span>
              </div>
              {w.state !== "pending" && (
                <Alert>
                  {w.error ?? "The server did not take this reply."}{" "}
                  <Link to="/sync" className="underline">
                    Sort it out on the Sync page
                  </Link>
                </Alert>
              )}
              <p className="whitespace-pre-wrap break-words">{String(w.body.body ?? "")}</p>
            </Card>
          </li>
        ))}
      </ol>
      {canWrite &&
        (thread.channel === "email" ? (
          <ReplyBox thread={thread} mailbox={mailbox ?? null} to={replyRecipients(messages)} />
        ) : (
          <TextReplyBox thread={thread} />
        ))}
      {making && (
        <MakeEnquiryDialog
          thread={thread}
          onClose={() => setMaking(false)}
          onMade={() => {
            setMaking(false);
            void qc.invalidateQueries({ queryKey: ["thread", id] });
            void qc.invalidateQueries({ queryKey: ["threads"] });
          }}
        />
      )}
    </article>
  );
}

/** What the conversation is linked to, with pickers to change it. */
function Links({
  thread,
  canWrite,
  onLink,
}: {
  thread: ThreadView;
  canWrite: boolean;
  onLink: (body: Schemas["ThreadPatch"]) => Promise<void>;
}) {
  const [linking, setLinking] = useState(false);
  const chips: { label: string; node: React.ReactNode }[] = [];
  if (thread.contact_id)
    chips.push({ label: "Contact", node: <Link to="/crm/$entity/$id" params={{ entity: "contacts", id: thread.contact_id }}>Contact</Link> });
  if (thread.organisation_id)
    chips.push({
      label: "Organisation",
      node: <Link to="/crm/$entity/$id" params={{ entity: "organisations", id: thread.organisation_id }}>Organisation</Link>,
    });
  if (thread.enquiry_id) chips.push({ label: "Enquiry", node: <Link to="/enquiries/$id" params={{ id: thread.enquiry_id }}>Enquiry</Link> });
  if (thread.booking_id) chips.push({ label: "Booking", node: <Link to="/bookings/$id" params={{ id: thread.booking_id }}>Booking</Link> });
  return (
    <div className="flex flex-wrap items-center gap-2 text-sm">
      {chips.length === 0 ? (
        <span className="text-muted-foreground">Not linked to a contact, enquiry or booking.</span>
      ) : (
        chips.map((c) => (
          <Badge key={c.label} className="border underline-offset-2 hover:underline">
            {c.node}
          </Badge>
        ))
      )}
      {canWrite && !linking && (
        <button type="button" className="text-sm underline" onClick={() => setLinking(true)}>
          {chips.length ? "Change links" : "Link"}
        </button>
      )}
      {linking && (
        <Card className="flex w-full flex-col gap-2 p-3">
          <Label htmlFor="link-contact">Contact</Label>
          <RecordPicker
            id="link-contact"
            entity="contact"
            value={thread.contact_id ? { id: thread.contact_id, name: "Linked contact" } : null}
            onChange={(p) => void onLink({ contact_id: p?.id ?? null })}
          />
          <Label htmlFor="link-enquiry">Enquiry</Label>
          <WorkPicker
            id="link-enquiry"
            kind="enquiry"
            linked={Boolean(thread.enquiry_id)}
            onChange={(picked) => void onLink({ enquiry_id: picked })}
          />
          <Label htmlFor="link-booking">Booking</Label>
          <WorkPicker
            id="link-booking"
            kind="booking"
            linked={Boolean(thread.booking_id)}
            onChange={(picked) => void onLink({ booking_id: picked })}
          />
          <div>
            <Button size="sm" variant="outline" onClick={() => setLinking(false)}>
              Done
            </Button>
          </div>
        </Card>
      )}
    </div>
  );
}

/** Finds an enquiry or booking to link, or unlinks the one linked. */
function WorkPicker({
  id,
  kind,
  linked,
  onChange,
}: {
  id: string;
  kind: "enquiry" | "booking";
  linked: boolean;
  onChange: (picked: string | null) => void;
}) {
  const sync = useOffline();
  const [q, setQ] = useState("");
  const found = useQuery({
    queryKey: ["pick", kind, q],
    queryFn: async () =>
      kind === "enquiry" ? (await fetchEnquiries(sync, { q, limit: 8 })).data.items : (await fetchBookings(sync, { q, limit: 8 })).data.items,
    enabled: q.trim().length > 0,
  });
  if (linked) {
    return (
      <div className="flex h-9 items-center justify-between rounded-md border px-3 text-sm">
        <span id={id}>Linked</span>
        <button type="button" className="underline" onClick={() => onChange(null)}>
          Unlink
        </button>
      </div>
    );
  }
  return (
    <div className="relative">
      <Input id={id} placeholder={`Search ${kind === "enquiry" ? "enquiries" : "bookings"}…`} value={q} onChange={(e) => setQ(e.target.value)} autoComplete="off" />
      {q.trim() && (
        <ul className="absolute z-20 mt-1 max-h-56 w-full overflow-y-auto rounded-md border bg-card text-sm shadow-md">
          {(found.data ?? []).map((r) => (
            <li key={r.id}>
              <button
                type="button"
                className="w-full px-3 py-2 text-left hover:bg-accent"
                onClick={() => {
                  setQ("");
                  onChange(r.id);
                }}
              >
                {r.title}
              </button>
            </li>
          ))}
          {found.data && found.data.length === 0 && <li className="p-2 text-muted-foreground">Nothing matches.</li>}
        </ul>
      )}
    </div>
  );
}

/** A message's HTML, already cleaned by the server, shown where nothing in it can run. */
export function MailBody({ html }: { html: string }) {
  const frame = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(120);
  const doc = useMemo(
    () =>
      `<!doctype html><html><head><meta charset="utf-8">` +
      `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'">` +
      `<base target="_blank"><style>body{margin:0;font:14px/1.5 system-ui,sans-serif;color:#111;word-wrap:break-word}` +
      `img{max-width:100%;height:auto}blockquote{margin:0 0 0 .5rem;padding-left:.5rem;border-left:2px solid #ccc;color:#555}</style>` +
      `</head><body>${html}</body></html>`,
    [html],
  );
  return (
    <iframe
      ref={frame}
      title="Message"
      // No scripts ever run inside; same origin only so the page can size it.
      sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"
      srcDoc={doc}
      className="w-full rounded bg-white"
      style={{ height }}
      onLoad={() => {
        const body = frame.current?.contentDocument?.body;
        if (body) setHeight(Math.min(Math.max(body.scrollHeight + 16, 60), 4000));
      }}
    />
  );
}

function MessageCard({ threadId, message: m, offline }: { threadId: string; message: MessageView; offline: boolean }) {
  const [error, setError] = useState<string | null>(null);
  const outbound = m.direction === "outbound";
  return (
    <Card className={cn("flex flex-col gap-2 p-4 text-sm", outbound && "border-primary/30 bg-primary/5", m.automatic && "opacity-75")}>
      <div className="flex flex-wrap items-baseline gap-x-2">
        <span className="font-medium">{m.from_name ?? m.from_address ?? "OnHold"}</span>
        {m.from_name && m.from_address && <span className="text-muted-foreground">{m.from_address}</span>}
        <span className="ml-auto text-xs text-muted-foreground" title={new Date(m.sent_at).toLocaleString()}>
          {relativeTime(m.sent_at)}
        </span>
      </div>
      <div className="text-xs text-muted-foreground">
        To {m.to.join(", ") || "—"}
        {m.cc.length > 0 && ` · Copy ${m.cc.join(", ")}`}
      </div>
      <div className="flex flex-wrap gap-1">
        {(m.status === "queued" || m.status === "sending") && <Badge className="border">Sending…</Badge>}
        {m.status === "failed" && <Badge className="border-destructive text-destructive">Not sent</Badge>}
        {m.automatic && <Badge className="border">Automatic reply</Badge>}
        {m.bulk && <Badge className="border">Mailing list</Badge>}
      </div>
      {m.status === "failed" && m.error && <Alert>{m.error}</Alert>}
      {m.html ? <MailBody html={m.html} /> : <p className="whitespace-pre-wrap break-words">{m.text}</p>}
      {m.files.length > 0 && (
        <ul className="flex flex-wrap gap-2">
          {m.files.map((f) => (
              <li key={f.id}>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={offline}
                  title={offline ? "Opens once you are back online" : undefined}
                  onClick={() =>
                    saveDownload(
                      api.GET("/api/v1/inbox/threads/{id}/files/{file_id}", { params: { path: { id: threadId, file_id: f.id } }, parseAs: "blob" }),
                      f.filename,
                    ).catch((e: unknown) => setError(failure(e)))
                  }
                >
                  <Paperclip /> {f.filename}
                </Button>
              </li>
            ))}
        </ul>
      )}
      {error && <Alert>{error}</Alert>}
    </Card>
  );
}

function ReplyBox({ thread, mailbox, to }: { thread: ThreadView; mailbox: string | null; to: string[] }) {
  const qc = useQueryClient();
  const sync = useOffline();
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const send = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const { queued } = await sendReply(sync, thread.id, body, `Reply: ${thread.subject ?? "(no subject)"}`, to);
      setBody("");
      if (queued) {
        setNotice("Saved on this device. It sends when you are back online.");
        void qc.invalidateQueries({ queryKey: ["outbox", sync?.ws] });
      } else {
        void qc.invalidateQueries({ queryKey: ["thread", thread.id] });
        void qc.invalidateQueries({ queryKey: ["threads"] });
      }
    } catch (e) {
      setError(failure(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form
      className="flex flex-col gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        if (body.trim()) void send();
      }}
    >
      <Label htmlFor="reply">Reply</Label>
      <Textarea id="reply" rows={5} value={body} onChange={(e) => setBody(e.target.value)} placeholder="Write a reply…" />
      <div className="flex flex-wrap items-center gap-3">
        <Button type="submit" disabled={busy || !body.trim()}>
          <Send /> Send
        </Button>
        <span className="text-xs text-muted-foreground">
          {mailbox ? `Sends from ${mailbox}.` : "Sends through OnHold in your workspace's name; answers come to you."}
        </span>
      </div>
      {notice && <Alert tone="success">{notice}</Alert>}
      {error && <Alert>{error}</Alert>}
    </form>
  );
}

/** The longest text the server takes on a channel. */
function textLimit(channel: string): number {
  return channel === "sms" ? 1600 : 4096;
}

/** How many texts an SMS is sent as: 160 plain characters (153 when split), 70 (67) with any other. */
function smsParts(body: string): number {
  // The GSM 7-bit alphabet's common characters; anything else sends the text as Unicode.
  const plain = /^[A-Za-z0-9 \r\n@£$¥èéùìòÇØøÅåΔ_ΦΓΛΩΠΨΣΘΞÆæßÉ!"#¤%&'()*+,\-./:;<=>?¡ÄÖÑÜ§¿äöñüà^{}\\[~\]|€]*$/.test(body);
  const [one, many] = plain ? [160, 153] : [70, 67];
  return body.length <= one ? 1 : Math.ceil(body.length / many);
}

/** WhatsApp templates, for writing outside the 24-hour window. */
function useWhatsappTemplates(enabled: boolean) {
  return useQuery({
    queryKey: ["templates", "whatsapp"],
    queryFn: () => ok(api.GET("/api/v1/templates", { params: { query: { kind: "whatsapp" } } })),
    enabled,
  });
}

/** Whether WhatsApp lets the business write freely now: within a day of their last message. */
function windowOpen(thread: ThreadView): boolean {
  if (thread.channel !== "whatsapp") return true;
  return !!thread.last_inbound_at && Date.now() - Date.parse(thread.last_inbound_at) < WHATSAPP_WINDOW_MS;
}

function TextReplyBox({ thread }: { thread: ThreadView }) {
  const qc = useQueryClient();
  const sync = useOffline();
  const open = windowOpen(thread);
  const templates = useWhatsappTemplates(!open);
  const [body, setBody] = useState("");
  const [template, setTemplate] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const keys = [...new Set(templates.data?.map((t) => t.key) ?? [])];
  const done = () => {
    void qc.invalidateQueries({ queryKey: ["thread", thread.id] });
    void qc.invalidateQueries({ queryKey: ["threads"] });
  };
  const send = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      if (open) {
        const { queued } = await sendReply(sync, thread.id, body, heading(thread), []);
        setBody("");
        if (queued) {
          setNotice("Saved on this device. It sends when you are back online.");
          void qc.invalidateQueries({ queryKey: ["outbox", sync?.ws] });
        } else done();
      } else {
        await ok(api.POST("/api/v1/inbox/threads/{id}/reply", { params: { path: { id: thread.id } }, body: { template_key: template } }));
        setTemplate("");
        done();
      }
    } catch (e) {
      setError(failure(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form
      className="flex flex-col gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        if (open ? body.trim() : template) void send();
      }}
    >
      {open ? (
        <>
          <Label htmlFor="reply">Reply</Label>
          <Textarea id="reply" rows={3} maxLength={textLimit(thread.channel)} value={body} onChange={(e) => setBody(e.target.value)} placeholder="Write a text…" />
        </>
      ) : (
        <>
          <Alert tone="info">
            WhatsApp lets you write freely only within 24 hours of their last message. Until they write again, send one of your approved WhatsApp
            templates.
          </Alert>
          <Label htmlFor="reply-template">Template</Label>
          <NativeSelect id="reply-template" value={template} onChange={(e) => setTemplate(e.target.value)}>
            <option value="">Choose a template…</option>
            {keys.map((k) => (
              <option key={k} value={k}>
                {templates.data?.find((t) => t.key === k)?.name ?? k}
              </option>
            ))}
          </NativeSelect>
          {templates.data && keys.length === 0 && (
            <p className="text-xs text-muted-foreground">
              No WhatsApp templates yet. Add one in Settings, Text messages, named after the template Meta approved.
            </p>
          )}
        </>
      )}
      <div className="flex flex-wrap items-center gap-3">
        <Button type="submit" disabled={busy || (open ? !body.trim() : !template)}>
          <Send /> Send
        </Button>
        <span className="text-xs text-muted-foreground">
          {thread.channel === "sms" && smsParts(body) > 1 ? `${smsParts(body)} texts long. ` : ""}
          Sends from your {CHANNEL_NAMES[thread.channel]} number.
        </span>
      </div>
      {notice && <Alert tone="success">{notice}</Alert>}
      {error && <Alert>{error}</Alert>}
    </form>
  );
}

/** Texting someone new: a contact (their first number that takes texts) or a number. */
function TextDialog({
  numbers,
  onClose,
  onSent,
}: {
  numbers: Schemas["MessagingChannel"][];
  onClose: () => void;
  onSent: (d: ThreadDetail) => void;
}) {
  const qc = useQueryClient();
  const kinds = [...new Set(numbers.map((n) => n.kind))];
  const [channel, setChannel] = useState(kinds[0] ?? "sms");
  const [contact, setContact] = useState<{ id: string; name: string } | null>(null);
  const [to, setTo] = useState("");
  const [body, setBody] = useState("");
  const [template, setTemplate] = useState("");
  const whatsapp = channel === "whatsapp";
  const templates = useWhatsappTemplates(whatsapp);
  const keys = [...new Set(templates.data?.map((t) => t.key) ?? [])];
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="New text">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          setBusy(true);
          try {
            const d = await ok(
              api.POST("/api/v1/inbox/texts", {
                body: {
                  channel,
                  contact_id: contact?.id ?? null,
                  to: contact ? null : to.trim(),
                  body: whatsapp ? null : body,
                  template_key: whatsapp ? template : null,
                },
              }),
            );
            void qc.invalidateQueries({ queryKey: ["threads"] });
            onSent(d);
          } catch (err) {
            setError(failure(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        {kinds.length > 1 && (
          <>
            <Label htmlFor="text-channel">Send by</Label>
            <NativeSelect id="text-channel" value={channel} onChange={(e) => setChannel(e.target.value)}>
              {kinds.map((k) => (
                <option key={k} value={k}>
                  {CHANNEL_NAMES[k]}
                </option>
              ))}
            </NativeSelect>
          </>
        )}
        <Label htmlFor="text-contact">Contact</Label>
        <RecordPicker id="text-contact" entity="contact" value={contact} onChange={setContact} />
        {!contact && (
          <>
            <Label htmlFor="text-to">Or a number</Label>
            <Input id="text-to" type="tel" required value={to} onChange={(e) => setTo(e.target.value)} placeholder="+44 7700 900123" />
          </>
        )}
        <p className="text-xs text-muted-foreground">Someone who replied STOP cannot be texted until they reply START.</p>
        {whatsapp ? (
          <>
            <Label htmlFor="text-template">Template</Label>
            <NativeSelect id="text-template" required value={template} onChange={(e) => setTemplate(e.target.value)}>
              <option value="">Choose a template…</option>
              {keys.map((k) => (
                <option key={k} value={k}>
                  {templates.data?.find((t) => t.key === k)?.name ?? k}
                </option>
              ))}
            </NativeSelect>
            <p className="text-xs text-muted-foreground">WhatsApp conversations a business starts must open with a template Meta approved.</p>
          </>
        ) : (
          <>
            <Label htmlFor="text-body">Message</Label>
            <Textarea id="text-body" required rows={4} maxLength={textLimit(channel)} value={body} onChange={(e) => setBody(e.target.value)} />
          </>
        )}
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button type="button" variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy}>
            <Send /> Send
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

function MakeEnquiryDialog({ thread, onClose, onMade }: { thread: ThreadView; onClose: () => void; onMade: () => void }) {
  const navigate = useNavigate();
  const [title, setTitle] = useState(thread.subject ?? "");
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="Make an enquiry">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          try {
            const made = await ok(
              api.POST("/api/v1/inbox/threads/{id}/enquiry", {
                params: { path: { id: thread.id } },
                body: { title, contact_name: name.trim() || null },
              }),
            );
            onMade();
            void navigate({ to: "/enquiries/$id", params: { id: made.enquiry_id } });
          } catch (err) {
            setError(failure(err));
          }
        }}
      >
        <Label htmlFor="enquiry-title">Title</Label>
        <Input id="enquiry-title" required value={title} onChange={(e) => setTitle(e.target.value)} />
        {!thread.contact_id && (
          <>
            <Label htmlFor="enquiry-contact">New contact's name</Label>
            <Input id="enquiry-contact" placeholder="As the sender signs" value={name} onChange={(e) => setName(e.target.value)} />
            <p className="text-xs text-muted-foreground">The sender is added as a contact with their email address.</p>
          </>
        )}
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button type="button" variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit">Make enquiry</Button>
        </div>
      </form>
    </Dialog>
  );
}

function addresses(s: string): string[] {
  return s
    .split(/[,;\s]+/)
    .map((a) => a.trim())
    .filter(Boolean);
}

export function ComposeDialog({
  mailboxes,
  initial,
  onClose,
  onSent,
}: {
  mailboxes: Mailbox[];
  initial?: { to?: string; contact_id?: string; enquiry_id?: string; booking_id?: string; subject?: string };
  onClose: () => void;
  onSent: (d: ThreadDetail) => void;
}) {
  const { me } = useSession();
  const qc = useQueryClient();
  const usable = mailboxes.filter((m) => m.status !== "reconnect" && m.owner_id === me.user.id);
  const [from, setFrom] = useState(usable.length === 1 ? (usable[0]?.id ?? "") : "");
  const [to, setTo] = useState(initial?.to ?? "");
  const [cc, setCc] = useState("");
  const [subject, setSubject] = useState(initial?.subject ?? "");
  const [body, setBody] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="New message">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          setBusy(true);
          try {
            const d = await ok(
              api.POST("/api/v1/inbox/threads", {
                body: {
                  integration_id: from || null,
                  from_onhold: !from,
                  to: addresses(to),
                  cc: addresses(cc),
                  subject,
                  body,
                  contact_id: initial?.contact_id ?? null,
                  enquiry_id: initial?.enquiry_id ?? null,
                  booking_id: initial?.booking_id ?? null,
                },
              }),
            );
            void qc.invalidateQueries({ queryKey: ["threads"] });
            onSent(d);
          } catch (err) {
            setError(failure(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <Label htmlFor="compose-from">From</Label>
        <NativeSelect id="compose-from" value={from} onChange={(e) => setFrom(e.target.value)}>
          {usable.map((m) => (
            <option key={m.id} value={m.id}>
              {m.display_name ? `${m.display_name} <${m.mailbox}>` : m.mailbox}
            </option>
          ))}
          <option value="">OnHold, in your workspace's name</option>
        </NativeSelect>
        <Label htmlFor="compose-to">To</Label>
        <Input id="compose-to" required value={to} onChange={(e) => setTo(e.target.value)} placeholder="name@example.com" />
        {from && (
          <>
            <Label htmlFor="compose-cc">Copy to</Label>
            <Input id="compose-cc" value={cc} onChange={(e) => setCc(e.target.value)} />
          </>
        )}
        <Label htmlFor="compose-subject">Subject</Label>
        <Input id="compose-subject" required value={subject} onChange={(e) => setSubject(e.target.value)} />
        <Label htmlFor="compose-body">Message</Label>
        <Textarea id="compose-body" required rows={8} value={body} onChange={(e) => setBody(e.target.value)} />
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button type="button" variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy}>
            <Send /> Send
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

/** The conversations about one contact, organisation, enquiry or booking. */
export function RelatedConversations({ filter }: { filter: { contact_id?: string; organisation_id?: string; enquiry_id?: string; booking_id?: string } }) {
  const sync = useOffline();
  const threads = useQuery({
    queryKey: ["threads", "related", filter],
    queryFn: async () => (await fetchThreads(sync, { ...filter, limit: 20 })).data,
  });
  const [open, setOpen] = useState<string | null>(null);
  if (threads.isError) return null;
  return (
    <section className="flex flex-col gap-3" aria-label="Conversations">
      <div className="flex items-center justify-between">
        <h2 className="font-semibold">Conversations</h2>
        <Link to="/inbox" search={{}} className="text-sm underline">
          Inbox
        </Link>
      </div>
      {threads.isPending ? (
        <Spinner />
      ) : threads.data.items.length === 0 ? (
        <p className="text-sm text-muted-foreground">No conversations yet.</p>
      ) : (
        <ul className="divide-y rounded-md border">
          {threads.data.items.map((t) => (
            <li key={t.id} className="text-sm">
              <button type="button" className="flex w-full flex-col px-3 py-2 text-left hover:bg-accent" onClick={() => setOpen(open === t.id ? null : t.id)}>
                <span className="flex gap-2">
                  <span className={cn("min-w-0 flex-1 truncate", t.unread && "font-semibold")}>{heading(t)}</span>
                  <span className="shrink-0 text-xs text-muted-foreground">{relativeTime(t.last_message_at)}</span>
                </span>
                <span className="truncate text-xs text-muted-foreground">{t.preview}</span>
              </button>
              {open === t.id && (
                <div className="border-t">
                  <ThreadPane id={t.id} />
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
