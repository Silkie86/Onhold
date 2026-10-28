// What a workspace set itself up to use (its setup form's answers, kept on
// the workspace): the modules and extras switched on and its own words. The
// menus, pages and buttons read it, so a one-person setup stays simple and an
// agency sees everything. Nothing switched off is deleted; it is only hidden.

import { useQuery } from "@tanstack/react-query";
import { fetchWorkspace, useOffline } from "./data.ts";
import type { OfflineSync } from "./offline/sync.ts";

/** The words the app can rename, with OnHold's own. */
const DEFAULT_WORDS: Record<string, Record<string, { one: string; many: string }>> = {
  crm: { talent: { one: "Talent", many: "Talent" } },
  bookings: { enquiry: { one: "Enquiry", many: "Enquiries" }, booking: { one: "Booking", many: "Bookings" } },
};

type Words = Record<string, Record<string, { one?: unknown; many?: unknown }>>;

export type Setup = {
  /** Whether the workspace data has arrived; until then everything shows. */
  known: boolean;
  /** Whether the setup form has been sent. */
  done: boolean;
  has: (module: string) => boolean;
  hasExtra: (extra: string) => boolean;
  /** The workspace's word for a term, e.g. `word("crm", "talent", true)` → "DJs". */
  word: (module: string, term: string, plural?: boolean) => string;
};

export function setupOf(
  ws: { enabled_modules: string[]; extras: string[]; vocabulary: unknown; setup_at?: string | null } | undefined,
): Setup {
  const words = (ws?.vocabulary ?? {}) as Words;
  return {
    known: !!ws,
    done: !!ws?.setup_at,
    has: (m) => !ws || ws.enabled_modules.includes(m),
    hasExtra: (e) => !ws || ws.extras.includes(e),
    word: (module, term, plural = false) => {
      const own = words[module]?.[term]?.[plural ? "many" : "one"];
      if (typeof own === "string" && own) return own;
      const fallback = DEFAULT_WORDS[module]?.[term];
      return fallback ? fallback[plural ? "many" : "one"] : term;
    },
  };
}

/**
 * The current workspace's setup. Works offline from the device's copy; the
 * app's frame passes its sync engine, pages take it from context.
 */
export function useSetup(engine?: OfflineSync | null): Setup {
  const fromContext = useOffline();
  const sync = engine === undefined ? fromContext : engine;
  const ws = useQuery({ queryKey: ["workspace"], queryFn: async () => (await fetchWorkspace(sync)).data, staleTime: 5 * 60_000 });
  return setupOf(ws.data);
}

/** Formats an amount in minor units, e.g. 2900 GBP → "£29". */
export function price(minor: number, currency: string): string {
  const whole = minor % 100 === 0;
  try {
    return new Intl.NumberFormat(undefined, {
      style: "currency",
      currency,
      minimumFractionDigits: whole ? 0 : 2,
      maximumFractionDigits: whole ? 0 : 2,
    }).format(minor / 100);
  } catch {
    return `${(minor / 100).toFixed(whole ? 0 : 2)} ${currency}`;
  }
}

export type PriceList = {
  currency: string;
  placeholder?: boolean;
  base: number;
  features: Record<string, number>;
  team: Record<string, number>;
};

/** A monthly estimate for a set of answers. */
export function estimate(list: PriceList, answers: { features: string[]; team?: string | null }): number {
  const features = answers.features.reduce((sum, f) => sum + (list.features[f] ?? 0), 0);
  return list.base + features + (answers.team ? (list.team[answers.team] ?? 0) : 0);
}
