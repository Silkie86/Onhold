// Lists and search over the copy of the CRM kept on the device, for when the
// app is offline. They follow the API's rules closely enough to be
// unsurprising: every search word must appear somewhere in the record,
// names that start with the search come first, and empty values sort last.

import type { RecordJson } from "@onhold/ui";

export interface ListParams {
  q?: string;
  sort?: string | null;
  tag?: string | null;
  organisation_id?: string;
  offset?: number;
  limit?: number;
}

export interface Page {
  items: RecordJson[];
  total: number;
}

function words(q: string): string[] {
  return q.toLowerCase().split(/\s+/).filter(Boolean);
}

/** Everything a search can match in a record, lower-cased. */
export function searchText(r: RecordJson): string {
  const parts: unknown[] = [
    r.name,
    r.legal_name,
    r.talent_type,
    r.base_location,
    r.org_type,
    r.tax_id,
    r.role,
    r.organisation_name,
    ...(Array.isArray(r.emails) ? r.emails : []),
    ...(Array.isArray(r.phones) ? r.phones : []),
    ...(Array.isArray(r.tags) ? r.tags : []),
  ];
  const custom = (r.custom ?? {}) as Record<string, unknown>;
  for (const v of Object.values(custom)) {
    if (typeof v === "string" || typeof v === "number") parts.push(v);
    else if (Array.isArray(v)) parts.push(...v.filter((x) => typeof x === "string"));
  }
  // Phone numbers also match without their spaces and punctuation.
  for (const p of Array.isArray(r.phones) ? r.phones : []) {
    if (typeof p === "string") parts.push(p.replace(/\D/g, ""));
  }
  return parts
    .filter((p) => p !== null && p !== undefined && p !== "")
    .join(" ")
    .toLowerCase();
}

export function matches(r: RecordJson, q: string): boolean {
  const text = searchText(r);
  return words(q).every((w) => text.includes(w));
}

/** The value a sort key reads from a record. */
function sortValue(r: RecordJson, key: string): unknown {
  if (key.startsWith("custom.")) return ((r.custom ?? {}) as Record<string, unknown>)[key.slice(7)];
  if (key === "email") return r.email ?? (Array.isArray(r.emails) ? r.emails[0] : undefined);
  if (key === "phone") return Array.isArray(r.phones) ? r.phones[0] : undefined;
  if (key === "organisation") return r.organisation_name;
  return r[key];
}

function comparable(v: unknown): string | number | null {
  if (v === null || v === undefined || v === "") return null;
  if (typeof v === "number") return v;
  if (typeof v === "boolean") return v ? 1 : 0;
  if (typeof v === "string") return v.toLowerCase();
  if (Array.isArray(v)) return v.length ? comparable(v[0]) : null;
  if (typeof v === "object" && "amount_minor" in v) return Number((v as { amount_minor: unknown }).amount_minor);
  return JSON.stringify(v);
}

function compareValues(a: unknown, b: unknown, desc: boolean): number {
  const x = comparable(a);
  const y = comparable(b);
  if (x === null && y === null) return 0;
  // Empty values last, whichever the direction.
  if (x === null) return 1;
  if (y === null) return -1;
  const order = typeof x === "number" && typeof y === "number" ? x - y : String(x).localeCompare(String(y));
  return desc ? -order : order;
}

const byName = (a: RecordJson, b: RecordJson) =>
  compareValues(a.name, b.name, false) || String(a.id).localeCompare(String(b.id));

export function sortRecords(records: RecordJson[], sort: string | null | undefined, q?: string): RecordJson[] {
  const list = [...records];
  if (!sort || sort === "relevance") {
    const first = q ? words(q)[0] : undefined;
    if (first) {
      const starts = (r: RecordJson) => (String(r.name ?? "").toLowerCase().startsWith(first) ? 0 : 1);
      return list.sort((a, b) => starts(a) - starts(b) || byName(a, b));
    }
    return list.sort(byName);
  }
  const desc = sort.startsWith("-");
  const key = desc ? sort.slice(1) : sort;
  return list.sort((a, b) => compareValues(sortValue(a, key), sortValue(b, key), desc) || byName(a, b));
}

export function localList(records: RecordJson[], params: ListParams): Page {
  let found = records;
  if (params.q?.trim()) found = found.filter((r) => matches(r, params.q ?? ""));
  if (params.tag) {
    const tag = params.tag.toLowerCase();
    found = found.filter((r) => Array.isArray(r.tags) && r.tags.includes(tag));
  }
  if (params.organisation_id) found = found.filter((r) => r.organisation_id === params.organisation_id);
  const sorted = sortRecords(found, params.sort, params.q);
  const offset = params.offset ?? 0;
  const limit = Math.min(Math.max(params.limit ?? 50, 1), 500);
  return { items: sorted.slice(offset, offset + limit), total: sorted.length };
}

/** Tag counts like `GET /tags`, most used first. */
export function localTags(records: RecordJson[]): { tag: string; count: number }[] {
  const counts = new Map<string, number>();
  for (const r of records) {
    for (const t of Array.isArray(r.tags) ? r.tags : []) {
      if (typeof t === "string") counts.set(t, (counts.get(t) ?? 0) + 1);
    }
  }
  return [...counts.entries()]
    .map(([tag, count]) => ({ tag, count }))
    .sort((a, b) => b.count - a.count || a.tag.localeCompare(b.tag));
}
