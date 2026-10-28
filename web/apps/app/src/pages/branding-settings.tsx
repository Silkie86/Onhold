// Branding: the workspace's name, logo and brand colour. They show in the
// app, on documents, and on the quotes, invoices and signing pages buyers open.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, Button, Input, Label, Spinner, brandColours, darkBrandColour, hexColour, readBranding } from "@onhold/ui";
import { ImageUp, Trash2 } from "lucide-react";
import { useRef, useState } from "react";
import { api, ok } from "../api.ts";
import { fetchWorkspace, useOffline } from "../data.ts";
import { useSession } from "../session.tsx";

/** OnHold's own blue, the colour shown until a workspace picks one. */
const DEFAULT_COLOUR = "#2747c7";
/** The logo travels with the branding (16 kB in all), so it is kept small. */
const MAX_LOGO_CHARS = 14_000;
/** Bigger files are refused before the browser decodes them. */
const MAX_UPLOAD_BYTES = 10_000_000;

/** The image as a small WebP (or PNG) data URI that fits the branding. */
export async function shrinkLogo(file: Blob): Promise<string> {
  if (file.size > MAX_UPLOAD_BYTES) throw new Error("That file is over 10 MB. Use a smaller version of the logo.");
  const url = URL.createObjectURL(file);
  try {
    const img = await new Promise<HTMLImageElement>((resolve, reject) => {
      const i = new Image();
      i.onload = () => resolve(i);
      i.onerror = () => reject(new Error("That file is not an image this browser can open."));
      i.src = url;
    });
    const w = img.naturalWidth || 400;
    const h = img.naturalHeight || 160;
    for (const box of [[480, 160], [360, 120], [240, 80], [160, 54]] as const) {
      const scale = Math.min(1, box[0] / w, box[1] / h);
      const canvas = document.createElement("canvas");
      canvas.width = Math.max(1, Math.round(w * scale));
      canvas.height = Math.max(1, Math.round(h * scale));
      const g = canvas.getContext("2d");
      if (!g) break;
      g.drawImage(img, 0, 0, canvas.width, canvas.height);
      for (const quality of [0.9, 0.7]) {
        const data = canvas.toDataURL("image/webp", quality);
        if (data.length <= MAX_LOGO_CHARS) return data;
      }
    }
    throw new Error("That logo is too detailed to keep small. Try a simpler or smaller version.");
  } finally {
    URL.revokeObjectURL(url);
  }
}

export function BrandingSettingsPage() {
  const { workspace } = useSession();
  const admin = workspace.role === "owner" || workspace.role === "admin";
  const sync = useOffline();
  const qc = useQueryClient();
  const ws = useQuery({ queryKey: ["workspace"], queryFn: async () => (await fetchWorkspace(sync)).data });
  const [name, setName] = useState<string | null>(null);
  const [logo, setLogo] = useState<string | null | undefined>(undefined);
  const [colour, setColour] = useState<string | null>(null);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  if (ws.isPending) return <Spinner />;
  if (ws.isError) return <Alert>{ws.error.message}</Alert>;

  const saved = readBranding(ws.data.branding);
  const shownName = name ?? ws.data.name;
  const shownLogo = logo === undefined ? saved.logo : logo;
  // "" is no colour of their own: OnHold's blue, in its light and dark shades.
  const shownColour = colour ?? saved.colour ?? "";
  const validColour = shownColour === "" ? "" : hexColour(shownColour);
  // In dark mode the app uses a lighter shade of the colour; so does the preview.
  const root = document.documentElement.classList;
  const dark = root.contains("dark") || (!root.contains("light") && window.matchMedia?.("(prefers-color-scheme: dark)").matches);
  const previewColour = validColour || DEFAULT_COLOUR;
  const preview = brandColours((dark && darkBrandColour(previewColour)) || previewColour);
  const changed = name !== null || logo !== undefined || colour !== null;

  const save = async () => {
    setMessage(null);
    setBusy(true);
    try {
      const branding: Record<string, unknown> = { ...(ws.data.branding as Record<string, unknown>) };
      if (colour !== null) branding.primary_color = validColour || null;
      if (logo !== undefined) {
        branding.logo = logo;
        // A logo chosen here replaces one uploaded another way.
        branding.logo_file_id = null;
      }
      const updated = await ok(
        api.PATCH("/api/v1/workspace", {
          headers: { "If-Match": `"${ws.data.version}"` },
          body: { ...(name !== null ? { name: name.trim() } : {}), branding: branding as never },
        }),
      );
      qc.setQueryData(["workspace"], updated);
      if (name !== null) void qc.invalidateQueries({ queryKey: ["me"] });
      setName(null);
      setLogo(undefined);
      setColour(null);
      setMessage({ ok: true, text: "Saved. Everyone in the workspace sees it the next time a page loads." });
    } catch (err) {
      setMessage({ ok: false, text: err instanceof Error ? err.message : String(err) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-6 p-4 sm:p-6">
      <div className="flex flex-col gap-1">
        <h1 className="text-xl font-semibold">Branding</h1>
        <p className="text-sm text-muted-foreground">
          Your name, logo and colour appear in the app, on contracts and invoices, and on the pages your clients open to
          accept quotes, pay and sign.
        </p>
      </div>
      {!admin && <Alert tone="info">Only owners and admins can change the branding.</Alert>}

      <fieldset disabled={!admin || busy} className="flex flex-col gap-6">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="brand-name">Business name</Label>
          <Input id="brand-name" maxLength={120} value={shownName} onChange={(e) => setName(e.target.value)} />
        </div>

        <div className="flex flex-col gap-2">
          <Label htmlFor="brand-logo">Logo</Label>
          <div className="flex flex-wrap items-center gap-3">
            <div className="flex h-16 min-w-32 items-center justify-center rounded-md border bg-background px-3">
              {shownLogo ? (
                <img src={shownLogo} alt="Your logo" className="h-12 w-auto max-w-56 object-contain brand-logo" />
              ) : (
                <span className="text-sm text-muted-foreground">No logo</span>
              )}
            </div>
            <Button type="button" variant="outline" onClick={() => input.current?.click()}>
              <ImageUp /> {shownLogo ? "Change logo" : "Add logo"}
            </Button>
            {shownLogo && (
              <Button type="button" variant="ghost" onClick={() => setLogo(null)}>
                <Trash2 /> Remove
              </Button>
            )}
            <input
              ref={input}
              id="brand-logo"
              type="file"
              tabIndex={-1}
              accept="image/png,image/jpeg,image/webp,image/gif,image/svg+xml"
              className="sr-only"
              onChange={async (e) => {
                const file = e.target.files?.[0];
                e.target.value = "";
                if (!file) return;
                setMessage(null);
                try {
                  setLogo(await shrinkLogo(file));
                } catch (err) {
                  setMessage({ ok: false, text: err instanceof Error ? err.message : String(err) });
                }
              }}
            />
          </div>
          <p className="text-xs text-muted-foreground">
            PNG, JPEG, WebP or SVG. A wide logo on a clear background works best. It is resized to keep the app light.
          </p>
        </div>

        <div className="flex flex-col gap-1.5">
          <Label htmlFor="brand-colour">Brand colour</Label>
          <div className="flex flex-wrap items-center gap-2">
            <input
              type="color"
              aria-label="Pick a brand colour"
              className="h-9 w-12 cursor-pointer rounded-md border bg-background p-1"
              value={validColour && validColour.length === 7 ? validColour : DEFAULT_COLOUR}
              tabIndex={-1}
              onChange={(e) => setColour(e.target.value)}
            />
            <Input
              id="brand-colour"
              className="w-32 font-mono"
              placeholder="OnHold blue"
              value={shownColour}
              aria-invalid={validColour === null}
              aria-describedby={validColour === null ? "brand-colour-error" : undefined}
              onChange={(e) => setColour(e.target.value.trim())}
            />
            {shownColour !== "" && (
              <Button type="button" variant="ghost" size="sm" onClick={() => setColour("")}>
                Use OnHold blue
              </Button>
            )}
          </div>
          {validColour === null && (
            <p id="brand-colour-error" className="text-sm text-destructive">
              Use a hex colour such as #2747c7.
            </p>
          )}
          <p className="text-xs text-muted-foreground">Used for buttons, links and highlights. Text on it turns white or black, whichever reads better.</p>
        </div>
      </fieldset>

      <section aria-label="Preview" className="flex flex-col gap-2">
        <h2 className="text-sm font-semibold">Preview</h2>
        <div className="flex flex-col gap-4 rounded-lg border bg-card p-4">
          <div className="flex items-center gap-3">
            {shownLogo && <img src={shownLogo} alt="" className="h-10 w-auto max-w-40 object-contain brand-logo" />}
            <span className="font-semibold">{shownName || "Your business"}</span>
          </div>
          <div className="flex flex-wrap items-center gap-3">
            <span
              className="inline-flex h-9 items-center rounded-md px-4 text-sm font-medium"
              style={{ background: preview.primary, color: preview["primary-foreground"] }}
            >
              Accept quote
            </span>
            <span className="text-sm font-medium underline underline-offset-2" style={{ color: preview.primary }}>
              View invoice
            </span>
          </div>
        </div>
      </section>

      {message && <Alert tone={message.ok ? "success" : "error"}>{message.text}</Alert>}
      {admin && (
        <div className="flex justify-end gap-2">
          {changed && (
            <Button
              type="button"
              variant="ghost"
              disabled={busy}
              onClick={() => {
                setName(null);
                setLogo(undefined);
                setColour(null);
                setMessage(null);
              }}
            >
              Undo changes
            </Button>
          )}
          <Button type="button" disabled={!changed || validColour === null || !shownName.trim() || busy} onClick={() => void save()}>
            Save
          </Button>
        </div>
      )}
    </div>
  );
}
