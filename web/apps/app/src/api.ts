// The API client the app uses, and what it needs to act as the signed-in
// person: the session's CSRF token (browsers) or bearer token (apps), and the
// current workspace.

import { type ApiError, type Schemas, createApiClient } from "@onhold/api-client";
import type { FieldIssue } from "@onhold/ui";
import { IS_APP, appToken, serverUrl } from "./platform.ts";

const WORKSPACE_KEY = "onhold.workspace";

let csrfToken: string | undefined;
let workspaceId: string | undefined = readStored();

function readStored(): string | undefined {
  try {
    return localStorage.getItem(WORKSPACE_KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

export function setCsrfToken(token: string | undefined) {
  csrfToken = token;
}

export function currentWorkspace(): string | undefined {
  return workspaceId;
}

export function setWorkspace(id: string | undefined) {
  workspaceId = id;
  try {
    if (id) localStorage.setItem(WORKSPACE_KEY, id);
    else localStorage.removeItem(WORKSPACE_KEY);
  } catch {
    // Private browsing: the choice lasts for this tab only.
  }
}

export const api = createApiClient(
  IS_APP
    ? { baseUrl: serverUrl, bearerToken: appToken, credentials: "omit", workspaceId: () => workspaceId }
    : { baseUrl: serverUrl(), csrfToken: () => csrfToken, workspaceId: () => workspaceId },
);

/** An error the API returned, with its per-field problems. */
export class ApiFailure extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly issues: FieldIssue[],
  ) {
    super(message);
  }
}

type Result<T> = { data?: T; error?: unknown; response: Response };

/** The data of a call, or an `ApiFailure` thrown with the API's explanation. */
export async function ok<T>(call: Promise<Result<T>>): Promise<T> {
  const { data, error, response } = await call;
  if (response.ok) return data as T;
  const body = (error as { error?: ApiError } | undefined)?.error;
  const issues: FieldIssue[] = body?.issues ?? (body?.field ? [{ field: body.field, message: body.message }] : []);
  throw new ApiFailure(
    response.status,
    body?.code ?? "http_error",
    body?.message ?? `${response.status} ${response.statusText}`,
    issues,
  );
}

export type Me = Schemas["MeResponse"];
export type EntitySchema = Schemas["EntitySchema"];

/** The CRM entities, with where their records live in the API and the app. */
export const ENTITIES = {
  contact: { entity: "contact", path: "contacts", label: "Contacts" },
  organisation: { entity: "organisation", path: "organisations", label: "Organisations" },
  talent: { entity: "talent", path: "talent", label: "Talent" },
} as const;

export type EntityKey = keyof typeof ENTITIES;
export type EntityPath = (typeof ENTITIES)[EntityKey]["path"];

export function entityByPath(path: string): (typeof ENTITIES)[EntityKey] | undefined {
  return Object.values(ENTITIES).find((e) => e.path === path);
}
