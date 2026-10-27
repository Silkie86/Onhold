// Makes the website installable ("Add to Home Screen", or "Install" in a
// desktop browser) and able to open with no connection: a web app manifest
// and a service worker that keeps the app's own files. Records are kept by
// the offline layer (src/offline), not here; the service worker never
// touches the API.
//
// Both files are written at build time, so the service worker knows exactly
// which files this version consists of. `VITE_APP_NAME` names the app (a
// client's own name on their own address); it defaults to OnHold.

import { createHash } from "node:crypto";
import { readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import type { Plugin } from "vite";

const THEME = "#4338ca";

function publicFiles(dir: string, root = dir): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? publicFiles(path, root) : [relative(root, path).replaceAll("\\", "/")];
  });
}

function serviceWorker(version: string, files: string[]): string {
  return `// Generated at build time by pwa.ts.
const CACHE = "onhold-${version}";
const FILES = ${JSON.stringify(["/", ...files.map((f) => `/${f}`)])};

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((c) => c.addAll(FILES)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k.startsWith("onhold-") && k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  const url = new URL(req.url);
  // The API, other sites and anything but reading are none of its business.
  if (req.method !== "GET" || url.origin !== self.location.origin || url.pathname.startsWith("/api/")) return;
  if (req.mode === "navigate") {
    // Pages: the newest from the server when it answers, else the saved app.
    event.respondWith(fetch(req).catch(() => caches.match("/", { cacheName: CACHE, ignoreVary: true })));
    return;
  }
  // The app's files have content hashes in their names: saved ones never go stale.
  // (Vary is ignored: a module script asks with an Origin header, the precache without.)
  event.respondWith(caches.match(req, { cacheName: CACHE, ignoreVary: true }).then((hit) => hit ?? fetch(req)));
});
`;
}

export function pwa(): Plugin {
  let publicDir = "";
  let name = "OnHold";
  return {
    name: "onhold-pwa",
    configResolved(config) {
      publicDir = config.publicDir;
      name = config.env.VITE_APP_NAME?.trim() || "OnHold";
    },
    transformIndexHtml() {
      return [
        { tag: "link", attrs: { rel: "manifest", href: "/manifest.webmanifest" }, injectTo: "head" },
        { tag: "link", attrs: { rel: "apple-touch-icon", href: "/icons/apple-touch-icon.png" }, injectTo: "head" },
        { tag: "meta", attrs: { name: "theme-color", content: THEME }, injectTo: "head" },
        { tag: "meta", attrs: { name: "apple-mobile-web-app-title", content: name }, injectTo: "head" },
        { tag: "meta", attrs: { name: "mobile-web-app-capable", content: "yes" }, injectTo: "head" },
      ];
    },
    generateBundle(_options, bundle) {
      const manifest = {
        name,
        short_name: name.length > 12 ? name.split(/\s+/)[0] : name,
        id: "/",
        start_url: "/",
        scope: "/",
        display: "standalone",
        background_color: "#ffffff",
        theme_color: THEME,
        icons: [
          { src: "/icons/icon-192.png", sizes: "192x192", type: "image/png" },
          { src: "/icons/icon-512.png", sizes: "512x512", type: "image/png" },
          { src: "/icons/icon-maskable-512.png", sizes: "512x512", type: "image/png", purpose: "maskable" },
        ],
      };
      this.emitFile({ type: "asset", fileName: "manifest.webmanifest", source: `${JSON.stringify(manifest, null, 2)}\n` });

      const built = Object.keys(bundle).filter((f) => !f.endsWith(".map"));
      const files = [...new Set([...built, ...publicFiles(publicDir), "manifest.webmanifest"])].sort();
      // A new version whenever any file changes, so browsers fetch the new set.
      const version = createHash("sha256").update(files.join("|")).digest("hex").slice(0, 16);
      this.emitFile({ type: "asset", fileName: "sw.js", source: serviceWorker(version, files) });
    },
  };
}
