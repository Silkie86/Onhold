// Holds and bookings: the list, placing a hold (or a booking outright) with a
// live clash check, and each booking with its status moves, clashes, quotes,
// tasks and history.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import type { Schemas } from "@onhold/api-client";
import {
  Alert,
  Badge,
  Button,
  Checkbox,
  Dialog,
  EntityForm,
  type EntitySchema,
  type FieldIssue,
  Input,
  Label,
  MoneyInput,
  NativeSelect,
  Spinner,
  Textarea,
  Timeline,
  type TimelineEntry,
  cn,
  formatMoney,
} from "@onhold/ui";
import { AlertTriangle, ArrowLeft, Plus, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import {
  changeStatus,
  checkClashes,
  createBooking,
  deleteRecord,
  fetchBookings,
  fetchRecord,
  updateBooking,
  useOffline,
  useSyncStatus,
} from "../data.ts";
import { isLocalId } from "../offline/store.ts";
import type { OfflineSync } from "../offline/sync.ts";
import { isNetworkError } from "../offline/sync.ts";
import { useSession } from "../session.tsx";
import {
  type Booking,
  type BookingStatus,
  type ClashReport,
  type Named,
  STATUS_LABEL,
  STATUS_STYLE,
  canOverride,
  formatWhen,
  fromLocalInput,
  nextStatuses,
  toLocalInput,
} from "../work.ts";
import { Loading, OfflineNote, PageError, failureIssues, useSchema, useWorkspaceCurrency } from "./crm.tsx";
import { RelationPicker, TalentPicker } from "./pickers.tsx";
import { QuotesPanel } from "./quotes.tsx";
import { BookingFinancePanel } from "./finance.tsx";
import { BookingDocumentsPanel } from "./documents.tsx";
import { RelatedTasks } from "./tasks.tsx";

type Money = Schemas["Money"];

export function StatusBadge({ status }: { status: BookingStatus }) {
  return <Badge className={cn("border", STATUS_STYLE[status])}>{STATUS_LABEL[status]}</Badge>;
}

/** A booking in a list: what, when, who, and its status. */
export function BookingCard({ booking: b }: { booking: Booking }) {
  return (
    <Link
      to="/bookings/$id"
      params={{ id: b.id }}
      className="flex flex-wrap items-center justify-between gap-2 rounded-md border p-3 text-sm hover:bg-accent"
    >
      <span className="flex min-w-0 flex-col">
        <span className="truncate font-medium">{b.title}</span>
        <span className="text-muted-foreground">{formatWhen(b.starts_at, b.ends_at, b.all_day)}</span>
        {b.talent.length > 0 && <span className="truncate">{b.talent.map((t) => t.name).join(", ")}</span>}
      </span>
      <StatusBadge status={b.status} />
    </Link>
  );
}

/** A record's history, newest first, a page at a time. */
export function RecordTimeline({
  path,
  id,
  schema,
  offline,
}: {
  path: "enquiries" | "bookings";
  id: string;
  schema?: EntitySchema;
  offline?: boolean;
}) {
  const [before, setBefore] = useState<string | null>(null);
  const [older, setOlder] = useState<TimelineEntry[]>([]);
  const timeline = useQuery({
    queryKey: ["timeline", path, id, before],
    queryFn: () =>
      ok(
        api.GET(`/api/v1/${path}/{id}/timeline` as "/api/v1/bookings/{id}/timeline", {
          params: { path: { id }, query: { before: before ?? undefined, limit: 50 } },
        }),
      ),
    enabled: !offline && !isLocalId(id),
    retry: (count, e) => !isNetworkError(e) && count < 2,
  });
  const entries = before ? [...older, ...(timeline.data?.entries ?? [])] : (timeline.data?.entries ?? []);
  const unreachable = offline || isLocalId(id) || (timeline.isError && isNetworkError(timeline.error));
  return (
    <section className="flex flex-col gap-3" aria-label="Activity">
      <h2 className="font-semibold">Activity</h2>
      {unreachable ? (
        <p className="text-sm text-muted-foreground">Activity shows when you are online.</p>
      ) : (
      <Timeline
        entries={entries}
        schema={schema}
        loading={timeline.isFetching}
        hasMore={!!timeline.data?.next_before}
        onMore={() => {
          setOlder(entries);
          setBefore(timeline.data?.next_before ?? null);
        }}
      />
      )}
    </section>
  );
}

const FILTERS: { value: string; label: string }[] = [
  { value: "hold,pending,confirmed", label: "Upcoming and held" },
  { value: "hold", label: "Holds" },
  { value: "pending", label: "Pending" },
  { value: "confirmed", label: "Confirmed" },
  { value: "completed", label: "Completed" },
  { value: "cancelled,expired", label: "Cancelled or expired" },
  { value: "", label: "Everything" },
];

export interface BookingsSearch {
  status?: string;
  q?: string;
}

export function validateBookingsSearch(s: Record<string, unknown>): BookingsSearch {
  const str = (v: unknown) => (typeof v === "string" ? v : undefined);
  return { status: str(s.status), q: str(s.q) || undefined };
}

export function BookingsPage() {
  const search = useSearch({ from: "/app/bookings" });
  const navigate = useNavigate({ from: "/bookings" });
  const sync = useOffline();
  const syncStatus = useSyncStatus();
  const [creating, setCreating] = useState(false);
  const status = search.status ?? FILTERS[0]!.value;
  const upcoming = status === FILTERS[0]!.value;
  const list = useQuery({
    queryKey: ["bookings", "list", status, search.q],
    queryFn: () =>
      fetchBookings(sync, {
        status,
        q: search.q,
        // What is coming up, soonest first; anything else newest first.
        from: upcoming ? new Date(Date.now() - 86_400_000).toISOString() : undefined,
        sort: upcoming ? "starts_at" : "-starts_at",
        limit: 200,
      }),
    placeholderData: (prev) => prev,
  });
  const items = (list.data?.data.items ?? []) as Booking[];
  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="flex-1 text-xl font-semibold">Bookings</h1>
        <NativeSelect
          aria-label="Show"
          className="w-auto"
          value={status}
          onChange={(e) => void navigate({ search: { ...search, status: e.target.value }, replace: true })}
        >
          {FILTERS.map((f) => (
            <option key={f.label} value={f.value}>
              {f.label}
            </option>
          ))}
        </NativeSelect>
        <Input
          aria-label="Search bookings"
          placeholder="Search…"
          className="w-44"
          defaultValue={search.q ?? ""}
          onChange={(e) => void navigate({ search: { ...search, q: e.target.value || undefined }, replace: true })}
        />
        <Button onClick={() => setCreating(true)}>
          <Plus /> Place a hold
        </Button>
      </div>
      {(list.data?.offline || syncStatus?.online === false) && <OfflineNote />}
      {list.isError && <Alert>{list.error.message}</Alert>}
      {list.isPending ? (
        <Loading />
      ) : items.length === 0 ? (
        <p className="py-12 text-center text-sm text-muted-foreground">No bookings here.</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {items.map((b) => (
            <li key={b.id}>
              <BookingCard booking={b} />
            </li>
          ))}
        </ul>
      )}
      {creating && <NewBookingDialog onClose={() => setCreating(false)} />}
    </div>
  );
}

/** The clashes a booking of these talent at this time would have, checked as the form changes. */
function useClashes(talentIds: string[], startsAt: string | null, endsAt: string | null, exceptId?: string) {
  const sync = useOffline();
  const [debounced, setDebounced] = useState({ talentIds, startsAt, endsAt });
  useEffect(() => {
    const t = setTimeout(() => setDebounced({ talentIds, startsAt, endsAt }), 300);
    return () => clearTimeout(t);
    // The ids are compared by value, not by array.
  }, [talentIds.join(","), startsAt, endsAt]);
  const ready =
    debounced.talentIds.length > 0 && !!debounced.startsAt && !!debounced.endsAt && debounced.endsAt > debounced.startsAt;
  return useQuery({
    queryKey: ["clashes", debounced, exceptId],
    queryFn: async () =>
      (
        await checkClashes(sync, {
          talent_ids: debounced.talentIds,
          starts_at: debounced.startsAt!,
          ends_at: debounced.endsAt!,
          except_id: exceptId ?? null,
        })
      ).data,
    enabled: ready,
  });
}

export function ClashList({ report }: { report: ClashReport | undefined }) {
  if (!report || report.clashes.length === 0) return null;
  return (
    <Alert tone={report.blocking ? "error" : "info"}>
      <p className="flex items-center gap-2 font-medium">
        <AlertTriangle className="size-4 shrink-0" />
        {report.blocking ? "This would double-book someone." : "Something else is pencilled in at this time."}
      </p>
      <ul className="mt-1 list-disc pl-5">
        {report.clashes.map((c, i) => (
          <li key={`${c.booking_id ?? c.availability_id}-${c.talent_id}-${i}`}>
            {c.talent_name}:{" "}
            {c.booking_id ? (
              <Link to="/bookings/$id" params={{ id: c.booking_id }} className="underline">
                {c.title ?? "a booking"}
              </Link>
            ) : (
              (c.title ?? "marked")
            )}{" "}
            ({c.kind === "booking" ? STATUS_LABEL[c.status as BookingStatus] ?? c.status : c.status}
            {c.blocking ? "" : ", a warning only"}) — {formatWhen(c.starts_at, c.ends_at, false)}
          </li>
        ))}
      </ul>
    </Alert>
  );
}

/** Asks for the reason to confirm despite a clash. */
function OverrideDialog({
  message,
  onConfirm,
  onClose,
}: {
  message: string;
  onConfirm: (reason: string) => Promise<void>;
  onClose: () => void;
}) {
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="Confirm despite the clash?">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          try {
            await onConfirm(reason);
            onClose();
          } catch (err) {
            setError(err instanceof Error ? err.message : String(err));
          }
        }}
      >
        <Alert>{message}</Alert>
        <Label htmlFor="override-reason">Why is this all right? It is kept with the booking and in the audit log.</Label>
        <Textarea id="override-reason" required value={reason} onChange={(e) => setReason(e.target.value)} />
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Back
          </Button>
          <Button type="submit" disabled={!reason.trim()}>
            Confirm anyway
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function isClash(e: unknown): e is ApiFailure {
  return e instanceof ApiFailure && e.code === "booking_clash";
}

export interface NewBookingInitial {
  enquiry_id?: string;
  title?: string;
  talent?: Named[];
  /** `YYYY-MM-DD`: the day to start on, 19:00 to 23:00. */
  day?: string;
}

export function NewBookingDialog({
  initial,
  onClose,
  onCreated,
}: {
  initial?: NewBookingInitial;
  onClose: () => void;
  onCreated?: (b: Booking) => void;
}) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const sync = useOffline();
  const { workspace } = useSession();
  const currency = useWorkspaceCurrency();
  const day = initial?.day ?? toLocalInput(new Date(Date.now() + 7 * 86_400_000).toISOString()).slice(0, 10);
  const [title, setTitle] = useState(initial?.title ?? "");
  const [talent, setTalent] = useState<Named[]>(initial?.talent ?? []);
  const [starts, setStarts] = useState(`${day}T19:00`);
  const [ends, setEnds] = useState(`${day}T23:00`);
  const [allDay, setAllDay] = useState(false);
  const [status, setStatus] = useState<BookingStatus>("hold");
  const [fee, setFee] = useState<Money | null>(null);
  const [venue, setVenue] = useState("");
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [override, setOverride] = useState<string | null>(null);
  const startsAt = fromLocalInput(allDay ? `${starts.slice(0, 10)}T00:00` : starts);
  const endsAt = fromLocalInput(allDay ? `${ends.slice(0, 10)}T00:00` : ends);
  const clashes = useClashes(
    talent.map((t) => t.id),
    startsAt,
    allDay && endsAt ? new Date(new Date(endsAt).getTime() + 86_400_000).toISOString() : endsAt,
  );

  const create = async (overrideReason?: string) => {
    const { record } = await createBooking(sync, {
      title,
      enquiry_id: initial?.enquiry_id ?? null,
      status,
      talent_ids: talent.map((t) => t.id),
      starts_at: startsAt ?? "",
      // An all-day booking runs to the start of the day after its last.
      ends_at: allDay && endsAt ? new Date(new Date(endsAt).getTime() + 86_400_000).toISOString() : (endsAt ?? ""),
      all_day: allDay,
      fee,
      venue_text: venue || null,
      override_reason: overrideReason ?? null,
    });
    const b = record as unknown as Booking;
    await qc.invalidateQueries({ queryKey: ["bookings"] });
    await qc.invalidateQueries({ queryKey: ["calendar"] });
    onClose();
    if (onCreated) onCreated(b);
    else void navigate({ to: "/bookings/$id", params: { id: b.id } });
  };
  const issue = (field: string) => issues.find((i) => i.field === field || i.field?.startsWith(`${field}[`))?.message;

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={status === "hold" ? "Place a hold" : "New booking"} className="max-w-xl">
      <form
        className="flex flex-col gap-4"
        onSubmit={async (e) => {
          e.preventDefault();
          setIssues([]);
          setError(null);
          try {
            await create();
          } catch (err) {
            const found = failureIssues(err);
            if (found) setIssues(found);
            else if (isClash(err) && canOverride(workspace.role)) setOverride(err.message);
            else setError(errorText(err));
          }
        }}
      >
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="booking-title">Title</Label>
          <Input id="booking-title" required value={title} onChange={(e) => setTitle(e.target.value)} aria-invalid={!!issue("title")} />
          {issue("title") && <p className="text-sm text-destructive">{issue("title")}</p>}
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="booking-talent">Talent</Label>
          <TalentPicker id="booking-talent" value={talent} onChange={setTalent} />
          {issue("talent_ids") && <p className="text-sm text-destructive">{issue("talent_ids")}</p>}
        </div>
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="booking-starts">Starts</Label>
            <Input
              id="booking-starts"
              type={allDay ? "date" : "datetime-local"}
              required
              value={allDay ? starts.slice(0, 10) : starts}
              onChange={(e) => setStarts(allDay ? `${e.target.value}T00:00` : e.target.value)}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="booking-ends">{allDay ? "Last day" : "Ends"}</Label>
            <Input
              id="booking-ends"
              type={allDay ? "date" : "datetime-local"}
              required
              value={allDay ? ends.slice(0, 10) : ends}
              onChange={(e) => setEnds(allDay ? `${e.target.value}T00:00` : e.target.value)}
              aria-invalid={!!issue("ends_at")}
            />
            {issue("ends_at") && <p className="text-sm text-destructive">{issue("ends_at")}</p>}
          </div>
        </div>
        <label className="flex items-center gap-2 text-sm">
          <Checkbox checked={allDay} onChange={(e) => setAllDay(e.target.checked)} /> All day
        </label>
        <ClashList report={clashes.data} />
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="booking-status">As</Label>
            <NativeSelect id="booking-status" value={status} onChange={(e) => setStatus(e.target.value as BookingStatus)}>
              <option value="hold">A hold (pencilled in)</option>
              <option value="pending">Pending (agreed, awaiting contract)</option>
              <option value="confirmed">Confirmed</option>
            </NativeSelect>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="booking-fee">Fee</Label>
            <MoneyInput id="booking-fee" value={fee} defaultCurrency={currency} onChange={setFee} />
          </div>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="booking-venue">Venue</Label>
          <Input id="booking-venue" value={venue} onChange={(e) => setVenue(e.target.value)} />
        </div>
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit">{status === "hold" ? "Place hold" : "Save booking"}</Button>
        </div>
      </form>
      {override && (
        <OverrideDialog message={override} onConfirm={(reason) => create(reason)} onClose={() => setOverride(null)} />
      )}
    </Dialog>
  );
}

/** The status buttons for a booking, with the reason and override questions they need. */
function StatusActions({ booking: b, onChanged }: { booking: Booking; onChanged: (b: Booking, queued: boolean) => void }) {
  const sync = useOffline();
  const { workspace } = useSession();
  const [error, setError] = useState<string | null>(null);
  const [override, setOverride] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const send = async (status: BookingStatus, reason?: string, overrideReason?: string) => {
    const result = await changeStatus(sync, b, { status, reason, override_reason: overrideReason });
    onChanged(result.record as unknown as Booking, result.queued);
  };
  const choose = async (status: BookingStatus) => {
    setError(null);
    let reason: string | undefined;
    if (status === "cancelled") {
      const answer = window.prompt("Why is it cancelled? (optional)");
      if (answer === null) return;
      reason = answer || undefined;
    }
    setBusy(true);
    try {
      await send(status, reason);
    } catch (err) {
      if (isClash(err) && canOverride(workspace.role)) setOverride(err.message);
      else setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };
  const moves = nextStatuses(b.status);
  return (
    <>
      {moves.map((m) => (
        <Button
          key={m.status}
          variant={m.status === "confirmed" ? "default" : "outline"}
          disabled={busy}
          onClick={() => void choose(m.status)}
        >
          {m.label}
        </Button>
      ))}
      {error && <Alert className="basis-full">{error}</Alert>}
      {override && (
        <OverrideDialog
          message={override}
          onConfirm={(reason) => send("confirmed", undefined, reason)}
          onClose={() => setOverride(null)}
        />
      )}
    </>
  );
}

export function BookingPage() {
  const { id } = useParams({ from: "/app/bookings/$id" });
  const qc = useQueryClient();
  const navigate = useNavigate();
  const sync = useOffline();
  const syncStatus = useSyncStatus();
  const { workspace } = useSession();
  const schema = useSchema("booking");
  const currency = useWorkspaceCurrency();
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [saved, setSaved] = useState<"sent" | "queued" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [override, setOverride] = useState<{ message: string; body: Record<string, unknown> } | null>(null);
  const booking = useQuery({
    queryKey: ["booking", id],
    queryFn: () => fetchRecord(sync, "booking", id),
  });
  // A booking made offline gets its real id once it reaches the server.
  const renamed = isLocalId(id) ? sync?.renamed.get(id) : undefined;
  useEffect(() => {
    if (renamed) void navigate({ to: "/bookings/$id", params: { id: renamed }, replace: true });
  }, [renamed, navigate]);
  const b = booking.data?.data as unknown as Booking | undefined;
  const live = !!b && ["hold", "pending", "confirmed"].includes(b.status);
  const clashes = useClashes(
    live ? b.talent.map((t) => t.id) : [],
    b?.starts_at ?? null,
    b?.ends_at ?? null,
    id,
  );

  if (schema.isPending || booking.isPending) return <Loading />;
  if (schema.isError) return <PageError error={schema.error} />;
  if (booking.isError || !b) return <PageError error={booking.error ?? new Error("not found")} />;

  const offline = booking.data?.offline || syncStatus?.online === false;
  const changed = (next: Booking | null, queued = false) => {
    if (next) qc.setQueryData(["booking", id], { data: next, offline: queued && !navigator.onLine });
    void qc.invalidateQueries({ queryKey: ["bookings"] });
    void qc.invalidateQueries({ queryKey: ["calendar"] });
    void qc.invalidateQueries({ queryKey: ["timeline", "bookings", id] });
    void qc.invalidateQueries({ queryKey: ["clashes"] });
    void qc.invalidateQueries({ queryKey: ["quotes", id] });
  };
  const patch = async (body: Record<string, unknown>) => {
    const result = await updateBooking(sync, b, body);
    changed(result.record as unknown as Booking | null, result.queued);
    return result;
  };

  return (
    <div className="mx-auto flex max-w-7xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="ghost" size="icon" asChild aria-label="Back to bookings">
          <Link to="/bookings">
            <ArrowLeft />
          </Link>
        </Button>
        <h1 className="min-w-0 flex-1 truncate text-xl font-semibold">{b.title}</h1>
        <StatusBadge status={b.status} />
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <StatusActions booking={b} onChanged={changed} />
        <div className="flex-1" />
        <Button
          variant="outline"
          onClick={async () => {
            if (!window.confirm(`Delete ${b.title}? Its open quotes are withdrawn.`)) return;
            await deleteRecord(sync, "booking", id, b.version);
            await qc.invalidateQueries({ queryKey: ["bookings"] });
            void qc.invalidateQueries({ queryKey: ["calendar"] });
            void navigate({ to: "/bookings" });
          }}
        >
          <Trash2 /> Delete
        </Button>
      </div>
      <dl className="grid gap-x-6 gap-y-1 text-sm sm:grid-cols-[auto_1fr]">
        <dt className="text-muted-foreground">When</dt>
        <dd>{formatWhen(b.starts_at, b.ends_at, b.all_day)}</dd>
        {b.status === "hold" && (
          <>
            <dt className="text-muted-foreground">Hold lapses</dt>
            <dd>{b.hold_expires_at ? new Date(b.hold_expires_at).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }) : "Never"}</dd>
          </>
        )}
        {b.fee && (
          <>
            <dt className="text-muted-foreground">Fee</dt>
            <dd>
              {formatMoney(b.fee)}
              {b.deposit_percent != null && ` · ${b.deposit_percent}% deposit`}
            </dd>
          </>
        )}
        {b.enquiry_id && (
          <>
            <dt className="text-muted-foreground">Enquiry</dt>
            <dd>
              <Link to="/enquiries/$id" params={{ id: b.enquiry_id }} className="underline">
                Open the enquiry
              </Link>
            </dd>
          </>
        )}
        {b.cancel_reason && (
          <>
            <dt className="text-muted-foreground">Cancelled because</dt>
            <dd>{b.cancel_reason}</dd>
          </>
        )}
        {b.clash_override && (
          <>
            <dt className="text-muted-foreground">Confirmed over a clash</dt>
            <dd>{b.override_reason}</dd>
          </>
        )}
      </dl>
      {offline && <OfflineNote />}
      {live && <ClashList report={clashes.data} />}
      {error && <Alert>{error}</Alert>}
      <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <section className="flex flex-col gap-6" aria-label="Details">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="booking-talent-edit">Talent</Label>
            <TalentPicker
              id="booking-talent-edit"
              value={b.talent}
              onChange={(talent) =>
                void patch({ talent_ids: talent.map((t) => t.id) }).catch((err: unknown) => setError(errorText(err)))
              }
            />
          </div>
          {saved === "sent" && <Alert tone="success">Saved.</Alert>}
          {saved === "queued" && <Alert tone="info">Saved on this device. It will be sent when you are back online.</Alert>}
          <EntityForm
            key={String(b.version)}
            schema={schema.data}
            record={b as never}
            issues={issues}
            defaultCurrency={currency}
            renderRelation={RelationPicker}
            onSubmit={async (body) => {
              setIssues([]);
              setSaved(null);
              setError(null);
              if (Object.keys(body).length === 0) return;
              try {
                const result = await patch(body);
                setSaved(result.queued ? "queued" : "sent");
              } catch (err) {
                if (err instanceof ApiFailure && err.status === 412) {
                  throw new Error("Someone else changed this booking while you were editing. Reload to see their changes.");
                }
                const found = failureIssues(err);
                if (found) setIssues(found);
                else if (isClash(err) && canOverride(workspace.role)) setOverride({ message: err.message, body });
                else throw err;
              }
            }}
          />
          <QuotesPanel booking={b} onBookingChanged={() => void booking.refetch()} />
          <BookingFinancePanel booking={b} />
          <BookingDocumentsPanel booking={b} />
        </section>
        <section aria-label="Tasks and activity" className="flex flex-col gap-6">
          <RelatedTasks entity="booking" id={id} />
          <RecordTimeline path="bookings" id={id} schema={schema.data} offline={offline} />
        </section>
      </div>
      {override && (
        <OverrideDialog
          message={override.message}
          onConfirm={async (reason) => {
            await patch({ ...override.body, override_reason: reason });
          }}
          onClose={() => setOverride(null)}
        />
      )}
    </div>
  );
}

/** Moves a booking to new times, asking for an override reason if it now clashes and the person may give one. */
export async function reschedule(
  sync: OfflineSync | null,
  b: Booking,
  times: { starts_at: string; ends_at: string },
  role: string,
): Promise<Booking | null> {
  const send = async (overrideReason?: string) =>
    (await updateBooking(sync, b, { ...times, ...(overrideReason && { override_reason: overrideReason }) }))
      .record as unknown as Booking;
  try {
    return await send();
  } catch (err) {
    if (!isClash(err) || !canOverride(role)) throw err;
    const reason = window.prompt(`${err.message}\n\nTo move it anyway, give a reason:`);
    if (!reason?.trim()) return null;
    return send(reason);
  }
}
