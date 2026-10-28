// The workspace's own look in the app: its brand colour (and any full theme
// OnHold's team set) on buttons, links and surfaces, and its logo beside its
// name. The colours (well under a kilobyte) are kept on the device so the next
// start shows them at once, before the server answers; the logo comes with
// the workspace itself.

import { useQuery } from "@tanstack/react-query";
import { type BrandTheme, applyBrandTheme, clearBranding, hexColour, readBranding, readTheme } from "@onhold/ui";
import { useLayoutEffect, useMemo } from "react";
import { fetchWorkspace } from "./data.ts";
import type { OfflineSync } from "./offline/sync.ts";

const KEY = "onhold.brand-look.";
/** What earlier versions kept: the colour alone. */
const OLD_KEY = "onhold.brand-colour.";

type Look = { colour: string | null; theme: BrandTheme | null };

function storedLook(workspaceId: string): Look | null {
  try {
    const raw = JSON.parse(localStorage.getItem(KEY + workspaceId) ?? "null") as { colour?: unknown; theme?: unknown } | null;
    return raw ? { colour: hexColour(raw.colour), theme: readTheme(raw.theme) } : null;
  } catch {
    return null;
  }
}

/** Forgets every workspace's look on this device, as signing out does. */
export function forgetBranding() {
  try {
    for (let i = localStorage.length - 1; i >= 0; i--) {
      const k = localStorage.key(i);
      if (k?.startsWith(KEY) || k?.startsWith(OLD_KEY)) localStorage.removeItem(k);
    }
  } catch {
    // Nothing kept in private browsing.
  }
}

function storeLook(workspaceId: string, look: Look) {
  try {
    forgetBranding();
    if (look.colour || look.theme) localStorage.setItem(KEY + workspaceId, JSON.stringify(look));
  } catch {
    // Private browsing: shown once the server answers.
  }
}

/**
 * Applies the workspace's colours and returns its logo, if it has one. On a
 * visit by OnHold's team (`keep` false) nothing is kept on the device.
 */
export function useWorkspaceBranding(workspaceId: string, sync: OfflineSync | null, keep = true): string | null {
  const ws = useQuery({ queryKey: ["workspace"], queryFn: async () => (await fetchWorkspace(sync)).data, staleTime: 5 * 60_000 });
  const fromServer = ws.data && ws.data.id === workspaceId ? readBranding(ws.data.branding) : undefined;
  const saved = useMemo(() => (keep ? storedLook(workspaceId) : null), [workspaceId, keep]);
  const look: Look = fromServer ? { colour: fromServer.colour, theme: fromServer.theme } : (saved ?? { colour: null, theme: null });
  // One string to compare, so the effects run only when the look changes.
  const key = JSON.stringify(look);
  // Before paint, so a start does not flash OnHold's own look first.
  useLayoutEffect(() => {
    applyBrandTheme(JSON.parse(key) as Look);
    return () => clearBranding();
  }, [key]);
  const known = fromServer !== undefined;
  useLayoutEffect(() => {
    if (known && keep) storeLook(workspaceId, JSON.parse(key) as Look);
  }, [known, keep, workspaceId, key]);
  return fromServer?.logo ?? null;
}
