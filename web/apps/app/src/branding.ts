// The workspace's own look in the app: its brand colour on buttons and links,
// and its logo beside its name. The colour (a few bytes) is kept on the device
// so the next start shows it at once, before the server answers; the logo
// comes with the workspace itself.

import { useQuery } from "@tanstack/react-query";
import { applyBrandColour, clearBranding, hexColour, readBranding } from "@onhold/ui";
import { useLayoutEffect, useMemo } from "react";
import { fetchWorkspace } from "./data.ts";
import type { OfflineSync } from "./offline/sync.ts";

const KEY = "onhold.brand-colour.";

function storedColour(workspaceId: string): string | null {
  try {
    return hexColour(localStorage.getItem(KEY + workspaceId));
  } catch {
    return null;
  }
}

/** Forgets every workspace's colour on this device, as signing out does. */
export function forgetBranding() {
  try {
    for (let i = localStorage.length - 1; i >= 0; i--) {
      const k = localStorage.key(i);
      if (k?.startsWith(KEY)) localStorage.removeItem(k);
    }
  } catch {
    // Nothing kept in private browsing.
  }
}

function storeColour(workspaceId: string, colour: string | null) {
  try {
    forgetBranding();
    if (colour) localStorage.setItem(KEY + workspaceId, colour);
  } catch {
    // Private browsing: shown once the server answers.
  }
}

/** Applies the workspace's colour and returns its logo, if it has one. */
export function useWorkspaceBranding(workspaceId: string, sync: OfflineSync | null): string | null {
  const ws = useQuery({ queryKey: ["workspace"], queryFn: async () => (await fetchWorkspace(sync)).data, staleTime: 5 * 60_000 });
  const fromServer = ws.data && ws.data.id === workspaceId ? readBranding(ws.data.branding) : undefined;
  const saved = useMemo(() => storedColour(workspaceId), [workspaceId]);
  const colour = fromServer ? fromServer.colour : saved;
  // Before paint, so a start does not flash OnHold's own blue first.
  useLayoutEffect(() => {
    applyBrandColour(colour);
    return () => clearBranding();
  }, [colour]);
  const known = fromServer !== undefined;
  useLayoutEffect(() => {
    if (known) storeColour(workspaceId, colour);
  }, [known, workspaceId, colour]);
  return fromServer?.logo ?? null;
}
