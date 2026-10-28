// Projects: a run of dates looked at as one thing (a tour, a season, a
// speaking circuit, a travelling exhibition, a shoot schedule). The list,
// one project with whether it pays date by date and overall, its budget,
// crew and shared costs, and a printable day sheet for each day.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import type { Schemas } from "@onhold/api-client";
import { Alert, Badge, Button, Dialog, Input, Label, MoneyInput, NativeSelect, Spinner, Textarea, cn, formatMoney } from "@onhold/ui";
import { AlertTriangle, ArrowLeft, CalendarPlus, Link2, Plus, Printer, Route as RouteIcon, Trash2, UserPlus } from "lucide-react";
import { type ReactNode, useState } from "react";
import { api, ok } from "../api.ts";
import { fetchSaved, useOffline, useSyncStatus } from "../data.ts";
import { type Booking, fromZonedInput, zonedTime } from "../work.ts";
import { Loading, OfflineNote, PageError, useWorkspaceCurrency } from "./crm.tsx";
import { NewBookingDialog, StatusBadge } from "./bookings.tsx";
import { RelatedTasks } from "./tasks.tsx";
import { COST_LABELS, INCOME_LABELS, TravelLine, travelTitle } from "./travel.tsx";

type Project = Schemas["ProjectView"];
type Overview = Schemas["ProjectOverview"];
type Itinerary = Schemas["Itinerary"];
type Day = Schemas["ItineraryDay"];
type Money = Schemas["Money"];
type Warning = Schemas["Warning"];

export const PROJECT_KINDS: Record<string, string> = {
  tour: "Tour",
  season: "Season",
  circuit: "Circuit",
  exhibition: "Exhibition",
  production: "Production",
  campaign: "Campaign",
  other: "Project",
};

export const PROJECT_STATUSES: Record<string, string> = {
  planning: "Planning",
  confirmed: "Confirmed",
  under_way: "Under way",
  done: "Done",
  cancelled: "Cancelled",
};

function failure(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function useOnline(): boolean {
  return useSyncStatus()?.online !== false;
}

function dayLabel(iso: string, opts: Intl.DateTimeFormatOptions = { weekday: "short", day: "numeric", month: "short" }) {
  return new Date(`${iso}T12:00:00Z`).toLocaleDateString(undefined, { ...opts, timeZone: "UTC" });
}

function span(p: Project): string {
  if (!p.first_at) return "No dates yet";
  const f = new Date(p.first_at).toLocaleDateString(undefined, { day: "numeric", month: "short" });
  const l = p.last_at ? new Date(p.last_at).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" }) : "";
  return p.first_at === p.last_at ? l : `${f} – ${l}`;
}

function Warnings({ warnings }: { warnings: Warning[] }) {
  return (
    <>
      {warnings.map((w) => (
        <div
          key={w.code}
          role={w.level === "loss" ? "alert" : "status"}
          className={cn(
            "flex items-start gap-2 rounded-md border p-2 text-sm",
            w.level === "loss" ? "border-destructive/40 bg-destructive/10 text-destructive" : "border-amber-500/40 bg-amber-500/10",
          )}
        >
          <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden />
          {w.message}
        </div>
      ))}
    </>
  );
}

function Net({ money, className }: { money?: Money | null; className?: string }) {
  if (!money) return <span className={cn("text-muted-foreground", className)}>No rate</span>;
  return <span className={cn("tabular-nums", money.minor < 0 && "text-destructive", className)}>{formatMoney(money)}</span>;
}

// The list ----------------------------------------------------------------------------------

export function ProjectsPage() {
  const sync = useOffline();
  const status = useSyncStatus();
  const [creating, setCreating] = useState(false);
  const list = useQuery({
    queryKey: ["projects"],
    queryFn: async () => fetchSaved(sync, "projects", () => ok(api.GET("/api/v1/projects", { params: { query: { limit: 200 } } }))),
  });
  const items = list.data?.data.items ?? [];
  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="flex-1 text-xl font-semibold">Tours and projects</h1>
        {status?.online !== false && (
          <Button onClick={() => setCreating(true)}>
            <Plus /> New tour or project
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">
        A run of dates looked at as one: a tour, a season, a speaking circuit, a travelling exhibition or a shoot schedule. Each date stays a
        booking with its own holds, contract and invoices.
      </p>
      {(list.data?.offline || status?.online === false) && <OfflineNote />}
      {list.isError && <Alert>{list.error.message}</Alert>}
      {list.isPending ? (
        <Loading />
      ) : items.length === 0 ? (
        <p className="py-12 text-center text-sm text-muted-foreground">No tours or projects yet.</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {items.map((p) => (
            <li key={p.id}>
              <Link
                to="/projects/$id"
                params={{ id: p.id }}
                className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-lg border bg-card p-3 hover:bg-accent"
              >
                <RouteIcon className="size-4 text-muted-foreground" aria-hidden />
                <span className="min-w-0 flex-1 truncate font-medium">{p.name}</span>
                <Badge>{PROJECT_KINDS[p.kind] ?? p.kind}</Badge>
                <Badge>{PROJECT_STATUSES[p.status] ?? p.status}</Badge>
                <span className="w-full text-sm text-muted-foreground sm:w-auto">
                  {span(p)} · {p.dates === 1 ? "1 date" : `${p.dates} dates`}
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
      {creating && <NewProjectDialog onClose={() => setCreating(false)} />}
    </div>
  );
}

function NewProjectDialog({ onClose }: { onClose: () => void }) {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const currency = useWorkspaceCurrency();
  const [name, setName] = useState("");
  const [kind, setKind] = useState("tour");
  const [code, setCode] = useState(currency);
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="New tour or project">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          try {
            const p = await ok(api.POST("/api/v1/projects", { body: { name, kind, currency: code } }));
            await qc.invalidateQueries({ queryKey: ["projects"] });
            onClose();
            void navigate({ to: "/projects/$id", params: { id: p.id } });
          } catch (err) {
            setError(failure(err));
          }
        }}
      >
        <Field label="Name" id="project-name">
          <Input id="project-name" required value={name} placeholder="Summer run 2031" onChange={(e) => setName(e.target.value)} />
        </Field>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="What it is" id="project-kind">
            <NativeSelect id="project-kind" value={kind} onChange={(e) => setKind(e.target.value)}>
              {Object.entries(PROJECT_KINDS).map(([k, l]) => (
                <option key={k} value={k}>
                  {l}
                </option>
              ))}
            </NativeSelect>
          </Field>
          <Field label="Totals in" id="project-currency">
            <Input id="project-currency" maxLength={3} value={code} onChange={(e) => setCode(e.target.value.toUpperCase())} />
          </Field>
        </div>
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit">Create</Button>
        </div>
      </form>
    </Dialog>
  );
}

function Field({ label, id, children }: { label: string; id: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      {children}
    </div>
  );
}

// One project -------------------------------------------------------------------------------

export function ProjectPage() {
  const { id } = useParams({ from: "/app/projects/$id" });
  const sync = useOffline();
  const online = useOnline();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [adding, setAdding] = useState<null | "new" | "existing" | "crew" | "cost" | "income">(null);
  const [error, setError] = useState<string | null>(null);
  const overview = useQuery({
    queryKey: ["project", id],
    queryFn: async () => fetchSaved(sync, `project:${id}`, () => ok(api.GET("/api/v1/projects/{id}/overview", { params: { path: { id } } }))),
  });
  const itinerary = useQuery({
    queryKey: ["itinerary", id],
    queryFn: async () => fetchSaved(sync, `itinerary:${id}`, () => ok(api.GET("/api/v1/projects/{id}/itinerary", { params: { path: { id } } }))),
  });
  if (overview.isPending) return <Loading />;
  if (overview.isError) return <PageError error={overview.error} />;
  const o: Overview = overview.data.data;
  const p = o.project;
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["project", id] });
    void qc.invalidateQueries({ queryKey: ["itinerary", id] });
    void qc.invalidateQueries({ queryKey: ["projects"] });
  };
  const act = async (run: () => Promise<unknown>) => {
    setError(null);
    try {
      await run();
      refresh();
    } catch (e) {
      setError(failure(e));
    }
  };
  const patch = (body: Schemas["ProjectPatch"]) =>
    act(() => ok(api.PATCH("/api/v1/projects/{id}", { params: { path: { id } }, headers: { "If-Match": `"${p.version}"` }, body })));

  return (
    <div className="mx-auto flex max-w-7xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="ghost" size="icon" asChild aria-label="Back to tours and projects">
          <Link to="/projects">
            <ArrowLeft />
          </Link>
        </Button>
        <h1 className="min-w-0 flex-1 truncate text-xl font-semibold">{p.name}</h1>
        <Badge>{PROJECT_KINDS[p.kind] ?? p.kind}</Badge>
        {online ? (
          <NativeSelect aria-label="Status" className="w-auto" value={p.status} onChange={(e) => void patch({ status: e.target.value })}>
            {Object.entries(PROJECT_STATUSES).map(([k, l]) => (
              <option key={k} value={k}>
                {l}
              </option>
            ))}
          </NativeSelect>
        ) : (
          <Badge>{PROJECT_STATUSES[p.status]}</Badge>
        )}
      </div>
      {(overview.data.offline || !online) && <OfflineNote />}
      {error && <Alert>{error}</Alert>}

      {/* The headline: does the whole run pay? */}
      <section aria-label="Does it pay" className="grid gap-3 sm:grid-cols-4">
        <Stat label="Income" value={formatMoney(o.total_income)} note="Fees, buyouts and other income" />
        <Stat label="Commission" value={formatMoney(o.total_commission)} />
        <Stat label="Costs" value={formatMoney(o.total_costs)} note="Travel, hotels, crew and the rest" />
        <Stat label="Left over" value={<Net money={o.net} />} strong />
      </section>
      <Warnings warnings={o.warnings} />

      <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <div className="flex flex-col gap-6">
          <section aria-label="Dates" className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="flex-1 font-semibold">Dates</h2>
              {online && (
                <>
                  <Button size="sm" variant="outline" onClick={() => setAdding("existing")}>
                    <Link2 /> Add a booking
                  </Button>
                  <Button size="sm" variant="outline" onClick={() => setAdding("new")}>
                    <CalendarPlus /> New date
                  </Button>
                </>
              )}
            </div>
            {o.dates.length === 0 ? (
              <p className="text-sm text-muted-foreground">No dates yet. Add bookings you already have, or make new ones here.</p>
            ) : (
              <ul className="flex flex-col divide-y rounded-lg border">
                {o.dates.map((d) => (
                  <li key={d.booking_id} className="flex flex-col gap-1 p-3 text-sm">
                    <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
                      <span className="w-24 shrink-0 tabular-nums text-muted-foreground">{zonedTime(d.starts_at, d.timezone, true).split(",")[0]}</span>
                      <Link to="/bookings/$id" params={{ id: d.booking_id }} className="min-w-0 flex-1 truncate font-medium underline-offset-2 hover:underline">
                        {d.title}
                      </Link>
                      <StatusBadge status={d.status} />
                      <Net money={d.net_in_currency ?? null} className="w-28 text-right font-medium" />
                      {online && (
                        <Button
                          size="icon"
                          variant="ghost"
                          className="size-7"
                          aria-label={`Take ${d.title} out of ${p.name}`}
                          onClick={() => {
                            if (window.confirm(`Take ${d.title} out of ${p.name}? The booking stays.`))
                              void act(() => ok(api.DELETE("/api/v1/projects/{id}/dates/{booking_id}", { params: { path: { id, booking_id: d.booking_id } } })));
                          }}
                        >
                          <Trash2 />
                        </Button>
                      )}
                    </div>
                    <div className="flex flex-wrap gap-x-3 pl-0 text-xs text-muted-foreground sm:pl-27">
                      {d.venue && <span>{d.venue}</span>}
                      <span>Income {formatMoney(d.income)}</span>
                      <span>Costs {formatMoney(d.costs)}</span>
                      {d.net.currency !== p.currency && <span>Net {formatMoney(d.net)}</span>}
                    </div>
                    {d.warnings.length > 0 && (
                      <div className="flex flex-wrap gap-2">
                        {d.warnings.map((w) => (
                          <span key={w.code} className={cn("text-xs", w.level === "loss" ? "text-destructive" : "text-amber-700 dark:text-amber-400")}>
                            {w.message}
                          </span>
                        ))}
                      </div>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </section>

          <ItineraryPanel projectId={id} itinerary={itinerary.data?.data} pending={itinerary.isPending} />
        </div>

        <div className="flex flex-col gap-6">
          <BudgetPanel overview={o} online={online} onSave={(budget) => void patch({ budget })} />

          <section aria-label="Shared costs and travel" className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="flex-1 font-semibold">Shared costs and income</h2>
              {online && (
                <>
                  <Button size="sm" variant="outline" onClick={() => setAdding("cost")}>
                    <Plus /> Cost
                  </Button>
                  <Button size="sm" variant="outline" onClick={() => setAdding("income")}>
                    <Plus /> Income
                  </Button>
                </>
              )}
            </div>
            <p className="text-xs text-muted-foreground">For what belongs to no one date: rehearsals, insurance, wages, tour support. Travel on days between dates is added from the itinerary.</p>
            {o.lines.length === 0 ? (
              <p className="text-sm text-muted-foreground">Nothing yet.</p>
            ) : (
              <ul className="flex flex-col divide-y rounded-lg border text-sm">
                {o.lines.map((l) => (
                  <li key={l.id} className="flex items-center gap-2 p-2">
                    <span className="min-w-0 flex-1">
                      {l.description}
                      <span className="block text-xs text-muted-foreground">
                        {l.direction === "income" ? INCOME_LABELS[l.category] : COST_LABELS[l.category]}
                      </span>
                    </span>
                    <span className="tabular-nums">
                      {l.direction === "cost" ? "−" : "+"}
                      {formatMoney(l.amount)}
                    </span>
                    {online && (
                      <Button
                        size="icon"
                        variant="ghost"
                        className="size-7"
                        aria-label={`Remove ${l.description}`}
                        onClick={() => void act(() => ok(api.DELETE("/api/v1/money-lines/{id}", { params: { path: { id: l.id } } })))}
                      >
                        <Trash2 />
                      </Button>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section aria-label="Crew" className="flex flex-col gap-3">
            <div className="flex items-center gap-2">
              <h2 className="flex-1 font-semibold">Crew and party</h2>
              {online && (
                <Button size="sm" variant="outline" onClick={() => setAdding("crew")}>
                  <UserPlus /> Add
                </Button>
              )}
            </div>
            {o.crew.length === 0 ? (
              <p className="text-sm text-muted-foreground">Nobody listed yet.</p>
            ) : (
              <ul className="flex flex-col divide-y rounded-lg border text-sm">
                {o.crew.map((c) => (
                  <li key={c.id} className="flex items-start gap-2 p-2">
                    <span className="min-w-0 flex-1">
                      <span className="font-medium">{c.name}</span>
                      {c.role && <span className="text-muted-foreground"> · {c.role}</span>}
                      <span className="block text-xs text-muted-foreground">{[c.phone, c.email, c.notes].filter(Boolean).join(" · ")}</span>
                    </span>
                    {online && (
                      <Button
                        size="icon"
                        variant="ghost"
                        className="size-7"
                        aria-label={`Remove ${c.name}`}
                        onClick={() => void act(() => ok(api.DELETE("/api/v1/project-crew/{id}", { params: { path: { id: c.id } } })))}
                      >
                        <Trash2 />
                      </Button>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </section>

          <RelatedTasks entity="project" id={id} />

          {online && (
            <Button
              variant="outline"
              className="self-start"
              onClick={async () => {
                if (!window.confirm(`Delete ${p.name}? Its dates stay as bookings.`)) return;
                await act(() => ok(api.DELETE("/api/v1/projects/{id}", { params: { path: { id } } })));
                void navigate({ to: "/projects" });
              }}
            >
              <Trash2 /> Delete
            </Button>
          )}
        </div>
      </div>

      {adding === "new" && (
        <NewBookingDialog
          initial={{ talent: [] }}
          onClose={() => setAdding(null)}
          onCreated={(b: Booking) => void act(() => ok(api.POST("/api/v1/projects/{id}/dates", { params: { path: { id } }, body: { booking_id: b.id } })))}
        />
      )}
      {adding === "existing" && (
        <ExistingBookingDialog
          exclude={o.dates.map((d) => d.booking_id)}
          onClose={() => setAdding(null)}
          onPick={(bookingId) => {
            setAdding(null);
            void act(() => ok(api.POST("/api/v1/projects/{id}/dates", { params: { path: { id } }, body: { booking_id: bookingId } })));
          }}
        />
      )}
      {adding === "crew" && (
        <CrewDialog
          onClose={() => setAdding(null)}
          onSave={async (body) => {
            await ok(api.POST("/api/v1/projects/{id}/crew", { params: { path: { id } }, body }));
            setAdding(null);
            refresh();
          }}
        />
      )}
      {(adding === "cost" || adding === "income") && (
        <LineDialog
          direction={adding}
          currency={p.currency}
          onClose={() => setAdding(null)}
          onSave={async (body) => {
            await ok(api.POST("/api/v1/projects/{id}/money", { params: { path: { id } }, body }));
            setAdding(null);
            refresh();
          }}
        />
      )}
    </div>
  );
}

function Stat({ label, value, note, strong }: { label: string; value: ReactNode; note?: string; strong?: boolean }) {
  return (
    <div className={cn("rounded-lg border bg-card p-3", strong && "border-primary/40")}>
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className={cn("text-lg tabular-nums", strong && "font-semibold")}>{value}</div>
      {note && <div className="text-xs text-muted-foreground">{note}</div>}
    </div>
  );
}

function BudgetPanel({ overview: o, online, onSave }: { overview: Overview; online: boolean; onSave: (b: Record<string, number>) => void }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState<Record<string, Money | null>>({});
  return (
    <section aria-label="Budget" className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <h2 className="flex-1 font-semibold">Budget</h2>
        {online && (
          <Button
            size="sm"
            variant="outline"
            onClick={() => {
              setDraft(
                Object.fromEntries(Object.keys(COST_LABELS).map((k) => [k, o.project.budget[k] ? { minor: o.project.budget[k]!, currency: o.project.currency } : null])),
              );
              setEditing(true);
            }}
          >
            Set budget
          </Button>
        )}
      </div>
      {o.budget.length === 0 ? (
        <p className="text-sm text-muted-foreground">No budget or costs yet.</p>
      ) : (
        <table className="w-full text-sm">
          <thead className="text-xs text-muted-foreground">
            <tr>
              <th className="py-1 text-left font-normal">Category</th>
              <th className="py-1 text-right font-normal">Budget</th>
              <th className="py-1 text-right font-normal">Booked or spent</th>
            </tr>
          </thead>
          <tbody>
            {o.budget.map((b) => {
              const over = b.budget.minor > 0 && b.actual.minor > b.budget.minor;
              return (
                <tr key={b.category} className="border-t">
                  <td className="py-1">{COST_LABELS[b.category] ?? b.category}</td>
                  <td className="py-1 text-right tabular-nums">{b.budget.minor ? formatMoney(b.budget) : "–"}</td>
                  <td className={cn("py-1 text-right tabular-nums", over && "font-medium text-destructive")}>{formatMoney(b.actual)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
      {editing && (
        <Dialog open onOpenChange={(v) => !v && setEditing(false)} title="Budget" description={`Expected costs, in ${o.project.currency}.`}>
          <form
            className="grid gap-3 sm:grid-cols-2"
            onSubmit={(e) => {
              e.preventDefault();
              onSave(Object.fromEntries(Object.entries(draft).flatMap(([k, m]) => (m && m.minor > 0 ? [[k, m.minor]] : []))));
              setEditing(false);
            }}
          >
            {Object.entries(COST_LABELS).map(([k, label]) => (
              <Field key={k} label={label} id={`budget-${k}`}>
                <MoneyInput
                  id={`budget-${k}`}
                  value={draft[k] ?? null}
                  currency={o.project.currency}
                  defaultCurrency={o.project.currency}
                  onChange={(m) => setDraft((d) => ({ ...d, [k]: m }))}
                />
              </Field>
            ))}
            <div className="flex justify-end gap-2 sm:col-span-2">
              <Button type="button" variant="ghost" onClick={() => setEditing(false)}>
                Cancel
              </Button>
              <Button type="submit">Save</Button>
            </div>
          </form>
        </Dialog>
      )}
    </section>
  );
}

function ExistingBookingDialog({ exclude, onClose, onPick }: { exclude: string[]; onClose: () => void; onPick: (id: string) => void }) {
  const [q, setQ] = useState("");
  const found = useQuery({
    queryKey: ["bookings", "pick", q],
    queryFn: () => ok(api.GET("/api/v1/bookings", { params: { query: { q: q || undefined, sort: "starts_at", limit: 50 } } })),
  });
  const items = (found.data?.items ?? []).filter((b) => !exclude.includes(b.id));
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="Add a booking">
      <div className="flex flex-col gap-3">
        <Input aria-label="Search bookings" placeholder="Search…" value={q} onChange={(e) => setQ(e.target.value)} />
        {found.isPending ? (
          <Spinner />
        ) : items.length === 0 ? (
          <p className="text-sm text-muted-foreground">No bookings found.</p>
        ) : (
          <ul className="flex max-h-80 flex-col divide-y overflow-y-auto rounded-md border text-sm">
            {items.map((b) => (
              <li key={b.id}>
                <button type="button" className="flex w-full items-center gap-3 p-2 text-left hover:bg-accent" onClick={() => onPick(b.id)}>
                  <span className="w-24 shrink-0 tabular-nums text-muted-foreground">{zonedTime(b.starts_at, b.timezone, true).split(",")[0]}</span>
                  <span className="min-w-0 flex-1 truncate">{b.title}</span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </Dialog>
  );
}

function CrewDialog({ onClose, onSave }: { onClose: () => void; onSave: (b: Schemas["CrewInput"]) => Promise<void> }) {
  const [v, setV] = useState({ name: "", role: "", phone: "", email: "", notes: "" });
  const [error, setError] = useState<string | null>(null);
  const set = (k: keyof typeof v) => (e: { target: { value: string } }) => setV((x) => ({ ...x, [k]: e.target.value }));
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="Add to the crew">
      <form
        className="grid gap-3 sm:grid-cols-2"
        onSubmit={async (e) => {
          e.preventDefault();
          try {
            await onSave({ name: v.name, role: v.role || null, phone: v.phone || null, email: v.email || null, notes: v.notes || null });
          } catch (err) {
            setError(failure(err));
          }
        }}
      >
        <Field label="Name" id="crew-name">
          <Input id="crew-name" required value={v.name} onChange={set("name")} />
        </Field>
        <Field label="Role" id="crew-role">
          <Input id="crew-role" value={v.role} placeholder="Tour manager, FOH, driver" onChange={set("role")} />
        </Field>
        <Field label="Phone" id="crew-phone">
          <Input id="crew-phone" type="tel" value={v.phone} onChange={set("phone")} />
        </Field>
        <Field label="Email" id="crew-email">
          <Input id="crew-email" type="email" value={v.email} onChange={set("email")} />
        </Field>
        <div className="sm:col-span-2">
          <Field label="Notes" id="crew-notes">
            <Textarea id="crew-notes" rows={2} value={v.notes} placeholder="Dietary needs, access" onChange={set("notes")} />
          </Field>
        </div>
        {error && <Alert className="sm:col-span-2">{error}</Alert>}
        <div className="flex justify-end gap-2 sm:col-span-2">
          <Button type="button" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit">Add</Button>
        </div>
      </form>
    </Dialog>
  );
}

function LineDialog({
  direction,
  currency,
  onClose,
  onSave,
}: {
  direction: "cost" | "income";
  currency: string;
  onClose: () => void;
  onSave: (b: Schemas["MoneyLineInput"]) => Promise<void>;
}) {
  const labels = direction === "income" ? INCOME_LABELS : COST_LABELS;
  const [category, setCategory] = useState(direction === "income" ? "other" : "crew");
  const [description, setDescription] = useState("");
  const [amount, setAmount] = useState<Money | null>(null);
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={direction === "income" ? "Add income" : "Add a cost"}>
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          if (!amount) return setError("Give the amount.");
          try {
            await onSave({ direction, category, description: description || labels[category] || category, amount });
          } catch (err) {
            setError(failure(err));
          }
        }}
      >
        <Field label="Kind" id="pline-category">
          <NativeSelect id="pline-category" value={category} onChange={(e) => setCategory(e.target.value)}>
            {Object.entries(labels).map(([k, l]) => (
              <option key={k} value={k}>
                {l}
              </option>
            ))}
          </NativeSelect>
        </Field>
        <Field label="Description" id="pline-description">
          <Input id="pline-description" value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        <Field label="Amount" id="pline-amount">
          <MoneyInput id="pline-amount" value={amount} defaultCurrency={currency} onChange={setAmount} />
        </Field>
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit">Add</Button>
        </div>
      </form>
    </Dialog>
  );
}

// The itinerary and day sheets -----------------------------------------------------------------

const DAY_KIND: Record<string, string> = { show: "On", travel: "Travel day", off: "Day off" };

function ItineraryPanel({ projectId, itinerary, pending }: { projectId: string; itinerary?: Itinerary; pending: boolean }) {
  return (
    <section aria-label="Day by day" className="flex flex-col gap-3">
      <h2 className="font-semibold">Day by day</h2>
      {pending ? (
        <Spinner />
      ) : !itinerary || itinerary.days.length === 0 ? (
        <p className="text-sm text-muted-foreground">The itinerary fills in as dates and travel are added.</p>
      ) : (
        <ol className="flex flex-col divide-y rounded-lg border">
          {itinerary.days.map((d) => (
            <li key={d.date}>
              <Link
                to="/projects/$id/days/$date"
                params={{ id: projectId, date: d.date }}
                className="flex flex-wrap items-start gap-x-3 gap-y-1 p-3 text-sm hover:bg-accent"
              >
                <span className="w-24 shrink-0 font-medium tabular-nums">{dayLabel(d.date)}</span>
                <span className="min-w-0 flex-1">
                  {d.dates.length > 0 ? (
                    d.dates.map((x) => (
                      <span key={x.booking_id} className="block font-medium">
                        {x.title}
                        {x.venue && x.venue !== x.title && <span className="font-normal text-muted-foreground"> · {x.venue}</span>}
                      </span>
                    ))
                  ) : (
                    <span className="block text-muted-foreground">{DAY_KIND[d.kind]}</span>
                  )}
                  {d.travel.map((t) => (
                    <span key={t.id} className="block text-xs text-muted-foreground">
                      {zonedTime(t.starts_at, t.start_tz)} {travelTitle(t)}
                      {(t.to_code || t.to_place) && ` to ${t.to_code ?? t.to_place}`}
                    </span>
                  ))}
                  {d.hotels.map((h) => (
                    <span key={h.id} className="block text-xs text-muted-foreground">
                      Sleeping at {h.carrier ?? "a hotel"}
                    </span>
                  ))}
                </span>
                <Badge className={cn(d.kind === "show" && "border-primary/40")}>{DAY_KIND[d.kind]}</Badge>
              </Link>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}

/** One day of a project, for the people on the road: print it, or open it on a phone. */
export function ProjectDayPage() {
  const { id, date } = useParams({ from: "/app/projects/$id/days/$date" });
  const sync = useOffline();
  const online = useOnline();
  const qc = useQueryClient();
  const [addingTravel, setAddingTravel] = useState(false);
  const itinerary = useQuery({
    queryKey: ["itinerary", id],
    queryFn: async () => fetchSaved(sync, `itinerary:${id}`, () => ok(api.GET("/api/v1/projects/{id}/itinerary", { params: { path: { id } } }))),
  });
  if (itinerary.isPending) return <Loading />;
  if (itinerary.isError) return <PageError error={itinerary.error} />;
  const it = itinerary.data.data;
  const index = it.days.findIndex((d) => d.date === date);
  const day: Day | undefined = it.days[index];
  const prev = it.days[index - 1];
  const next = it.days[index + 1];
  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-4 p-4 sm:p-6 print:max-w-none print:p-0">
      <div className="flex flex-wrap items-center gap-2 print:hidden">
        <Button variant="ghost" size="icon" asChild aria-label={`Back to ${it.name}`}>
          <Link to="/projects/$id" params={{ id }}>
            <ArrowLeft />
          </Link>
        </Button>
        <span className="flex-1 truncate text-sm text-muted-foreground">{it.name}</span>
        {prev && (
          <Button size="sm" variant="ghost" asChild>
            <Link to="/projects/$id/days/$date" params={{ id, date: prev.date }}>
              ‹ {dayLabel(prev.date, { day: "numeric", month: "short" })}
            </Link>
          </Button>
        )}
        {next && (
          <Button size="sm" variant="ghost" asChild>
            <Link to="/projects/$id/days/$date" params={{ id, date: next.date }}>
              {dayLabel(next.date, { day: "numeric", month: "short" })} ›
            </Link>
          </Button>
        )}
        <Button size="sm" variant="outline" onClick={() => window.print()}>
          <Printer /> Print
        </Button>
      </div>
      <div>
        <p className="text-sm text-muted-foreground">
          {it.name} · Day {index + 1} of {it.days.length}
        </p>
        <h1 className="text-2xl font-semibold">{dayLabel(date, { weekday: "long", day: "numeric", month: "long", year: "numeric" })}</h1>
      </div>
      {!day ? (
        <p className="text-sm text-muted-foreground">Nothing planned on this day.</p>
      ) : (
        <>
          {day.dates.length === 0 && <Badge className="self-start">{DAY_KIND[day.kind]}</Badge>}
          {day.dates.map((d) => (
            <section key={d.booking_id} aria-label={d.title} className="flex flex-col gap-2 rounded-lg border p-4">
              <div className="flex flex-wrap items-center gap-2">
                <h2 className="flex-1 text-lg font-semibold">{d.title}</h2>
                <StatusBadge status={d.status} />
              </div>
              <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
                {d.venue && (
                  <>
                    <dt className="text-muted-foreground">Venue</dt>
                    <dd>{d.venue}</dd>
                  </>
                )}
                {d.talent.length > 0 && (
                  <>
                    <dt className="text-muted-foreground">Who</dt>
                    <dd>{d.talent.join(", ")}</dd>
                  </>
                )}
                {d.contact_name && (
                  <>
                    <dt className="text-muted-foreground">On the day</dt>
                    <dd>{d.contact_name}</dd>
                  </>
                )}
              </dl>
              <h3 className="mt-2 text-sm font-semibold">Schedule</h3>
              <table className="w-full text-sm">
                <tbody>
                  {(d.schedule.length > 0
                    ? d.schedule
                    : [
                        { label: "Start", at: d.starts_at },
                        { label: "Finish", at: d.ends_at },
                      ]
                  ).map((s) => (
                    <tr key={`${s.label}${s.at}`} className="border-t">
                      <td className="w-20 py-1 tabular-nums">{d.all_day ? "" : zonedTime(s.at, d.timezone)}</td>
                      <td className="py-1">{s.label}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
              {d.notes && <p className="whitespace-pre-wrap text-sm text-muted-foreground">{d.notes}</p>}
            </section>
          ))}
          <section aria-label="Travel" className="flex flex-col gap-2">
            <div className="flex items-center gap-2">
              <h2 className="flex-1 font-semibold">Travel</h2>
              {online && (
                <Button size="sm" variant="outline" className="print:hidden" onClick={() => setAddingTravel(true)}>
                  <Plus /> Add travel
                </Button>
              )}
            </div>
            {day.travel.length === 0 ? (
              <p className="text-sm text-muted-foreground">No journeys today.</p>
            ) : (
              <ul className="flex flex-col gap-2">
                {day.travel.map((t) => (
                  <TravelLine key={t.id} item={t} />
                ))}
              </ul>
            )}
          </section>
          <section aria-label="Tonight" className="flex flex-col gap-2">
            <h2 className="font-semibold">Tonight</h2>
            {day.hotels.length === 0 ? (
              <p className="text-sm text-muted-foreground">No hotel booked for tonight.</p>
            ) : (
              <ul className="flex flex-col gap-2">
                {day.hotels.map((h) => (
                  <TravelLine key={h.id} item={h} />
                ))}
              </ul>
            )}
          </section>
        </>
      )}
      {it.crew.length > 0 && (
        <section aria-label="Contacts" className="flex flex-col gap-2">
          <h2 className="font-semibold">Crew and party</h2>
          <ul className="grid gap-1 text-sm sm:grid-cols-2">
            {it.crew.map((c) => (
              <li key={c.id}>
                <span className="font-medium">{c.name}</span>
                {c.role && <span className="text-muted-foreground"> · {c.role}</span>}
                {c.phone && (
                  <a href={`tel:${c.phone}`} className="block text-muted-foreground underline-offset-2 hover:underline">
                    {c.phone}
                  </a>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}
      {addingTravel && (
        <ProjectTravelDialog
          projectId={id}
          date={date}
          onClose={() => setAddingTravel(false)}
          onSaved={() => {
            setAddingTravel(false);
            void qc.invalidateQueries({ queryKey: ["itinerary", id] });
            void qc.invalidateQueries({ queryKey: ["project", id] });
          }}
        />
      )}
    </div>
  );
}

/** A quick journey on a day between dates (the drive to the next city). */
function ProjectTravelDialog({ projectId, date, onClose, onSaved }: { projectId: string; date: string; onClose: () => void; onSaved: () => void }) {
  const currency = useWorkspaceCurrency();
  const [kind, setKind] = useState("car");
  const [carrier, setCarrier] = useState("");
  const [number, setNumber] = useState("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [leaves, setLeaves] = useState("09:00");
  const [zone, setZone] = useState(Intl.DateTimeFormat().resolvedOptions().timeZone);
  const [cost, setCost] = useState<Money | null>(null);
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="Add travel" description="On this day, for the whole party.">
      <form
        className="grid gap-3 sm:grid-cols-2"
        onSubmit={async (e) => {
          e.preventDefault();
          const startsAt = fromZonedInput(`${date}T${leaves}`, zone);
          if (!startsAt) return setError("Give the time it leaves.");
          try {
            await ok(
              api.POST("/api/v1/projects/{id}/travel", {
                params: { path: { id: projectId } },
                body: {
                  kind,
                  carrier: carrier || null,
                  number: number || null,
                  from_place: from || null,
                  to_place: to || null,
                  starts_at: startsAt,
                  start_tz: zone,
                  cost,
                  status: "booked",
                },
              }),
            );
            onSaved();
          } catch (err) {
            setError(failure(err));
          }
        }}
      >
        <Field label="What" id="ptravel-kind">
          <NativeSelect id="ptravel-kind" value={kind} onChange={(e) => setKind(e.target.value)}>
            <option value="car">Car or van</option>
            <option value="bus">Bus or nightliner</option>
            <option value="train">Train</option>
            <option value="flight">Flight</option>
            <option value="ferry">Ferry</option>
            <option value="taxi">Taxi or transfer</option>
          </NativeSelect>
        </Field>
        <Field label="Company" id="ptravel-carrier">
          <Input id="ptravel-carrier" value={carrier} onChange={(e) => setCarrier(e.target.value)} />
        </Field>
        <Field label="From" id="ptravel-from">
          <Input id="ptravel-from" value={from} onChange={(e) => setFrom(e.target.value)} />
        </Field>
        <Field label="To" id="ptravel-to">
          <Input id="ptravel-to" value={to} onChange={(e) => setTo(e.target.value)} />
        </Field>
        <Field label="Leaves" id="ptravel-time">
          <Input id="ptravel-time" type="time" value={leaves} onChange={(e) => setLeaves(e.target.value)} />
        </Field>
        <Field label="Time zone" id="ptravel-zone">
          <Input id="ptravel-zone" value={zone} onChange={(e) => setZone(e.target.value)} />
        </Field>
        <Field label="Number" id="ptravel-number">
          <Input id="ptravel-number" value={number} onChange={(e) => setNumber(e.target.value)} />
        </Field>
        <Field label="Cost" id="ptravel-cost">
          <MoneyInput id="ptravel-cost" value={cost} defaultCurrency={currency} onChange={setCost} />
        </Field>
        {error && <Alert className="sm:col-span-2">{error}</Alert>}
        <div className="flex justify-end gap-2 sm:col-span-2">
          <Button type="button" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit">Add</Button>
        </div>
      </form>
    </Dialog>
  );
}
