// The signed-in app's frame: navigation, workspace switcher, ⌘K search and
// the user menu. Anyone not signed in is sent to sign in first.

import { Link, Navigate, Outlet, useLocation, useNavigate } from "@tanstack/react-router";
import {
  Button,
  CommandPalette,
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
  ChevronsUpDown,
  Contact,
  FileUp,
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
import { type ReactNode, useCallback, useEffect, useState } from "react";
import { api, entityByPath, ok } from "./api.ts";
import { type Session, SessionContext, useMe, useSessionActions } from "./session.tsx";

const NAV: { group: string; items: { to: string; label: string; icon: ReactNode }[] }[] = [
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
    items: [{ to: "/imports", label: "Imports", icon: <FileUp /> }],
  },
  {
    group: "Settings",
    items: [
      { to: "/settings/fields", label: "Custom fields", icon: <SlidersHorizontal /> },
      { to: "/settings/pipelines", label: "Pipelines", icon: <Workflow /> },
    ],
  },
];

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
    return <Navigate to="/sign-in" search={{ redirect: location.href }} replace />;
  }
  if (!me.data.session.mfa_verified) {
    return <Navigate to="/auth/mfa" search={{ redirect: location.href }} replace />;
  }
  if (me.data.workspaces.length === 0) {
    return (
      <div className="mx-auto max-w-md p-6 text-sm">
        You are signed in as {me.data.user.email} but are not a member of any workspace yet. Ask for an invitation.
      </div>
    );
  }
  return <SignedIn me={me.data} />;
}

function SignedIn({ me }: { me: Session["me"] }) {
  const { workspace, switchWorkspace, signOut } = useSessionActions(me);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [navOpen, setNavOpen] = useState(false);
  const [theme, setTheme] = useState<Theme>(storedTheme);
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

  const search = useCallback(async (q: string): Promise<PaletteHit[]> => {
    const hits = await ok(api.GET("/api/v1/search", { params: { query: { q, limit: 12 } } }));
    const groups: Record<string, string> = { contact: "Contacts", organisation: "Organisations", talent: "Talent" };
    return hits.map((h) => ({ id: h.id, title: h.name, detail: h.detail, group: groups[h.entity] ?? h.entity }));
  }, []);
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
                <MenuSeparator />
                <MenuItem onSelect={() => void signOut()}>
                  <LogOut className="size-4" /> Sign out
                </MenuItem>
              </MenuContent>
            </Menu>
          </header>
          <main className="min-w-0 flex-1 overflow-y-auto">
            <Outlet />
          </main>
        </div>
      </div>
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
