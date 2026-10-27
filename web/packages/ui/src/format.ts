// Showing field values as text, the same way in tables, forms and timelines.

import type { Schemas } from "@onhold/api-client";

export type EntitySchema = Schemas["EntitySchema"];
export type SchemaField = Schemas["SchemaField"];
export type Money = Schemas["Money"];

/** A record as the API returns it: core fields at the top, custom ones under `custom`. */
export type RecordJson = { id: string; custom?: Record<string, unknown> } & Record<string, unknown>;

/** A field's value on a record: `custom.genre` reads `record.custom.genre`. */
export function fieldValue(record: RecordJson, key: string): unknown {
  if (key.startsWith("custom.")) return record.custom?.[key.slice(7)];
  return record[key];
}

/** Digits after the decimal point for a currency (JPY has none, BHD three). */
export function currencyDigits(currency: string): number {
  try {
    return new Intl.NumberFormat("en", { style: "currency", currency }).resolvedOptions().maximumFractionDigits ?? 2;
  } catch {
    return 2;
  }
}

export function formatMoney(m: Money, locale?: string): string {
  const digits = currencyDigits(m.currency);
  const amount = m.minor / 10 ** digits;
  try {
    return new Intl.NumberFormat(locale, { style: "currency", currency: m.currency }).format(amount);
  } catch {
    return `${amount.toFixed(digits)} ${m.currency}`;
  }
}

/** Parses "1,200.50" into minor units for `currency`; `null` when it is not a number. */
export function parseMoneyInput(text: string, currency: string): number | null {
  const cleaned = text.replace(/[\s,]/g, "");
  if (cleaned === "" || !/^-?\d*\.?\d*$/.test(cleaned)) return null;
  const value = Number(cleaned);
  if (!Number.isFinite(value)) return null;
  return Math.round(value * 10 ** currencyDigits(currency));
}

export function moneyInputText(m: Money | null | undefined): string {
  if (!m) return "";
  return (m.minor / 10 ** currencyDigits(m.currency)).toFixed(currencyDigits(m.currency));
}

type Address = { label?: string; line1?: string; line2?: string; city?: string; region?: string; postcode?: string; country?: string };

export function formatAddress(a: Address): string {
  return [a.line1, a.line2, a.city, a.region, a.postcode, a.country].filter(Boolean).join(", ");
}

/** A value as short text for a table cell or a timeline entry. */
export function formatValue(field: SchemaField | undefined, value: unknown, locale?: string): string {
  if (value === null || value === undefined || value === "") return "";
  const type = field?.field_type;
  if (type === "money" && typeof value === "object" && value && "minor" in value) {
    return formatMoney(value as Money, locale);
  }
  if (type === "boolean" || typeof value === "boolean") return value ? "Yes" : "No";
  if (type === "percent" && typeof value === "number") return `${value}%`;
  if (type === "date" && typeof value === "string") {
    const d = new Date(`${value}T00:00:00`);
    return Number.isNaN(d.getTime()) ? value : d.toLocaleDateString(locale, { dateStyle: "medium" });
  }
  if (type === "datetime" && typeof value === "string") {
    const d = new Date(value);
    return Number.isNaN(d.getTime()) ? value : d.toLocaleString(locale, { dateStyle: "medium", timeStyle: "short" });
  }
  if ((type === "select" || type === "multi_select") && field) {
    const label = (v: unknown) => field.options.find((o) => o.value === v)?.label ?? String(v);
    return Array.isArray(value) ? value.map(label).join(", ") : label(value);
  }
  if (type === "address_list" && Array.isArray(value)) {
    return value.map((a) => formatAddress(a as Address)).join("; ");
  }
  if (Array.isArray(value)) return value.map((v) => (typeof v === "object" ? JSON.stringify(v) : String(v))).join(", ");
  if (typeof value === "object") {
    if ("minor" in (value as object)) return formatMoney(value as Money, locale);
    return JSON.stringify(value);
  }
  return String(value);
}
