// The signed-in app's frame: navigation, workspace switcher, ⌘K search and
// the user menu. Anyone not signed in is sent to sign in first.

import { Link, Outlet, useLocation, useNavigate } from "@tanstack/react-router";
import {
  Button,
  CommandPalette,
  Dialog,
  Menu,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuSeparator,
  MenuTrigger,
  type PaletteAction,
  type PaletteHit,
  Spinner,
  type Theme,
  applyTheme,
  cn,
} from "@onhold/ui";
import {
  Building2,
  CalendarCheck,
  CalendarDays,
  CalendarCog,
  CreditCard,
  CheckSquare,
  ChevronsUpDown,
  Inbox,
  Mail,
  AtSign,
  CloudOff,
  Download,
  Contact,
  ChartColumn,
  FileSearch,
  FileText,
  FileUp,
  HandCoins,
  Landmark,
  Receipt,
  RefreshCw,
  LogOut,
  Menu as MenuIcon,
  Mic2,
  Monitor,
  Moon,
  Search,
  SlidersHorizontal,
  Sun,
  Workflow,
  X,
} from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useCallback, useEffect, useState } from "react";
import { ENTITIES, entityByPath } from "./api.ts";
import { OfflineContext, searchEverything, useSyncStatus } from "./data.ts";
import { claimFor, forgetMailExcept } from "./offline/store.ts";
import { OfflineSync } from "./offline/sync.ts";
import { IS_APP, KEEPS_OFFLINE_COPY, serverUrl } from "./platform.ts";
import { GoTo } from "./go-to.tsx";
import { useInstallOffer } from "./install.ts";
import { type Session, SessionContext, useMe, useSessionActions } from "./session.tsx";

const NAV: { group: string; items: { to: string; label: string; icon: ReactNode }[] }[] = [
  {
    group: "Bookings",
    items: [
      { to: "/inbox", label: "Inbox", icon: <Mail /> },
      { to: "/calendar", label: "Calendar", icon: <CalendarDays /> },
      { to: "/enquiries", label: "Enquiries", icon: <Inbox /> },
      { to: "/bookings", label: "Bookings", icon: <CalendarCheck /> },
      { to: "/tasks", label: "Tasks", icon: <CheckSquare /> },
    ],
  },
  {
    group: "Money",
    items: [
      { to: "/finance/invoices", label: "Invoices", icon: <Receipt /> },
      { to: "/finance/payments", label: "Payments", icon: <HandCoins /> },
      { to: "/finance/reports", label: "Reports", icon: <ChartColumn /> },
    ],
  },
  {
    group: "CRM",
    items: [
      { to: "/crm/contacts", label: "Contacts", icon: <Contact /> },
      { to: "/crm/organisations", label: "Organisations", icon: <Building2 /> },
      { to: "/crm/talent", label: "Talent", icon: <Mic2 /> },
    ],
  },
  {
    group: "Data",
    items: [
      { to: "/documents", label: "Documents", icon: <FileText /> },
      { to: "/documents/imports", label: "Import past PDFs", icon: <FileSearch /> },
      { to: "/imports", label: "Imports", icon: <FileUp /> },
    ],
  },
  {
    group: "Settings",
    items: [
      { to: "/settings/email", label: "Email accounts", icon: <AtSign /> },
      { to: "/settings/fields", label: "Custom fields", icon: <SlidersHorizontal /> },
      { to: "/settings/pipelines", label: "Pipelines", icon: <Workflow /> },
      { to: "/settings/calendar", label: "Calendar settings", icon: <CalendarCog /> },
      { to: "/settings/finance", label: "Invoicing", icon: <Landmark /> },
      { to: "/settings/payments", label: "Card payments", icon: <CreditCard /> },
    ],
  },
];

/** Starts keeping the workspace on the device (apps only) for as long as it is open. */
/** The queries that read the device's copy, refreshed when it changes. */
const REFRESHED_BY_SYNC = [
  "list",
  "record",
  "tags",
  "schema",
  "org-search",
  "enquiries",
  "enquiry",
  "enquiry-stages",
  "bookings",
  "booking",
  "tasks",
  "calendar",
  "clashes",
  "members",
  "quotes",
  "pick",
];

function useOfflineSync(workspaceId: string | undefined, userId: string): OfflineSync | null {
  const qc = useQueryClient();
  const [sync, setSync] = useState<OfflineSync | null>(null);
  useEffect(() => {
    if (!KEEPS_OFFLINE_COPY || !workspaceId) return;
    const engine = new OfflineSync(workspaceId);
    let stopped = false;
    const off = engine.onRecordsChanged(() => {
      void qc.invalidateQueries({
        predicate: (q) => REFRESHED_BY_SYNC.includes(String(q.queryKey[0])),
      });
    });
    void claimFor(userId).then(() => {
      if (stopped) return;
      setSync(engine);
      void engine.start();
    });
    return () => {
      stopped = true;
      off();
      engine.stop();
      setSync(null);
    };
  }, [workspaceId, userId, qc]);
  return sync;
}

const THEME_KEY = "onhold.theme";

function storedTheme(): Theme {
  try {
    const t = localStorage.getItem(THEME_KEY);
    return t === "light" || t === "dark" ? t : "system";
  } catch {
    return "system";
  }
}

export function AppShell() {
  const me = useMe();
  const location = useLocation();
  if (me.isPending) {
    return (
      <div className="flex h-dvh items-center justify-center">
        <Spinner className="size-6" />
      </div>
    );
  }
  if (me.isError) {
    return <div className="p-6 text-sm">Could not reach OnHold: {me.error.message}</div>;
  }
  if (!me.data) {
    return <GoTo to="/sign-in" redirect={location.href} />;
  }
  if (!me.data.session.mfa_verified) {
    return <GoTo to="/auth/mfa" redirect={location.href} />;
  }
  if (me.data.workspaces.length === 0) {
    return (
      <div className="mx-auto max-w-md p-6 text-sm">
        You are signed in as {me.data.user.email} but are not a member of any workspace yet. Ask for an invitation.
      </div>
    );
  }
  if (IS_APP && !me.data.workspaces.some((w) => w.apps_allowed)) {
    return (
      <div className="mx-auto max-w-md p-6 text-sm">
        The desktop and mobile apps are not part of your workspace&apos;s plan. You can use OnHold in your browser at{" "}
        {serverUrl() || "your OnHold address"}.
      </div>
    );
  }
  return <SignedIn me={me.data} />;
}

function SignedIn({ me }: { me: Session["me"] }) {
  const { workspace, switchWorkspace, signOut } = useSessionActions(me);
  const current = workspace ?? me.workspaces[0];
  // The plan switch is for the apps; the installed website is the website.
  const sync = useOfflineSync(current && (!IS_APP || current.apps_allowed) ? current.id : undefined, me.user.id);
  // Email kept for a workspace the person has left, or whose plan dropped the apps, goes.
  const keptFor = me.workspaces.filter((w) => !IS_APP || w.apps_allowed).map((w) => w.id).join(",");
  useEffect(() => {
    if (KEEPS_OFFLINE_COPY) void forgetMailExcept(keptFor.split(",")).catch(() => undefined);
  }, [keptFor]);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [navOpen, setNavOpen] = useState(false);
  const [theme, setTheme] = useState<Theme>(storedTheme);
  const installOffer = useInstallOffer();
  const [installHelp, setInstallHelp] = useState(false);
  const navigate = useNavigate();
  const location = useLocation();

  useEffect(() => {
    applyTheme(theme);
    try {
      localStorage.setItem(THEME_KEY, theme);
    } catch {
      // Not remembered in private browsing.
    }
  }, [theme]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen((o) => !o);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  // Close the mobile menu after navigating.
  useEffect(() => setNavOpen(false), [location.pathname]);

  const search = useCallback(
    async (q: string): Promise<PaletteHit[]> => {
      const hits = await searchEverything(sync, q);
      return hits.map((h) => ({ id: h.id, title: h.name, detail: h.detail, group: ENTITIES[h.entity]?.label ?? h.entity }));
    },
    [sync],
  );
  const actions: PaletteAction[] = NAV.flatMap((g) =>
    g.items.map((i) => ({ id: i.to, title: `Go to ${i.label}`, group: "Pages", icon: i.icon })),
  );
  const paths: Record<string, string> = { Contacts: "contacts", Organisations: "organisations", Talent: "talent" };

  const session: Session = { me, workspace: workspace ?? me.workspaces[0]!, switchWorkspace, signOut };
  const initials = me.user.name
    .split(/\s+/)
    .map((p) => p[0])
    .join("")
    .slice(0, 2)
    .toUpperCase();

  const nav = (
    <nav aria-label="Main" className="flex flex-col gap-4 p-3">
      {NAV.map((g) => (
        <div key={g.group} className="flex flex-col gap-0.5">
          <p className="px-2 pb-1 text-xs font-medium text-muted-foreground">{g.group}</p>
          {g.items.map((i) => (
            <Link
              key={i.to}
              to={i.to}
              className="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-accent [&_svg]:size-4"
              activeProps={{ className: "bg-accent font-medium text-accent-foreground" }}
            >
              {i.icon}
              {i.label}
            </Link>
          ))}
        </div>
      ))}
    </nav>
  );

  return (
    <SessionContext.Provider value={session}>
      <OfflineContext.Provider value={sync}>
      <div className="flex h-dvh">
        <aside className="hidden w-56 shrink-0 flex-col border-r bg-muted/30 md:flex">
          <WorkspaceSwitcher session={session} />
          {nav}
        </aside>
        {navOpen && (
          <div className="fixed inset-0 z-30 md:hidden">
            <button type="button" aria-label="Close menu" className="absolute inset-0 bg-black/40" onClick={() => setNavOpen(false)} />
            <aside className="relative flex h-full w-64 flex-col border-r bg-background">
              <div className="flex items-center justify-between pr-2">
                <WorkspaceSwitcher session={session} />
                <Button variant="ghost" size="icon" aria-label="Close menu" onClick={() => setNavOpen(false)}>
                  <X />
                </Button>
              </div>
              {nav}
            </aside>
          </div>
        )}
        <div className="flex min-w-0 flex-1 flex-col">
          <header className="flex h-14 shrink-0 items-center gap-2 border-b px-3 sm:px-4">
            <Button variant="ghost" size="icon" className="md:hidden" aria-label="Open menu" onClick={() => setNavOpen(true)}>
              <MenuIcon />
            </Button>
            <button
              type="button"
              onClick={() => setPaletteOpen(true)}
              className="flex h-9 max-w-md flex-1 items-center gap-2 rounded-md border bg-background px-3 text-sm text-muted-foreground hover:bg-accent"
            >
              <Search className="size-4" />
              <span className="flex-1 text-left">Search…</span>
              <kbd className="hidden rounded border px-1.5 text-xs sm:inline">⌘K</kbd>
            </button>
            <div className="flex-1" />
            {sync && <SyncIndicator />}
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
                {installOffer && (
                  <>
                    <MenuSeparator />
                    <MenuItem
                      onSelect={() =>
                        installOffer.kind === "prompt" ? void installOffer.install() : setInstallHelp(true)
                      }
                    >
                      <Download className="size-4" /> Install the app
                    </MenuItem>
                  </>
                )}
                <MenuSeparator />
                <MenuItem onSelect={() => void signOut()}>
                  <LogOut className="size-4" /> Sign out
                </MenuItem>
              </MenuContent>
            </Menu>
          </header>
          <main className="min-w-0 flex-1 overflow-y-auto">
            {IS_APP && !session.workspace.apps_allowed ? (
              <p className="mx-auto max-w-md p-6 text-sm">
                The apps are not part of {session.workspace.name}&apos;s plan. Switch workspace, or use OnHold in your
                browser.
              </p>
            ) : (
              <Outlet />
            )}
          </main>
        </div>
      </div>
      <Dialog open={installHelp} onOpenChange={setInstallHelp} title="Install the app">
        <p className="text-sm">
          In Safari, tap the Share button, then <strong>Add to Home Screen</strong>. The app then opens from its own icon,
          full screen, and keeps working without a connection.
        </p>
      </Dialog>
      <CommandPalette
        open={paletteOpen}
        onOpenChange={setPaletteOpen}
        search={search}
        actions={actions}
        onSelect={(item) => {
          if ("detail" in item || !item.id.startsWith("/")) {
            const hit = item as PaletteHit;
            const entity = entityByPath(paths[hit.group] ?? "");
            if (entity) void navigate({ to: "/crm/$entity/$id", params: { entity: entity.path, id: hit.id } });
          } else {
            void navigate({ to: item.id });
          }
        }}
      />
      </OfflineContext.Provider>
    </SessionContext.Provider>
  );
}

function WorkspaceSwitcher({ session }: { session: Session }) {
  const { me, workspace, switchWorkspace } = session;
  const label = (
    <span className="flex min-w-0 flex-col text-left">
      <span className="truncate text-sm font-semibold">{workspace.name}</span>
      <span className="truncate text-xs text-muted-foreground capitalize">{workspace.role}</span>
    </span>
  );
  if (me.workspaces.length < 2) {
    return <div className={cn("flex h-14 items-center border-b px-4")}>{label}</div>;
  }
  return (
    <Menu>
      <MenuTrigger asChild>
        <button type="button" className="flex h-14 w-full items-center justify-between gap-2 border-b px-4 hover:bg-accent">
          {label}
          <ChevronsUpDown className="size-4 shrink-0 text-muted-foreground" />
        </button>
      </MenuTrigger>
      <MenuContent align="start">
        <MenuLabel>Workspaces</MenuLabel>
        {me.workspaces.map((w) => (
          <MenuItem key={w.id} onSelect={() => switchWorkspace(w.id)}>
            {w.name} {w.id === workspace.id && "✓"}
          </MenuItem>
        ))}
      </MenuContent>
    </Menu>
  );
}

/** Online, offline, or changes waiting: a link to the sync page either way. */
function SyncIndicator() {
  const status = useSyncStatus();
  if (!status) return null;
  const label = !status.online
    ? status.pending
      ? `Offline · ${status.pending} waiting`
      : "Offline"
    : status.attention
      ? `${status.attention} need${status.attention === 1 ? "s" : ""} attention`
      : // Not "up to date" before the first download has finished.
        status.syncing || status.pending || !status.ready
        ? "Syncing…"
        : "Up to date";
  const warn = !status.online || status.attention > 0;
  return (
    <Link
      to="/sync"
      className={cn(
        "flex h-8 items-center gap-1.5 rounded-full border px-3 text-xs [&_svg]:size-3.5",
        warn ? "border-amber-500/50 text-amber-700 dark:text-amber-400" : "text-muted-foreground",
      )}
    >
      {status.online ? <RefreshCw className={cn(status.syncing && "animate-spin")} /> : <CloudOff />}
      <span className="hidden sm:inline">{label}</span>
      <span className="sr-only sm:hidden">{label}</span>
    </Link>
  );
}
