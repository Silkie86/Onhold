// Builds the desktop app under a client's own name and icon, pointed at
// their OnHold address. Usage:
//
//   node scripts/brand.mjs brands/example.json [--prepare-only] [-- <tauri build args>]
//
// The brand file names the app, its identifier, its icon (an SVG, or a PNG
// of at least 1024×1024) and the server. Icons and a config overlay are
// written to src-tauri/brands/<slug>/ and the build uses them; the shared
// code is untouched, so every client's app is the same OnHold underneath.

import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const appDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const tauriDir = join(appDir, "src-tauri");

const [file, ...rest] = process.argv.slice(2);
if (!file) {
  console.error("usage: node scripts/brand.mjs <brand.json> [--prepare-only] [-- <tauri build args>]");
  process.exit(2);
}
const prepareOnly = rest.includes("--prepare-only");
const passThrough = rest.includes("--") ? rest.slice(rest.indexOf("--") + 1) : [];

const brandPath = resolve(file);
const brand = JSON.parse(readFileSync(brandPath, "utf8"));
const problems = [];
if (!/^[a-z0-9][a-z0-9-]{0,39}$/.test(brand.slug ?? "")) problems.push("slug: lower-case letters, digits and dashes");
if (!brand.name?.trim() || brand.name.length > 60) problems.push("name: 1 to 60 characters");
// Reverse-DNS, as the app stores and the operating systems expect; it also
// keeps each client's stored session and data apart on a shared computer.
if (!/^[a-zA-Z][a-zA-Z0-9-]*(\.[a-zA-Z][a-zA-Z0-9-]*){2,}$/.test(brand.identifier ?? "")) {
  problems.push("identifier: reverse-DNS such as com.example.bookings");
}
if (!brand.icon) problems.push("icon: path to an SVG or a 1024×1024 PNG");
try {
  const url = new URL(brand.server);
  if (url.protocol !== "https:" || url.pathname !== "/") throw new Error();
} catch {
  problems.push("server: the client's OnHold address, https://… with no path");
}
if (problems.length) {
  console.error(`${file} is not a valid brand file:\n  ${problems.join("\n  ")}`);
  process.exit(2);
}

const outDir = join(tauriDir, "brands", brand.slug);
const iconDir = join(outDir, "icons");
mkdirSync(iconDir, { recursive: true });

const run = (args, env = {}) => {
  const r = spawnSync("pnpm", ["exec", "tauri", ...args], {
    cwd: appDir,
    stdio: "inherit",
    env: { ...process.env, ...env },
    shell: process.platform === "win32",
  });
  if (r.status !== 0) process.exit(r.status ?? 1);
};

run(["icon", resolve(dirname(brandPath), brand.icon), "-o", iconDir]);

// Merged over tauri.conf.json (arrays are replaced whole, so the window is repeated in full).
const base = JSON.parse(readFileSync(join(tauriDir, "tauri.conf.json"), "utf8"));
const icons = (base.bundle.icon ?? []).map((p) => relative(tauriDir, join(iconDir, p.replace(/^icons\//, ""))).replaceAll("\\", "/"));
const overlay = {
  productName: brand.name,
  identifier: brand.identifier,
  app: { windows: base.app.windows.map((w) => ({ ...w, title: brand.name })) },
  bundle: { icon: icons, shortDescription: brand.name },
};
const overlayPath = join(outDir, "tauri.conf.json");
writeFileSync(overlayPath, `${JSON.stringify(overlay, null, 2)}\n`);
console.log(`wrote ${relative(appDir, overlayPath)}`);

if (!prepareOnly) {
  run(["build", "--config", overlayPath, ...passThrough], {
    VITE_API_URL: new URL(brand.server).origin,
    VITE_APP_NAME: brand.name,
  });
}
