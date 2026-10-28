// Money: invoices and credit notes from draft to paid, payments and
// refunds, a booking's invoices and settlements, the finance report, the
// finance settings and tax codes, and the page a payer opens from the
// invoice email. Lists and invoices are kept on the device as last seen, so
// they can be read offline; changes need a connection (numbers are given
// out by the server).

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import type { Schemas } from "@onhold/api-client";
import {
  Alert,
  Badge,
  Button,
  Card,
  Dialog,
  type FieldIssue,
  Input,
  Label,
  MoneyInput,
  NativeSelect,
  Spinner,
  Textarea,
  applyBrandTheme,
  readBranding,
  cn,
  formatMoney,
} from "@onhold/ui";
import { Ban, CreditCard, Download, FileMinus2, FileText, HandCoins, Plus, Receipt, Send, Stamp, Trash2, X } from "lucide-react";
import { useEffect, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { fetchSaved, useOffline, useSyncStatus } from "../data.ts";
import { isLocalId } from "../offline/store.ts";
import type { Booking, Named } from "../work.ts";
import { Loading, OfflineNote, PageError, failureIssues, useWorkspaceCurrency } from "./crm.tsx";
import { RecordPicker } from "./pickers.tsx";

export type Invoice = Schemas["InvoiceView"];
type InvoiceStatus = Schemas["InvoiceStatus"];
type Payment = Schemas["PaymentView"];
type Settlement = Schemas["SettlementView"];
type Money = { minor: number; currency: string };

const STATUS_LABEL: Record<InvoiceStatus, string> = {
  draft: "Draft",
  issued: "Issued",
  part_paid: "Part paid",
  paid: "Paid",
  overdue: "Overdue",
  credited: "Credited",
  void: "Void",
};

const STATUS_STYLE: Partial<Record<InvoiceStatus, string>> = {
  issued: "border-sky-500 text-sky-800 dark:text-sky-300",
  part_paid: "border-amber-500 text-amber-800 dark:text-amber-300",
  paid: "border-emerald-700 bg-emerald-700 text-white",
  overdue: "border-destructive bg-destructive text-destructive-foreground",
  void: "text-muted-foreground line-through",
};

export function InvoiceBadge({ status }: { status: InvoiceStatus }) {
  return <Badge className={cn("border", STATUS_STYLE[status])}>{STATUS_LABEL[status]}</Badge>;
}

const PURPOSE_LABEL: Record<string, string> = {
  deposit: "Deposit",
  balance: "Balance",
  full: "Full fee",
  commission: "Commission",
  other: "Invoice",
};

function failure(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function day(iso: string | null | undefined): string {
  return iso ? new Date(`${iso}T12:00:00`).toLocaleDateString(undefined, { dateStyle: "medium" }) : "";
}

function invoiceName(i: Invoice): string {
  const kind = i.kind === "credit_note" ? "Credit note" : (PURPOSE_LABEL[i.purpose] ?? "Invoice");
  return i.reference ? `${kind} ${i.reference}` : `${kind} (draft)`;
}

const EXTENSIONS: Record<string, string> = {
  "application/pdf": "pdf",
  "application/zip": "zip",
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
  "text/csv": "csv",
};

/** A fallback name's extension, made to match what was actually sent. */
function withExtension(name: string, type: string): string {
  const ext = EXTENSIONS[type.split(";")[0]?.trim() ?? ""];
  if (!ext) return name.replace(/\.pdf$/i, "");
  return name.replace(/\.[a-z0-9]+$/i, "") + "." + ext;
}

/** Saves what a download call returns as a file (works with cookies and app tokens alike). */
export async function saveDownload(call: Promise<{ data?: unknown; response: Response }>, fallbackName: string) {
  const { data, response } = await call;
  if (!response.ok) throw new ApiFailure(response.status, "download_failed", `${response.status} ${response.statusText}`, []);
  const disposition = response.headers.get("content-disposition") ?? "";
  const name = /filename\*=UTF-8''([^;]+)/i.exec(disposition)?.[1];
  const blob = data as Blob;
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name ? decodeURIComponent(name) : withExtension(fallbackName, blob.type);
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

function useOnline(): boolean {
  const status = useSyncStatus();
  return status?.online !== false;
}

// Lines ----------------------------------------------------------------------------------------

interface LineDraft {
  /** Keeps each line's inputs with it when another is removed. */
  key: string;
  description: string;
  quantity: string;
  unit_minor: number | null;
  tax_code_id: string;
}

let lineCount = 0;
const newLine = (): LineDraft => ({ key: `line-${++lineCount}`, description: "", quantity: "1", unit_minor: null, tax_code_id: "" });

function useTaxCodes() {
  const sync = useOffline();
  return useQuery({
    queryKey: ["tax-codes"],
    queryFn: async () => (await fetchSaved(sync, "tax-codes", () => ok(api.GET("/api/v1/tax-codes")))).data,
  });
}

function LinesEditor({
  lines,
  setLines,
  currency,
  issue,
}: {
  lines: LineDraft[];
  setLines: (f: (l: LineDraft[]) => LineDraft[]) => void;
  currency: string;
  issue: string;
}) {
  const codes = useTaxCodes();
  const set = (i: number, patch: Partial<LineDraft>) => setLines((ls) => ls.map((l, j) => (j === i ? { ...l, ...patch } : l)));
  const net = lines.reduce((s, l) => s + Math.round((Number(l.quantity) || 0) * (l.unit_minor ?? 0)), 0);
  return (
    <fieldset className="flex flex-col gap-2">
      <legend className="mb-1 text-sm font-medium">Lines (prices before tax)</legend>
      {lines.map((l, i) => (
        <div key={l.key} className="grid grid-cols-[1fr_4rem_7.5rem_8rem_auto] items-start gap-2">
          <Input aria-label={`Line ${i + 1} description`} value={l.description} onChange={(e) => set(i, { description: e.target.value })} />
          <Input aria-label={`Line ${i + 1} quantity`} inputMode="decimal" value={l.quantity} onChange={(e) => set(i, { quantity: e.target.value })} />
          <MoneyInput
            // Typed in another currency's units, a price must be read again.
            key={currency}
            aria-label={`Line ${i + 1} price`}
            value={l.unit_minor === null ? null : { minor: l.unit_minor, currency }}
            currency={currency}
            defaultCurrency={currency}
            onChange={(m) => set(i, { unit_minor: m?.minor ?? null })}
          />
          <NativeSelect aria-label={`Line ${i + 1} tax`} value={l.tax_code_id} onChange={(e) => set(i, { tax_code_id: e.target.value })}>
            <option value="">Default tax</option>
            {codes.data?.filter((c) => !c.archived).map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </NativeSelect>
          <Button variant="ghost" size="icon" aria-label={`Remove line ${i + 1}`} onClick={() => setLines((ls) => ls.filter((_, j) => j !== i))}>
            <X />
          </Button>
        </div>
      ))}
      {issue && <p className="text-sm text-destructive">{issue}</p>}
      <div className="flex items-center justify-between">
        <Button
          variant="outline"
          size="sm"
          onClick={() => setLines((ls) => [...ls, newLine()])}
        >
          <Plus /> Add a line
        </Button>
        <span className="text-sm">Net {formatMoney({ minor: net, currency })}</span>
      </div>
    </fieldset>
  );
}

const linesBody = (lines: LineDraft[]) =>
  lines.map((l) => ({
    description: l.description,
    quantity: Number(l.quantity) || 0,
    unit_minor: l.unit_minor ?? 0,
    tax_code_id: l.tax_code_id || null,
  }));

// The invoice list ---------------------------------------------------------------------------

export function InvoicesPage() {
  const sync = useOffline();
  const online = useOnline();
  const navigate = useNavigate();
  const [status, setStatus] = useState("");
  const [q, setQ] = useState("");
  const [creating, setCreating] = useState(false);
  const list = useQuery({
    queryKey: ["invoices", status, q],
    queryFn: () =>
      fetchSaved(sync, `invoices:${status}:${q}`, () =>
        ok(api.GET("/api/v1/invoices", { params: { query: { status: status || undefined, q: q || undefined, limit: 200 } } })),
      ),
  });
  return (
    <div className="flex flex-col gap-4 p-4 md:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="flex-1 text-xl font-semibold">Invoices</h1>
        <Button variant="outline" asChild>
          <Link to="/finance/payments">Payments</Link>
        </Button>
        <Button variant="outline" asChild>
          <Link to="/finance/reports">Reports</Link>
        </Button>
        <Button disabled={!online} onClick={() => setCreating(true)}>
          <Plus /> New invoice
        </Button>
      </div>
      <div className="flex flex-wrap gap-2">
        <Input className="max-w-xs" placeholder="Search by number or payer…" value={q} onChange={(e) => setQ(e.target.value)} aria-label="Search invoices" />
        <NativeSelect value={status} onChange={(e) => setStatus(e.target.value)} aria-label="Status" className="w-40">
          <option value="">Any status</option>
          {Object.entries(STATUS_LABEL).map(([k, v]) => (
            <option key={k} value={k}>
              {v}
            </option>
          ))}
        </NativeSelect>
      </div>
      {list.data?.offline && <OfflineNote>You are offline. These are the invoices as last seen on this device.</OfflineNote>}
      {list.isPending ? (
        <Loading />
      ) : list.isError ? (
        <PageError error={list.error} />
      ) : list.data.data.items.length === 0 ? (
        <p className="text-sm text-muted-foreground">No invoices yet. Make one from a booking, or start one here.</p>
      ) : (
        <InvoiceTable invoices={list.data.data.items} />
      )}
      {creating && (
        <NewInvoiceDialog
          onClose={() => setCreating(false)}
          onCreated={(i) => void navigate({ to: "/finance/invoices/$id", params: { id: i.id } })}
        />
      )}
    </div>
  );
}

function InvoiceTable({ invoices }: { invoices: Invoice[] }) {
  return (
    <div className="overflow-x-auto rounded-md border">
      <table className="w-full text-sm">
        <thead className="bg-muted/50 text-left text-muted-foreground">
          <tr>
            <th className="px-3 py-2 font-normal">Number</th>
            <th className="hidden px-3 py-2 font-normal sm:table-cell">Payer</th>
            <th className="px-3 py-2 font-normal">Status</th>
            <th className="hidden px-3 py-2 font-normal sm:table-cell">Due</th>
            <th className="hidden px-3 py-2 text-right font-normal sm:table-cell">Total</th>
            <th className="px-3 py-2 text-right font-normal">Owed</th>
          </tr>
        </thead>
        <tbody>
          {invoices.map((i) => (
            <tr key={i.id} className="border-t">
              <td className="px-3 py-2">
                <Link to="/finance/invoices/$id" params={{ id: i.id }} className="font-medium underline-offset-2 hover:underline">
                  {invoiceName(i)}
                </Link>
                {i.historical && <span className="ml-2 text-xs text-muted-foreground">past</span>}
                {i.booking_title && <div className="text-xs text-muted-foreground">{i.booking_title}</div>}
                {/* On a phone the payer, due date and total go under the number. */}
                <div className="text-xs text-muted-foreground sm:hidden">
                  {[i.payer_name, i.due_date && `due ${day(i.due_date)}`, formatMoney(i.total)].filter(Boolean).join(" · ")}
                </div>
              </td>
              <td className="hidden px-3 py-2 sm:table-cell">{i.payer_name}</td>
              <td className="px-3 py-2">
                <InvoiceBadge status={i.status} />
              </td>
              <td className="hidden px-3 py-2 sm:table-cell">{day(i.due_date)}</td>
              <td className="hidden px-3 py-2 text-right sm:table-cell">{formatMoney(i.total)}</td>
              <td className="px-3 py-2 text-right whitespace-nowrap">{i.status === "draft" || i.status === "void" ? "" : formatMoney(i.outstanding)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function NewInvoiceDialog({ onClose, onCreated }: { onClose: () => void; onCreated: (i: Invoice) => void }) {
  const workspaceCurrency = useWorkspaceCurrency();
  const [payer, setPayer] = useState<Named | null>(null);
  const [payerName, setPayerName] = useState("");
  const [currency, setCurrency] = useState("");
  const [lines, setLines] = useState<LineDraft[]>([newLine()]);
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [error, setError] = useState<string | null>(null);
  const code = (currency || workspaceCurrency).toUpperCase();
  const issue = (p: string) => issues.filter((x) => x.field?.startsWith(p)).map((x) => x.message).join("; ");
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="New invoice" className="max-w-3xl">
      <form
        className="flex flex-col gap-4"
        onSubmit={async (e) => {
          e.preventDefault();
          setIssues([]);
          setError(null);
          try {
            const i = await ok(
              api.POST("/api/v1/invoices", {
                body: {
                  payer: payer ? { organisation_id: payer.id } : { name: payerName },
                  currency: code,
                  line_items: linesBody(lines),
                },
              }),
            );
            onCreated(i);
          } catch (err) {
            const found = failureIssues(err);
            if (found) setIssues(found);
            else setError(failure(err));
          }
        }}
      >
        <div className="grid gap-3 sm:grid-cols-[1fr_8rem]">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="invoice-payer">Bill to (organisation)</Label>
            <RecordPicker id="invoice-payer" entity="organisation" value={payer} onChange={setPayer} />
            {!payer && (
              <Input aria-label="Or a name" placeholder="…or type a name" value={payerName} onChange={(e) => setPayerName(e.target.value)} />
            )}
            {issue("payer") && <p className="text-sm text-destructive">{issue("payer")}</p>}
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="invoice-currency">Currency</Label>
            <Input id="invoice-currency" maxLength={3} placeholder={workspaceCurrency} value={currency} onChange={(e) => setCurrency(e.target.value)} />
          </div>
        </div>
        <LinesEditor lines={lines} setLines={setLines} currency={code} issue={issue("line_items")} />
        <p className="text-xs text-muted-foreground">It is saved as a draft. It gets its number when you issue it.</p>
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit">Save draft</Button>
        </div>
      </form>
    </Dialog>
  );
}

// One invoice ----------------------------------------------------------------------------------

export function InvoicePage() {
  const { id } = useParams({ from: "/app/finance/invoices/$id" });
  const sync = useOffline();
  const online = useOnline();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const invoice = useQuery({
    queryKey: ["invoice", id],
    queryFn: () => fetchSaved(sync, `invoice:${id}`, () => ok(api.GET("/api/v1/invoices/{id}", { params: { path: { id } } }))),
  });
  const allocations = useQuery({
    queryKey: ["invoice-allocations", id],
    queryFn: () =>
      fetchSaved(sync, `invoice-allocations:${id}`, () =>
        ok(api.GET("/api/v1/invoices/{id}/allocations", { params: { path: { id } } })),
      ),
  });
  const [dialog, setDialog] = useState<"issue" | "send" | "pay" | "credit" | "void" | null>(null);
  const [link, setLink] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  if (invoice.isPending) return <Loading />;
  if (invoice.isError) return <PageError error={invoice.error} />;
  const i = invoice.data.data;
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["invoice", id] });
    void qc.invalidateQueries({ queryKey: ["invoice-allocations", id] });
    void qc.invalidateQueries({ queryKey: ["invoices"] });
  };
  const canChange = online && !invoice.data.offline;
  const open = ["issued", "part_paid", "overdue"].includes(i.status);
  const ifMatch = { "If-Match": `"${i.version}"` };
  return (
    <div className="flex flex-col gap-4 p-4 md:p-6">
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="text-xl font-semibold">{invoiceName(i)}</h1>
        <InvoiceBadge status={i.status} />
        {i.historical && <Badge className="border">Past invoice</Badge>}
      </div>
      {invoice.data.offline && <OfflineNote>You are offline. This is the invoice as last seen on this device.</OfflineNote>}
      <p className="text-sm text-muted-foreground">
        {[
          `To ${i.payer_name}`,
          i.booking_title && (
            <Link key="b" to="/bookings/$id" params={{ id: i.booking_id! }} className="underline">
              {i.booking_title}
            </Link>
          ),
          i.issue_date && `issued ${day(i.issue_date)}`,
          i.due_date && i.kind === "invoice" && `due ${day(i.due_date)}`,
          i.sent_at && `sent to ${i.sent_to} ${new Date(i.sent_at).toLocaleDateString()}`,
          i.viewed_at && `opened ${new Date(i.viewed_at).toLocaleDateString()}`,
        ]
          .filter(Boolean)
          .flatMap((x, n) => (n ? [" · ", x] : [x]))}
      </p>
      {error && <Alert>{error}</Alert>}
      {link && <Alert tone="success">Sent. The payer&apos;s link: {link}</Alert>}
      <div className={cn("flex flex-wrap gap-2", !canChange && "hidden")}>
        {i.status === "draft" && (
          <>
            <Button onClick={() => setDialog("issue")}>
              <Stamp /> Issue
            </Button>
            <Button
              variant="ghost"
              onClick={async () => {
                if (!window.confirm("Delete this draft?")) return;
                try {
                  await ok(api.DELETE("/api/v1/invoices/{id}", { params: { path: { id } } }));
                  void navigate({ to: "/finance/invoices" });
                } catch (e) {
                  setError(failure(e));
                }
              }}
            >
              <Trash2 /> Delete draft
            </Button>
          </>
        )}
        {i.status !== "draft" && !i.historical && i.status !== "void" && (
          <Button variant="outline" onClick={() => setDialog("send")}>
            <Send /> {i.sent_at ? "Send again" : "Send"}
          </Button>
        )}
        {open && i.kind === "invoice" && (
          <Button variant="outline" onClick={() => setDialog("pay")}>
            <HandCoins /> Record payment
          </Button>
        )}
        {i.kind === "invoice" && i.status !== "draft" && i.status !== "void" && i.status !== "credited" && !i.historical && (
          <Button variant="outline" onClick={() => setDialog("credit")}>
            <FileMinus2 /> Credit note
          </Button>
        )}
        {i.status !== "draft" && i.status !== "void" && i.paid.minor === 0 && i.credited.minor === 0 && (
          <Button variant="ghost" onClick={() => setDialog("void")}>
            <Ban /> Void
          </Button>
        )}
      </div>
      <div className="flex flex-wrap gap-2">
        {i.status !== "draft" && (
          <Button
            variant="outline"
            onClick={() =>
              void saveDownload(
                api.GET("/api/v1/invoices/{id}/pdf", { params: { path: { id } }, parseAs: "blob" }),
                `${i.reference ?? "invoice"}.pdf`,
              ).catch((e: unknown) => setError(failure(e)))
            }
          >
            <Download /> PDF
          </Button>
        )}
        {i.credits_invoice_id && (
          <Button variant="ghost" asChild>
            <Link to="/finance/invoices/$id" params={{ id: i.credits_invoice_id }}>
              The invoice it credits
            </Link>
          </Button>
        )}
      </div>
      <InvoiceBody invoice={i} />
      {i.base_total && i.base_currency && i.base_currency !== i.currency && (
        <p className="text-sm text-muted-foreground">
          {formatMoney(i.base_total)} at {i.fx_rate} {i.base_currency} per {i.currency}
          {i.fx_source && ` (${i.fx_source === "ecb" ? "European Central Bank" : i.fx_source} rate of ${day(i.fx_date)})`}
        </p>
      )}
      {(allocations.data?.data.length ?? 0) > 0 && (
        <section className="flex flex-col gap-2" aria-label="Payments">
          <h2 className="font-semibold">Payments</h2>
          <ul className="flex flex-col gap-1 text-sm">
            {allocations.data?.data.map((a) => (
              <li key={a.id} className="flex justify-between gap-2 border-b py-1">
                <span>
                  {day(a.paid_on)} · {a.method?.replace("_", " ")}
                </span>
                <span>{formatMoney({ minor: a.amount_minor, currency: i.currency })}</span>
              </li>
            ))}
          </ul>
        </section>
      )}
      {i.void_reason && <p className="text-sm">Voided: {i.void_reason}</p>}
      {dialog === "issue" && <IssueDialog invoice={i} onClose={() => setDialog(null)} onDone={refresh} />}
      {dialog === "send" && (
        <SimpleDialog
          title={`Send ${invoiceName(i)}`}
          label="Send to"
          placeholder="Leave blank for the payer contact's email"
          action="Send"
          onClose={() => setDialog(null)}
          run={async (to) => {
            const sent = await ok(
              api.POST("/api/v1/invoices/{id}/send", { params: { path: { id } }, headers: ifMatch, body: { to: to || null } }),
            );
            setLink(sent.link);
            refresh();
          }}
        />
      )}
      {dialog === "pay" && <PaymentDialog invoices={[i]} onClose={() => setDialog(null)} onDone={refresh} />}
      {dialog === "credit" && (
        <CreditDialog
          invoice={i}
          onClose={() => setDialog(null)}
          onDone={(c) => void navigate({ to: "/finance/invoices/$id", params: { id: c.id } })}
        />
      )}
      {dialog === "void" && (
        <SimpleDialog
          title={`Void ${invoiceName(i)}`}
          label="Why"
          placeholder="e.g. issued twice by mistake"
          action="Void it"
          note="A voided invoice keeps its number and stays in the records, marked void."
          onClose={() => setDialog(null)}
          run={async (reason) => {
            await ok(api.POST("/api/v1/invoices/{id}/void", { params: { path: { id } }, headers: ifMatch, body: { reason: reason || null } }));
            refresh();
          }}
        />
      )}
    </div>
  );
}

function InvoiceBody({ invoice: i }: { invoice: Invoice }) {
  return (
    <Card className="flex flex-col gap-3 p-4">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b text-left text-muted-foreground">
            <th className="py-1 font-normal">Item</th>
            <th className="py-1 font-normal">Tax</th>
            <th className="py-1 text-right font-normal">Amount</th>
          </tr>
        </thead>
        <tbody>
          {i.line_items.map((l, n) => (
            <tr key={n} className="border-b">
              <td className="py-2">
                {l.description}
                {l.quantity !== 1 && (
                  <span className="text-muted-foreground">
                    {" "}
                    ({l.quantity} × {formatMoney({ minor: l.unit_minor, currency: i.currency })})
                  </span>
                )}
              </td>
              <td className="py-2 text-muted-foreground">{l.tax_name ?? ""}</td>
              <td className="py-2 text-right">{formatMoney({ minor: l.amount_minor, currency: i.currency })}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <dl className="ml-auto grid grid-cols-[auto_auto] gap-x-6 gap-y-1 text-sm">
        <dt className="text-muted-foreground">Subtotal</dt>
        <dd className="text-right">{formatMoney(i.subtotal)}</dd>
        {i.tax_lines.map((t) => (
          <Frag key={t.name}>
            <dt className="text-muted-foreground">{t.name}</dt>
            <dd className="text-right">{formatMoney({ minor: t.tax_minor, currency: i.currency })}</dd>
          </Frag>
        ))}
        <dt className="font-medium">Total</dt>
        <dd className="text-right font-semibold">{formatMoney(i.total)}</dd>
        {i.paid.minor > 0 && (
          <>
            <dt className="text-muted-foreground">Paid</dt>
            <dd className="text-right">{formatMoney(i.paid)}</dd>
          </>
        )}
        {i.credited.minor > 0 && (
          <>
            <dt className="text-muted-foreground">Credited</dt>
            <dd className="text-right">{formatMoney(i.credited)}</dd>
          </>
        )}
        {i.status !== "draft" && i.status !== "void" && i.kind === "invoice" && (
          <>
            <dt className="font-medium">Still owed</dt>
            <dd className="text-right font-medium">{formatMoney(i.outstanding)}</dd>
          </>
        )}
      </dl>
      {i.notes && <p className="whitespace-pre-line text-sm">{i.notes}</p>}
    </Card>
  );
}

function Frag({ children }: { children: React.ReactNode }) {
  return <>{children}</>;
}

function IssueDialog({ invoice: i, onClose, onDone }: { invoice: Invoice; onClose: () => void; onDone: () => void }) {
  const [issueDate, setIssueDate] = useState("");
  const [dueDate, setDueDate] = useState("");
  const [rate, setRate] = useState("");
  const [needsRate, setNeedsRate] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={`Issue ${invoiceName(i)}`}>
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          setBusy(true);
          try {
            await ok(
              api.POST("/api/v1/invoices/{id}/issue", {
                params: { path: { id: i.id } },
                headers: { "If-Match": `"${i.version}"` },
                body: { issue_date: issueDate || null, due_date: dueDate || null, fx_rate: rate || null },
              }),
            );
            onDone();
            onClose();
          } catch (err) {
            if (err instanceof ApiFailure && err.code === "no_rate") setNeedsRate(true);
            setError(failure(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <p className="text-sm">
          Issuing gives it the next number and makes its PDF. After that it cannot be changed: mistakes are put right
          with a credit note.
        </p>
        <Label htmlFor="issue-date">Issue date</Label>
        <Input id="issue-date" type="date" value={issueDate} onChange={(e) => setIssueDate(e.target.value)} />
        <Label htmlFor="due-date">Due date</Label>
        <Input id="due-date" type="date" value={dueDate} onChange={(e) => setDueDate(e.target.value)} />
        <p className="text-xs text-muted-foreground">Leave blank for today and the usual payment terms.</p>
        {(needsRate || rate) && (
          <>
            <Label htmlFor="fx-rate">Exchange rate (your base currency per {i.currency})</Label>
            <Input id="fx-rate" inputMode="decimal" value={rate} onChange={(e) => setRate(e.target.value)} />
          </>
        )}
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy}>
            <Stamp /> Issue
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

function SimpleDialog({
  title,
  label,
  placeholder,
  action,
  note,
  onClose,
  run,
}: {
  title: string;
  label: string;
  placeholder?: string;
  action: string;
  note?: string;
  onClose: () => void;
  run: (value: string) => Promise<void>;
}) {
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={title}>
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          setBusy(true);
          try {
            await run(value.trim());
            onClose();
          } catch (err) {
            setError(failure(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        {note && <p className="text-sm">{note}</p>}
        <Label htmlFor="simple-value">{label}</Label>
        <Input id="simple-value" placeholder={placeholder} value={value} onChange={(e) => setValue(e.target.value)} />
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy}>
            {action}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

const METHODS = ["bank_transfer", "card", "cash", "cheque", "stripe", "other"];

export function PaymentDialog({ invoices, onClose, onDone }: { invoices: Invoice[]; onClose: () => void; onDone: () => void }) {
  const first = invoices[0]!;
  const owed = invoices.reduce((s, i) => s + i.outstanding.minor, 0);
  const [amount, setAmount] = useState<Money | null>({ minor: owed, currency: first.currency });
  const [method, setMethod] = useState("bank_transfer");
  const [paidOn, setPaidOn] = useState("");
  const [reference, setReference] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="Record a payment">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          if (!amount) return;
          setError(null);
          setBusy(true);
          try {
            await ok(
              api.POST("/api/v1/payments", {
                body: {
                  amount: { minor: amount.minor, currency: first.currency },
                  method,
                  paid_on: paidOn || null,
                  reference: reference || null,
                  allocations: invoices.map((i) => ({ invoice_id: i.id })),
                },
              }),
            );
            onDone();
            onClose();
          } catch (err) {
            setError(failure(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <p className="text-sm">
          Towards {invoices.map(invoiceName).join(", ")}: {formatMoney({ minor: owed, currency: first.currency })} owed. Any
          extra is kept on the payment to use later or refund.
        </p>
        <Label htmlFor="pay-amount">Amount received</Label>
        <MoneyInput id="pay-amount" value={amount} currency={first.currency} defaultCurrency={first.currency} onChange={setAmount} />
        <Label htmlFor="pay-method">How</Label>
        <NativeSelect id="pay-method" value={method} onChange={(e) => setMethod(e.target.value)}>
          {METHODS.map((m) => (
            <option key={m} value={m}>
              {m.replace("_", " ")}
            </option>
          ))}
        </NativeSelect>
        <Label htmlFor="pay-date">Date received</Label>
        <Input id="pay-date" type="date" value={paidOn} onChange={(e) => setPaidOn(e.target.value)} />
        <Label htmlFor="pay-ref">Reference</Label>
        <Input id="pay-ref" placeholder="e.g. the bank statement line" value={reference} onChange={(e) => setReference(e.target.value)} />
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy || !amount}>
            <HandCoins /> Record
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

function CreditDialog({ invoice: i, onClose, onDone }: { invoice: Invoice; onClose: () => void; onDone: (c: Invoice) => void }) {
  const [full, setFull] = useState(true);
  const [lines, setLines] = useState<LineDraft[]>([newLine()]);
  const [notes, setNotes] = useState("");
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [error, setError] = useState<string | null>(null);
  const issue = (p: string) => issues.filter((x) => x.field?.startsWith(p)).map((x) => x.message).join("; ");
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={`Credit note for ${invoiceName(i)}`} className="max-w-3xl">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setIssues([]);
          setError(null);
          try {
            const c = await ok(
              api.POST("/api/v1/invoices/{id}/credit-notes", {
                params: { path: { id: i.id } },
                body: { line_items: full ? null : linesBody(lines), notes: notes || null },
              }),
            );
            onDone(c);
          } catch (err) {
            const found = failureIssues(err);
            if (found) setIssues(found);
            else setError(failure(err));
          }
        }}
      >
        <label className="flex items-center gap-2 text-sm">
          <input type="radio" checked={full} onChange={() => setFull(true)} /> The whole invoice
        </label>
        <label className="flex items-center gap-2 text-sm">
          <input type="radio" checked={!full} onChange={() => setFull(false)} /> Part of it
        </label>
        {!full && <LinesEditor lines={lines} setLines={setLines} currency={i.currency} issue={issue("line_items")} />}
        <Label htmlFor="credit-notes">Note on the credit note</Label>
        <Textarea id="credit-notes" value={notes} onChange={(e) => setNotes(e.target.value)} />
        <p className="text-xs text-muted-foreground">It is numbered and issued straight away.</p>
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit">
            <FileMinus2 /> Issue credit note
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

// Payments -------------------------------------------------------------------------------------

export function PaymentsPage() {
  const sync = useOffline();
  const online = useOnline();
  const qc = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const list = useQuery({
    queryKey: ["payments"],
    queryFn: () => fetchSaved(sync, "payments", () => ok(api.GET("/api/v1/payments", { params: { query: { limit: 200 } } }))),
  });
  const refresh = () => void qc.invalidateQueries({ queryKey: ["payments"] });
  return (
    <div className="flex flex-col gap-4 p-4 md:p-6">
      <div className="flex items-center gap-2">
        <h1 className="flex-1 text-xl font-semibold">Payments</h1>
        <Button variant="outline" asChild>
          <Link to="/finance/invoices">Invoices</Link>
        </Button>
      </div>
      {list.data?.offline && <OfflineNote>You are offline. These are the payments as last seen on this device.</OfflineNote>}
      {error && <Alert>{error}</Alert>}
      {list.isPending ? (
        <Loading />
      ) : list.isError ? (
        <PageError error={list.error} />
      ) : list.data.data.items.length === 0 ? (
        <p className="text-sm text-muted-foreground">No payments yet. Record one from an invoice.</p>
      ) : (
        <ul className="flex flex-col divide-y rounded-md border text-sm">
          {list.data.data.items.map((p: Payment) => (
            <li key={p.id} className="flex flex-wrap items-center gap-3 px-3 py-2">
              <span className="w-28">{day(p.paid_on)}</span>
              <span className="flex-1">
                {p.kind === "refund" ? "Refund" : p.kind === "payout" ? "Paid out" : "Received"}
                {p.organisation_name && ` · ${p.organisation_name}`}
                {p.talent_name && ` · ${p.talent_name}`}
                {p.reference && ` · ${p.reference}`}
                {p.voided_at && <span className="text-destructive"> · void</span>}
                {p.unallocated.minor > 0 && !p.voided_at && p.kind === "received" && (
                  <span className="text-muted-foreground"> · {formatMoney(p.unallocated)} not yet allocated</span>
                )}
              </span>
              <span className={cn("font-medium", p.voided_at && "line-through")}>{formatMoney(p.amount)}</span>
              {online && !p.voided_at && (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={async () => {
                    const reason = window.prompt("Why void this payment? (e.g. it bounced)");
                    if (reason === null) return;
                    try {
                      await ok(
                        api.POST("/api/v1/payments/{id}/void", {
                          params: { path: { id: p.id } },
                          headers: { "If-Match": `"${p.version}"` },
                          body: { reason: reason || null },
                        }),
                      );
                      refresh();
                    } catch (e) {
                      setError(failure(e));
                    }
                  }}
                >
                  Void
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// Reports --------------------------------------------------------------------------------------

const CSV_TABLES: { table: string; label: string }[] = [
  { table: "invoices", label: "Invoices" },
  { table: "payments", label: "Payments" },
  { table: "revenue_by_month", label: "Revenue by month" },
  { table: "revenue_by_payer", label: "Revenue by payer" },
  { table: "revenue_by_talent", label: "Revenue by talent" },
  { table: "outstanding", label: "Owed" },
];

export function ReportsPage() {
  const sync = useOffline();
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [error, setError] = useState<string | null>(null);
  const report = useQuery({
    queryKey: ["finance-report", from, to],
    queryFn: () =>
      fetchSaved(sync, `finance-report:${from}:${to}`, () =>
        ok(api.GET("/api/v1/reports/finance", { params: { query: { from: from || undefined, to: to || undefined } } })),
      ),
  });
  const r = report.data?.data;
  return (
    <div className="flex flex-col gap-6 p-4 md:p-6">
      <div className="flex flex-wrap items-end gap-3">
        <h1 className="flex-1 text-xl font-semibold">Finance report</h1>
        <div className="flex flex-col gap-1">
          <Label htmlFor="report-from">From</Label>
          <Input id="report-from" type="date" value={from} onChange={(e) => setFrom(e.target.value)} />
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="report-to">To</Label>
          <Input id="report-to" type="date" value={to} onChange={(e) => setTo(e.target.value)} />
        </div>
      </div>
      {report.data?.offline && <OfflineNote>You are offline. This is the report as last seen on this device.</OfflineNote>}
      {error && <Alert>{error}</Alert>}
      {report.isPending ? (
        <Loading />
      ) : report.isError ? (
        <PageError error={report.error} />
      ) : r ? (
        <>
          <p className="text-sm text-muted-foreground">
            {day(r.from)} to {day(r.to)}, in {r.base_currency}. Credit notes are taken off.
          </p>
          <section className="grid gap-4 md:grid-cols-4" aria-label="Owed">
            {(
              [
                ["Not yet due", r.ageing.current],
                ["1–30 days late", r.ageing.days_1_30],
                ["31–60 days late", r.ageing.days_31_60],
                ["Over 60 days late", { minor: r.ageing.days_61_90.minor + r.ageing.over_90.minor, currency: r.base_currency }],
              ] as [string, Money][]
            ).map(([label, m]) => (
              <Card key={label} className="p-4">
                <p className="text-xs text-muted-foreground">{label}</p>
                <p className="text-lg font-semibold">{formatMoney(m)}</p>
              </Card>
            ))}
          </section>
          <div className="grid gap-6 lg:grid-cols-3">
            <RevenueTable title="By month" rows={r.by_month} />
            <RevenueTable title="By payer" rows={r.by_payer} />
            <RevenueTable title="By talent (net of tax)" rows={r.by_talent} />
          </div>
          <section className="flex flex-col gap-2" aria-label="Cash">
            <h2 className="font-semibold">Cash</h2>
            <ul className="text-sm">
              {r.cash.map((c) => (
                <li key={c.currency}>
                  {c.currency}: received {formatMoney({ minor: c.received_minor, currency: c.currency })}, refunded{" "}
                  {formatMoney({ minor: c.refunded_minor, currency: c.currency })}, paid out{" "}
                  {formatMoney({ minor: c.paid_out_minor, currency: c.currency })}
                </li>
              ))}
            </ul>
          </section>
          <section className="flex flex-col gap-2" aria-label="Enquiries">
            <h2 className="font-semibold">Enquiries</h2>
            <p className="text-sm">
              {r.funnel.enquiries} enquiries, {r.funnel.quoted} quoted, {r.funnel.won} won, {r.funnel.lost} lost, {r.funnel.open} still open
              {r.funnel.win_rate !== null && r.funnel.win_rate !== undefined && ` · ${Math.round(r.funnel.win_rate * 100)}% won of those decided`}
            </p>
          </section>
          <section className="flex flex-col gap-2" aria-label="Downloads">
            <h2 className="font-semibold">Download as CSV</h2>
            <div className="flex flex-wrap gap-2">
              {CSV_TABLES.map((t) => (
                <Button
                  key={t.table}
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    void saveDownload(
                      api.GET("/api/v1/reports/{table}/csv", {
                        params: { path: { table: t.table }, query: { from: from || undefined, to: to || undefined } },
                        parseAs: "blob",
                      }),
                      `${t.table}.csv`,
                    ).catch((e: unknown) => setError(failure(e)))
                  }
                >
                  <Download /> {t.label}
                </Button>
              ))}
            </div>
          </section>
        </>
      ) : null}
    </div>
  );
}

function RevenueTable({ title, rows }: { title: string; rows: Schemas["RevenueRow"][] }) {
  return (
    <section className="flex flex-col gap-2" aria-label={title}>
      <h2 className="font-semibold">{title}</h2>
      {rows.length === 0 ? (
        <p className="text-sm text-muted-foreground">Nothing invoiced.</p>
      ) : (
        <table className="w-full text-sm">
          <tbody>
            {rows.map((row) => (
              <tr key={row.key} className="border-b">
                <td className="py-1">{row.key}</td>
                <td className="py-1 text-right">{formatMoney(row.invoiced)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}

// A booking's money ---------------------------------------------------------------------------

/** The invoices and settlements of a booking, with the next sensible step. */
export function BookingFinancePanel({ booking }: { booking: Booking }) {
  const sync = useOffline();
  const online = useOnline();
  const qc = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const [paying, setPaying] = useState<Invoice | null>(null);
  const local = isLocalId(booking.id);
  const invoices = useQuery({
    queryKey: ["booking-invoices", booking.id],
    enabled: !local,
    queryFn: async () =>
      (
        await fetchSaved(sync, `booking-invoices:${booking.id}`, () =>
          ok(api.GET("/api/v1/invoices", { params: { query: { booking_id: booking.id, limit: 50 } } })),
        )
      ).data,
  });
  const settlements = useQuery({
    queryKey: ["booking-settlements", booking.id],
    enabled: !local,
    queryFn: async () =>
      (
        await fetchSaved(sync, `booking-settlements:${booking.id}`, () =>
          ok(api.GET("/api/v1/settlements", { params: { query: { booking_id: booking.id } } })),
        )
      ).data,
  });
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["booking-invoices", booking.id] });
    void qc.invalidateQueries({ queryKey: ["booking-settlements", booking.id] });
  };
  const act = async (run: () => Promise<unknown>) => {
    setError(null);
    try {
      await run();
      refresh();
    } catch (e) {
      setError(failure(e));
    }
  };
  const items = invoices.data?.items ?? [];
  const live = items.filter((i) => i.kind === "invoice" && i.status !== "void");
  const has = (p: string) => live.some((i) => i.purpose === p || i.purpose === "full");
  const make = (purpose: "deposit" | "balance" | "full") =>
    act(() => ok(api.POST("/api/v1/bookings/{id}/invoices", { params: { path: { id: booking.id } }, body: { purpose, issue: false } })));
  const canChange = online && !local;
  return (
    <section className="flex flex-col gap-3" aria-label="Invoices">
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="flex-1 font-semibold">Invoices</h2>
        {canChange && booking.deposit_percent && !has("deposit") && !has("balance") && (
          <Button size="sm" variant="outline" onClick={() => void make("deposit")}>
            <Receipt /> Deposit invoice
          </Button>
        )}
        {canChange && has("deposit") && !has("balance") && (
          <Button size="sm" variant="outline" onClick={() => void make("balance")}>
            <Receipt /> Balance invoice
          </Button>
        )}
        {canChange && live.length === 0 && (
          <Button size="sm" variant="outline" onClick={() => void make("full")}>
            <Receipt /> Invoice in full
          </Button>
        )}
      </div>
      {!online && <p className="text-sm text-muted-foreground">Invoices can be made and sent when you are online.</p>}
      {error && <Alert>{error}</Alert>}
      {invoices.isPending && !local ? (
        <Spinner />
      ) : items.length === 0 ? (
        <p className="text-sm text-muted-foreground">No invoices yet.</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {items.map((i) => (
            <li key={i.id}>
              <Card className="flex flex-wrap items-center gap-2 p-3 text-sm">
                <Link to="/finance/invoices/$id" params={{ id: i.id }} className="font-medium hover:underline">
                  {invoiceName(i)}
                </Link>
                <InvoiceBadge status={i.status} />
                <span className="flex-1 text-right">{formatMoney(i.total)}</span>
                {canChange && ["issued", "part_paid", "overdue"].includes(i.status) && i.kind === "invoice" && (
                  <Button size="sm" variant="ghost" onClick={() => setPaying(i)}>
                    <HandCoins /> Paid
                  </Button>
                )}
              </Card>
            </li>
          ))}
        </ul>
      )}
      <div className="flex flex-wrap items-center gap-2 pt-2">
        <h3 className="flex-1 text-sm font-semibold">Settlements</h3>
        {canChange && booking.talent.length > 0 && (settlements.data?.length ?? 0) < booking.talent.length && (
          <Button
            size="sm"
            variant="outline"
            onClick={() => void act(() => ok(api.POST("/api/v1/bookings/{id}/settlements", { params: { path: { id: booking.id } } })))}
          >
            <FileText /> Work out what talent are owed
          </Button>
        )}
      </div>
      {(settlements.data ?? []).map((s) => (
        <SettlementCard key={s.id} settlement={s} canChange={canChange} onChanged={refresh} />
      ))}
      {paying && <PaymentDialog invoices={[paying]} onClose={() => setPaying(null)} onDone={refresh} />}
    </section>
  );
}

function SettlementCard({ settlement: s, canChange, onChanged }: { settlement: Settlement; canChange: boolean; onChanged: () => void }) {
  const [error, setError] = useState<string | null>(null);
  const run = async (call: Promise<{ data?: unknown; error?: unknown; response: Response }>) => {
    setError(null);
    try {
      await ok(call);
      onChanged();
    } catch (e) {
      setError(failure(e));
    }
  };
  const ifMatch = { "If-Match": `"${s.version}"` };
  const owed = s.net.minor - s.paid_out.minor;
  return (
    <Card className="flex flex-col gap-1 p-3 text-sm">
      <div className="flex items-center gap-2">
        <span className="flex-1 font-medium">{s.talent_name}</span>
        <Badge className="border">{s.status === "final" ? "Final" : "Draft"}</Badge>
      </div>
      <p className="text-muted-foreground">
        {formatMoney(s.gross)} gross, less {s.commission_bps / 100}% commission ({formatMoney(s.commission)})
        {s.expenses_total.minor > 0 && ` and ${formatMoney(s.expenses_total)} expenses`}: {formatMoney(s.net)} to pay
        {s.paid_out.minor > 0 && `, ${formatMoney(s.paid_out)} paid`}.
      </p>
      {error && <Alert>{error}</Alert>}
      {canChange && (
        <div className="flex flex-wrap gap-2">
          {s.status === "draft" ? (
            <Button size="sm" variant="outline" onClick={() => void run(api.POST("/api/v1/settlements/{id}/finalise", { params: { path: { id: s.id } }, headers: ifMatch }))}>
              Finalise and make the statement
            </Button>
          ) : (
            <>
              {owed > 0 && (
                <Button size="sm" variant="outline" onClick={() => void run(api.POST("/api/v1/settlements/{id}/payouts", { params: { path: { id: s.id } }, body: {} }))}>
                  <HandCoins /> Mark {formatMoney({ minor: owed, currency: s.net.currency })} paid out
                </Button>
              )}
              {s.paid_out.minor === 0 && (
                <Button size="sm" variant="ghost" onClick={() => void run(api.POST("/api/v1/settlements/{id}/reopen", { params: { path: { id: s.id } }, headers: ifMatch }))}>
                  Reopen
                </Button>
              )}
            </>
          )}
        </div>
      )}
    </Card>
  );
}

// Settings -------------------------------------------------------------------------------------

export function FinanceSettingsPage() {
  const qc = useQueryClient();
  const settings = useQuery({ queryKey: ["finance-settings"], queryFn: () => ok(api.GET("/api/v1/finance/settings")) });
  const codes = useTaxCodes();
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [newCode, setNewCode] = useState({ name: "", rate: "" });
  if (settings.isPending) return <Loading />;
  if (settings.isError) return <PageError error={settings.error} />;
  const s = settings.data;
  const issue = (f: string) => issues.filter((x) => x.field?.startsWith(f)).map((x) => x.message).join("; ");
  const field = (name: keyof typeof s, label: string, hint?: string) => (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={`fs-${name}`}>{label}</Label>
      <Input id={`fs-${name}`} name={name} defaultValue={(s[name] as string | null) ?? ""} />
      {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
      {issue(name) && <p className="text-sm text-destructive">{issue(name)}</p>}
    </div>
  );
  const bank = s.bank_accounts[0];
  return (
    <div className="flex max-w-3xl flex-col gap-6 p-4 md:p-6">
      <h1 className="text-xl font-semibold">Invoicing</h1>
      <form
        className="flex flex-col gap-4"
        onSubmit={async (e) => {
          e.preventDefault();
          const f = new FormData(e.currentTarget);
          const text = (k: string) => {
            const v = String(f.get(k) ?? "").trim();
            return v || null;
          };
          setIssues([]);
          setError(null);
          setSaved(false);
          try {
            await ok(
              api.PUT("/api/v1/finance/settings", {
                headers: { "If-Match": `"${s.version}"` },
                body: {
                  legal_name: text("legal_name"),
                  tax_number: text("tax_number"),
                  company_number: text("company_number"),
                  email: text("email"),
                  phone: text("phone"),
                  invoice_format: text("invoice_format") ?? undefined,
                  credit_note_format: text("credit_note_format") ?? undefined,
                  payment_terms_days: Number(f.get("payment_terms_days")) || undefined,
                  invoicing_model: text("invoicing_model") ?? undefined,
                  invoice_footer: text("invoice_footer"),
                  late_fee_text: text("late_fee_text"),
                  address: {
                    ...(s.address as object | null),
                    line1: text("line1"),
                    line2: text("line2"),
                    city: text("city"),
                    postcode: text("postcode"),
                    country: text("country"),
                  } as never,
                  // Only the first account is edited here; the others stay.
                  bank_accounts: [
                    ...(text("bank_details")
                      ? [{ currency: text("bank_currency") ?? s.base_currency, label: text("bank_name"), details: text("bank_details") ?? "" }]
                      : []),
                    ...s.bank_accounts.slice(1),
                  ],
                },
              }),
            );
            setSaved(true);
            void qc.invalidateQueries({ queryKey: ["finance-settings"] });
          } catch (err) {
            const found = failureIssues(err);
            if (found) setIssues(found);
            else setError(failure(err));
          }
        }}
      >
        <div className="grid gap-4 sm:grid-cols-2">
          {field("legal_name", "Name on invoices")}
          {field("tax_number", "VAT or tax number")}
          {field("company_number", "Company number")}
          {field("email", "Email for payers")}
          {field("phone", "Phone")}
        </div>
        <fieldset className="grid gap-4 sm:grid-cols-2">
          <legend className="mb-2 text-sm font-medium">Address</legend>
          {(["line1", "line2", "city", "postcode", "country"] as const).map((k) => (
            <div key={k} className="flex flex-col gap-1.5">
              <Label htmlFor={`fs-${k}`}>{{ line1: "Address", line2: "Address line 2", city: "Town or city", postcode: "Postcode", country: "Country code" }[k]}</Label>
              <Input id={`fs-${k}`} name={k} defaultValue={String((s.address as Record<string, unknown>)?.[k] ?? "")} />
            </div>
          ))}
        </fieldset>
        <div className="grid gap-4 sm:grid-cols-3">
          {field("invoice_format", "Invoice numbers", "{YYYY} year, {0000} the counter")}
          {field("credit_note_format", "Credit note numbers")}
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="fs-terms">Days to pay</Label>
            <Input id="fs-terms" name="payment_terms_days" type="number" min={0} defaultValue={s.payment_terms_days} />
          </div>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="fs-model">Who invoices the buyer</Label>
          <NativeSelect id="fs-model" name="invoicing_model" defaultValue={s.invoicing_model}>
            <option value="agency">The agency invoices, then pays the talent their share</option>
            <option value="talent">The talent invoices; the agency invoices them for commission</option>
            <option value="split">The agency invoices the deposit, the talent the balance</option>
          </NativeSelect>
        </div>
        <fieldset className="grid gap-4 sm:grid-cols-[6rem_1fr]">
          <legend className="mb-2 text-sm font-medium">Bank details printed on invoices</legend>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="fs-bank-currency">Currency</Label>
            <Input id="fs-bank-currency" name="bank_currency" defaultValue={bank?.currency ?? s.base_currency} />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="fs-bank-name">Label (e.g. the account name)</Label>
            <Input id="fs-bank-name" name="bank_name" defaultValue={bank?.label ?? ""} />
          </div>
          <div className="flex flex-col gap-1.5 sm:col-span-2">
            <Label htmlFor="fs-bank-details">Sort code and account, or IBAN</Label>
            <Textarea id="fs-bank-details" name="bank_details" defaultValue={bank?.details ?? ""} />
          </div>
        </fieldset>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="fs-footer">Footer</Label>
          <Textarea id="fs-footer" name="invoice_footer" defaultValue={s.invoice_footer ?? ""} />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="fs-late">Late payment wording</Label>
          <Textarea id="fs-late" name="late_fee_text" defaultValue={s.late_fee_text ?? ""} />
        </div>
        {error && <Alert>{error}</Alert>}
        {saved && <Alert tone="success">Saved.</Alert>}
        <div>
          <Button type="submit">Save</Button>
        </div>
      </form>

      <section className="flex flex-col gap-3" aria-label="Tax codes">
        <h2 className="font-semibold">Tax codes</h2>
        <ul className="divide-y rounded-md border text-sm">
          {codes.data?.map((c) => (
            <li key={c.id} className={cn("flex items-center gap-3 px-3 py-2", c.archived && "text-muted-foreground")}>
              <span className="flex-1">
                {c.name} · {c.rate_bps / 100}%{c.is_default && " · default"}
                {c.archived && " · archived"}
              </span>
              {!c.is_default && !c.archived && (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={async () => {
                    await ok(
                      api.PUT("/api/v1/tax-codes/{id}", {
                        params: { path: { id: c.id } },
                        body: { name: c.name, rate_bps: c.rate_bps, kind: c.kind, note: c.note, is_default: true },
                      }),
                    ).catch((e: unknown) => setError(failure(e)));
                    void codes.refetch();
                  }}
                >
                  Make default
                </Button>
              )}
            </li>
          ))}
        </ul>
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={async (e) => {
            e.preventDefault();
            try {
              await ok(api.POST("/api/v1/tax-codes", { body: { name: newCode.name, rate_bps: Math.round(Number(newCode.rate) * 100) } }));
              setNewCode({ name: "", rate: "" });
              void codes.refetch();
            } catch (err) {
              setError(failure(err));
            }
          }}
        >
          <div className="flex flex-col gap-1">
            <Label htmlFor="tc-name">New tax code</Label>
            <Input id="tc-name" value={newCode.name} onChange={(e) => setNewCode((c) => ({ ...c, name: e.target.value }))} />
          </div>
          <div className="flex w-24 flex-col gap-1">
            <Label htmlFor="tc-rate">Rate %</Label>
            <Input id="tc-rate" inputMode="decimal" value={newCode.rate} onChange={(e) => setNewCode((c) => ({ ...c, rate: e.target.value }))} />
          </div>
          <Button type="submit" variant="outline" disabled={!newCode.name.trim()}>
            <Plus /> Add
          </Button>
        </form>
      </section>
    </div>
  );
}

// The payer's page -----------------------------------------------------------------------------

export function PublicInvoicePage() {
  const { token } = useParams({ from: "/invoice/$token" });
  const { paid } = useSearch({ from: "/invoice/$token" });
  const [error, setError] = useState<string | null>(null);
  const [paying, setPaying] = useState(false);
  // Back from paying by card: look again for two minutes while Stripe's
  // confirmation lands, then offer the Pay button again.
  const [waiting, setWaiting] = useState(Boolean(paid));
  useEffect(() => {
    if (!waiting) return;
    const t = setTimeout(() => setWaiting(false), 120_000);
    return () => clearTimeout(t);
  }, [waiting]);
  const invoice = useQuery({
    queryKey: ["public-invoice", token],
    queryFn: () => ok(api.GET("/api/v1/public/invoices/{token}", { params: { path: { token } } })),
    retry: false,
    refetchInterval: (q) => (waiting && q.state.data && q.state.data.outstanding.minor > 0 ? 5000 : false),
  });
  useEffect(() => {
    if (invoice.data) applyBrandTheme(readBranding(invoice.data.branding));
    if (invoice.data) document.title = `${invoice.data.kind === "credit_note" ? "Credit note" : "Invoice"} ${invoice.data.reference} · ${invoice.data.workspace_name}`;
  }, [invoice.data]);
  if (invoice.isPending) {
    return (
      <div className="flex h-dvh items-center justify-center">
        <Spinner className="size-6" />
      </div>
    );
  }
  if (invoice.isError) {
    return (
      <main className="mx-auto max-w-xl p-6">
        <Alert>
          {invoice.error instanceof ApiFailure && invoice.error.status === 404
            ? "This invoice link does not work. A newer email may have replaced it; ask whoever sent it."
            : invoice.error.message}
        </Alert>
      </main>
    );
  }
  const i = invoice.data;
  const logo = readBranding(i.branding).logo;
  const bank = i.bank_account as { label?: string | null; details?: string } | null;
  const issuer = i.issuer as { name?: string } | null;
  const payer = i.payer as { name?: string } | null;
  return (
    <main className="mx-auto flex max-w-2xl flex-col gap-6 p-4 sm:p-8">
      <header className="flex items-center gap-3">
        {logo && <img src={logo} alt="" className="h-10 w-auto max-w-40 rounded object-contain" />}
        <div>
          <p className="text-sm text-muted-foreground">{issuer?.name ?? i.workspace_name}</p>
          <h1 className="text-2xl font-semibold">
            {i.kind === "credit_note" ? "Credit note" : "Invoice"} {i.reference}
          </h1>
        </div>
      </header>
      <Card className="flex flex-col gap-4 p-5 text-sm">
        <p>
          {payer?.name && <>To {payer.name} · </>}
          {i.issue_date && `Issued ${day(i.issue_date)}`}
          {i.due_date && i.kind !== "credit_note" && ` · Due ${day(i.due_date)}`}
        </p>
        <table className="w-full">
          <tbody>
            {i.line_items.map((l, n) => (
              <tr key={n} className="border-b">
                <td className="py-2">{l.description}</td>
                <td className="py-2 text-right">{formatMoney({ minor: l.amount_minor, currency: i.total.currency })}</td>
              </tr>
            ))}
          </tbody>
          <tfoot>
            <tr>
              <td className="pt-2 text-muted-foreground">Subtotal</td>
              <td className="pt-2 text-right">{formatMoney(i.subtotal)}</td>
            </tr>
            {i.tax_lines.map((t) => (
              <tr key={t.name}>
                <td className="text-muted-foreground">{t.name}</td>
                <td className="text-right">{formatMoney({ minor: t.tax_minor, currency: i.total.currency })}</td>
              </tr>
            ))}
            <tr>
              <td className="pt-2 font-medium">Total</td>
              <td className="pt-2 text-right text-lg font-semibold">{formatMoney(i.total)}</td>
            </tr>
          </tfoot>
        </table>
        {paid && i.status !== "paid" && (
          <Alert tone={waiting ? "success" : "info"}>
            {waiting
              ? "Thank you. Your card payment is being confirmed; this page will show it as paid shortly."
              : "Your card payment has not shown here yet. It can take a few minutes; refresh this page later before paying again."}
          </Alert>
        )}
        {i.status === "paid" ? (
          <Alert tone="success">Paid in full. Thank you.</Alert>
        ) : (
          i.kind !== "credit_note" &&
          i.outstanding.minor > 0 && (
            <p className="font-medium">
              {formatMoney(i.outstanding)} to pay{i.due_date && ` by ${day(i.due_date)}`}.
            </p>
          )
        )}
        {i.card_payments && !waiting && (
          <div>
            <Button
              disabled={paying}
              onClick={async () => {
                setError(null);
                setPaying(true);
                try {
                  const { url } = await ok(api.POST("/api/v1/public/invoices/{token}/pay", { params: { path: { token } } }));
                  window.location.assign(url);
                } catch (e) {
                  setError(failure(e));
                  setPaying(false);
                }
              }}
            >
              <CreditCard /> Pay {formatMoney(i.outstanding)} by card
            </Button>
          </div>
        )}
        {bank?.details && i.outstanding.minor > 0 && (
          <div>
            <p className="font-medium">Pay by bank transfer</p>
            <p className="whitespace-pre-line">
              {bank.label && `${bank.label}\n`}
              {bank.details}
              {`\nReference: ${i.reference}`}
            </p>
          </div>
        )}
      </Card>
      {error && <Alert>{error}</Alert>}
      <div>
        <Button
          variant="outline"
          onClick={() =>
            void saveDownload(
              api.GET("/api/v1/public/invoices/{token}/pdf", { params: { path: { token } }, parseAs: "blob" }),
              `${i.reference}.pdf`,
            ).catch((e: unknown) => setError(failure(e)))
          }
        >
          <Download /> Download PDF
        </Button>
      </div>
    </main>
  );
}
