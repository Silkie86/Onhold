import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

/** Joins class names, letting later Tailwind classes win over earlier ones. */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** A workspace's colours, as CSS colour values, keyed by token name. */
export type Branding = Partial<Record<"primary" | "primary-foreground" | "ring" | "accent", string>>;

/** Overrides the design tokens with a workspace's branding. */
export function applyBranding(branding: Branding, root: HTMLElement = document.documentElement) {
  for (const [token, value] of Object.entries(branding)) {
    if (value) root.style.setProperty(`--${token}`, value);
  }
}

export type Theme = "light" | "dark" | "system";

/** Light, dark, or whatever the device prefers. */
export function applyTheme(theme: Theme, root: HTMLElement = document.documentElement) {
  root.classList.toggle("dark", theme === "dark");
  root.classList.toggle("light", theme === "light");
}
