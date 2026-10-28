import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

/** Joins class names, letting later Tailwind classes win over earlier ones. */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** A workspace's colours, as CSS colour values, keyed by token name. */
export type Branding = Partial<Record<"primary" | "primary-foreground" | "ring" | "accent", string>>;

const TOKENS = ["primary", "primary-foreground", "ring", "accent"] as const;

/** Overrides the design tokens with a workspace's branding. */
export function applyBranding(branding: Branding, root: HTMLElement = document.documentElement) {
  for (const [token, value] of Object.entries(branding)) {
    if (value) root.style.setProperty(`--${token}`, value);
  }
}

/** Puts the design tokens back to OnHold's own. */
export function clearBranding(root: HTMLElement = document.documentElement) {
  for (const token of TOKENS) root.style.removeProperty(`--${token}`);
}

/** A hex colour (#abc or #aabbcc), lower case, or null. */
export function hexColour(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const v = value.trim().toLowerCase();
  return /^#([0-9a-f]{3}|[0-9a-f]{6})$/.test(v) ? v : null;
}

/** The colour's relative luminance, 0 (black) to 1 (white). */
function luminance(hex: string): number {
  const h = hex.length === 4 ? hex.replace(/^#(.)(.)(.)$/, "#$1$1$2$2$3$3") : hex;
  const channel = (i: number) => {
    const c = Number.parseInt(h.slice(i, i + 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5);
}

/** Tokens for one brand colour, with text on it in white or near-black, whichever reads better. */
export function brandColours(hex: string): Branding {
  const c = hexColour(hex);
  if (!c) return {};
  const onWhite = 1.05 / (luminance(c) + 0.05);
  const onDark = (luminance(c) + 0.05) / 0.05;
  return { primary: c, ring: c, "primary-foreground": onWhite >= onDark ? "#ffffff" : "#111111" };
}

/** A logo the page may show: an image data URI only. */
export function logoSource(value: unknown): string | null {
  return typeof value === "string" && /^data:image\/(png|jpeg|gif|webp|svg\+xml);base64,[A-Za-z0-9+/=]+$/.test(value) ? value : null;
}

/** A workspace's stored branding as a logo and design tokens. */
export function readBranding(branding: unknown): { logo: string | null; colours: Branding } {
  const b = (branding && typeof branding === "object" ? branding : {}) as Record<string, unknown>;
  const colour = hexColour(b.primary_color) ?? hexColour(b.primary);
  return { logo: logoSource(b.logo), colours: colour ? brandColours(colour) : {} };
}

export type Theme = "light" | "dark" | "system";

/** Light, dark, or whatever the device prefers. */
export function applyTheme(theme: Theme, root: HTMLElement = document.documentElement) {
  root.classList.toggle("dark", theme === "dark");
  root.classList.toggle("light", theme === "light");
}
