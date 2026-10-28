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

/** Design tokens a full theme may set (the server allows the same). */
export const THEME_TOKENS = [
  "background",
  "foreground",
  "card",
  "muted",
  "muted-foreground",
  "border",
  "input",
  "ring",
  "primary",
  "primary-foreground",
  "secondary",
  "secondary-foreground",
  "accent",
  "accent-foreground",
  "destructive",
  "destructive-foreground",
] as const;
export type ThemeToken = (typeof THEME_TOKENS)[number];

/** Typefaces a theme may use: system font stacks, so nothing is downloaded. */
export const THEME_FONTS = {
  default: '"Inter", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif',
  system: 'system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif',
  humanist: 'Seravek, "Gill Sans Nova", Ubuntu, Calibri, "DejaVu Sans", source-sans-pro, sans-serif',
  geometric: 'Avenir, Montserrat, Corbel, "URW Gothic", source-sans-pro, sans-serif',
  serif: 'Charter, "Bitstream Charter", "Sitka Text", Cambria, Georgia, serif',
  rounded: 'ui-rounded, "Hiragino Maru Gothic ProN", Quicksand, Comfortaa, Manjari, "Arial Rounded MT", Calibri, sans-serif',
  mono: 'ui-monospace, "Cascadia Code", "Source Code Pro", Menlo, Consolas, "DejaVu Sans Mono", monospace',
} as const;
export type ThemeFont = keyof typeof THEME_FONTS;

/** A full theme, set by OnHold's team: colours per mode, a typeface and a corner radius. */
export interface BrandTheme {
  light?: Partial<Record<ThemeToken, string>>;
  dark?: Partial<Record<ThemeToken, string>>;
  font?: ThemeFont;
  radius?: number;
}

/** The theme's valid parts only. */
export function readTheme(value: unknown): BrandTheme | null {
  if (!value || typeof value !== "object") return null;
  const v = value as Record<string, unknown>;
  const colours = (m: unknown) => {
    const out: Partial<Record<ThemeToken, string>> = {};
    if (m && typeof m === "object") {
      for (const t of THEME_TOKENS) {
        const c = hexColour((m as Record<string, unknown>)[t]);
        if (c) out[t] = c;
      }
    }
    return out;
  };
  const theme: BrandTheme = { light: colours(v.light), dark: colours(v.dark) };
  if (typeof v.font === "string" && v.font in THEME_FONTS) theme.font = v.font as ThemeFont;
  if (typeof v.radius === "number" && v.radius >= 0 && v.radius <= 24) theme.radius = Math.round(v.radius);
  return theme;
}

/**
 * Puts a workspace's look on the page, in light and dark mode alike: its
 * brand colour, then any full theme on top. A style element rather than
 * inline styles, so the dark theme gets its own shades and stays readable.
 */
export function applyBrandTheme(look: { colour: string | null; theme?: BrandTheme | null }, doc: Document = document) {
  doc.getElementById(STYLE_ID)?.remove();
  const light: Record<string, string> = { ...(look.colour ? brandColours(look.colour) : {}) };
  const darkColour = look.colour ? darkBrandColour(look.colour) : null;
  const dark: Record<string, string> = { ...(darkColour ? brandColours(darkColour) : {}) };
  const t = look.theme;
  Object.assign(light, withTextOn(t?.light));
  Object.assign(dark, withTextOn(t?.dark));
  const shared: Record<string, string> = {};
  if (t?.font) shared["font-sans"] = THEME_FONTS[t.font];
  if (t?.radius !== undefined) shared.radius = `${t.radius}px`;
  if (!Object.keys(light).length && !Object.keys(dark).length && !Object.keys(shared).length) return;
  const tokens = (b: Record<string, string>) =>
    Object.entries(b)
      .map(([k, v]) => `--${k}: ${v};`)
      .join(" ");
  const style = doc.createElement("style");
  style.id = STYLE_ID;
  // Light colours only where the page is light, so a token set for light
  // alone never shows in dark mode; dark ones as styles.css switches.
  style.textContent =
    `:root { ${tokens(shared)} } :root.light { ${tokens(light)} } @media (prefers-color-scheme: light) { :root:not(.dark) { ${tokens(light)} } } ` +
    `@media (prefers-color-scheme: dark) { :root:not(.light) { ${tokens(dark)} } } :root.dark { ${tokens(dark)} }`;
  doc.head.appendChild(style);
}

/**
 * A mode's colours, with readable text added on a button colour set without
 * its own: OnHold's default text for that colour was chosen for its own.
 */
function withTextOn(colours: Partial<Record<ThemeToken, string>> | undefined): Record<string, string> {
  const out: Record<string, string> = { ...colours };
  for (const bg of ["primary", "destructive"] as const) {
    const fg = `${bg}-foreground` as const;
    const c = colours?.[bg];
    if (c && !colours?.[fg]) out[fg] = brandColours(c)["primary-foreground"] ?? "#ffffff";
  }
  return out;
}

/** Colours the page in a brand colour (see `applyBrandTheme`). */
export function applyBrandColour(hex: string | null, doc: Document = document) {
  applyBrandTheme({ colour: hex }, doc);
}

/** Contrast ratio of two hex colours, 1 to 21. */
export function contrastRatio(a: string, b: string): number {
  const x = hexColour(a);
  const y = hexColour(b);
  return x && y ? contrast(luminance(x), luminance(y)) : 1;
}

/** A logo the page may show: an image data URI only. */
export function logoSource(value: unknown): string | null {
  return typeof value === "string" && /^data:image\/(png|jpeg|gif|webp|svg\+xml);base64,[A-Za-z0-9+/=]+$/.test(value) ? value : null;
}

/** A workspace's stored branding: its logo, brand colour and full theme, if valid. */
export function readBranding(branding: unknown): { logo: string | null; colour: string | null; theme: BrandTheme | null } {
  const b = (branding && typeof branding === "object" ? branding : {}) as Record<string, unknown>;
  return { logo: logoSource(b.logo), colour: hexColour(b.primary_color) ?? hexColour(b.primary), theme: readTheme(b.theme) };
}

export type Theme = "light" | "dark" | "system";

/** Light, dark, or whatever the device prefers. */
export function applyTheme(theme: Theme, root: HTMLElement = document.documentElement) {
  root.classList.toggle("dark", theme === "dark");
  root.classList.toggle("light", theme === "light");
}
