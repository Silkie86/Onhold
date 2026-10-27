import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { ApiFailure } from "./api.ts";
import { APP_NAME, IS_APP, KEEPS_OFFLINE_COPY, isTauri, loadAppToken } from "./platform.ts";
import { makeRouter } from "./router.tsx";
import "./styles.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      // Refusals do not get better by asking again.
      retry: (count, error) => !(error instanceof ApiFailure && error.status < 500) && count < 2,
      // With an offline copy, pages answer from the device when offline, so
      // queries must run then too rather than wait for a connection.
      networkMode: KEEPS_OFFLINE_COPY ? "always" : "online",
    },
    mutations: { networkMode: KEEPS_OFFLINE_COPY ? "always" : "online" },
  },
});
const router = makeRouter(queryClient);

if (IS_APP) document.title = APP_NAME;

// The website opens without a connection, and can be installed, once its
// service worker has saved the app's files (production builds only).
if (import.meta.env.PROD && !isTauri() && "serviceWorker" in navigator) {
  window.addEventListener("load", () => void navigator.serviceWorker.register("/sw.js").catch(() => undefined));
}

const root = document.getElementById("root");
// The apps read their stored session before the first request.
void loadAppToken().then(() => {
  if (!root) return;
  createRoot(root).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </StrictMode>,
  );
});
