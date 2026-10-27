import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";
import { pwa } from "./pwa.ts";

// In development the API runs beside Vite and is proxied, so the session
// cookie stays same-origin. Production serves the built files from the same
// origin as the API (or a proxy in front of both).
const api = process.env.ONHOLD_API ?? "http://localhost:8080";

export default defineConfig({
  plugins: [react(), tailwindcss(), pwa()],
  server: { port: 5173, proxy: { "/api": api } },
  preview: { port: 4173, proxy: { "/api": api } },
  // The main chunk (React, router, query, Radix) is about 170 kB gzipped;
  // imports and settings load on demand. Warn only if it grows well past that.
  build: { sourcemap: true, chunkSizeWarningLimit: 700 },
  test: { environment: "jsdom", globals: true, include: ["src/**/*.test.{ts,tsx}"] },
});
