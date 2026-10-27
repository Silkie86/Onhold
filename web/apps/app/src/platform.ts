// Where the app is running: in a browser on the same origin as the API, or
// inside the desktop or mobile app (Tauri), which talks to a server
// elsewhere with a bearer token kept in the device's keychain.
//
// `VITE_APP_MODE=app` runs the app mode in a plain browser, for the browser
// tests; tokens then live in localStorage.

const SERVER_KEY = "onhold.server";
const TOKEN_KEY = "onhold.app-token";

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** The app's name: OnHold, or a client's own in their branded build. */
export const APP_NAME: string = import.meta.env.VITE_APP_NAME?.trim() || "OnHold";

/** True in the desktop and mobile apps (and in their browser test mode). */
export const IS_APP: boolean = isTauri() || import.meta.env.VITE_APP_MODE === "app";

function readLocal(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeLocal(key: string, value: string | null) {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {
    // Storage blocked: the value lasts until the app closes.
  }
}

/** True when the website runs installed (added to the home screen or desktop). */
export function isInstalled(): boolean {
  if (typeof window === "undefined") return false;
  const standalone = (navigator as Navigator & { standalone?: boolean }).standalone === true;
  return standalone || window.matchMedia?.("(display-mode: standalone)").matches === true;
}

/**
 * Whether this copy keeps the CRM on the device for offline use: the apps,
 * and the website once installed. A website open in an ordinary tab keeps
 * nothing, since that may be a shared or borrowed computer.
 */
export const KEEPS_OFFLINE_COPY: boolean = IS_APP || isInstalled();

/** The server the app talks to: chosen on the sign-in screen, or built in. Empty means this origin. */
export function serverUrl(): string {
  if (!IS_APP) return import.meta.env.VITE_API_URL ?? "";
  return (readLocal(SERVER_KEY) ?? import.meta.env.VITE_API_URL ?? "").replace(/\/+$/, "");
}

export function setServerUrl(url: string | null) {
  writeLocal(SERVER_KEY, url ? url.trim().replace(/\/+$/, "") : null);
}

/** Checks a server address typed by a person: `https://…`, or `http://` for this machine only. */
export function validServerUrl(input: string): string | null {
  try {
    const url = new URL(input.trim());
    const local = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
    if (url.protocol !== "https:" && !(url.protocol === "http:" && local)) return null;
    return url.origin;
  } catch {
    return null;
  }
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const core = await import("@tauri-apps/api/core");
  return core.invoke<T>(cmd, args);
}

let token: string | null = null;

/** The app's session token, loaded once at start. Null in the browser. */
export function appToken(): string | null {
  return token;
}

/** Reads the stored token before the first request. */
export async function loadAppToken(): Promise<void> {
  if (!IS_APP) return;
  token = isTauri() ? await invoke<string | null>("token_get").catch(() => null) : readLocal(TOKEN_KEY);
}

export async function saveAppToken(value: string | null): Promise<void> {
  token = value;
  if (isTauri()) {
    await invoke(value ? "token_set" : "token_clear", value ? { token: value } : undefined);
  } else if (IS_APP) {
    writeLocal(TOKEN_KEY, value);
  }
}

/** A name for this device, shown when approving it and in the session list. */
export async function deviceName(): Promise<string> {
  if (isTauri()) {
    const name = await invoke<string>("device_name").catch(() => "");
    if (name.trim()) return name.trim().slice(0, 100);
  }
  const ua = navigator.userAgent;
  const os = /Windows/.test(ua)
    ? "Windows"
    : /iPhone|iPad/.test(ua)
      ? "iOS"
      : /Mac OS X/.test(ua)
        ? "Mac"
        : /Android/.test(ua)
          ? "Android"
          : /Linux/.test(ua)
            ? "Linux"
            : "this device";
  return `${APP_NAME} on ${os}`;
}

/** Opens a page in the person's own browser (a new tab outside the apps). */
export async function openInBrowser(url: string): Promise<void> {
  if (isTauri()) {
    const opener = await import("@tauri-apps/plugin-opener");
    await opener.openUrl(url);
  } else {
    window.open(url, "_blank", "noopener");
  }
}
