// Offering to install the website as an app. Chrome, Edge and Android hand
// the page an install prompt to show when the person asks; Safari on iPhone
// and iPad has none, so there the person is told where Add to Home Screen is.

import { useSyncExternalStore } from "react";
import { IS_APP, isInstalled } from "./platform.ts";

interface InstallPrompt extends Event {
  prompt: () => Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed" }>;
}

let deferred: InstallPrompt | null = null;
const listeners = new Set<() => void>();
const changed = () => {
  for (const l of listeners) l();
};

if (typeof window !== "undefined" && !IS_APP) {
  window.addEventListener("beforeinstallprompt", (e) => {
    e.preventDefault();
    deferred = e as InstallPrompt;
    changed();
  });
  window.addEventListener("appinstalled", () => {
    deferred = null;
    changed();
  });
}

const isIos = () =>
  typeof navigator !== "undefined" &&
  (/iPhone|iPad|iPod/.test(navigator.userAgent) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1));

export type InstallOffer = { kind: "prompt"; install: () => Promise<void> } | { kind: "ios" } | null;

/** How this browser can install the website, or null when it is already installed or cannot. */
export function useInstallOffer(): InstallOffer {
  const prompt = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => deferred,
  );
  if (IS_APP || isInstalled()) return null;
  if (prompt) {
    return {
      kind: "prompt",
      install: async () => {
        await prompt.prompt();
        await prompt.userChoice;
        deferred = null;
        changed();
      },
    };
  }
  return isIos() ? { kind: "ios" } : null;
}
