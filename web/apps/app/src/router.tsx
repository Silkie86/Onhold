// Every page and its address. Addresses the API puts in emails and
// responses (`/sign-in`, `/auth/magic`, `/auth/mfa`, `/invitations/accept`,
// `/link-app`, `/crm/<entity>/<id>`) must keep working.

import type { QueryClient } from "@tanstack/react-query";
import {
  Outlet,
  createRootRouteWithContext,
  createRoute,
  createRouter,
  lazyRouteComponent,
  redirect,
} from "@tanstack/react-router";
import { AcceptInvitationPage, LinkAppPage, MagicLinkPage, MfaPage, SignInPage } from "./pages/auth.tsx";
import { EntityListPage, EntityRecordPage, validateListSearch } from "./pages/crm.tsx";
import { SyncPage } from "./pages/sync.tsx";
import { AppShell } from "./shell.tsx";

const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);

const root = createRootRouteWithContext<{ queryClient: QueryClient }>()({
  component: Outlet,
  notFoundComponent: () => <p className="p-6 text-sm">There is no page here.</p>,
});

const signIn = createRoute({
  getParentRoute: () => root,
  path: "/sign-in",
  validateSearch: (s: Record<string, unknown>): { redirect?: string; error?: string } => ({
    redirect: str(s.redirect),
    error: str(s.error),
  }),
  component: SignInPage,
});
const magic = createRoute({
  getParentRoute: () => root,
  path: "/auth/magic",
  validateSearch: (s: Record<string, unknown>): { token?: string; redirect?: string } => ({
    token: str(s.token),
    redirect: str(s.redirect),
  }),
  component: MagicLinkPage,
});
const mfa = createRoute({
  getParentRoute: () => root,
  path: "/auth/mfa",
  validateSearch: (s: Record<string, unknown>): { redirect?: string } => ({ redirect: str(s.redirect) }),
  component: MfaPage,
});
const invitation = createRoute({
  getParentRoute: () => root,
  path: "/invitations/accept",
  validateSearch: (s: Record<string, unknown>): { token?: string } => ({ token: str(s.token) }),
  component: AcceptInvitationPage,
});

// Where people approve a desktop or mobile app's sign-in (the API's `verification_url`).
const linkApp = createRoute({ getParentRoute: () => root, path: "/link-app", component: LinkAppPage });

const app = createRoute({ getParentRoute: () => root, id: "app", component: AppShell });
const home = createRoute({
  getParentRoute: () => app,
  path: "/",
  beforeLoad: () => {
    throw redirect({ to: "/crm/$entity", params: { entity: "contacts" } });
  },
});
const list = createRoute({
  getParentRoute: () => app,
  path: "/crm/$entity",
  validateSearch: validateListSearch,
  component: EntityListPage,
});
const record = createRoute({ getParentRoute: () => app, path: "/crm/$entity/$id", component: EntityRecordPage });
const imports = createRoute({ getParentRoute: () => app, path: "/imports", component: lazyRouteComponent(() => import("./pages/imports.tsx"), "ImportsPage") });
// Pages used now and then load when first opened.
const settings = () => import("./pages/settings.tsx");
const fields = createRoute({
  getParentRoute: () => app,
  path: "/settings/fields",
  component: lazyRouteComponent(settings, "FieldsPage"),
});
const pipelines = createRoute({
  getParentRoute: () => app,
  path: "/settings/pipelines",
  component: lazyRouteComponent(settings, "PipelinesPage"),
});

const sync = createRoute({ getParentRoute: () => app, path: "/sync", component: SyncPage });

const tree = root.addChildren([
  signIn,
  magic,
  mfa,
  invitation,
  linkApp,
  app.addChildren([home, list, record, imports, fields, pipelines, sync]),
]);

export function makeRouter(queryClient: QueryClient) {
  return createRouter({ routeTree: tree, context: { queryClient }, defaultPreload: "intent" });
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof makeRouter>;
  }
}
