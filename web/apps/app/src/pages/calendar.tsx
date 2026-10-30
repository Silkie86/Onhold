// The calendar: a month at a glance, a week with a lane per talent, or a list
// of what is coming. Holds are dashed, marked-off time is hatched, and a
// booking dragged to another day is moved there (the clash check still applies).

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { Alert, Button, NativeSelect, Spinner, cn } from "@onhold/ui";
import { ChevronLeft, ChevronRight, Plus } from "lucide-react";
import { type ReactNode, useState } from "react";
import { fetchCalendar, fetchList, useOffline, useSyncStatus } from "../data.ts";
import { useSession } from "../session.tsx";
import {
  type Availability,
  type Booking,
  STATUS_LABEL,
  STATUS_STYLE,
  addDays,
  dayKey,
  daysBetween,
  monthGrid,
  onDay,
  parseDayKey,
  sameDay,
  shiftByDays,
  startOfWeek,
} from "../work.ts";
import { NewBookingDialog, reschedule } from "./bookings.tsx";
import { OfflineNote } from "./crm.tsx";

type View = "month" | "week" | "agenda";

export interface CalendarSearch {
  view?: View;
  date?: string;
  talent?: string;
}

export function validateCalendarSearch(s: Record<string, unknown>): CalendarSearch {
  const view = s.view === "week" || s.view === "agenda" ? s.view : undefined;
  const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);
  return { view, date: str(s.date), talent: str(s.talent) };
}

const AGENDA_DAYS = 31;

/** The days a view shows, from its first to the day after its last. */
export function viewRange(view: View, day: Date): { from: Date; to: Date; days: Date[] } {
  if (view === "month") {
    const days = monthGrid(day);
    return { from: days[0]!, to: addDays(days[days.length - 1]!, 1), days };
  }
  if (view === "week") {
    const start = startOfWeek(day);
    const days = Array.from({ length: 7 }, (_, i) => addDays(start, i));
    return { from: start, to: addDays(start, 7), days };
  }
  const days = Array.from({ length: AGENDA_DAYS }, (_, i) => addDays(day, i));
  return { from: day, to: addDays(day, AGENDA_DAYS), days };
}

function step(view: View, day: Date, dir: -1 | 1): Date {
  if (view === "month") return new Date(day.getFullYear(), day.getMonth() + dir, 1);
  return addDays(day, dir * (view === "week" ? 7 : AGENDA_DAYS));
}

function title(view: View, day: Date): string {
  if (view === "month") return day.toLocaleDateString(undefined, { month: "long", year: "numeric" });
  const { from, to } = viewRange(view, day);
  const fmt = (d: Date) => d.toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
  return `${fmt(from)} – ${fmt(addDays(to, -1))}`;
}

const DRAG_TYPE = "text/booking";

export function CalendarPage() {
  const search = useSearch({ from: "/app/calendar" });
  const navigate = useNavigate({ from: "/calendar" });
  const qc = useQueryClient();
  const sync = useOffline();
  const { workspace } = useSession();
  const view: View = search.view ?? "month";
  const day = parseDayKey(search.date);
  const { from, to, days } = viewRange(view, day);
  const [creatingOn, setCreatingOn] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const status = useSyncStatus();
  const calendar = useQuery({
    queryKey: ["calendar", from.toISOString(), to.toISOString(), search.talent],
    queryFn: () => fetchCalendar(sync, { from: from.toISOString(), to: to.toISOString(), talent_id: search.talent }),
    placeholderData: (prev) => prev,
  });
  const talent = useQuery({
    queryKey: ["list", "talent", "calendar"],
    queryFn: async () => (await fetchList(sync, "talent", { limit: 200, sort: "name" })).data,
  });

  const go = (next: Partial<CalendarSearch>) => void navigate({ search: { ...search, ...next }, replace: true });
  const move = async (bookingId: string, toDay: Date) => {
    const b = calendar.data?.data.bookings.find((x) => x.id === bookingId);
    if (!b) return;
    const by = daysBetween(new Date(b.starts_at), toDay);
    if (by === 0) return;
    setError(null);
    try {
      await reschedule(sync, b, shiftByDays(b.starts_at, b.ends_at, by), workspace.role);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
    await qc.invalidateQueries({ queryKey: ["calendar"] });
    void qc.invalidateQueries({ queryKey: ["booking", bookingId] });
  };

  const bookings = calendar.data?.data.bookings ?? [];
  const availability = calendar.data?.data.availability ?? [];

  return (
    <div className="flex flex-col gap-3 p-4 sm:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="mr-2 text-xl font-semibold">Calendar</h1>
        <div className="flex items-center gap-1">
          <Button variant="outline" size="icon" aria-label="Earlier" onClick={() => go({ date: dayKey(step(view, day, -1)) })}>
            <ChevronLeft />
          </Button>
          <Button variant="outline" onClick={() => go({ date: undefined })}>
            Today
          </Button>
          <Button variant="outline" size="icon" aria-label="Later" onClick={() => go({ date: dayKey(step(view, day, 1)) })}>
            <ChevronRight />
          </Button>
        </div>
        {/* Wide enough for the longest title, so a phone wraps it to its own line instead of cutting it off. */}
        <p className="min-w-48 flex-1 font-medium" aria-live="polite">
          {title(view, day)}
        </p>
        {calendar.isFetching && <Spinner />}
        <NativeSelect aria-label="Talent" className="w-auto" value={search.talent ?? ""} onChange={(e) => go({ talent: e.target.value || undefined })}>
          <option value="">All talent</option>
          {talent.data?.items.map((t) => (
            <option key={t.id} value={t.id}>
              {String(t.name)}
            </option>
          ))}
        </NativeSelect>
        <div role="tablist" aria-label="View" className="flex rounded-md border p-0.5">
          {(["month", "week", "agenda"] as View[]).map((v) => (
            <button
              key={v}
              type="button"
              role="tab"
              aria-selected={view === v}
              className={cn("rounded px-3 py-1 text-sm capitalize", view === v && "bg-accent font-medium")}
              onClick={() => go({ view: v === "month" ? undefined : v })}
            >
              {v}
            </button>
          ))}
        </div>
        <Button onClick={() => setCreatingOn(dayKey(day))}>
          <Plus /> Place a hold
        </Button>
      </div>
      <Legend />
      {(calendar.data?.offline || status?.online === false) && <OfflineNote />}
      {error && <Alert>{error}</Alert>}
      {calendar.isError && <Alert>{calendar.error.message}</Alert>}
      {view === "month" && (
        <MonthView
          month={day}
          days={days}
          bookings={bookings}
          availability={availability}
          onMove={(id, d) => void move(id, d)}
          onAdd={(d) => setCreatingOn(dayKey(d))}
        />
      )}
      {view === "week" && (
        <WeekLanes
          days={days}
          bookings={bookings}
          availability={availability}
          onMove={(id, d) => void move(id, d)}
          onAdd={(d) => setCreatingOn(dayKey(d))}
        />
      )}
      {view === "agenda" && <Agenda days={days} bookings={bookings} availability={availability} />}
      {creatingOn && (
        <NewBookingDialog
          initial={{ day: creatingOn }}
          onClose={() => setCreatingOn(null)}
          onCreated={() => void qc.invalidateQueries({ queryKey: ["calendar"] })}
        />
      )}
    </div>
  );
}

function Legend() {
  return (
    <ul className="flex flex-wrap gap-3 text-xs text-muted-foreground" aria-label="Key">
      {(["hold", "pending", "confirmed", "completed"] as const).map((s) => (
        <li key={s} className="flex items-center gap-1.5">
          <span className={cn("inline-block h-3 w-5 rounded-sm border", STATUS_STYLE[s])} />
          {STATUS_LABEL[s]}
        </li>
      ))}
      <li className="flex items-center gap-1.5">
        <span className="inline-block h-3 w-5 rounded-sm border unavailable-hatch" />
        Unavailable
      </li>
    </ul>
  );
}

function BookingChip({ booking: b, showTalent = true }: { booking: Booking; showTalent?: boolean }) {
  const time = b.all_day ? "" : `${new Date(b.starts_at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })} `;
  const who = showTalent && b.talent.length ? ` · ${b.talent.map((t) => t.name).join(", ")}` : "";
  const draggable = ["hold", "pending", "confirmed"].includes(b.status);
  return (
    <Link
      to="/bookings/$id"
      params={{ id: b.id }}
      draggable={draggable}
      onDragStart={(e) => e.dataTransfer.setData(DRAG_TYPE, b.id)}
      title={`${STATUS_LABEL[b.status]}: ${b.title}${who}`}
      className={cn("block truncate rounded border px-1.5 py-0.5 text-xs", STATUS_STYLE[b.status])}
    >
      {time}
      {b.title}
      {who}
    </Link>
  );
}

function AvailabilityChip({ item, showTalent = true }: { item: Availability; showTalent?: boolean }) {
  const label = item.kind === "unavailable" ? "Unavailable" : item.kind === "tentative" ? "Tentative" : "Available only";
  return (
    <span
      title={[label, item.talent_name, item.note].filter(Boolean).join(" · ")}
      className={cn(
        "block truncate rounded border px-1.5 py-0.5 text-xs text-muted-foreground",
        item.kind === "unavailable" ? "unavailable-hatch" : "border-dashed",
      )}
    >
      {showTalent ? `${item.talent_name}: ` : ""}
      {item.note ?? label}
    </span>
  );
}

/** A day cell that takes dropped bookings. */
function DropDay({
  day,
  onMove,
  className,
  children,
  label,
}: {
  day: Date;
  onMove: (bookingId: string, day: Date) => void;
  className?: string;
  children: ReactNode;
  label: string;
}) {
  const [over, setOver] = useState(false);
  return (
    <div
      aria-label={label}
      className={cn(className, over && "bg-accent")}
      onDragOver={(e) => {
        if (e.dataTransfer.types.includes(DRAG_TYPE)) {
          e.preventDefault();
          setOver(true);
        }
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setOver(false);
        const id = e.dataTransfer.getData(DRAG_TYPE);
        if (id) onMove(id, day);
      }}
    >
      {children}
    </div>
  );
}

const WEEKDAYS = Array.from({ length: 7 }, (_, i) =>
  addDays(startOfWeek(new Date(2024, 0, 1)), i).toLocaleDateString(undefined, { weekday: "short" }),
);

function MonthView({
  month,
  days,
  bookings,
  availability,
  onMove,
  onAdd,
}: {
  month: Date;
  days: Date[];
  bookings: Booking[];
  availability: Availability[];
  onMove: (id: string, day: Date) => void;
  onAdd: (day: Date) => void;
}) {
  const today = new Date();
  return (
    <div className="overflow-x-auto">
      <div className="grid min-w-[42rem] grid-cols-7 border-t border-l text-sm">
        {WEEKDAYS.map((w) => (
          <div key={w} className="border-r border-b bg-muted/40 px-2 py-1 text-xs font-medium text-muted-foreground">
            {w}
          </div>
        ))}
        {days.map((d) => {
          const here = bookings.filter((b) => onDay(b.starts_at, b.ends_at, d));
          const away = availability.filter((a) => onDay(a.starts_at, a.ends_at, d));
          return (
            <DropDay
              key={d.toISOString()}
              day={d}
              onMove={onMove}
              label={d.toLocaleDateString(undefined, { dateStyle: "full" })}
              className={cn("group flex min-h-28 flex-col gap-0.5 border-r border-b p-1", d.getMonth() !== month.getMonth() && "bg-muted/30")}
            >
              <div className="flex items-center justify-between">
                <span
                  className={cn(
                    "flex size-6 items-center justify-center rounded-full text-xs",
                    sameDay(d, today) && "bg-primary text-primary-foreground",
                  )}
                >
                  {d.getDate()}
                </span>
                <button
                  type="button"
                  aria-label={`Place a hold on ${d.toLocaleDateString(undefined, { dateStyle: "long" })}`}
                  className="invisible rounded p-0.5 text-muted-foreground hover:bg-accent group-hover:visible focus:visible"
                  onClick={() => onAdd(d)}
                >
                  <Plus className="size-3.5" />
                </button>
              </div>
              {away.map((a) => (
                <AvailabilityChip key={a.id} item={a} />
              ))}
              {here.map((b) => (
                <BookingChip key={b.id} booking={b} />
              ))}
            </DropDay>
          );
        })}
      </div>
    </div>
  );
}

const NO_TALENT = "";

/** A row per talent (and one for bookings without any), a column per day. */
function WeekLanes({
  days,
  bookings,
  availability,
  onMove,
  onAdd,
}: {
  days: Date[];
  bookings: Booking[];
  availability: Availability[];
  onMove: (id: string, day: Date) => void;
  onAdd: (day: Date) => void;
}) {
  const lanes = new Map<string, string>();
  for (const b of bookings) for (const t of b.talent) lanes.set(t.id, t.name);
  for (const a of availability) lanes.set(a.talent_id, a.talent_name);
  const rows = [...lanes.entries()].sort((a, b) => a[1].localeCompare(b[1]));
  if (bookings.some((b) => b.talent.length === 0)) rows.push([NO_TALENT, "No talent yet"]);
  const today = new Date();
  return (
    <div className="overflow-x-auto">
      <div className="grid min-w-[48rem] grid-cols-[10rem_repeat(7,minmax(0,1fr))] border-t border-l text-sm">
        <div className="border-r border-b bg-muted/40" />
        {days.map((d) => (
          <div
            key={d.toISOString()}
            className={cn("border-r border-b bg-muted/40 px-2 py-1 text-xs font-medium", sameDay(d, today) && "text-primary")}
          >
            {d.toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short" })}
          </div>
        ))}
        {rows.length === 0 && (
          <p className="col-span-8 border-r border-b p-6 text-center text-muted-foreground">
            Nothing booked or marked this week.{" "}
            <button type="button" className="underline" onClick={() => onAdd(days[0]!)}>
              Place a hold
            </button>
          </p>
        )}
        {rows.map(([id, name]) => (
          <Lane
            key={id || "none"}
            name={name}
            days={days}
            bookings={bookings.filter((b) => (id === NO_TALENT ? b.talent.length === 0 : b.talent.some((t) => t.id === id)))}
            availability={availability.filter((a) => a.talent_id === id)}
            onMove={onMove}
          />
        ))}
      </div>
    </div>
  );
}

function Lane({
  name,
  days,
  bookings,
  availability,
  onMove,
}: {
  name: string;
  days: Date[];
  bookings: Booking[];
  availability: Availability[];
  onMove: (id: string, day: Date) => void;
}) {
  return (
    <>
      <div className="truncate border-r border-b px-2 py-1.5 font-medium" title={name}>
        {name}
      </div>
      {days.map((d) => (
        <DropDay
          key={d.toISOString()}
          day={d}
          onMove={onMove}
          label={`${name}, ${d.toLocaleDateString(undefined, { dateStyle: "full" })}`}
          className="flex min-h-16 flex-col gap-0.5 border-r border-b p-1"
        >
          {availability
            .filter((a) => onDay(a.starts_at, a.ends_at, d))
            .map((a) => (
              <AvailabilityChip key={a.id} item={a} showTalent={false} />
            ))}
          {bookings
            .filter((b) => onDay(b.starts_at, b.ends_at, d))
            .map((b) => (
              <BookingChip key={b.id} booking={b} showTalent={false} />
            ))}
        </DropDay>
      ))}
    </>
  );
}

function Agenda({ days, bookings, availability }: { days: Date[]; bookings: Booking[]; availability: Availability[] }) {
  const filled = days
    .map((d) => ({
      day: d,
      bookings: bookings.filter((b) => sameDay(new Date(b.starts_at), d)),
      away: availability.filter((a) => sameDay(new Date(a.starts_at), d)),
    }))
    .filter((x) => x.bookings.length || x.away.length);
  if (filled.length === 0) return <p className="py-12 text-center text-sm text-muted-foreground">Nothing in these dates.</p>;
  return (
    <ol className="flex flex-col gap-4">
      {filled.map(({ day, bookings: list, away }) => (
        <li key={day.toISOString()} className="flex flex-col gap-1.5">
          <h2 className="text-sm font-medium">{day.toLocaleDateString(undefined, { dateStyle: "full" })}</h2>
          <ul className="flex flex-col gap-1.5">
            {away.map((a) => (
              <li key={a.id}>
                <AvailabilityChip item={a} />
              </li>
            ))}
            {list.map((b) => (
              <li key={b.id}>
                <BookingChip booking={b} />
              </li>
            ))}
          </ul>
        </li>
      ))}
    </ol>
  );
}
