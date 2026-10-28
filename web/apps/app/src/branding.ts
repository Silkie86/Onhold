// The workspace's own look in the app: its brand colour on buttons and links,
// and its logo beside its name. Kept on the device so the next start shows it
// at once, before the server answers.

import { useQuery } from "@tanstack/react-query";
import { applyBranding, clearBranding, readBranding } from "@onhold/ui";
import { useEffect } from "react";
import { fetchWorkspace } from "./data.ts";
import type { OfflineSync } from "./offline/sync.ts";

const KEY = "onhold.branding.";

function stored(workspaceId: string): unknown {
  try {
    const raw = localStorage.getItem(KEY + workspaceId);
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
}

function store(workspaceId: string, branding: { logo: string | null; colour: string | null }) {
  try {
    // One workspace's look at a time: a logo is up to about 14 kB.
    for (let i = localStorage.length - 1; i >= 0; i--) {
      const k = localStorage.key(i);
      if (k?.startsWith(KEY) && k !== KEY + workspaceId) localStorage.removeItem(k);
    }
    localStorage.setItem(KEY + workspaceId, JSON.stringify(branding));
  } catch {
    // Private browsing: shown once the server answers.
  }
}

/** Applies the workspace's colour and returns its logo, if it has one. */
export function useWorkspaceBranding(workspaceId: string, sync: OfflineSync | null): string | null {
  const ws = useQuery({ queryKey: ["workspace"], queryFn: async () => (await fetchWorkspace(sync)).data, staleTime: 5 * 60_000 });
  const fromServer = ws.data && ws.data.id === workspaceId ? ws.data.branding : undefined;
  const saved = stored(workspaceId) as { logo?: unknown; colour?: unknown } | null;
  const { logo, colours } = readBranding(
    fromServer ?? (saved ? { logo: saved.logo, primary_color: saved.colour } : null),
  );
  const colour = colours.primary ?? null;
  useEffect(() => {
    clearBranding();
    if (colour) applyBranding(readBranding({ primary_color: colour }).colours);
    return () => clearBranding();
  }, [colour]);
  useEffect(() => {
    if (fromServer !== undefined) store(workspaceId, { logo, colour });
  }, [fromServer, workspaceId, logo, colour]);
  return logo;
}
