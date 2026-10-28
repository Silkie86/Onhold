// Every page and its address. Addresses the API puts in emails and
// responses (`/sign-in`, `/auth/magic`, `/auth/mfa`, `/invitations/accept`,
// `/link-app`, `/crm/<entity>/<id>`, `/quote/<token>`, `/bookings/<id>`,
// `/enquiries/<id>`, `/tasks`, `/invoice/<token>`, `/sign/<token>`,
// `/finance/invoices/<id>`, `/documents/<id>`) must keep working.

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
import { BookingPage, BookingsPage, validateBookingsSearch } from "./pages/bookings.tsx";
import { CalendarPage, validateCalendarSearch } from "./pages/calendar.tsx";
import { EntityListPage, EntityRecordPage, validateListSearch } from "./pages/crm.tsx";
import { EnquiriesPage, EnquiryPage } from "./pages/enquiries.tsx";
import { PublicQuotePage } from "./pages/quotes.tsx";
import { FinanceSettingsPage, InvoicePage, InvoicesPage, PaymentsPage, PublicInvoicePage, ReportsPage } from "./pages/finance.tsx";
import { DocumentPage, DocumentsPage, PdfImportsPage, PublicSigningPage } from "./pages/documents.tsx";
import { TasksPage, validateTasksSearch } from "./pages/tasks.tsx";
import { SyncPage } from "./pages/sync.tsx";
import { InboxPage, validateInboxSearch } from "./pages/inbox.tsx";
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

// The buyer's page for a quote: no sign-in, the link is the key.
const publicQuote = createRoute({
  getParentRoute: () => root,
  path: "/quote/$token",
  component: PublicQuotePage,
});

// The payer's page for an invoice, and a signer's for a document.
const publicInvoice = createRoute({
  getParentRoute: () => root,
  path: "/invoice/$token",
  validateSearch: (s: Record<string, unknown>): { paid?: string } => ({ paid: str(s.paid) }),
  component: PublicInvoicePage,
});
const publicSigning = createRoute({ getParentRoute: () => root, path: "/sign/$token", component: PublicSigningPage });

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
const enquiries = createRoute({ getParentRoute: () => app, path: "/enquiries", component: EnquiriesPage });
const enquiry = createRoute({ getParentRoute: () => app, path: "/enquiries/$id", component: EnquiryPage });
const bookings = createRoute({
  getParentRoute: () => app,
  path: "/bookings",
  validateSearch: validateBookingsSearch,
  component: BookingsPage,
});
const booking = createRoute({ getParentRoute: () => app, path: "/bookings/$id", component: BookingPage });
const calendar = createRoute({
  getParentRoute: () => app,
  path: "/calendar",
  validateSearch: validateCalendarSearch,
  component: CalendarPage,
});
const tasks = createRoute({ getParentRoute: () => app, path: "/tasks", validateSearch: validateTasksSearch, component: TasksPage });
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

const calendarSettings = createRoute({
  getParentRoute: () => app,
  path: "/settings/calendar",
  component: lazyRouteComponent(() => import("./pages/calendar-settings.tsx"), "CalendarSettingsPage"),
});

const inbox = createRoute({ getParentRoute: () => app, path: "/inbox", validateSearch: validateInboxSearch, component: InboxPage });
const emailSettings = createRoute({
  getParentRoute: () => app,
  path: "/settings/email",
  // Google and Microsoft send the member back here with these.
  validateSearch: (s: Record<string, unknown>): { code?: string; state?: string; error?: string; error_description?: string } => ({
    code: str(s.code),
    state: str(s.state),
    error: str(s.error),
    error_description: str(s.error_description),
  }),
  component: lazyRouteComponent(() => import("./pages/email-settings.tsx"), "EmailSettingsPage"),
});

const sendingDomain = createRoute({
  getParentRoute: () => app,
  path: "/settings/email/domain",
  // A DNS provider sends the owner back here after Domain Connect.
  validateSearch: (s: Record<string, unknown>): { checked?: string; error?: string } => ({
    checked: str(s.checked),
    error: str(s.error),
  }),
  component: lazyRouteComponent(() => import("./pages/sending-domain.tsx"), "SendingDomainPage"),
});

const sync = createRoute({ getParentRoute: () => app, path: "/sync", component: SyncPage });

const invoices = createRoute({ getParentRoute: () => app, path: "/finance/invoices", component: InvoicesPage });
const invoice = createRoute({ getParentRoute: () => app, path: "/finance/invoices/$id", component: InvoicePage });
const payments = createRoute({ getParentRoute: () => app, path: "/finance/payments", component: PaymentsPage });
const reports = createRoute({ getParentRoute: () => app, path: "/finance/reports", component: ReportsPage });
const documents = createRoute({ getParentRoute: () => app, path: "/documents", component: DocumentsPage });
const pdfImports = createRoute({ getParentRoute: () => app, path: "/documents/imports", component: PdfImportsPage });
const documentPage = createRoute({ getParentRoute: () => app, path: "/documents/$id", component: DocumentPage });
const paymentsSettings = createRoute({
  getParentRoute: () => app,
  path: "/settings/payments",
  // Stripe sends the admin back here with these.
  validateSearch: (s: Record<string, unknown>): { code?: string; state?: string; error?: string; error_description?: string } => ({
    code: str(s.code),
    state: str(s.state),
    error: str(s.error),
    error_description: str(s.error_description),
  }),
  component: lazyRouteComponent(() => import("./pages/payments-settings.tsx"), "PaymentsSettingsPage"),
});
const financeSettings = createRoute({
  getParentRoute: () => app,
  path: "/settings/finance",
  component: FinanceSettingsPage,
});

const tree = root.addChildren([
  signIn,
  magic,
  mfa,
  invitation,
  linkApp,
  publicQuote,
  publicInvoice,
  publicSigning,
  app.addChildren([
    home,
    inbox,
    emailSettings,
    calendar,
    enquiries,
    enquiry,
    bookings,
    booking,
    tasks,
    list,
    record,
    imports,
    fields,
    pipelines,
    calendarSettings,
    sendingDomain,
    sync,
    invoices,
    invoice,
    payments,
    reports,
    documents,
    pdfImports,
    documentPage,
    financeSettings,
    paymentsSettings,
  ]),
]);

export function makeRouter(queryClient: QueryClient) {
  return createRouter({ routeTree: tree, context: { queryClient }, defaultPreload: "intent" });
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof makeRouter>;
  }
}
