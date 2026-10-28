// The talent portal: the free viewer an artist, performer or other creative
// gets when their agency invites them. Phone first and plain: their dates
// with times, travel and documents; confirming or declining a date; their
// availability and their own calendar; and, if the agency shares them, fees
// and statements. Loaded only for people whose role in the workspace is
// talent, in place of the staff app's frame.

import type { Schemas } from "@onhold/api-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, Outlet, useLocation, useNavigate, useParams } from "@tanstack/react-router";
import {
  Alert,
  Badge,
  Button,
  Card,
  EmptyState,
  Menu,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuSeparator,
  MenuTrigger,
  Spinner,
  Textarea,
  type Theme,
  applyTheme,
  cn,
  formatMoney,
} from "@onhold/ui";
import {
  Briefcase,
  CalendarCheck,
  CalendarClock,
  CalendarDays,
  CalendarX2,
  ChevronLeft,
  Download,
  ExternalLink,
  FileSignature,
  FileText,
  Inbox,
  Link2,
  Lock,
  LogOut,
  MapPin,
  Monitor,
  Moon,
  Plus,
  Receipt,
  Route,
  Sparkles,
  Sun,
  Trash2,
  Wallet,
} from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { useWorkspaceBranding } from "../branding.ts";
import { openInBrowser } from "../platform.ts";
import { type Session, SessionContext, useSession, useSessionActions } from "../session.tsx";
import { storedTheme } from "../shell.tsx";
import { formatWhen, zonedTime } from "../work.ts";
import { AvailabilityForm } from "./availability.tsx";
import { saveDownload } from "./finance.tsx";
import { TravelLine } from "./travel.tsx";

type Home = Schemas["PortalHome"];
type PortalDate = Schemas["PortalDate"];
type Detail = Schemas["PortalDateDetail"];
type PortalDocument = Schemas["PortalDocument"];
type Statement = Schemas["PortalStatement"];

const THEME_KEY = "onhold.theme";

function failure(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const homeQuery = { queryKey: ["portal"], queryFn: () => ok(api.GET("/api/v1/portal")), staleTime: 60_000 };

/** The portal's own home data: whose dates, and what the agency shares. */
function usePortal() {
  return useQuery(homeQuery);
}

const STATUS: Record<string, { label: string; className: string }> = {
  hold: { label: "Pencilled", className: "border-dashed" },
  pending: { label: "Being confirmed", className: "border-info text-info" },
  confirmed: { label: "Confirmed", className: "border-success bg-success text-success-foreground" },
  completed: { label: "Done", className: "" },
  cancelled: { label: "Cancelled", className: "border-destructive text-destructive" },
  expired: { label: "Lapsed", className: "" },
};

function StatusBadge({ status }: { status: string }) {
  const s = STATUS[status] ?? { label: status, className: "" };
  return <Badge className={cn("border", s.className)}>{s.label}</Badge>;
}

function AnswerBadge({ d }: { d: PortalDate }) {
  if (d.answer === "confirmed") return <Badge className="border border-success text-success">You confirmed</Badge>;
  if (d.answer === "declined") return <Badge className="border border-destructive text-destructive">You declined</Badge>;
  if (d.can_answer) return <Badge className="border border-warning text-warning">Waiting for your answer</Badge>;
  return null;
}

const HALF_DAY = 12 * 60 * 60 * 1000;

/**
 * The days an all-day date covers, the same wherever the talent is. All-day
 * dates start at midnight where they were entered, never more than twelve
 * hours from UTC midnight, so half a day in from each end is safely inside.
 */
function allDay(starts: string, ends: string): string {
  const day = (ms: number) =>
    new Intl.DateTimeFormat(undefined, { timeZone: "UTC", weekday: "short", day: "numeric", month: "short", year: "numeric" }).format(new Date(ms));
  const first = day(Date.parse(starts) + HALF_DAY);
  const last = day(Date.parse(ends) - HALF_DAY);
  return first === last ? first : `${first} – ${last}`;
}

/** "Sat 14 Jun" and "19:00–22:00 (Europe/London)" for a date, in the venue's clock. */
function when(d: PortalDate): { day: string; time: string } {
  if (d.all_day) return { day: allDay(d.starts_at, d.ends_at), time: "All day" };
  let day: string;
  try {
    day = new Intl.DateTimeFormat(undefined, { timeZone: d.timezone, weekday: "short", day: "numeric", month: "short", year: "numeric" }).format(
      new Date(d.starts_at),
    );
  } catch {
    day = new Date(d.starts_at).toDateString();
  }
  const local = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const zone = d.timezone === local ? "" : ` (${d.timezone.replace(/_/g, " ")})`;
  return { day, time: `${zonedTime(d.starts_at, d.timezone)}–${zonedTime(d.ends_at, d.timezone)}${zone}` };
}

// The frame ---------------------------------------------------------------------------------------

export function PortalShell({ me }: { me: Session["me"] }) {
  const actions = useSessionActions(me);
  const workspace = actions.workspace ?? me.workspaces[0]!;
  const location = useLocation();
  const navigate = useNavigate();
  // Another workspace may be a staff one, whose app starts at its home.
  const switchWorkspace = (id: string) => {
    actions.switchWorkspace(id);
    void navigate({ to: "/", replace: true });
  };
  const session: Session = { me, workspace, switchWorkspace, signOut: actions.signOut };
  const home = usePortal();
  const logo = useWorkspaceBranding(workspace.id, null, true, home.data ? { branding: home.data.branding } : null);
  const [theme, setTheme] = useState<Theme>(storedTheme);
  useEffect(() => {
    applyTheme(theme);
    try {
      localStorage.setItem(THEME_KEY, theme);
    } catch {
      // Not remembered in private browsing.
    }
  }, [theme]);
  // The staff app's addresses mean nothing here.
  const inPortal = location.pathname === "/portal" || location.pathname.startsWith("/portal/");
  useEffect(() => {
    if (!inPortal) void navigate({ to: "/portal", replace: true });
  }, [inPortal, navigate]);

  const settings = home.data?.settings;
  const tabs: { to: string; label: string; icon: ReactNode; shown: boolean }[] = [
    { to: "/portal", label: "Dates", icon: <CalendarCheck />, shown: true },
    { to: "/portal/availability", label: "Availability", icon: <CalendarClock />, shown: true },
    { to: "/portal/documents", label: "Documents", icon: <FileText />, shown: !!settings?.show_documents },
    { to: "/portal/money", label: "Money", icon: <Wallet />, shown: !!settings?.show_money },
  ];
  const shown = tabs.filter((t) => t.shown);
  const locked = !!settings && !settings.hide_locked;
  const initials = me.user.name
    .split(/\s+/)
    .map((p) => p[0])
    .join("")
    .slice(0, 2)
    .toUpperCase();

  return (
    <SessionContext.Provider value={session}>
      <div className="flex min-h-dvh flex-col pb-16 sm:pb-0">
        <header className="sticky top-0 z-20 flex h-14 items-center gap-3 border-b bg-background/95 px-4 backdrop-blur">
          {logo ? (
            <img src={logo} alt="" className="h-8 w-auto max-w-28 object-contain brand-logo" />
          ) : null}
          <div className="flex min-w-0 flex-1 flex-col">
            <span className="truncate text-sm font-semibold">{home.data?.talent.map((t) => t.name).join(", ") || me.user.name}</span>
            <span className="truncate text-xs text-muted-foreground">with {workspace.name}</span>
          </div>
          <nav aria-label="Portal" className="hidden min-w-0 gap-1 overflow-x-auto sm:flex">
            {shown.map((t) => (
              <Link
                key={t.to}
                to={t.to}
                activeOptions={{ exact: t.to === "/portal" }}
                className="flex shrink-0 items-center gap-1.5 rounded-md px-2.5 py-1.5 text-sm hover:bg-accent [&_svg]:size-4"
                activeProps={{ className: "bg-accent font-medium" }}
              >
                {t.icon}
                {t.label}
              </Link>
            ))}
            {locked && (
              <Link
                to="/portal/unlock"
                aria-label="Unlock features"
                className="flex shrink-0 items-center gap-1.5 rounded-md border border-dashed px-2.5 py-1.5 text-sm hover:bg-accent [&_svg]:size-4"
                activeProps={{ className: "bg-accent font-medium" }}
              >
                <Sparkles />
                <span className="hidden lg:inline">Unlock features</span>
              </Link>
            )}
          </nav>
          <Menu>
            <MenuTrigger asChild>
              <Button variant="ghost" size="icon" aria-label="Account" className="rounded-full bg-secondary">
                {initials || "?"}
              </Button>
            </MenuTrigger>
            <MenuContent>
              <MenuLabel>
                {me.user.name}
                <br />
                {me.user.email}
              </MenuLabel>
              {me.workspaces.length > 1 && (
                <>
                  <MenuSeparator />
                  <MenuLabel>Switch to</MenuLabel>
                  {me.workspaces.map((w) => (
                    <MenuItem key={w.id} onSelect={() => switchWorkspace(w.id)}>
                      {w.name} {w.id === workspace.id && "✓"}
                    </MenuItem>
                  ))}
                </>
              )}
              <MenuSeparator />
              <MenuItem onSelect={() => setTheme("light")}>
                <Sun className="size-4" /> Light {theme === "light" && "✓"}
              </MenuItem>
              <MenuItem onSelect={() => setTheme("dark")}>
                <Moon className="size-4" /> Dark {theme === "dark" && "✓"}
              </MenuItem>
              <MenuItem onSelect={() => setTheme("system")}>
                <Monitor className="size-4" /> Device setting {theme === "system" && "✓"}
              </MenuItem>
              <MenuSeparator />
              <MenuItem onSelect={() => void actions.signOut()}>
                <LogOut className="size-4" /> Sign out
              </MenuItem>
            </MenuContent>
          </Menu>
        </header>
        <main className="mx-auto w-full max-w-2xl flex-1 px-4 py-4">
          {home.isPending ? (
            <div className="flex justify-center p-12">
              <Spinner className="size-6" />
            </div>
          ) : home.isError ? (
            <Alert>{home.error.message}</Alert>
          ) : home.data.talent.length === 0 ? (
            <Alert tone="info">Nothing is shared with you in {workspace.name} at the moment. Ask them to invite you again.</Alert>
          ) : inPortal ? (
            <Outlet />
          ) : null}
        </main>
        <nav aria-label="Portal" className="fixed inset-x-0 bottom-0 z-20 flex border-t bg-background sm:hidden">
          {shown.map((t) => (
            <Link
              key={t.to}
              to={t.to}
              activeOptions={{ exact: t.to === "/portal" }}
              className="flex flex-1 flex-col items-center gap-0.5 py-2 text-xs text-muted-foreground [&_svg]:size-5"
              activeProps={{ className: "text-foreground font-medium" }}
            >
              {t.icon}
              {t.label}
            </Link>
          ))}
          {locked && (
            <Link
              to="/portal/unlock"
              className="flex flex-1 flex-col items-center gap-0.5 py-2 text-xs text-muted-foreground [&_svg]:size-5"
              activeProps={{ className: "text-foreground font-medium" }}
            >
              <Sparkles />
              Unlock
            </Link>
          )}
        </nav>
      </div>
    </SessionContext.Provider>
  );
}

// Dates -----------------------------------------------------------------------------------------

export function PortalDatesPage() {
  const [past, setPast] = useState(false);
  const home = usePortal();
  const dates = useQuery({
    queryKey: ["portal", "dates", past ? "past" : "upcoming"],
    queryFn: () => ok(api.GET("/api/v1/portal/dates", { params: { query: { when: past ? "past" : "upcoming" } } })),
  });
  const waiting = home.data?.awaiting_answer ?? 0;
  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-2">
        <h1 className="text-xl font-semibold">Your dates</h1>
        <div role="tablist" className="flex rounded-md border p-0.5 text-sm">
          {[false, true].map((p) => (
            <button
              key={String(p)}
              type="button"
              role="tab"
              aria-selected={past === p}
              onClick={() => setPast(p)}
              className={cn("rounded px-3 py-1", past === p && "bg-accent font-medium")}
            >
              {p ? "Past" : "Coming up"}
            </button>
          ))}
        </div>
      </div>
      {!past && waiting > 0 && (
        <Alert tone="info">
          {waiting === 1 ? "One date is" : `${waiting} dates are`} waiting for your answer.
        </Alert>
      )}
      {dates.isPending ? (
        <Spinner />
      ) : dates.isError ? (
        <Alert>{dates.error.message}</Alert>
      ) : dates.data.length === 0 ? (
        <EmptyState title={past ? "No past dates yet" : "Nothing coming up yet"}>
          {past ? "Dates you have played show here." : "When your agency books or pencils you in, the date shows here."}
        </EmptyState>
      ) : (
        <ul className="flex flex-col gap-2">
          {dates.data.map((d) => (
            <DateCard key={`${d.id}-${d.talent_id}`} d={d} showTalent={(home.data?.talent.length ?? 0) > 1} />
          ))}
        </ul>
      )}
      {home.data && !home.data.settings.hide_locked && (
        <Link
          to="/portal/unlock"
          className="flex items-center gap-3 rounded-lg border border-dashed p-3 text-sm hover:bg-accent/50"
        >
          <Lock className="size-4 shrink-0 text-muted-foreground" />
          <span className="flex min-w-0 flex-1 flex-col">
            <span className="font-medium">Add gigs you book yourself</span>
            <span className="text-muted-foreground">Take your own bookings, send invoices and contracts, plan tours.</span>
          </span>
          <span className="shrink-0 font-medium">Unlock features</span>
        </Link>
      )}
    </div>
  );
}

function DateCard({ d, showTalent }: { d: PortalDate; showTalent: boolean }) {
  const w = when(d);
  return (
    <li>
      <Link to="/portal/dates/$id" params={{ id: d.id }} className="block rounded-lg border p-3 hover:bg-accent/50">
        <div className="flex items-start gap-3">
          <div className="flex min-w-0 flex-1 flex-col gap-1">
            <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">{w.day}</span>
            <span className={cn("font-semibold", d.status === "cancelled" && "line-through")}>{d.title}</span>
            <span className="flex flex-wrap items-center gap-x-3 text-sm text-muted-foreground">
              {d.venue && (
                <span className="flex items-center gap-1">
                  <MapPin className="size-3.5" /> {d.venue}
                </span>
              )}
              <span>{w.time}</span>
              {showTalent && <span>{d.talent_name}</span>}
            </span>
            <span className="flex flex-wrap gap-1.5 pt-0.5">
              <StatusBadge status={d.status} />
              <AnswerBadge d={d} />
            </span>
          </div>
          {d.fee && <span className="shrink-0 text-sm font-medium tabular-nums">{formatMoney(d.fee)}</span>}
        </div>
      </Link>
    </li>
  );
}

export function PortalDatePage() {
  const { id } = useParams({ strict: false }) as { id: string };
  const qc = useQueryClient();
  const detail = useQuery({
    queryKey: ["portal", "date", id],
    queryFn: () => ok(api.GET("/api/v1/portal/dates/{id}", { params: { path: { id } } })),
  });
  const [error, setError] = useState<string | null>(null);
  if (detail.isPending) return <Spinner />;
  if (detail.isError) {
    return (
      <div className="flex flex-col gap-3">
        <BackLink />
        <Alert>{detail.error instanceof ApiFailure && detail.error.status === 404 ? "This date is not shared with you." : detail.error.message}</Alert>
      </div>
    );
  }
  const d = detail.data.entries[0]!;
  const w = when(d);
  const saved = (next: Detail) => {
    qc.setQueryData(["portal", "date", id], next);
    void qc.invalidateQueries({ queryKey: ["portal"] });
  };
  return (
    <div className="flex flex-col gap-5">
      <BackLink />
      <div className="flex flex-col gap-1">
        <span className="text-sm font-medium text-muted-foreground">{w.day}</span>
        <h1 className={cn("text-xl font-semibold", d.status === "cancelled" && "line-through")}>{d.title}</h1>
        <div className="flex flex-wrap gap-1.5">
          <StatusBadge status={d.status} />
        </div>
      </div>
      <Card className="flex flex-col gap-2 p-4 text-sm">
        <Row label="When">{w.time}</Row>
        {d.venue && <Row label="Where">{d.venue}</Row>}
        {d.organisation_name && <Row label="For">{d.organisation_name}</Row>}
        {d.fee && <Row label="Fee">{formatMoney(d.fee)}</Row>}
        {d.status === "hold" && d.hold_expires_at && (
          <Row label="Pencilled until">{new Date(d.hold_expires_at).toLocaleDateString(undefined, { day: "numeric", month: "short" })}</Row>
        )}
      </Card>
      {Array.isArray(d.schedule) && d.schedule.length > 0 && (
        <section className="flex flex-col gap-2">
          <h2 className="font-semibold">Running order</h2>
          <ul className="flex flex-col divide-y rounded-lg border text-sm">
            {(d.schedule as { label: string; at: string }[]).map((s) => (
              <li key={`${s.label}-${s.at}`} className="flex gap-3 px-3 py-2">
                <span className="w-20 shrink-0 tabular-nums">{zonedTime(s.at, d.timezone)}</span>
                <span>{s.label}</span>
              </li>
            ))}
          </ul>
        </section>
      )}
      {detail.data.entries.map((e) => (
        <AnswerBox
            key={e.talent_id}
            d={e}
            several={detail.data.entries.length > 1}
            onSaved={saved}
            onError={(m) => {
              setError(m);
              // The date may have moved on (confirmed, cancelled): show it as it is now.
              if (m) void qc.invalidateQueries({ queryKey: ["portal"] });
            }}
          />
      ))}
      {error && <Alert>{error}</Alert>}
      {detail.data.travel.length > 0 && (
        <section className="flex flex-col gap-2">
          <h2 className="font-semibold">Travel</h2>
          <ul className="flex flex-col gap-2">
            {detail.data.travel.map((t) => (
              <TravelLine
                key={t.id}
                item={{ ...t, booking_id: d.id, project_id: null, cost: null, paid_by: "us", version: 0, created_at: t.starts_at, updated_at: t.starts_at }}
              />
            ))}
          </ul>
        </section>
      )}
      {detail.data.documents.length > 0 && (
        <section className="flex flex-col gap-2">
          <h2 className="font-semibold">Documents</h2>
          <DocumentList documents={detail.data.documents} />
        </section>
      )}
    </div>
  );
}

function BackLink() {
  return (
    <Link to="/portal" className="flex w-fit items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
      <ChevronLeft className="size-4" /> Your dates
    </Link>
  );
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex gap-3">
      <span className="w-28 shrink-0 text-muted-foreground">{label}</span>
      <span className="min-w-0 flex-1">{children}</span>
    </div>
  );
}

/** Confirm or decline: the agent hears either way, and can still be asked. */
function AnswerBox({
  d,
  several,
  onSaved,
  onError,
}: {
  d: PortalDate;
  several: boolean;
  onSaved: (next: Detail) => void;
  onError: (message: string | null) => void;
}) {
  const [declining, setDeclining] = useState(false);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const send = async (answer: "confirmed" | "declined") => {
    setBusy(true);
    onError(null);
    try {
      const next = await ok(
        api.POST("/api/v1/portal/dates/{id}/answer", {
          params: { path: { id: d.id } },
          body: { answer, talent_id: d.talent_id, note: note.trim() || null },
        }),
      );
      setDeclining(false);
      setNote("");
      onSaved(next);
    } catch (e) {
      onError(failure(e));
    } finally {
      setBusy(false);
    }
  };
  if (!d.can_answer && !d.answer) return null;
  return (
    <Card className="flex flex-col gap-3 p-4">
      <div className="flex flex-col gap-1">
        <h2 className="font-semibold">{several ? `Can ${d.talent_name} do it?` : "Can you do it?"}</h2>
        {d.answer ? (
          <p className="text-sm">
            {d.answer === "confirmed" ? "You confirmed this date" : "You said you can't do this date"}
            {d.answered_at && ` on ${new Date(d.answered_at).toLocaleDateString(undefined, { day: "numeric", month: "short" })}`}.
            {d.answer_note && <span className="block text-muted-foreground">“{d.answer_note}”</span>}
          </p>
        ) : (
          <p className="text-sm text-muted-foreground">Your agency is told straight away.</p>
        )}
      </div>
      {d.can_answer &&
        (declining ? (
          <div className="flex flex-col gap-2">
            <Textarea
              aria-label="Why you can't do it"
              placeholder="A few words for your agency (optional)"
              maxLength={500}
              value={note}
              onChange={(e) => setNote(e.target.value)}
            />
            <div className="flex justify-end gap-2">
              <Button variant="ghost" onClick={() => setDeclining(false)}>
                Back
              </Button>
              <Button variant="destructive" disabled={busy} onClick={() => void send("declined")}>
                Send: I can't do it
              </Button>
            </div>
          </div>
        ) : (
          <div className="flex flex-wrap gap-2">
            {d.answer !== "confirmed" && (
              <Button className="flex-1" disabled={busy} onClick={() => void send("confirmed")}>
                <CalendarCheck /> Confirm
              </Button>
            )}
            {d.answer !== "declined" && (
              <Button variant="outline" className="flex-1" disabled={busy} onClick={() => setDeclining(true)}>
                <CalendarX2 /> I can't do it
              </Button>
            )}
          </div>
        ))}
    </Card>
  );
}

// Documents ----------------------------------------------------------------------------------------

const KINDS: Record<string, string> = {
  contract: "Contract",
  confirmation: "Confirmation",
  advance_sheet: "Advance sheet",
  rider: "Rider",
  tech_spec: "Tech spec",
  stage_plot: "Stage plot",
  epk: "Press kit",
  insurance: "Insurance",
  passport: "Passport",
  visa: "Visa",
  statement: "Statement",
};

function download(id: string, title: string) {
  return saveDownload(
    api.GET("/api/v1/portal/documents/{id}/download", { params: { path: { id } }, parseAs: "blob" }),
    `${title}.pdf`,
  );
}

function DocumentList({ documents }: { documents: PortalDocument[] }) {
  const [error, setError] = useState<string | null>(null);
  return (
    <>
      <ul className="flex flex-col divide-y rounded-lg border">
        {documents.map((doc) => (
          <li key={doc.id} className="flex items-center gap-3 px-3 py-2 text-sm">
            <FileText className="size-4 shrink-0 text-muted-foreground" />
            <span className="flex min-w-0 flex-1 flex-col">
              <span className="truncate font-medium">{doc.title}</span>
              <span className="text-xs text-muted-foreground">
                {KINDS[doc.kind] ?? doc.kind}
                {doc.signature_status === "signed" && " · signed"}
                {doc.expires_on && ` · runs out ${new Date(`${doc.expires_on}T12:00:00`).toLocaleDateString()}`}
              </span>
            </span>
            <Button
              variant="ghost"
              size="icon"
              aria-label={`Download ${doc.title}`}
              onClick={() => void download(doc.id, doc.title).catch((e: unknown) => setError(failure(e)))}
            >
              <Download />
            </Button>
          </li>
        ))}
      </ul>
      {error && <Alert>{error}</Alert>}
    </>
  );
}

export function PortalDocumentsPage() {
  const docs = useQuery({ queryKey: ["portal", "documents"], queryFn: () => ok(api.GET("/api/v1/portal/documents")) });
  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-xl font-semibold">Documents</h1>
      {docs.isPending ? (
        <Spinner />
      ) : docs.isError ? (
        <Alert>{docs.error.message}</Alert>
      ) : docs.data.length === 0 ? (
        <EmptyState title="No documents yet">Contracts, advance sheets and riders your agency shares show here.</EmptyState>
      ) : (
        <DocumentList documents={docs.data} />
      )}
    </div>
  );
}

// Availability -----------------------------------------------------------------------------------

const AVAILABILITY_LABEL: Record<string, string> = {
  unavailable: "Unavailable",
  tentative: "Tentative",
  available_only: "Available only",
};

const CALENDAR_PROVIDER: Record<string, string> = {
  google: "Google Calendar",
  microsoft: "Outlook",
  ical: "iCal link",
};

export function PortalAvailabilityPage() {
  const qc = useQueryClient();
  const { workspace } = useSession();
  const home = usePortal();
  const data = useQuery({ queryKey: ["portal", "availability"], queryFn: () => ok(api.GET("/api/v1/portal/availability")) });
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [connecting, setConnecting] = useState(false);
  const talent = home.data?.talent ?? [];
  const [forTalent, setForTalent] = useState<string | undefined>(undefined);
  const chosen = forTalent ?? talent[0]?.id;
  const refresh = () => void qc.invalidateQueries({ queryKey: ["portal", "availability"] });
  if (data.isPending) return <Spinner />;
  if (data.isError) return <Alert>{data.error.message}</Alert>;
  const now = Date.now();
  const upcoming = data.data.entries.filter((a) => new Date(a.ends_at).getTime() > now);
  return (
    <div className="flex flex-col gap-5">
      <div className="flex items-center justify-between gap-2">
        <h1 className="text-xl font-semibold">Availability</h1>
        {data.data.can_edit && !adding && (
          <Button variant="outline" size="sm" onClick={() => setAdding(true)}>
            <Plus /> Mark dates
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">
        Mark when you can&apos;t work, so {workspace.name} doesn&apos;t book you then. Tentative dates only warn them.
      </p>
      {talent.length > 1 && data.data.can_edit && (
        <label className="flex items-center gap-2 text-sm">
          For
          <select className="rounded-md border bg-background px-2 py-1" value={chosen} onChange={(e) => setForTalent(e.target.value)}>
            {talent.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </select>
        </label>
      )}
      {adding && (
        <AvailabilityForm
          save={(body) =>
            ok(api.POST("/api/v1/portal/availability", { body: { ...body, talent_id: chosen ?? null } })).then(() => undefined)
          }
          onDone={() => {
            setAdding(false);
            refresh();
          }}
          onCancel={() => setAdding(false)}
        />
      )}
      {error && <Alert>{error}</Alert>}
      {upcoming.length === 0 ? (
        <p className="text-sm text-muted-foreground">Nothing marked from today on.</p>
      ) : (
        <ul className="flex flex-col divide-y rounded-lg border">
          {upcoming.map((a) => (
            <li key={a.id} className="flex items-center gap-2 px-3 py-2 text-sm">
              <span className="flex min-w-0 flex-1 flex-col">
                <span>
                  <span className="font-medium">{AVAILABILITY_LABEL[a.kind] ?? a.kind}</span> · {formatWhen(a.starts_at, a.ends_at, a.all_day)}
                </span>
                <span className="truncate text-xs text-muted-foreground">
                  {[talent.length > 1 ? a.talent_name : null, a.source === "external" ? "From your calendar" : a.source === "manual" ? `Added by ${workspace.name}` : null, a.source === "external" ? null : a.note]
                    .filter(Boolean)
                    .join(" · ")}
                </span>
              </span>
              {data.data.can_edit && a.source === "portal" && (
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={`Remove ${formatWhen(a.starts_at, a.ends_at, a.all_day)}`}
                  onClick={async () => {
                    setError(null);
                    try {
                      await ok(api.DELETE("/api/v1/portal/availability/{id}", { params: { path: { id: a.id } } }));
                      refresh();
                    } catch (e) {
                      setError(failure(e));
                    }
                  }}
                >
                  <Trash2 />
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
      <section className="flex flex-col gap-2">
        <h2 className="font-semibold">Your own calendar</h2>
        <p className="text-sm text-muted-foreground">
          Connect Google, Outlook or Apple Calendar and your busy times update by themselves. Only when you are busy is
          read, never what you are doing.
        </p>
        {data.data.calendars.length > 0 && (
          <ul className="flex flex-col divide-y rounded-lg border text-sm">
            {data.data.calendars.map((c) => (
              <li key={c.id} className="flex flex-wrap items-center gap-2 px-3 py-2">
                <span className="font-medium">{CALENDAR_PROVIDER[c.provider] ?? c.provider}</span>
                <span className="text-muted-foreground">{c.person ?? c.account}</span>
                <span className="flex-1" />
                {c.status === "reconnect" ? (
                  <Badge className="border border-destructive text-destructive">Needs connecting again</Badge>
                ) : (
                  <Badge className="border border-success text-success">Connected</Badge>
                )}
              </li>
            ))}
          </ul>
        )}
        {data.data.can_edit && (
          <Button
            variant="outline"
            className="w-fit"
            disabled={connecting}
            onClick={async () => {
              setConnecting(true);
              setError(null);
              try {
                const link = await ok(api.POST("/api/v1/portal/calendar-link", { body: { talent_id: chosen ?? null } }));
                // The same page an agency's link opens: it walks them through connecting.
                window.location.assign(new URL(link.url).pathname);
              } catch (e) {
                setError(failure(e));
                setConnecting(false);
              }
            }}
          >
            <Link2 /> Connect a calendar
          </Button>
        )}
      </section>
    </div>
  );
}

// Money ---------------------------------------------------------------------------------------------

export function PortalMoneyPage() {
  const money = useQuery({ queryKey: ["portal", "money"], queryFn: () => ok(api.GET("/api/v1/portal/money")) });
  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-xl font-semibold">Money</h1>
      {money.isPending ? (
        <Spinner />
      ) : money.isError ? (
        <Alert>{money.error.message}</Alert>
      ) : money.data.length === 0 ? (
        <EmptyState title="No statements yet">After a date is settled, what it paid you shows here.</EmptyState>
      ) : (
        <ul className="flex flex-col gap-2">
          {money.data.map((s) => (
            <StatementCard key={s.id} s={s} />
          ))}
        </ul>
      )}
    </div>
  );
}

function StatementCard({ s }: { s: Statement }) {
  const [error, setError] = useState<string | null>(null);
  const owed = s.net.minor - s.paid.minor;
  return (
    <li className="flex flex-col gap-2 rounded-lg border p-3 text-sm">
      <div className="flex items-start gap-2">
        <span className="flex min-w-0 flex-1 flex-col">
          <span className="font-medium">{s.booking_title}</span>
          <span className="text-xs text-muted-foreground">
            {new Date(s.starts_at).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" })}
          </span>
        </span>
        <span className="text-right font-semibold tabular-nums">{formatMoney(s.net)}</span>
      </div>
      <div className="grid grid-cols-2 gap-x-3 text-xs text-muted-foreground">
        <span>Fee</span>
        <span className="text-right tabular-nums">{formatMoney(s.gross)}</span>
        <span>Commission</span>
        <span className="text-right tabular-nums">−{formatMoney(s.commission)}</span>
        {s.expenses.minor > 0 && (
          <>
            <span>Expenses</span>
            <span className="text-right tabular-nums">−{formatMoney(s.expenses)}</span>
          </>
        )}
        <span>Paid to you</span>
        <span className="text-right tabular-nums">{formatMoney(s.paid)}</span>
      </div>
      <div className="flex items-center gap-2">
        {owed > 0 ? (
          <Badge className="border border-warning text-warning">{formatMoney({ minor: owed, currency: s.net.currency })} still to come</Badge>
        ) : (
          <Badge className="border border-success text-success">Paid</Badge>
        )}
        <span className="flex-1" />
        {s.document_id && (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => void download(s.document_id!, `Statement ${s.booking_title}`).catch((e: unknown) => setError(failure(e)))}
          >
            <Download /> Statement
          </Button>
        )}
      </div>
      {error && <Alert>{error}</Alert>}
    </li>
  );
}

// Unlock features ---------------------------------------------------------------------------------

/** What the full app adds; each shows padlocked here. */
const LOCKED: { label: string; icon: ReactNode; blurb: string }[] = [
  { label: "Your own bookings", icon: <Briefcase />, blurb: "Take enquiries, pencil holds and confirm gigs yourself, next to the ones your agency books." },
  { label: "The full calendar", icon: <CalendarDays />, blurb: "Every gig, hold and day off on one calendar, shared with whoever you choose." },
  { label: "Invoices and deposits", icon: <Receipt />, blurb: "Send invoices, take deposits by card and see who still owes you." },
  { label: "Contracts and signing", icon: <FileSignature />, blurb: "Send contracts and riders and have them signed online." },
  { label: "Email inbox", icon: <Inbox />, blurb: "Booking emails filed next to the gig they belong to." },
  { label: "Tours and travel", icon: <Route />, blurb: "Plan runs of dates with travel, hotels and a day-by-day itinerary." },
];

const BOARD: { status: string; label: string }[] = [
  { status: "hold", label: "Pencilled" },
  { status: "pending", label: "Being confirmed" },
  { status: "confirmed", label: "Confirmed" },
];

/**
 * A preview of the full app built from the talent's own dates, with what it
 * would add shown padlocked, and the way to upgrade: on the website, never
 * inside the app. Hidden when the agency hides locked features.
 */
export function PortalUnlockPage() {
  const home = usePortal();
  const navigate = useNavigate();
  const hidden = home.data?.settings.hide_locked;
  useEffect(() => {
    if (hidden) void navigate({ to: "/portal", replace: true });
  }, [hidden, navigate]);
  const upcoming = useQuery({
    queryKey: ["portal", "dates", "upcoming"],
    queryFn: () => ok(api.GET("/api/v1/portal/dates", { params: { query: { when: "upcoming" } } })),
    enabled: hidden === false,
  });
  const past = useQuery({
    queryKey: ["portal", "dates", "past"],
    queryFn: () => ok(api.GET("/api/v1/portal/dates", { params: { query: { when: "past" } } })),
    enabled: hidden === false,
  });
  if (!home.data || hidden) return <Spinner />;
  // A date with several of their acts on it is still one date.
  const once = (xs: PortalDate[]) => [...new Map(xs.map((d) => [d.id, d])).values()];
  const coming = once((upcoming.data ?? []).filter((d) => d.status !== "cancelled"));
  const yearAgo = Date.now() - 365 * 24 * 3600 * 1000;
  const lastYear = once((past.data ?? []).filter((d) => d.status !== "cancelled" && Date.parse(d.starts_at) >= yearAgo));
  const failed = upcoming.isError || past.isError;
  const venues = new Set([...coming, ...lastYear].map((d) => d.venue).filter(Boolean)).size;
  const url = home.data.upgrade_url;
  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-col gap-1">
        <h1 className="text-xl font-semibold">Unlock the full app</h1>
        <p className="text-sm text-muted-foreground">
          This page is free and stays free. The full app adds the tools to run your own bookings and business. Here is how it would look with your
          dates.
        </p>
      </div>

      <section aria-label="Preview" className="relative flex flex-col gap-3 rounded-lg border border-dashed p-3">
        <span className="absolute -top-2.5 left-3 bg-background px-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
          Preview
        </span>
        <dl className="grid grid-cols-3 gap-2 text-center">
          {[
            { n: coming.length, label: "coming up" },
            { n: lastYear.length, label: "in the past year" },
            { n: venues, label: venues === 1 ? "venue" : "venues" },
          ].map((s) => (
            <div key={s.label} className="rounded-md bg-secondary px-2 py-2">
              <dt className="sr-only">{s.label}</dt>
              <dd className="text-lg font-semibold tabular-nums">{failed ? "–" : upcoming.isPending || past.isPending ? "…" : s.n}</dd>
              <dd className="text-xs text-muted-foreground" aria-hidden>
                {s.label}
              </dd>
            </div>
          ))}
        </dl>
        {failed && <Alert>Your dates could not be loaded just now.</Alert>}
        <section className="grid gap-2 sm:grid-cols-3" aria-label="Your bookings board">
          {BOARD.map((col) => {
            const items = coming.filter((d) => d.status === col.status);
            return (
              <div key={col.status} className="flex flex-col gap-1.5 rounded-md bg-secondary/60 p-2">
                <span className="flex items-center justify-between text-xs font-medium">
                  {col.label}
                  <span className="tabular-nums text-muted-foreground">{items.length}</span>
                </span>
                {items.slice(0, 3).map((d) => (
                  <div key={d.id} className="rounded border bg-background px-2 py-1.5 text-xs">
                    <span className="block truncate font-medium">{d.title}</span>
                    <span className="text-muted-foreground">{when(d).day}</span>
                  </div>
                ))}
                {items.length > 3 && <span className="text-xs text-muted-foreground">and {items.length - 3} more</span>}
                <span className="flex items-center gap-1 rounded border border-dashed px-2 py-1.5 text-xs text-muted-foreground">
                  <Lock className="size-3" /> Add your own
                </span>
              </div>
            );
          })}
        </section>
      </section>

      <ul className="grid gap-2 sm:grid-cols-2" aria-label="Locked features">
        {LOCKED.map((f) => (
          <li key={f.label} className="flex items-start gap-3 rounded-lg border p-3 [&>svg]:size-5">
            <span className="mt-0.5 text-muted-foreground [&_svg]:size-5">{f.icon}</span>
            <span className="flex min-w-0 flex-1 flex-col gap-0.5">
              <span className="flex items-center gap-1.5 text-sm font-medium">
                {f.label}
                <Lock className="size-3.5 text-muted-foreground" aria-label="Locked" />
              </span>
              <span className="text-xs text-muted-foreground">{f.blurb}</span>
            </span>
          </li>
        ))}
      </ul>

      <Card className="flex flex-col gap-2 p-4">
        {url ? (
          <>
            <Button
              onClick={() => {
                const link = new URL(url);
                link.searchParams.set("from", "portal");
                void openInBrowser(link.toString());
              }}
            >
              Upgrade on the website <ExternalLink />
            </Button>
            <p className="text-center text-xs text-muted-foreground">
              Upgrades are made on the OnHold website, not in this app. Your dates with {home.data.workspace_name} stay as they are.
            </p>
          </>
        ) : (
          <p className="text-center text-sm text-muted-foreground">
            Upgrades open on the OnHold website soon. Nothing is sold inside this app.
          </p>
        )}
      </Card>
    </div>
  );
}
