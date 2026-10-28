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
  root.ownerDocument.getElementById(STYLE_ID)?.remove();
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

function toHex(rgb: number[]): string {
  return `#${rgb.map((c) => Math.round(c).toString(16).padStart(2, "0")).join("")}`;
}

function rgbOf(hex: string): number[] {
  const h = hex.length === 4 ? hex.replace(/^#(.)(.)(.)$/, "#$1$1$2$2$3$3") : hex;
  return [1, 3, 5].map((i) => Number.parseInt(h.slice(i, i + 2), 16));
}

function contrast(a: number, b: number): number {
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

/** Tokens for one brand colour, with text on it in white or near-black, whichever reads better. */
export function brandColours(hex: string): Branding {
  const c = hexColour(hex);
  if (!c) return {};
  const onWhite = contrast(1, luminance(c));
  const onDark = contrast(luminance(c), 0);
  return { primary: c, ring: c, "primary-foreground": onWhite >= onDark ? "#ffffff" : "#111111" };
}

/** The brand colour for dark mode: lightened until links in it read on the dark background. */
export function darkBrandColour(hex: string): string | null {
  const c = hexColour(hex);
  if (!c) return null;
  const rgb = rgbOf(c);
  // The dark theme's background is about oklch(0.17), luminance ~0.012.
  for (let t = 0; t <= 1; t += 0.05) {
    const mixed = toHex(rgb.map((v) => v + (255 - v) * t));
    if (contrast(luminance(mixed), 0.012) >= 4.5) return mixed;
  }
  return "#ffffff";
}

const STYLE_ID = "onhold-brand";

/**
 * Colours the app in a brand colour, in light and dark mode alike: a style
 * element rather than inline styles, so the dark theme gets its own, lighter
 * shade and stays readable.
 */
export function applyBrandColour(hex: string | null, doc: Document = document) {
  doc.getElementById(STYLE_ID)?.remove();
  const light = hex ? brandColours(hex) : {};
  const dark = hex ? darkBrandColour(hex) : null;
  if (!light.primary || !dark) return;
  const tokens = (b: Branding) =>
    Object.entries(b)
      .map(([k, v]) => `--${k}: ${v};`)
      .join(" ");
  const darkTokens = tokens(brandColours(dark));
  const style = doc.createElement("style");
  style.id = STYLE_ID;
  style.textContent = `:root { ${tokens(light)} } @media (prefers-color-scheme: dark) { :root:not(.light) { ${darkTokens} } } :root.dark { ${darkTokens} }`;
  doc.head.appendChild(style);
}

/** A logo the page may show: an image data URI only. */
export function logoSource(value: unknown): string | null {
  return typeof value === "string" && /^data:image\/(png|jpeg|gif|webp|svg\+xml);base64,[A-Za-z0-9+/=]+$/.test(value) ? value : null;
}

/** A workspace's stored branding: its logo and brand colour, if valid. */
export function readBranding(branding: unknown): { logo: string | null; colour: string | null } {
  const b = (branding && typeof branding === "object" ? branding : {}) as Record<string, unknown>;
  return { logo: logoSource(b.logo), colour: hexColour(b.primary_color) ?? hexColour(b.primary) };
}

export type Theme = "light" | "dark" | "system";

/** Light, dark, or whatever the device prefers. */
export function applyTheme(theme: Theme, root: HTMLElement = document.documentElement) {
  root.classList.toggle("dark", theme === "dark");
  root.classList.toggle("light", theme === "light");
}
