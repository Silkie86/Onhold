// Typed client for the OnHold API. `schema.ts` is generated from
// `openapi.json` (run `pnpm generate`); never edit it by hand.
//
// Writes that may be retried (an offline queue, a flaky connection) should
// pass the same `Idempotency-Key` header on every attempt of one operation.

import createClient, { type Middleware } from "openapi-fetch";
import type { components, paths } from "./schema.ts";

export type { components, paths };
export type Schemas = components["schemas"];
export type ApiError = Schemas["ErrorBody"]["error"];

export interface ClientOptions {
  /** Origin of the API, e.g. `https://app.onhold.example`. Empty for same-origin. */
  baseUrl?: string;
  /** The workspace requests act in, sent as `X-Workspace-Id`. */
  workspaceId?: () => string | undefined;
  /** The session's CSRF token, sent as `X-CSRF-Token` on writes. */
  csrfToken?: () => string | undefined;
  /** An API key (`ohk_…`) for scripts; replaces the session cookie. */
  apiKey?: string;
  fetch?: typeof globalThis.fetch;
}

const WRITES = new Set(["POST", "PUT", "PATCH", "DELETE"]);

export function createApiClient(options: ClientOptions = {}) {
  const client = createClient<paths>({
    // Paths in the spec already start with `/api/v1`.
    baseUrl: options.baseUrl ?? "",
    credentials: "include",
    fetch: options.fetch,
  });

  const headers: Middleware = {
    onRequest({ request }) {
      if (options.apiKey) request.headers.set("Authorization", `Bearer ${options.apiKey}`);
      const workspace = options.workspaceId?.();
      if (workspace && !request.headers.has("X-Workspace-Id")) request.headers.set("X-Workspace-Id", workspace);
      const csrf = options.csrfToken?.();
      if (csrf && WRITES.has(request.method)) request.headers.set("X-CSRF-Token", csrf);
      return request;
    },
  };
  client.use(headers);
  return client;
}

export type ApiClient = ReturnType<typeof createApiClient>;
