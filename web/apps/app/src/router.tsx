// Every page and its address. Addresses the API puts in emails and
// responses (`/sign-in`, `/auth/magic`, `/auth/mfa`, `/invitations/accept`,
// `/link-app`, `/crm/<entity>/<id>`, `/quote/<token>`, `/bookings/<id>`,
// `/enquiries/<id>`, `/tasks`, `/invoice/<token>`, `/sign/<token>`,
// `/finance/invoices/<id>`, `/documents/<id>`, `/portal`) must keep working.

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
import { ConnectCalendarPage, ConnectCalendarReturnPage } from "./pages/talent-calendars.tsx";
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
const projectsPage = () => import("./pages/projects.tsx");
const projects = createRoute({ getParentRoute: () => app, path: "/projects", component: lazyRouteComponent(projectsPage, "ProjectsPage") });
const project = createRoute({ getParentRoute: () => app, path: "/projects/$id", component: lazyRouteComponent(projectsPage, "ProjectPage") });
const projectDay = createRoute({
  getParentRoute: () => app,
  path: "/projects/$id/days/$date",
  component: lazyRouteComponent(projectsPage, "ProjectDayPage"),
});
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
  // Google and Microsoft send the member back here with these.
  validateSearch: (s: Record<string, unknown>): { code?: string; state?: string; error?: string; error_description?: string } => ({
    code: str(s.code),
    state: str(s.state),
    error: str(s.error),
    error_description: str(s.error_description),
  }),
  component: lazyRouteComponent(() => import("./pages/calendar-settings.tsx"), "CalendarSettingsPage"),
});

const themeStudio = createRoute({
  getParentRoute: () => app,
  path: "/settings/theme",
  component: lazyRouteComponent(() => import("./pages/theme-studio.tsx"), "ThemeStudioPage"),
});
// Outside the app's frame: OnHold's team may belong to no workspace themselves.
const operator = createRoute({
  getParentRoute: () => root,
  path: "/operator",
  component: lazyRouteComponent(() => import("./pages/operator.tsx"), "OperatorPage"),
});

const streamline = createRoute({
  getParentRoute: () => app,
  path: "/settings/streamline",
  component: lazyRouteComponent(() => import("./pages/streamline.tsx"), "StreamlinePage"),
});

const setupSettings = createRoute({
  getParentRoute: () => app,
  path: "/settings/setup",
  component: lazyRouteComponent(() => import("./pages/setup-settings.tsx"), "SetupSettingsPage"),
});

const brandingSettings = createRoute({
  getParentRoute: () => app,
  path: "/settings/branding",
  component: lazyRouteComponent(() => import("./pages/branding-settings.tsx"), "BrandingSettingsPage"),
});

// A talent connecting their own calendar from the agency's link, and where
// Google or Microsoft send them back to.
const connectCalendar = createRoute({
  getParentRoute: () => root,
  path: "/connect-calendar/$token",
  component: ConnectCalendarPage,
});
const connectCalendarReturn = createRoute({
  getParentRoute: () => root,
  path: "/connect-calendar",
  validateSearch: (s: Record<string, unknown>): { code?: string; state?: string; error?: string } => ({
    code: str(s.code),
    state: str(s.state),
    error: str(s.error),
  }),
  component: ConnectCalendarReturnPage,
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

// The talent portal: what an invited artist sees in place of the staff app.
const portalPage = () => import("./pages/portal.tsx");
const portalDates = createRoute({ getParentRoute: () => app, path: "/portal", component: lazyRouteComponent(portalPage, "PortalDatesPage") });
const portalDate = createRoute({
  getParentRoute: () => app,
  path: "/portal/dates/$id",
  component: lazyRouteComponent(portalPage, "PortalDatePage"),
});
const portalAvailability = createRoute({
  getParentRoute: () => app,
  path: "/portal/availability",
  component: lazyRouteComponent(portalPage, "PortalAvailabilityPage"),
});
const portalDocuments = createRoute({
  getParentRoute: () => app,
  path: "/portal/documents",
  component: lazyRouteComponent(portalPage, "PortalDocumentsPage"),
});
const portalMoney = createRoute({ getParentRoute: () => app, path: "/portal/money", component: lazyRouteComponent(portalPage, "PortalMoneyPage") });
const portalMessages = createRoute({
  getParentRoute: () => app,
  path: "/portal/messages",
  component: lazyRouteComponent(portalPage, "PortalMessagesPage"),
});
const portalProfile = createRoute({
  getParentRoute: () => app,
  path: "/portal/profile",
  component: lazyRouteComponent(portalPage, "PortalProfilePage"),
});
const portalUnlock = createRoute({
  getParentRoute: () => app,
  path: "/portal/unlock",
  component: lazyRouteComponent(portalPage, "PortalUnlockPage"),
});
const portalSettings = createRoute({
  getParentRoute: () => app,
  path: "/settings/portal",
  component: lazyRouteComponent(() => import("./pages/portal-settings.tsx"), "PortalSettingsPage"),
});

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
const accountingSettings = createRoute({
  getParentRoute: () => app,
  path: "/settings/accounting",
  // Xero and QuickBooks send the person back here with these.
  validateSearch: (
    s: Record<string, unknown>,
  ): { code?: string; state?: string; realmId?: string; error?: string; error_description?: string } => ({
    code: str(s.code),
    state: str(s.state),
    realmId: str(s.realmId),
    error: str(s.error),
    error_description: str(s.error_description),
  }),
  component: lazyRouteComponent(() => import("./pages/accounting-settings.tsx"), "AccountingSettingsPage"),
});
const textsSettings = createRoute({
  getParentRoute: () => app,
  path: "/settings/texts",
  component: lazyRouteComponent(() => import("./pages/texts-settings.tsx"), "TextsSettingsPage"),
});
const followUpsSettings = createRoute({
  getParentRoute: () => app,
  path: "/settings/follow-ups",
  component: lazyRouteComponent(() => import("./pages/follow-ups-settings.tsx"), "FollowUpsSettingsPage"),
});
const pitchingSettings = createRoute({
  getParentRoute: () => app,
  path: "/settings/pitching",
  component: lazyRouteComponent(() => import("./pages/pitching.tsx"), "PitchingSettingsPage"),
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
  operator,
  publicQuote,
  publicInvoice,
  publicSigning,
  connectCalendar,
  connectCalendarReturn,
  app.addChildren([
    home,
    inbox,
    emailSettings,
    calendar,
    enquiries,
    enquiry,
    bookings,
    booking,
    projects,
    project,
    projectDay,
    tasks,
    list,
    record,
    imports,
    fields,
    pipelines,
    calendarSettings,
    accountingSettings,
    textsSettings,
    followUpsSettings,
    pitchingSettings,
    sendingDomain,
    brandingSettings,
    setupSettings,
    streamline,
    themeStudio,
    sync,
    portalDates,
    portalDate,
    portalAvailability,
    portalDocuments,
    portalMoney,
    portalUnlock,
    portalMessages,
    portalProfile,
    portalSettings,
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
