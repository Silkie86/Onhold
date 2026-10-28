// What a workspace set itself up to use (its setup form's answers, kept on
// the workspace): the modules and extras switched on and its own words. The
// menus, pages and buttons read it, so a one-person setup stays simple and an
// agency sees everything. Nothing switched off is deleted; it is only hidden.

import { useQuery } from "@tanstack/react-query";
import { currencyDigits } from "@onhold/ui";
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
  /** Whether OnHold's team left this shown: `nav:<path>`, `section:<key>`. */
  shows: (key: string) => boolean;
  /** The workspace's word for a term, e.g. `word("crm", "talent", true)` → "DJs". */
  word: (module: string, term: string, plural?: boolean) => string;
};

export function setupOf(
  ws:
    | { enabled_modules: string[]; extras?: string[]; vocabulary?: unknown; setup_at?: string | null; hidden?: string[] }
    | undefined,
): Setup {
  const words = (ws?.vocabulary ?? {}) as Words;
  return {
    known: !!ws,
    done: !!ws?.setup_at,
    // A device's copy saved before setups existed has no extras and no
    // finance module; it keeps showing everything, as the server did then.
    has: (m) => !ws || ws.enabled_modules.includes(m) || (m === "finance" && !ws.extras),
    hasExtra: (e) => !ws?.extras || ws.extras.includes(e),
    shows: (key) => !ws?.hidden?.includes(key),
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

/** Parts of pages OnHold's team can hide to streamline a client's app. */
export const SECTIONS: { key: string; label: string }[] = [
  { key: "section:booking.quotes", label: "Quotes on a booking" },
  { key: "section:booking.finance", label: "Invoices and payments on a booking" },
  { key: "section:booking.documents", label: "Contracts and documents on a booking" },
  { key: "section:booking.settlements", label: "Settlements (what talent are owed) on a booking" },
  { key: "section:tasks.assign", label: "Assigning tasks to people" },
  { key: "section:talent.members", label: "Members of an act" },
  { key: "section:talent.availability", label: "Availability on a talent" },
  { key: "section:talent.calendars", label: "Connected calendars on a talent" },
  { key: "section:records.conversations", label: "Email conversations on contacts and organisations" },
];

/** Formats an amount in minor units, e.g. 2900 GBP → "£29" (pence only when there are some). */
export function price(minor: number, currency: string): string {
  const digits = currencyDigits(currency);
  const amount = minor / 10 ** digits;
  const shown = Number.isInteger(amount) ? 0 : digits;
  try {
    return new Intl.NumberFormat(undefined, {
      style: "currency",
      currency,
      minimumFractionDigits: shown,
      maximumFractionDigits: shown,
    }).format(amount);
  } catch {
    return `${amount.toFixed(shown)} ${currency}`;
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
