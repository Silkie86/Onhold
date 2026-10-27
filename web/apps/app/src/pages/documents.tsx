// Documents: everything filed (made from a template, uploaded, or imported
// from past PDFs), one document with its versions and signatures, a
// booking's documents and pack, bringing in past PDFs with what was read
// from them, and the page a signer opens from the signing email. Lists and
// documents are kept on the device as last seen for reading offline.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import type { Schemas } from "@onhold/api-client";
import { Alert, Badge, Button, Card, Checkbox, Dialog, Input, Label, NativeSelect, Spinner, Textarea, applyBranding, type Branding, cn, formatMoney } from "@onhold/ui";
import { Download, FilePlus2, FileSignature, FileUp, PackageOpen, RefreshCw, Trash2, Upload, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { fetchSaved, useOffline, useSyncStatus } from "../data.ts";
import { isLocalId } from "../offline/store.ts";
import type { Booking } from "../work.ts";
import { Loading, OfflineNote, PageError } from "./crm.tsx";
import { saveDownload } from "./finance.tsx";

type DocumentView = Schemas["DocumentView"];
type PdfImport = Schemas["PdfImportView"];

export const KINDS: Record<string, string> = {
  contract: "Contract",
  confirmation: "Confirmation",
  advance_sheet: "Advance sheet",
  invoice: "Invoice",
  credit_note: "Credit note",
  quote: "Quote",
  statement: "Statement",
  rider: "Rider",
  tech_spec: "Tech spec",
  stage_plot: "Stage plot",
  epk: "Press kit",
  insurance: "Insurance",
  passport: "Passport",
  visa: "Visa",
  receipt: "Receipt",
  purchase_order: "Purchase order",
  misc: "Other",
};

const SIGNATURE_LABEL: Record<string, string> = {
  awaiting: "Waiting for signatures",
  signed: "Signed",
  declined: "Declined",
};

function failure(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function useOnline(): boolean {
  return useSyncStatus()?.online !== false;
}

/** Uploads a file as it is; the server works out its type from the bytes. */
export async function uploadFile(file: File): Promise<Schemas["FileView"]> {
  return ok(
    api.POST("/api/v1/files", {
      params: { query: { filename: file.name } },
      body: file as never,
      bodySerializer: (b: unknown) => b as BodyInit,
      headers: { "Content-Type": file.type || "application/octet-stream" },
    }),
  );
}

function downloadDocument(id: string, title: string, version?: number) {
  return saveDownload(
    api.GET("/api/v1/documents/{id}/content", { params: { path: { id }, query: { version } }, parseAs: "blob" }),
    `${title}.pdf`,
  );
}

function SignatureBadge({ status }: { status: string }) {
  if (!SIGNATURE_LABEL[status]) return null;
  return (
    <Badge className={cn("border", status === "signed" && "border-emerald-600 bg-emerald-600 text-white", status === "declined" && "border-destructive text-destructive")}>
      {SIGNATURE_LABEL[status]}
    </Badge>
  );
}

function DocumentRow({ d }: { d: DocumentView }) {
  return (
    <li className="flex flex-wrap items-center gap-2 px-3 py-2 text-sm">
      <Link to="/documents/$id" params={{ id: d.id }} className="font-medium hover:underline">
        {d.title}
      </Link>
      <span className="text-muted-foreground">{KINDS[d.kind] ?? d.kind}</span>
      {d.status === "rendering" && <span className="text-xs text-muted-foreground">printing…</span>}
      {d.status === "failed" && <span className="text-xs text-destructive">printing failed</span>}
      <SignatureBadge status={d.signature_status} />
      <span className="flex-1 text-right text-xs text-muted-foreground">
        {d.expires_on && `runs out ${new Date(`${d.expires_on}T12:00:00`).toLocaleDateString()} · `}
        {d.source === "imported" ? "imported" : d.source === "generated" ? "made here" : "uploaded"}
      </span>
    </li>
  );
}

// The list ---------------------------------------------------------------------------------------

export function DocumentsPage() {
  const sync = useOffline();
  const online = useOnline();
  const navigate = useNavigate();
  const [q, setQ] = useState("");
  const [kind, setKind] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const list = useQuery({
    queryKey: ["documents", q, kind],
    queryFn: () =>
      fetchSaved(sync, `documents:${q}:${kind}`, () =>
        ok(api.GET("/api/v1/documents", { params: { query: { q: q || undefined, kind: kind || undefined, limit: 200 } } })),
      ),
  });
  return (
    <div className="flex flex-col gap-4 p-4 md:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="flex-1 text-xl font-semibold">Documents</h1>
        <Button variant="outline" asChild>
          <Link to="/documents/imports">
            <FileUp /> Import past PDFs
          </Link>
        </Button>
        <Button disabled={!online || busy} onClick={() => input.current?.click()}>
          <Upload /> Upload
        </Button>
        <input
          ref={input}
          type="file"
          className="hidden"
          onChange={async (e) => {
            const file = e.target.files?.[0];
            e.target.value = "";
            if (!file) return;
            setError(null);
            setBusy(true);
            try {
              const f = await uploadFile(file);
              const d = await ok(api.POST("/api/v1/documents", { body: { file_id: f.id, kind: "misc", title: file.name.replace(/\.[^.]+$/, "") } }));
              void navigate({ to: "/documents/$id", params: { id: d.id } });
            } catch (err) {
              setError(failure(err));
            } finally {
              setBusy(false);
            }
          }}
        />
      </div>
      <div className="flex flex-wrap gap-2">
        <Input className="max-w-sm" placeholder="Search titles and the words inside PDFs…" value={q} onChange={(e) => setQ(e.target.value)} aria-label="Search documents" />
        <NativeSelect className="w-44" value={kind} onChange={(e) => setKind(e.target.value)} aria-label="Kind">
          <option value="">Any kind</option>
          {Object.entries(KINDS).map(([k, v]) => (
            <option key={k} value={k}>
              {v}
            </option>
          ))}
        </NativeSelect>
      </div>
      {list.data?.offline && <OfflineNote>You are offline. These are the documents as last seen on this device.</OfflineNote>}
      {error && <Alert>{error}</Alert>}
      {list.isPending ? (
        <Loading />
      ) : list.isError ? (
        <PageError error={list.error} />
      ) : list.data.data.items.length === 0 ? (
        <p className="text-sm text-muted-foreground">{q ? "Nothing matches." : "No documents yet."}</p>
      ) : (
        <ul className="divide-y rounded-md border">
          {list.data.data.items.map((d) => (
            <DocumentRow key={d.id} d={d} />
          ))}
        </ul>
      )}
    </div>
  );
}

// One document ---------------------------------------------------------------------------------

const RELATED_PATH: Record<string, string> = {
  booking: "/bookings/$id",
  enquiry: "/enquiries/$id",
  contact: "/crm/contacts/$id",
  organisation: "/crm/organisations/$id",
  talent: "/crm/talent/$id",
  invoice: "/finance/invoices/$id",
};

export function DocumentPage() {
  const { id } = useParams({ from: "/app/documents/$id" });
  const sync = useOffline();
  const online = useOnline();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [signing, setSigning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const doc = useQuery({
    queryKey: ["document", id],
    queryFn: () => fetchSaved(sync, `document:${id}`, () => ok(api.GET("/api/v1/documents/{id}", { params: { path: { id } } }))),
    // Printing takes a moment: look again until it is done.
    refetchInterval: (query) => (query.state.data?.data.status === "rendering" ? 2000 : false),
  });
  if (doc.isPending) return <Loading />;
  if (doc.isError) return <PageError error={doc.error} />;
  const d = doc.data.data;
  const canChange = online && !doc.data.offline;
  const refresh = () => void qc.invalidateQueries({ queryKey: ["document", id] });
  const run = async (call: () => Promise<unknown>) => {
    setError(null);
    try {
      await call();
      refresh();
    } catch (e) {
      setError(failure(e));
    }
  };
  const ifMatch = { "If-Match": `"${d.version}"` };
  const relatedPath = d.related_entity ? RELATED_PATH[d.related_entity] : undefined;
  return (
    <div className="flex max-w-4xl flex-col gap-4 p-4 md:p-6">
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="text-xl font-semibold">{d.title}</h1>
        <Badge className="border">{KINDS[d.kind] ?? d.kind}</Badge>
        <SignatureBadge status={d.signature_status} />
      </div>
      {doc.data.offline && <OfflineNote>You are offline. This is the document as last seen on this device.</OfflineNote>}
      {relatedPath && d.related_id && (
        <p className="text-sm">
          About{" "}
          <Link to={relatedPath} params={{ id: d.related_id } as never} className="underline">
            the {d.related_entity}
          </Link>
        </p>
      )}
      {d.status === "rendering" && (
        <p className="flex items-center gap-2 text-sm text-muted-foreground">
          <Spinner /> Printing the PDF…
        </p>
      )}
      {error && <Alert>{error}</Alert>}
      <div className="flex flex-wrap gap-2">
        {d.file && (
          <Button variant="outline" onClick={() => void downloadDocument(d.id, d.title).catch((e: unknown) => setError(failure(e)))}>
            <Download /> Download
          </Button>
        )}
        {canChange && d.signature_status !== "awaiting" && d.signature_status !== "signed" && (
          <Button onClick={() => setSigning(true)}>
            <FileSignature /> Ask for signatures
          </Button>
        )}
        {canChange && d.signature_status === "awaiting" && (
          <Button variant="outline" onClick={() => void run(() => ok(api.DELETE("/api/v1/documents/{id}/signatures", { params: { path: { id } } })))}>
            <X /> Stop waiting for signatures
          </Button>
        )}
        {canChange && d.template_key && d.signature_status !== "signed" && (
          <Button variant="outline" onClick={() => void run(() => ok(api.POST("/api/v1/documents/{id}/regenerate", { params: { path: { id } } })))}>
            <RefreshCw /> Make again from the record
          </Button>
        )}
        {canChange && (
          <Button
            variant="ghost"
            onClick={async () => {
              if (!window.confirm(`Delete “${d.title}”?`)) return;
              try {
                await ok(api.DELETE("/api/v1/documents/{id}", { params: { path: { id } } }));
                void navigate({ to: "/documents" });
              } catch (e) {
                setError(failure(e));
              }
            }}
          >
            <Trash2 /> Delete
          </Button>
        )}
      </div>
      {canChange && (
        <form
          className="grid gap-3 sm:grid-cols-[1fr_12rem_10rem_auto] sm:items-end"
          onSubmit={(e) => {
            e.preventDefault();
            const f = new FormData(e.currentTarget);
            void run(() =>
              ok(
                api.PATCH("/api/v1/documents/{id}", {
                  params: { path: { id } },
                  headers: ifMatch,
                  body: {
                    title: String(f.get("title") ?? ""),
                    kind: String(f.get("kind") ?? d.kind),
                    expires_on: String(f.get("expires_on") ?? "") || null,
                  },
                }),
              ),
            );
          }}
        >
          <div className="flex flex-col gap-1">
            <Label htmlFor="doc-title">Title</Label>
            <Input id="doc-title" name="title" defaultValue={d.title} />
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="doc-kind">Kind</Label>
            <NativeSelect id="doc-kind" name="kind" defaultValue={d.kind}>
              {Object.entries(KINDS).map(([k, v]) => (
                <option key={k} value={k}>
                  {v}
                </option>
              ))}
            </NativeSelect>
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="doc-expires">Runs out</Label>
            <Input id="doc-expires" name="expires_on" type="date" defaultValue={d.expires_on ?? ""} />
          </div>
          <Button type="submit" variant="outline">
            Save
          </Button>
        </form>
      )}
      <section className="flex flex-col gap-2" aria-label="Versions">
        <h2 className="font-semibold">Versions</h2>
        <ul className="divide-y rounded-md border text-sm">
          {d.versions.map((v) => (
            <li key={v.number} className="flex flex-wrap items-center gap-2 px-3 py-2">
              <span className="font-medium">{v.number}</span>
              <span>{{ original: "First version", regenerated: "Made again", signed: "Signed, with certificate", uploaded: "Uploaded" }[v.purpose] ?? v.purpose}</span>
              <span className="flex-1 text-xs text-muted-foreground">{new Date(v.created_at).toLocaleString()}</span>
              {v.status === "failed" && <span className="text-xs text-destructive">{v.error ?? "printing failed"}</span>}
              {v.file && (
                <Button size="sm" variant="ghost" onClick={() => void downloadDocument(d.id, `${d.title} v${v.number}`, v.number).catch((e: unknown) => setError(failure(e)))}>
                  <Download /> PDF
                </Button>
              )}
            </li>
          ))}
        </ul>
      </section>
      {d.signatures.length > 0 && (
        <section className="flex flex-col gap-2" aria-label="Signatures">
          <h2 className="font-semibold">Signatures</h2>
          <ul className="divide-y rounded-md border text-sm">
            {d.signatures.map((s) => (
              <li key={s.id} className="flex flex-col px-3 py-2">
                <span>
                  {s.signer_name} &lt;{s.signer_email}&gt; ·{" "}
                  {{ pending: "not yet signed", signed: `signed as “${s.signed_name}”`, declined: "declined", cancelled: "cancelled" }[s.status] ?? s.status}
                </span>
                <span className="text-xs text-muted-foreground">
                  Sent {new Date(s.sent_at).toLocaleString()}
                  {s.viewed_at && ` · opened ${new Date(s.viewed_at).toLocaleString()}`}
                  {s.decided_at && ` · ${new Date(s.decided_at).toLocaleString()}`}
                  {s.decline_reason && ` · “${s.decline_reason}”`}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}
      {signing && <SignersDialog document={d} onClose={() => setSigning(false)} onDone={refresh} />}
    </div>
  );
}

function SignersDialog({ document: d, onClose, onDone }: { document: DocumentView; onClose: () => void; onDone: () => void }) {
  const [signers, setSigners] = useState<{ name: string; email: string }[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const set = (i: number, patch: Partial<{ name: string; email: string }>) => setSigners((s) => s.map((x, j) => (j === i ? { ...x, ...patch } : x)));
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={`Ask for signatures on ${d.title}`}>
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          setBusy(true);
          try {
            await ok(api.POST("/api/v1/documents/{id}/signatures", { params: { path: { id: d.id } }, body: { signers } }));
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
          Each signer gets an email with their own link. When everyone has signed, a signed copy with a certificate page
          is kept as a new version.
        </p>
        {signers.map((s, i) => (
          <div key={i} className="grid grid-cols-[1fr_1fr_auto] gap-2">
            <Input aria-label={`Signer ${i + 1} name`} placeholder="Name" value={s.name} onChange={(e) => set(i, { name: e.target.value })} />
            <Input aria-label={`Signer ${i + 1} email`} type="email" placeholder="Email" value={s.email} onChange={(e) => set(i, { email: e.target.value })} />
            <Button variant="ghost" size="icon" aria-label={`Remove signer ${i + 1}`} onClick={() => setSigners((x) => x.filter((_, j) => j !== i))}>
              <X />
            </Button>
          </div>
        ))}
        <Button variant="outline" size="sm" className="self-start" onClick={() => setSigners((s) => [...s, { name: "", email: "" }])}>
          Add a signer
        </Button>
        {signers.length === 0 && <p className="text-xs text-muted-foreground">With nobody added, the booking&apos;s contact is asked.</p>}
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy}>
            <FileSignature /> Send
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

// A booking's documents --------------------------------------------------------------------

export function BookingDocumentsPanel({ booking }: { booking: Booking }) {
  const sync = useOffline();
  const online = useOnline();
  const qc = useQueryClient();
  const [template, setTemplate] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const local = isLocalId(booking.id);
  const docs = useQuery({
    queryKey: ["booking-documents", booking.id],
    enabled: !local,
    queryFn: async () =>
      (
        await fetchSaved(sync, `booking-documents:${booking.id}`, () =>
          ok(api.GET("/api/v1/documents", { params: { query: { related_id: booking.id, limit: 100 } } })),
        )
      ).data,
    refetchInterval: (query) => (query.state.data?.items.some((d) => d.status === "rendering") ? 2000 : false),
  });
  const templates = useQuery({
    queryKey: ["document-templates"],
    enabled: online,
    queryFn: () => ok(api.GET("/api/v1/document-templates")),
  });
  const forBookings = (templates.data ?? []).filter((t) => t.entity === "booking" && !["invoice", "credit_note", "statement"].includes(t.kind));
  const refresh = () => void qc.invalidateQueries({ queryKey: ["booking-documents", booking.id] });
  const canChange = online && !local;
  const run = async (call: () => Promise<unknown>) => {
    setError(null);
    setBusy(true);
    try {
      await call();
      refresh();
    } catch (e) {
      setError(failure(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="flex flex-col gap-3" aria-label="Documents">
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="flex-1 font-semibold">Documents</h2>
        {(docs.data?.items.length ?? 0) > 0 && online && (
          <Button
            size="sm"
            variant="ghost"
            onClick={() =>
              void saveDownload(
                api.GET("/api/v1/bookings/{id}/pack", { params: { path: { id: booking.id } }, parseAs: "blob" }),
                `${booking.title}.zip`,
              ).catch((e: unknown) => setError(failure(e)))
            }
          >
            <PackageOpen /> Pack
          </Button>
        )}
      </div>
      {canChange && (
        <div className="flex flex-wrap gap-2">
          <NativeSelect className="w-52" aria-label="Template" value={template} onChange={(e) => setTemplate(e.target.value)}>
            <option value="">Make from a template…</option>
            {forBookings.map((t) => (
              <option key={t.key} value={t.key}>
                {t.name}
              </option>
            ))}
          </NativeSelect>
          <Button
            size="sm"
            variant="outline"
            disabled={!template || busy}
            onClick={() =>
              void run(() =>
                ok(api.POST("/api/v1/documents/generate", { body: { template_key: template, related_entity: "booking", related_id: booking.id } })),
              )
            }
          >
            <FilePlus2 /> Make
          </Button>
          <Button size="sm" variant="outline" disabled={busy} onClick={() => input.current?.click()}>
            <Upload /> Upload
          </Button>
          <input
            ref={input}
            type="file"
            className="hidden"
            onChange={(e) => {
              const file = e.target.files?.[0];
              e.target.value = "";
              if (!file) return;
              void run(async () => {
                const f = await uploadFile(file);
                await ok(
                  api.POST("/api/v1/documents", {
                    body: { file_id: f.id, kind: "misc", title: file.name.replace(/\.[^.]+$/, ""), related_entity: "booking", related_id: booking.id },
                  }),
                );
              });
            }}
          />
        </div>
      )}
      {error && <Alert>{error}</Alert>}
      {docs.isPending && !local ? (
        <Spinner />
      ) : (docs.data?.items.length ?? 0) === 0 ? (
        <p className="text-sm text-muted-foreground">No documents yet.</p>
      ) : (
        <ul className="divide-y rounded-md border">
          {docs.data?.items.map((d) => (
            <DocumentRow key={d.id} d={d} />
          ))}
        </ul>
      )}
    </section>
  );
}

// Past PDFs --------------------------------------------------------------------------------------

export function PdfImportsPage() {
  const online = useOnline();
  const qc = useQueryClient();
  const [show, setShow] = useState("reading,ready,failed");
  const [error, setError] = useState<string | null>(null);
  const [uploading, setUploading] = useState<{ done: number; of: number } | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const imports = useQuery({
    queryKey: ["pdf-imports", show],
    enabled: online,
    queryFn: () => ok(api.GET("/api/v1/pdf-imports", { params: { query: { status: show } } })),
    refetchInterval: (query) => (query.state.data?.some((i) => i.status === "reading") ? 2000 : false),
  });
  const refresh = () => void qc.invalidateQueries({ queryKey: ["pdf-imports"] });
  if (!online) return <div className="p-6"><OfflineNote>Past PDFs can be brought in when you are online.</OfflineNote></div>;
  return (
    <div className="flex flex-col gap-4 p-4 md:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="flex-1 text-xl font-semibold">Import past PDFs</h1>
        <NativeSelect className="w-44" aria-label="Show" value={show} onChange={(e) => setShow(e.target.value)}>
          <option value="reading,ready,failed">To look at</option>
          <option value="confirmed">Kept</option>
          <option value="discarded">Discarded</option>
        </NativeSelect>
        <Button disabled={!!uploading} onClick={() => input.current?.click()}>
          <Upload /> Choose PDFs
        </Button>
        <input
          ref={input}
          type="file"
          multiple
          accept="application/pdf,.pdf"
          className="hidden"
          onChange={async (e) => {
            const files = Array.from(e.target.files ?? []);
            e.target.value = "";
            if (files.length === 0) return;
            setError(null);
            setUploading({ done: 0, of: files.length });
            const ids: string[] = [];
            const failed: string[] = [];
            for (const file of files) {
              try {
                ids.push((await uploadFile(file)).id);
              } catch (err) {
                failed.push(`${file.name}: ${failure(err)}`);
              }
              setUploading((u) => u && { ...u, done: u.done + 1 });
            }
            try {
              // The server takes up to 100 at a time.
              for (let i = 0; i < ids.length; i += 100) {
                await ok(api.POST("/api/v1/pdf-imports", { body: { file_ids: ids.slice(i, i + 100) } }));
              }
            } catch (err) {
              failed.push(failure(err));
            }
            setUploading(null);
            if (failed.length) setError(failed.join("\n"));
            refresh();
          }}
        />
      </div>
      <p className="text-sm text-muted-foreground">
        Each PDF is read for its numbers, dates, amounts and names, and matched to records you already have. Check what was
        found, choose where it belongs, and keep it. Nothing is filed until you do. Scanned PDFs without text can still be
        kept, but have to be filled in by hand.
      </p>
      {uploading && (
        <p className="flex items-center gap-2 text-sm">
          <Spinner /> Uploading {uploading.done} of {uploading.of}…
        </p>
      )}
      {error && <Alert className="whitespace-pre-line">{error}</Alert>}
      {imports.isPending ? (
        <Loading />
      ) : imports.isError ? (
        <PageError error={imports.error} />
      ) : imports.data.length === 0 ? (
        <p className="text-sm text-muted-foreground">Nothing here.</p>
      ) : (
        <ul className="flex flex-col gap-3">
          {imports.data.map((i) => (
            <li key={i.id}>
              <ImportCard key={`${i.id}:${i.status}`} item={i} onChanged={refresh} />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

interface Found {
  field: string;
  value: { type: string; value: unknown };
  confidence: string;
  source: { text?: string };
}

interface Match {
  entity: string;
  id: string;
  name: string;
  reason: string;
}

function shown(v: Found["value"]): string {
  if (v.type === "money") return formatMoney(v.value as { minor: number; currency: string });
  if (v.type === "date") return new Date(`${String(v.value)}T12:00:00`).toLocaleDateString(undefined, { dateStyle: "medium" });
  return String(v.value);
}

const FIELD_LABEL: Record<string, string> = {
  invoice_number: "Invoice number",
  order_number: "Order number",
  issue_date: "Date",
  due_date: "Due",
  start_date: "Starts",
  end_date: "Ends",
  signed_date: "Signed",
  subtotal: "Subtotal",
  tax: "Tax",
  total: "Total",
  email: "Email",
  phone: "Phone",
  vat_number: "VAT number",
  company_number: "Company number",
  iban: "IBAN",
  party: "To / party",
  issuer: "From",
};

function ImportCard({ item: i, onChanged }: { item: PdfImport; onChanged: () => void }) {
  const found = (i.found as { kinds?: { kind: string; score: number }[]; fields?: Found[] }) ?? {};
  const matches = (i.matches as unknown as Match[] | null) ?? [];
  const best = (f: string) => found.fields?.find((x) => x.field === f);
  const guessed = found.kinds?.[0]?.kind ?? "misc";
  const [kind, setKind] = useState(guessed);
  const [related, setRelated] = useState<string>(() => {
    const m = matches.find((x) => ["booking", "organisation", "talent", "contact"].includes(x.entity));
    return m ? `${m.entity}:${m.id}` : "";
  });
  const total = best("total")?.value.value as { minor: number; currency: string } | undefined;
  const [asInvoice, setAsInvoice] = useState(guessed === "invoice" && !!total);
  const [reference, setReference] = useState(String(best("invoice_number")?.value.value ?? ""));
  const [issueDate, setIssueDate] = useState(String(best("issue_date")?.value.value ?? ""));
  const [paid, setPaid] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const duplicate = matches.find((m) => m.entity === "invoice");
  const [relEntity, relId] = related ? (related.split(":") as [string, string]) : [undefined, undefined];
  const payerOrg = matches.find((m) => m.entity === "organisation");
  const issuerTalent = matches.find((m) => m.entity === "talent");
  return (
    <Card className="flex flex-col gap-3 p-4 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <span className="flex-1 font-medium">{i.filename}</span>
        {i.status === "reading" && (
          <span className="flex items-center gap-1 text-muted-foreground">
            <Spinner /> reading…
          </span>
        )}
        {i.status === "confirmed" && <Badge className="border">Kept</Badge>}
        {i.status === "discarded" && <Badge className="border">Discarded</Badge>}
        {i.page_count !== null && i.page_count !== undefined && <span className="text-xs text-muted-foreground">{i.page_count} pages</span>}
      </div>
      {i.needs_ocr && <Alert tone="info">This PDF looks scanned: it has no text to read. Fill in what it is by hand.</Alert>}
      {i.error && i.status !== "ready" && <Alert>{i.error}</Alert>}
      {duplicate && <Alert>{duplicate.reason}. Keeping it again would record it twice.</Alert>}
      {(found.fields?.length ?? 0) > 0 && (
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
          {found.fields
            ?.filter((f, n, all) => all.findIndex((x) => x.field === f.field) === n)
            .map((f) => (
              <div key={f.field} className="contents">
                <dt className="text-muted-foreground">{FIELD_LABEL[f.field] ?? f.field}</dt>
                <dd className={cn(f.confidence === "low" && "text-muted-foreground")} title={f.source.text}>
                  {shown(f.value)}
                </dd>
              </div>
            ))}
        </dl>
      )}
      {i.status === "ready" && (
        <form
          className="flex flex-col gap-3 border-t pt-3"
          onSubmit={async (e) => {
            e.preventDefault();
            setError(null);
            try {
              await ok(
                api.POST("/api/v1/pdf-imports/{id}/confirm", {
                  params: { path: { id: i.id } },
                  headers: { "If-Match": `"${i.version}"` },
                  body: {
                    kind,
                    related_entity: relEntity ?? null,
                    related_id: relId ?? null,
                    document_date: issueDate || null,
                    invoice:
                      kind === "invoice" && asInvoice && total
                        ? {
                            reference,
                            payer: payerOrg ? { organisation_id: payerOrg.id } : { name: String(best("party")?.value.value ?? "Unknown") },
                            issuer_talent_id: issuerTalent?.id ?? null,
                            currency: total.currency,
                            issue_date: issueDate,
                            total_minor: total.minor,
                            paid,
                          }
                        : null,
                  },
                }),
              );
              onChanged();
            } catch (err) {
              setError(failure(err));
            }
          }}
        >
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="flex flex-col gap-1">
              <Label htmlFor={`kind-${i.id}`}>What it is</Label>
              <NativeSelect id={`kind-${i.id}`} value={kind} onChange={(e) => setKind(e.target.value)}>
                {Object.entries(KINDS).map(([k, v]) => (
                  <option key={k} value={k}>
                    {v}
                  </option>
                ))}
              </NativeSelect>
            </div>
            <div className="flex flex-col gap-1">
              <Label htmlFor={`rel-${i.id}`}>File it on</Label>
              <NativeSelect id={`rel-${i.id}`} value={related} onChange={(e) => setRelated(e.target.value)}>
                <option value="">Nothing in particular</option>
                {matches
                  .filter((m) => m.entity !== "invoice")
                  .map((m) => (
                    <option key={m.id} value={`${m.entity}:${m.id}`}>
                      {m.name} ({m.entity}) – {m.reason}
                    </option>
                  ))}
              </NativeSelect>
            </div>
          </div>
          {kind === "invoice" && total && (
            <div className="flex flex-col gap-2 rounded-md border p-3">
              <label className="flex items-center gap-2">
                <Checkbox checked={asInvoice} onChange={() => setAsInvoice((x) => !x)} /> Also keep it as a past invoice of{" "}
                {formatMoney(total)}
              </label>
              {asInvoice && (
                <div className="grid gap-2 sm:grid-cols-3">
                  <Input aria-label="Its number" value={reference} onChange={(e) => setReference(e.target.value)} />
                  <Input aria-label="Its date" type="date" value={issueDate} onChange={(e) => setIssueDate(e.target.value)} />
                  <label className="flex items-center gap-2">
                    <Checkbox checked={paid} onChange={() => setPaid((x) => !x)} /> Paid
                  </label>
                </div>
              )}
            </div>
          )}
          {error && <Alert>{error}</Alert>}
          <div className="flex gap-2">
            <Button type="submit">Keep it</Button>
            <Button
              variant="ghost"
              onClick={() => void ok(api.POST("/api/v1/pdf-imports/{id}/discard", { params: { path: { id: i.id } } })).then(onChanged, (e: unknown) => setError(failure(e)))}
            >
              Discard
            </Button>
          </div>
        </form>
      )}
      {i.document_id && (
        <Link to="/documents/$id" params={{ id: i.document_id }} className="text-sm underline">
          Open the document
        </Link>
      )}
    </Card>
  );
}

// The signer's page ------------------------------------------------------------------------------

export function PublicSigningPage() {
  const { token } = useParams({ from: "/sign/$token" });
  const page = useQuery({
    queryKey: ["public-signing", token],
    queryFn: () => ok(api.GET("/api/v1/public/sign/{token}", { params: { path: { token } } })),
    retry: false,
  });
  const [pdfUrl, setPdfUrl] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [agree, setAgree] = useState(false);
  const [declining, setDeclining] = useState(false);
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    const b = page.data?.branding as { colours?: Branding } | undefined;
    if (b?.colours) applyBranding(b.colours);
    if (page.data) {
      document.title = `${page.data.document_title} · ${page.data.workspace_name}`;
      setName((n) => n || page.data.signer_name);
    }
  }, [page.data]);
  useEffect(() => {
    if (!page.data) return;
    let url: string | null = null;
    void api.GET("/api/v1/public/sign/{token}/pdf", { params: { path: { token } }, parseAs: "blob" }).then(({ data }) => {
      if (data) {
        url = URL.createObjectURL(data as Blob);
        setPdfUrl(url);
      }
    });
    return () => {
      if (url) URL.revokeObjectURL(url);
    };
  }, [page.data, token]);
  if (page.isPending) {
    return (
      <div className="flex h-dvh items-center justify-center">
        <Spinner className="size-6" />
      </div>
    );
  }
  if (page.isError) {
    return (
      <main className="mx-auto max-w-xl p-6">
        <Alert>
          {page.error instanceof ApiFailure && page.error.status === 404
            ? "This signing link does not work. It may have been cancelled or replaced; ask whoever sent it."
            : page.error.message}
        </Alert>
      </main>
    );
  }
  const p = page.data;
  const logo = (p.branding as { logo_url?: string } | null)?.logo_url;
  const decide = async (sign: boolean) => {
    setError(null);
    setBusy(true);
    try {
      await ok(
        api.POST("/api/v1/public/sign/{token}", {
          params: { path: { token } },
          body: sign ? { sign: true, signed_name: name, agree } : { sign: false, reason: reason || null },
        }),
      );
      void page.refetch();
    } catch (err) {
      setError(failure(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <main className="mx-auto flex max-w-3xl flex-col gap-5 p-4 sm:p-8">
      <header className="flex items-center gap-3">
        {logo && <img src={logo} alt="" className="size-10 rounded object-contain" />}
        <div>
          <p className="text-sm text-muted-foreground">{p.workspace_name} asks you to sign</p>
          <h1 className="text-2xl font-semibold">{p.document_title}</h1>
        </div>
      </header>
      {pdfUrl ? (
        <iframe title={p.document_title} src={pdfUrl} className="h-[70dvh] w-full rounded-md border" />
      ) : (
        <Spinner />
      )}
      {pdfUrl && (
        <a href={pdfUrl} download={`${p.document_title}.pdf`} className="flex items-center gap-1 text-sm underline">
          <Download className="size-4" /> Download to read it first
        </a>
      )}
      {p.status === "signed" ? (
        <Alert tone="success">
          Signed as “{p.signed_name}”{p.decided_at && ` on ${new Date(p.decided_at).toLocaleString()}`}. You will get the signed copy
          once everyone has signed.
        </Alert>
      ) : p.status === "declined" ? (
        <Alert tone="info">You declined to sign. {p.workspace_name} has been told.</Alert>
      ) : p.status !== "pending" ? (
        <Alert tone="info">This request is closed.</Alert>
      ) : declining ? (
        <Card className="flex flex-col gap-3 p-5">
          <Label htmlFor="decline-reason">Why are you not signing? (optional)</Label>
          <Textarea id="decline-reason" value={reason} onChange={(e) => setReason(e.target.value)} />
          {error && <Alert>{error}</Alert>}
          <div className="flex gap-2">
            <Button variant="outline" disabled={busy} onClick={() => void decide(false)}>
              Decline to sign
            </Button>
            <Button variant="ghost" onClick={() => setDeclining(false)}>
              Back
            </Button>
          </div>
        </Card>
      ) : (
        <Card className="flex flex-col gap-3 p-5">
          <Label htmlFor="signed-name">Type your full name to sign</Label>
          <Input id="signed-name" autoComplete="name" value={name} onChange={(e) => setName(e.target.value)} className="font-serif text-lg italic" />
          <label className="flex items-start gap-2 text-sm">
            <Checkbox checked={agree} onChange={() => setAgree((a) => !a)} className="mt-0.5" />
            <span>
              I have read this document and agree to sign it electronically. My typed name is my signature, and the time,
              my connection&apos;s address and a fingerprint of the document are recorded with it.
            </span>
          </label>
          {error && <Alert>{error}</Alert>}
          <div className="flex gap-2">
            <Button disabled={busy || !agree || !name.trim()} onClick={() => void decide(true)}>
              <FileSignature /> Sign
            </Button>
            <Button variant="ghost" onClick={() => setDeclining(true)}>
              I won&apos;t sign
            </Button>
          </div>
          <p className="text-xs text-muted-foreground">Document fingerprint (SHA-256): {p.sha256}</p>
        </Card>
      )}
    </main>
  );
}
