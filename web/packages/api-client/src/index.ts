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
  /**
   * Origin of the API, e.g. `https://app.onhold.example`. Empty for
   * same-origin. A function is asked on every request, for apps whose server
   * can change while they run.
   */
  baseUrl?: string | (() => string);
  /** The workspace requests act in, sent as `X-Workspace-Id`. */
  workspaceId?: () => string | undefined;
  /** The session's CSRF token, sent as `X-CSRF-Token` on writes. */
  csrfToken?: () => string | undefined;
  /** An API key (`ohk_…`) for scripts; replaces the session cookie. */
  apiKey?: string;
  /** An app's session token (`ohs_…`), asked on every request; replaces the session cookie. */
  bearerToken?: () => string | null | undefined;
  /** Whether the browser sends cookies. Defaults to `include`; apps use `omit`. */
  credentials?: RequestCredentials;
  fetch?: typeof globalThis.fetch;
}

const WRITES = new Set(["POST", "PUT", "PATCH", "DELETE"]);

export function createApiClient(options: ClientOptions = {}) {
  const client = createClient<paths>({
    // Paths in the spec already start with `/api/v1`.
    baseUrl: typeof options.baseUrl === "string" ? options.baseUrl : "",
    credentials: options.credentials ?? "include",
    fetch: options.fetch,
  });

  const dynamicBase = typeof options.baseUrl === "function" ? options.baseUrl : null;
  const headers: Middleware = {
    async onRequest({ request }) {
      const base = dynamicBase?.();
      if (base) {
        const url = new URL(request.url);
        // Rebuilt field by field: a streamed body cannot be handed from one Request to another.
        request = new Request(`${base}${url.pathname}${url.search}`, {
          method: request.method,
          headers: request.headers,
          body: request.method === "GET" || request.method === "HEAD" ? undefined : await request.arrayBuffer(),
          credentials: request.credentials,
          signal: request.signal,
        });
      }
      const bearer = options.apiKey ?? options.bearerToken?.();
      if (bearer) request.headers.set("Authorization", `Bearer ${bearer}`);
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
