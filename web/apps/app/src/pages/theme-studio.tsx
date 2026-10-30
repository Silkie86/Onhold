// Theme studio: OnHold's team sets a client's full look (every colour in
// light and dark mode, the typeface and the corners) at onboarding or later.
// Clients themselves set only their name, logo and brand colour (Branding).
// The whole app previews the draft as it is edited.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Alert,
  Badge,
  type BrandTheme,
  Button,
  Card,
  Input,
  Label,
  NativeSelect,
  Spinner,
  THEME_FONTS,
  THEME_TOKENS,
  type ThemeFont,
  type ThemeToken,
  applyBrandTheme,
  applyTheme,
  cn,
  contrastRatio,
  hexColour,
  readBranding,
  readTheme,
} from "@onhold/ui";
import { Moon, RotateCcw, Sun, X } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { api, ok } from "../api.ts";
import { fetchWorkspace, useOffline } from "../data.ts";
import { useSession } from "../session.tsx";
import { storedTheme } from "../shell.tsx";

type Mode = "light" | "dark";

const LABELS: Record<ThemeToken, string> = {
  background: "Page background",
  foreground: "Text",
  card: "Cards and panels",
  muted: "Quiet surfaces",
  "muted-foreground": "Secondary text",
  border: "Lines",
  input: "Field borders",
  ring: "Focus ring",
  primary: "Buttons and links",
  "primary-foreground": "Text on buttons",
  secondary: "Secondary buttons",
  "secondary-foreground": "Text on secondary buttons",
  accent: "Highlights (hover, selected)",
  "accent-foreground": "Text on highlights",
  destructive: "Danger",
  "destructive-foreground": "Text on danger",
  success: "Done (paid, confirmed)",
  "success-foreground": "Text on done",
  warning: "Needs attention (hold, overdue soon)",
  "warning-foreground": "Text on needs attention",
  info: "In progress (sent, pending)",
  "info-foreground": "Text on in progress",
};

const FONT_LABELS: Record<ThemeFont, string> = {
  default: "OnHold default (Inter)",
  system: "The device's own",
  humanist: "Humanist sans",
  geometric: "Geometric sans",
  serif: "Serif",
  rounded: "Rounded",
  mono: "Monospace",
};

/** Pairs that must read, with the contrast each needs (WCAG AA). */
const CHECKS: { label: string; fg: ThemeToken; bg: ThemeToken; min: number }[] = [
  { label: "Text on the page", fg: "foreground", bg: "background", min: 4.5 },
  { label: "Text on cards", fg: "foreground", bg: "card", min: 4.5 },
  { label: "Secondary text", fg: "muted-foreground", bg: "background", min: 4.5 },
  { label: "Links on the page", fg: "primary", bg: "background", min: 4.5 },
  { label: "Text on buttons", fg: "primary-foreground", bg: "primary", min: 4.5 },
  { label: "Text on secondary buttons", fg: "secondary-foreground", bg: "secondary", min: 4.5 },
  { label: "Text on highlights", fg: "accent-foreground", bg: "accent", min: 4.5 },
  { label: "Text on danger", fg: "destructive-foreground", bg: "destructive", min: 4.5 },
  { label: "Text on done badges", fg: "success-foreground", bg: "success", min: 4.5 },
  { label: "Needs-attention text", fg: "warning", bg: "background", min: 4.5 },
  { label: "Text on needs-attention banners", fg: "warning-foreground", bg: "warning", min: 4.5 },
  { label: "In-progress text", fg: "info", bg: "background", min: 4.5 },
  { label: "Focus ring on the page", fg: "ring", bg: "background", min: 3 },
];

/** A CSS colour as #rrggbb, as the browser draws it (tokens may be oklch). */
function drawnHex(css: string): string | null {
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = 1;
  const g = canvas.getContext("2d", { willReadFrequently: true });
  if (!g) return null;
  g.fillStyle = "#000";
  g.fillStyle = css;
  g.fillRect(0, 0, 1, 1);
  const [r, gr, b] = g.getImageData(0, 0, 1, 1).data;
  return `#${[r, gr, b].map((v) => (v ?? 0).toString(16).padStart(2, "0")).join("")}`;
}

/** Every token's colour as the page now shows it. */
function readTokens(): Partial<Record<ThemeToken, string>> {
  const probe = document.createElement("span");
  document.body.appendChild(probe);
  const out: Partial<Record<ThemeToken, string>> = {};
  for (const t of THEME_TOKENS) {
    probe.style.color = `var(--${t})`;
    const hex = drawnHex(getComputedStyle(probe).color);
    if (hex) out[t] = hex;
  }
  probe.remove();
  return out;
}

function cleanTheme(t: BrandTheme): BrandTheme | null {
  const out: BrandTheme = {};
  if (t.light && Object.keys(t.light).length) out.light = t.light;
  if (t.dark && Object.keys(t.dark).length) out.dark = t.dark;
  if (t.font && t.font !== "default") out.font = t.font;
  if (t.radius !== undefined) out.radius = t.radius;
  return Object.keys(out).length ? out : null;
}

export function ThemeStudioPage() {
  // A fresh studio for each workspace, so one's draft never saves onto another.
  return <ThemeStudio key={useSession().workspace.id} />;
}

function ThemeStudio() {
  const { me, workspace } = useSession();
  const sync = useOffline();
  const qc = useQueryClient();
  const ws = useQuery({ queryKey: ["workspace"], queryFn: async () => (await fetchWorkspace(sync)).data });
  const saved = ws.data ? readBranding(ws.data.branding) : null;
  const [draft, setDraft] = useState<BrandTheme | null>(null);
  const [mode, setMode] = useState<Mode>(() => (document.documentElement.classList.contains("dark") ? "dark" : "light"));
  const [shown, setShown] = useState<Partial<Record<ThemeToken, string>>>({});
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const theme: BrandTheme = draft ?? saved?.theme ?? {};
  // Only valid colours reach the page while editing.
  const look = JSON.stringify({ colour: saved?.colour ?? null, theme: readTheme(cleanTheme(theme)) });
  const savedLook = JSON.stringify({ colour: saved?.colour ?? null, theme: saved?.theme ?? null });

  // The whole app previews the draft, in the mode being edited; leaving puts
  // back the saved look and the person's own light or dark choice.
  useLayoutEffect(() => {
    if (!saved) return;
    applyBrandTheme(JSON.parse(look));
  }, [look, saved]);
  const savedRef = useRef(savedLook);
  savedRef.current = savedLook;
  useEffect(() => () => applyBrandTheme(JSON.parse(savedRef.current)), []);
  useLayoutEffect(() => applyTheme(mode), [mode]);
  useEffect(() => {
    // Following the header's light or dark switch while here, and leaving
    // with the person's own choice as it then stands.
    const root = document.documentElement;
    const watch = new MutationObserver(() => {
      if (root.classList.contains("dark")) setMode("dark");
      else if (root.classList.contains("light")) setMode("light");
    });
    watch.observe(root, { attributes: true, attributeFilter: ["class"] });
    return () => {
      watch.disconnect();
      applyTheme(storedTheme());
    };
  }, []);
  useLayoutEffect(() => setShown(readTokens()), [look, mode]);

  if (!me.user.is_superadmin) {
    return (
      <div className="mx-auto max-w-3xl p-4 sm:p-6">
        <h1 className="text-xl font-semibold">Theme studio</h1>
        <p className="mt-2 text-sm text-muted-foreground">Only OnHold&apos;s team sets the full theme. Your logo, name and colour are under Settings → Branding.</p>
      </div>
    );
  }
  if (ws.isError) return <Alert>{ws.error.message}</Alert>;
  if (ws.isPending || !saved) return <Spinner />;

  const tokens = theme[mode] ?? {};
  const setToken = (t: ThemeToken, value: string | null) =>
    setDraft((d) => {
      const base = d ?? saved.theme ?? {};
      const next = { ...(base[mode] ?? {}) };
      if (value === null) delete next[t];
      else next[t] = value;
      return { ...base, [mode]: next };
    });
  const changed = JSON.stringify(cleanTheme(theme)) !== JSON.stringify(saved.theme ? cleanTheme(saved.theme) : null);
  const badIn = (m: Mode) => Object.values(theme[m] ?? {}).some((v) => !hexColour(v));
  const invalid = badIn("light") || badIn("dark");

  const save = async () => {
    setBusy(true);
    setMessage(null);
    try {
      const branding = { ...(ws.data.branding as Record<string, unknown>), theme: cleanTheme(theme) };
      const updated = await ok(
        api.PATCH("/api/v1/workspace", { headers: { "If-Match": `"${ws.data.version}"` }, body: { branding: branding as never } }),
      );
      qc.setQueryData(["workspace"], updated);
      setDraft(null);
      setMessage({ ok: true, text: `Saved. Everyone in ${workspace.name} sees it the next time a page loads.` });
    } catch (err) {
      setMessage({ ok: false, text: err instanceof Error ? err.message : String(err) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-6 p-4 sm:p-6">
      <div className="flex flex-col gap-1">
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-xl font-semibold">Theme studio</h1>
          <Badge className="border">OnHold team</Badge>
        </div>
        <p className="text-sm text-muted-foreground">
          The full look of <strong>{workspace.name}</strong>. Anything left empty follows their brand colour
          {saved.colour ? ` (${saved.colour})` : ""}, then OnHold&apos;s own. The whole app shows your changes as you make them; the
          client sees them once you save.
        </p>
      </div>

      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_20rem]">
        <div className="flex min-w-0 flex-col gap-6">
          <div className="grid gap-4 sm:grid-cols-2">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="theme-font">Typeface</Label>
              <NativeSelect
                id="theme-font"
                value={theme.font ?? "default"}
                onChange={(e) => setDraft({ ...theme, font: e.target.value as ThemeFont })}
              >
                {(Object.keys(THEME_FONTS) as ThemeFont[]).map((f) => (
                  <option key={f} value={f}>
                    {FONT_LABELS[f]}
                  </option>
                ))}
              </NativeSelect>
              <p className="text-xs text-muted-foreground">Fonts already on people&apos;s devices, so nothing extra is downloaded.</p>
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="theme-radius">Corners: {theme.radius ?? 8}px</Label>
              <input
                id="theme-radius"
                type="range"
                min={0}
                max={24}
                value={theme.radius ?? 8}
                onChange={(e) => setDraft({ ...theme, radius: Number(e.target.value) })}
                className="accent-primary"
              />
              <p className="text-xs text-muted-foreground">0 is square; 8 is OnHold&apos;s default.</p>
            </div>
          </div>

          <section aria-label="Colours" className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="flex-1 font-semibold">Colours</h2>
              <div role="tablist" aria-label="Mode" className="flex rounded-md border p-0.5">
                {(["light", "dark"] as Mode[]).map((m) => (
                  <button
                    key={m}
                    type="button"
                    role="tab"
                    aria-selected={mode === m}
                    className={cn("flex items-center gap-1.5 rounded px-3 py-1 text-sm capitalize", mode === m && "bg-accent font-medium")}
                    onClick={() => setMode(m)}
                  >
                    {m === "light" ? <Sun className="size-4" /> : <Moon className="size-4" />} {m}
                  </button>
                ))}
              </div>
            </div>
            <div className="divide-y rounded-md border">
              {THEME_TOKENS.map((t) => {
                const set = tokens[t];
                const value = set ?? shown[t] ?? "#000000";
                return (
                  <div key={t} className="flex flex-wrap items-center gap-2 px-3 py-2">
                    <input
                      type="color"
                      aria-label={`${LABELS[t]}, ${mode} mode`}
                      className="h-8 w-10 shrink-0 cursor-pointer rounded border bg-background p-0.5"
                      value={hexColour(value)?.length === 7 ? (hexColour(value) as string) : "#000000"}
                      onChange={(e) => setToken(t, e.target.value)}
                    />
                    <span className="min-w-0 flex-1 text-sm">
                      {LABELS[t]}
                      {!set && <span className="ml-2 text-xs text-muted-foreground">follows the default</span>}
                    </span>
                    <Input
                      aria-label={`${LABELS[t]} hex, ${mode} mode`}
                      className="h-8 w-28 font-mono text-xs"
                      placeholder={shown[t] ?? ""}
                      value={set ?? ""}
                      aria-invalid={set !== undefined && !hexColour(set)}
                      onChange={(e) => setToken(t, e.target.value.trim() || null)}
                    />
                    {set && (
                      <Button type="button" variant="ghost" size="icon" aria-label={`Clear ${LABELS[t]}`} onClick={() => setToken(t, null)}>
                        <X />
                      </Button>
                    )}
                  </div>
                );
              })}
            </div>
            {invalid && (
              <p className="text-sm text-destructive">
                Use hex colours such as #2747c7 ({[badIn("light") && "light", badIn("dark") && "dark"].filter(Boolean).join(" and ")} mode).
              </p>
            )}
          </section>
        </div>

        <aside className="flex flex-col gap-4 lg:sticky lg:top-4 lg:self-start">
          <Card className="flex flex-col gap-3 p-4">
            <h2 className="text-sm font-semibold">Preview</h2>
            <p className="text-sm">Summer ball · The Midnight Owls</p>
            <p className="text-sm text-muted-foreground">Sat 12 June, 7pm to 11pm, Grand Hotel</p>
            <div className="flex flex-wrap gap-2">
              <Button size="sm">Confirm</Button>
              <Button size="sm" variant="secondary">
                Hold
              </Button>
              <Button size="sm" variant="outline">
                Quote
              </Button>
              <Button size="sm" variant="destructive">
                Cancel
              </Button>
            </div>
            <div className="flex flex-wrap gap-2">
              <Badge className="border-success bg-success text-success-foreground">Confirmed</Badge>
              <Badge className="border border-dashed border-warning bg-warning/10 text-warning">Hold</Badge>
              <Badge className="border border-info text-info">Sent</Badge>
            </div>
            <Input aria-label="Sample field" placeholder="A field" />
            <a href="#preview" className="text-sm text-primary underline underline-offset-2" onClick={(e) => e.preventDefault()}>
              A link
            </a>
          </Card>
          <Card className="flex flex-col gap-2 p-4">
            <h2 className="text-sm font-semibold">Readability, {mode} mode</h2>
            <ul className="flex flex-col gap-1">
              {CHECKS.map((c) => {
                const ratio = contrastRatio(shown[c.fg] ?? "#000000", shown[c.bg] ?? "#ffffff");
                const pass = ratio >= c.min;
                return (
                  <li key={c.label} className="flex items-center gap-2 text-xs">
                    <span className={cn("rounded px-1.5 py-0.5 font-medium tabular-nums", pass ? "bg-success/15 text-success" : "bg-destructive/15 text-destructive")}>
                      {ratio.toFixed(1)}
                    </span>
                    <span className="flex-1">{c.label}</span>
                    {!pass && <span className="text-destructive">needs {c.min}</span>}
                  </li>
                );
              })}
            </ul>
          </Card>
        </aside>
      </div>

      {message && <Alert tone={message.ok ? "success" : "error"}>{message.text}</Alert>}
      <div className="flex flex-wrap justify-end gap-2">
        <Button type="button" variant="ghost" disabled={busy || (!saved.theme && !draft)} onClick={() => setDraft({})}>
          <RotateCcw /> Back to their brand colour only
        </Button>
        {changed && (
          <Button type="button" variant="ghost" disabled={busy} onClick={() => setDraft(null)}>
            Undo changes
          </Button>
        )}
        <Button type="button" disabled={!changed || invalid || busy} onClick={() => void save()}>
          Save theme
        </Button>
      </div>
    </div>
  );
}
