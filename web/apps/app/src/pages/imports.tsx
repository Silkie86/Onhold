// Bringing records in from a spreadsheet, or from the export of the app a
// workspace is moving from: pick a file, match its columns to fields (done
// for you for exports we recognise), check what would happen and correct
// the rows at fault, then let the worker do it.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import type { Schemas } from "@onhold/api-client";
import { Alert, Badge, Button, Card, Checkbox, Input, Label, NativeSelect, Spinner, relativeTime } from "@onhold/ui";
import { FileUp, Plus } from "lucide-react";
import { type ReactNode, useState } from "react";
import { ENTITIES, type EntityKey, api, ok } from "../api.ts";

type Preview = Schemas["ImportPreview"];
type DryRun = Schemas["DryRunResult"];
type ImportView = Schemas["ImportView"];
type Options = Schemas["ImportOptions"];
type ImportEntity = EntityKey | "auto";

/** Reads an uploaded file's text whatever it was saved as: UTF-8, UTF-16
 * (Excel's "Unicode text") or Windows-1252 (older Excel and Outlook
 * exports). A UTF-8 file with the odd stray byte stays UTF-8. */
export function decodeFile(bytes: Uint8Array): string {
  if (bytes[0] === 0xff && bytes[1] === 0xfe) return new TextDecoder("utf-16le").decode(bytes);
  if (bytes[0] === 0xfe && bytes[1] === 0xff) return new TextDecoder("utf-16be").decode(bytes);
  const utf8 = new TextDecoder("utf-8").decode(bytes);
  let bad = 0;
  let good = 0;
  for (const c of utf8) {
    if (c === "\uFFFD") bad++;
    else if (c.charCodeAt(0) > 0x7f) good++;
  }
  // Mostly well-formed UTF-8 is UTF-8; otherwise the bytes are Windows-1252.
  return bad === 0 || good > bad ? utf8 : new TextDecoder("windows-1252").decode(bytes);
}

const HEAVY_CARD_LINE = /^(?:[\w-]+\.)?(?:PHOTO|LOGO|SOUND|KEY)[;:]/i;

/** Takes photos and other attachments out of contact cards, which can make
 * an address book many times larger than its text and are not imported. */
export function stripCardAttachments(text: string): string {
  if (!/^\s*BEGIN:VCARD/i.test(text.replace(/^\uFEFF/, ""))) return text;
  const out: string[] = [];
  let dropping = false;
  // A vCard 2.1 quoted-printable line ending in `=` carries on in the next
  // line, whatever that starts with.
  let carriesOn = false;
  let head = "";
  for (const line of text.split(/\r?\n/)) {
    const continued = carriesOn || line.startsWith(" ") || line.startsWith("\t");
    if (!continued) {
      dropping = HEAVY_CARD_LINE.test(line);
      head = line.split(":")[0] ?? "";
    }
    carriesOn = /QUOTED-PRINTABLE/i.test(head) && line.endsWith("=");
    if (!dropping) out.push(line);
  }
  return out.join("\n");
}

/** Corrections made while checking, by `row:column`. */
type Edits = Record<string, string>;

const STATUS: Record<string, string> = {
  uploaded: "Not run",
  queued: "Waiting",
  running: "Importing",
  done: "Done",
  failed: "Failed",
};

export function ImportsPage() {
  const [wizard, setWizard] = useState(false);
  const imports = useQuery({ queryKey: ["imports"], queryFn: () => ok(api.GET("/api/v1/imports")) });
  if (wizard) return <ImportWizard onDone={() => setWizard(false)} />;
  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex items-center justify-between gap-2">
        <h1 className="text-xl font-semibold">Imports</h1>
        <Button onClick={() => setWizard(true)}>
          <Plus /> Import
        </Button>
      </div>
      {imports.isPending && <Spinner />}
      {imports.data?.length === 0 && (
        <Card className="flex flex-col items-center gap-3 p-8 text-center">
          <FileUp className="size-8 text-muted-foreground" />
          <p className="font-medium">Bring in your talent, contacts and organisations</p>
          <p className="max-w-md text-sm text-muted-foreground">
            Moving from another app? Upload its export as it is: we recognise Google Contacts, Outlook, Apple and iCloud contact cards,
            HubSpot, Salesforce, Pipedrive, Mailchimp and LinkedIn, and match any other spreadsheet's columns for you. You see what will
            happen, and can correct rows, before anything is saved.
          </p>
          <Button onClick={() => setWizard(true)}>Import</Button>
        </Card>
      )}
      {imports.data && imports.data.length > 0 && (
        <div className="overflow-x-auto rounded-md border">
          <table className="w-full text-left text-sm">
            <thead className="bg-muted/50 text-xs text-muted-foreground">
              <tr>
                <th className="px-3 py-2">File</th>
                <th className="px-3 py-2">Records</th>
                <th className="px-3 py-2">Status</th>
                <th className="px-3 py-2">Created</th>
                <th className="px-3 py-2">Updated</th>
                <th className="px-3 py-2">Failed</th>
                <th className="px-3 py-2">When</th>
              </tr>
            </thead>
            <tbody>
              {imports.data.map((i) => (
                <tr key={i.id} className="border-t">
                  <td className="px-3 py-2">{i.filename ?? "Untitled"}</td>
                  <td className="px-3 py-2 capitalize">{i.entity}</td>
                  <td className="px-3 py-2">
                    <Badge>{STATUS[i.status] ?? i.status}</Badge>
                  </td>
                  <td className="px-3 py-2">{i.created}</td>
                  <td className="px-3 py-2">{i.updated}</td>
                  <td className="px-3 py-2">{i.failed}</td>
                  <td className="px-3 py-2 whitespace-nowrap">{relativeTime(i.created_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function Steps({ step }: { step: number }) {
  const names = ["Choose a file", "Match columns", "Check", "Import"];
  return (
    <ol className="flex flex-wrap gap-x-4 gap-y-1 text-sm">
      {names.map((n, i) => (
        <li key={n} className={i === step ? "font-semibold" : "text-muted-foreground"} aria-current={i === step ? "step" : undefined}>
          {i + 1}. {n}
        </li>
      ))}
    </ol>
  );
}

function Frame({ step, children }: { step: number; children: ReactNode }) {
  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 p-4 sm:p-6">
      <h1 className="text-xl font-semibold">Import</h1>
      <Steps step={step} />
      {children}
    </div>
  );
}

export function ImportWizard({ onDone }: { onDone: () => void }) {
  const qc = useQueryClient();
  const [entity, setEntity] = useState<ImportEntity>("auto");
  const [file, setFile] = useState<File | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [columns, setColumns] = useState<(string | null)[]>([]);
  const [options, setOptions] = useState<Options>({ on_match: "skip", create_organisations: true, run_rules: false, tags: [] });
  const [tagText, setTagText] = useState("");
  const [dry, setDry] = useState<DryRun | null>(null);
  const [edits, setEdits] = useState<Edits>({});
  const [skipRows, setSkipRows] = useState<number[]>([]);
  const [running, setRunning] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const attempt = async (f: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const plan = () => ({
    columns,
    options: {
      ...options,
      tags: tagText.split(",").map((t) => t.trim()).filter(Boolean),
      edits: Object.entries(edits).map(([key, value]) => {
        const [row, column] = key.split(":").map(Number);
        return { row: row ?? 0, column: column ?? 0, value };
      }),
      skip_rows: skipRows,
    },
  });
  const check = () =>
    attempt(async () => {
      if (!preview) return;
      setDry(await ok(api.POST("/api/v1/imports/{id}/dry-run", { params: { path: { id: preview.import.id } }, body: plan() })));
    });

  const progress = useQuery({
    queryKey: ["import", running],
    queryFn: () => ok(api.GET("/api/v1/imports/{id}", { params: { path: { id: running ?? "" } } })),
    enabled: !!running,
    refetchInterval: (q) => (q.state.data && ["done", "failed"].includes(q.state.data.status) ? false : 1000),
  });

  if (running) {
    return (
      <Frame step={3}>
        <ImportProgress
          view={progress.data}
          onFinish={() => {
            void qc.invalidateQueries({ queryKey: ["imports"] });
            void qc.invalidateQueries({ queryKey: ["list"] });
            void qc.invalidateQueries({ queryKey: ["tags"] });
            onDone();
          }}
        />
      </Frame>
    );
  }

  if (dry && preview) {
    return (
      <Frame step={2}>
        <Card className="flex flex-col gap-4 p-4">
          <p className="text-sm">If you import now:</p>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
            <Stat label="New records" value={dry.create} />
            <Stat label="Updated" value={dry.update} />
            <Stat label="Skipped (already there)" value={dry.skip} />
            <Stat label="Rows with problems" value={dry.fail} tone={dry.fail > 0 ? "warn" : undefined} />
          </div>
          {dry.new_organisations.length > 0 && (
            <p className="text-sm">
              New organisations: {dry.new_organisations.slice(0, 10).join(", ")}
              {dry.new_organisations.length > 10 && ` and ${dry.new_organisations.length - 10} more`}
            </p>
          )}
          {dry.problem_rows.length > 0 || skipRows.length > 0 ? (
            <ProblemRows
              dry={dry}
              headers={preview.import.headers}
              columns={columns}
              targets={preview.targets}
              edits={edits}
              onEdit={(row, column, value) => setEdits({ ...edits, [`${row}:${column}`]: value })}
              skipRows={skipRows}
              onSkip={(row, skip) => setSkipRows(skip ? [...skipRows, row] : skipRows.filter((r) => r !== row))}
            />
          ) : (
            dry.issues.length > 0 && <Issues issues={dry.issues} />
          )}
          {error && <Alert>{error}</Alert>}
          <div className="flex flex-wrap justify-end gap-2">
            <Button variant="ghost" onClick={() => setDry(null)}>
              Back
            </Button>
            {(dry.fail > 0 || Object.keys(edits).length > 0) && (
              <Button variant="outline" disabled={busy} onClick={check}>
                Check again
              </Button>
            )}
            <Button
              disabled={busy || dry.create + dry.update === 0}
              onClick={() =>
                attempt(async () => {
                  const view = await ok(
                    api.POST("/api/v1/imports/{id}/run", { params: { path: { id: preview.import.id } }, body: plan() }),
                  );
                  setRunning(view.id);
                })
              }
            >
              {busy && <Spinner />}
              Import {dry.create + dry.update} {dry.create + dry.update === 1 ? "record" : "records"}
            </Button>
          </div>
        </Card>
      </Frame>
    );
  }

  if (preview) {
    const targets = preview.targets;
    const used = new Map<string, number>();
    for (const c of columns) if (c) used.set(c, (used.get(c) ?? 0) + 1);
    const missing = targets.filter((t) => t.required && !used.has(t.key));
    const holds = Object.values(ENTITIES).find((e) => e.entity === preview.import.entity);
    const kind = holds?.label.toLowerCase() ?? preview.import.entity;
    return (
      <Frame step={1}>
        {preview.source ? (
          <Alert tone="success">
            This is a {preview.source.label} export of {kind}, so its columns are matched for you. Check them below and carry on.
          </Alert>
        ) : (
          <p className="text-sm text-muted-foreground">
            {preview.import.filename} has {preview.import.row_count} rows of {kind}. Choose the field each column goes into; we have
            matched what we could.
          </p>
        )}
        {entity === "auto" && (
          <p className="text-sm text-muted-foreground">
            Not {kind}?{" "}
            <button type="button" className="underline" onClick={() => setPreview(null)}>
              Go back and choose what the file holds
            </button>
            .
          </p>
        )}
        <div className="overflow-x-auto rounded-md border">
          <table className="w-full text-left text-sm">
            <thead className="bg-muted/50 text-xs text-muted-foreground">
              <tr>
                <th className="px-3 py-2">Column</th>
                <th className="px-3 py-2">Example</th>
                <th className="px-3 py-2">Goes into</th>
              </tr>
            </thead>
            <tbody>
              {preview.import.headers.map((h, i) => {
                const chosen = columns[i] ?? null;
                const clash = chosen && (used.get(chosen) ?? 0) > 1 && !targets.find((t) => t.key === chosen)?.many;
                return (
                  <tr key={`${h}-${i}`} className="border-t">
                    <td className="px-3 py-2 font-medium">{h}</td>
                    <td className="max-w-56 truncate px-3 py-2 text-muted-foreground">
                      {preview.sample.map((r) => r[i]).filter(Boolean).slice(0, 2).join(", ")}
                    </td>
                    <td className="px-3 py-2">
                      <NativeSelect
                        aria-label={`Field for ${h}`}
                        aria-invalid={clash || undefined}
                        value={chosen ?? ""}
                        onChange={(e) => setColumns(columns.map((c, j) => (j === i ? e.target.value || null : c)))}
                      >
                        <option value="">Don't import</option>
                        {targets.map((t) => (
                          <option key={t.key} value={t.key}>
                            {t.label}
                            {t.required ? " *" : ""}
                          </option>
                        ))}
                      </NativeSelect>
                      {clash && <p className="text-xs text-destructive">Another column already goes here.</p>}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
        <Card className="grid gap-4 p-4 sm:grid-cols-2">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="on-match">When a row matches an existing record</Label>
            <NativeSelect
              id="on-match"
              value={options.on_match}
              onChange={(e) => setOptions({ ...options, on_match: e.target.value as Options["on_match"] })}
            >
              <option value="skip">Leave the existing record alone</option>
              <option value="update">Update it with the sheet's values</option>
              <option value="create">Create a new record anyway</option>
            </NativeSelect>
            <p className="text-xs text-muted-foreground">
              {preview.import.entity === "contact" ? "Contacts match by email or phone number." : "Records match by name."}
            </p>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="date-order">Dates like 07/03/2027 are</Label>
            <NativeSelect
              id="date-order"
              value={options.date_order ?? ""}
              onChange={(e) => setOptions({ ...options, date_order: (e.target.value || null) as Options["date_order"] })}
            >
              <option value="">As usual for this workspace</option>
              <option value="dmy">Day / month / year</option>
              <option value="mdy">Month / day / year</option>
              <option value="ymd">Year / month / day</option>
            </NativeSelect>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="import-tags">Tag every imported record with</Label>
            <Input id="import-tags" placeholder="e.g. imported, 2026 list" value={tagText} onChange={(e) => setTagText(e.target.value)} />
          </div>
          <div className="flex flex-col gap-2 text-sm">
            {preview.import.entity === "contact" && (
              <label className="flex items-center gap-2">
                <Checkbox
                  checked={options.create_organisations ?? true}
                  onChange={(e) => setOptions({ ...options, create_organisations: e.target.checked })}
                />
                Create organisations the sheet names that don't exist yet
              </label>
            )}
            <label className="flex items-center gap-2">
              <Checkbox checked={options.run_rules ?? false} onChange={(e) => setOptions({ ...options, run_rules: e.target.checked })} />
              Let automation rules react (e.g. send welcome emails)
            </label>
          </div>
        </Card>
        {missing.length > 0 && <Alert tone="info">Choose a column for {missing.map((m) => m.label).join(", ")}.</Alert>}
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={() => setPreview(null)}>
            Back
          </Button>
          <Button disabled={busy || missing.length > 0} onClick={check}>
            {busy && <Spinner />}
            Check
          </Button>
        </div>
      </Frame>
    );
  }

  return (
    <Frame step={0}>
      <Card className="flex flex-col gap-4 p-4">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="import-entity">What does the file hold?</Label>
          <NativeSelect id="import-entity" value={entity} onChange={(e) => setEntity(e.target.value as ImportEntity)}>
            <option value="auto">Work it out from the file</option>
            {Object.values(ENTITIES).map((e) => (
              <option key={e.entity} value={e.entity}>
                {e.label}
              </option>
            ))}
          </NativeSelect>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="import-file">File</Label>
          <Input
            id="import-file"
            type="file"
            accept=".csv,.tsv,.txt,.vcf,.vcard,text/csv,text/tab-separated-values,text/vcard,text/x-vcard"
            className="py-1.5"
            onChange={(e) => setFile(e.target.files?.[0] ?? null)}
          />
          <p className="text-xs text-muted-foreground">
            A CSV export from the app you are moving from, as it comes: Google Contacts, Outlook, HubSpot, Salesforce, Pipedrive,
            Mailchimp, LinkedIn or a booking system. Contact cards (.vcf) from Apple Contacts, iCloud or Android work too. For a
            spreadsheet, in Excel or Numbers choose File → Save As (or Export) → CSV UTF-8; in Google Sheets, File → Download → CSV. Up to
            10 MB and 50,000 rows.
          </p>
        </div>
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onDone}>
            Cancel
          </Button>
          <Button
            disabled={!file || busy}
            onClick={() =>
              attempt(async () => {
                if (!file) return;
                if (file.size > 100 * 1024 * 1024) throw new Error("The file is over 100 MB. Split it into smaller files.");
                const csv = stripCardAttachments(decodeFile(new Uint8Array(await file.arrayBuffer())));
                if (new TextEncoder().encode(csv).length > 10 * 1024 * 1024)
                  throw new Error("The file is over 10 MB. Split it into smaller files.");
                const p = await ok(api.POST("/api/v1/imports", { body: { entity, filename: file.name, csv } }));
                setPreview(p);
                setColumns(p.suggested);
                setDry(null);
                setEdits({});
                setSkipRows([]);
              })
            }
          >
            {busy && <Spinner />}
            Next
          </Button>
        </div>
      </Card>
    </Frame>
  );
}

function Stat({ label, value, tone }: { label: string; value: number; tone?: "warn" }) {
  return (
    <div className="rounded-md border p-3">
      <p className={`text-2xl font-semibold ${tone === "warn" ? "text-warning" : ""}`}>{value}</p>
      <p className="text-xs text-muted-foreground">{label}</p>
    </div>
  );
}

/** The rows at fault, with their cells to correct (the ones the problems
 * are about, else every imported one) or the row to leave out. */
function ProblemRows({
  dry,
  headers,
  columns,
  targets,
  edits,
  onEdit,
  skipRows,
  onSkip,
}: {
  dry: DryRun;
  headers: string[];
  columns: (string | null)[];
  targets: Schemas["ImportTarget"][];
  edits: Edits;
  onEdit: (row: number, column: number, value: string) => void;
  skipRows: number[];
  onSkip: (row: number, skip: boolean) => void;
}) {
  const label = (key: string | null | undefined) => targets.find((t) => t.key === key)?.label ?? key;
  // Rows left out no longer come back as problems, so they are listed here
  // to be put back.
  const shown = new Set(dry.problem_rows.map((p) => p.row));
  const leftOut = skipRows.filter((r) => !shown.has(r));
  return (
    <div className="flex flex-col gap-2">
      <p className="text-sm font-medium">
        {dry.fail === 0
          ? "No rows have problems now."
          : `${dry.fail} ${dry.fail === 1 ? "row needs" : "rows need"} a look. Correct them here and check again, or leave them out${
              dry.fail > dry.problem_rows.length ? ` (the first ${dry.problem_rows.length} are shown)` : ""
            }.`}
      </p>
      <ul className="flex max-h-[28rem] flex-col gap-2 overflow-y-auto">
        {dry.problem_rows.map((p) => {
          const issues = p.issues;
          const fields = new Set(issues.map((i) => i.field).filter(Boolean));
          let cols = columns.flatMap((c, i) => (c && fields.has(c) ? [i] : []));
          if (cols.length === 0) cols = columns.flatMap((c, i) => (c ? [i] : []));
          const skipped = skipRows.includes(p.row);
          return (
            <li key={p.row} className="flex flex-col gap-2 rounded-md border p-3 text-sm">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="font-medium">Row {p.row}</span>
                <label className="flex items-center gap-2 text-xs">
                  <Checkbox checked={skipped} onChange={(e) => onSkip(p.row, e.target.checked)} />
                  Leave this row out
                </label>
              </div>
              <ul className="text-destructive">
                {issues.map((i, n) => (
                  <li key={`${i.field}-${n}`}>
                    {i.field ? `${label(i.field)}: ` : ""}
                    {i.message}
                  </li>
                ))}
              </ul>
              {!skipped && (
                <div className="grid gap-2 sm:grid-cols-2">
                  {cols.map((c) => {
                    const id = `fix-${p.row}-${c}`;
                    return (
                      <div key={c} className="flex flex-col gap-1">
                        <Label htmlFor={id} className="text-xs">
                          {headers[c]} → {label(columns[c])}
                        </Label>
                        <Input
                          id={id}
                          value={edits[`${p.row}:${c}`] ?? p.cells[c] ?? ""}
                          onChange={(e) => onEdit(p.row, c, e.target.value)}
                        />
                      </div>
                    );
                  })}
                </div>
              )}
            </li>
          );
        })}
      </ul>
      {leftOut.length > 0 && (
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <span className="text-muted-foreground">Left out:</span>
          {leftOut.map((row) => (
            <Button key={row} size="sm" variant="outline" onClick={() => onSkip(row, false)} aria-label={`Put row ${row} back`}>
              Row {row} ✕
            </Button>
          ))}
        </div>
      )}
    </div>
  );
}

function Issues({ issues }: { issues: Schemas["ImportIssue"][] }) {
  return (
    <details className="rounded-md border p-3 text-sm" open={issues.length <= 10}>
      <summary className="cursor-pointer font-medium">
        {issues.length} {issues.length === 1 ? "problem" : "problems"} (those rows are left out)
      </summary>
      <ul className="mt-2 flex max-h-64 flex-col gap-1 overflow-y-auto">
        {issues.map((i, n) => (
          // Several problems can share a row and field.
          <li key={`${i.row}-${i.field}-${n}`}>
            Row {i.row}
            {i.field ? `, ${i.field}` : ""}: {i.message}
          </li>
        ))}
      </ul>
    </details>
  );
}

function ImportProgress({ view, onFinish }: { view: ImportView | undefined; onFinish: () => void }) {
  if (!view) return <Spinner />;
  const finished = view.status === "done" || view.status === "failed";
  const pct = view.row_count ? Math.round((view.processed / view.row_count) * 100) : 0;
  const list = Object.values(ENTITIES).find((e) => e.entity === view.entity);
  return (
    <Card className="flex flex-col gap-4 p-4">
      {!finished && (
        <>
          <p className="text-sm">
            Importing… {view.processed} of {view.row_count} rows. You can leave this page; it carries on.
          </p>
          <div className="h-2 overflow-hidden rounded-full bg-muted" role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100}>
            <div className="h-full bg-primary transition-all" style={{ width: `${pct}%` }} />
          </div>
        </>
      )}
      {view.status === "failed" && <Alert>The import stopped: {view.error}</Alert>}
      {finished && (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          <Stat label="Created" value={view.created} />
          <Stat label="Updated" value={view.updated} />
          <Stat label="Skipped" value={view.skipped} />
          <Stat label="Failed" value={view.failed} tone={view.failed > 0 ? "warn" : undefined} />
        </div>
      )}
      {finished && view.issues.length > 0 && <Issues issues={view.issues} />}
      {finished && (
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onFinish}>
            Back to imports
          </Button>
          {list && (
            <Button asChild>
              <Link to="/crm/$entity" params={{ entity: list.path }} onClick={onFinish}>
                See {list.label.toLowerCase()}
              </Link>
            </Button>
          )}
        </div>
      )}
    </Card>
  );
}
