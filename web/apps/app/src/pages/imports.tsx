// Bringing records in from a spreadsheet: pick a file, match its columns to
// fields, check what would happen, then let the worker do it.

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
          <Plus /> Import a spreadsheet
        </Button>
      </div>
      {imports.isPending && <Spinner />}
      {imports.data?.length === 0 && (
        <Card className="flex flex-col items-center gap-3 p-8 text-center">
          <FileUp className="size-8 text-muted-foreground" />
          <p className="font-medium">Bring in contacts, organisations or talent from a spreadsheet</p>
          <p className="max-w-md text-sm text-muted-foreground">
            Save it as CSV from Excel, Numbers or Google Sheets. You will match its columns to fields and see what will happen before
            anything is saved.
          </p>
          <Button onClick={() => setWizard(true)}>Import a spreadsheet</Button>
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
      <h1 className="text-xl font-semibold">Import a spreadsheet</h1>
      <Steps step={step} />
      {children}
    </div>
  );
}

export function ImportWizard({ onDone }: { onDone: () => void }) {
  const qc = useQueryClient();
  const [entity, setEntity] = useState<EntityKey>("contact");
  const [file, setFile] = useState<File | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [columns, setColumns] = useState<(string | null)[]>([]);
  const [options, setOptions] = useState<Options>({ on_match: "skip", create_organisations: true, run_rules: false, tags: [] });
  const [tagText, setTagText] = useState("");
  const [dry, setDry] = useState<DryRun | null>(null);
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
    options: { ...options, tags: tagText.split(",").map((t) => t.trim()).filter(Boolean) },
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
          {dry.issues.length > 0 && <Issues issues={dry.issues} />}
          {error && <Alert>{error}</Alert>}
          <div className="flex flex-wrap justify-end gap-2">
            <Button variant="ghost" onClick={() => setDry(null)}>
              Back
            </Button>
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
    return (
      <Frame step={1}>
        <p className="text-sm text-muted-foreground">
          {preview.import.filename} has {preview.import.row_count} rows. Choose the field each column goes into; we have guessed where we
          could.
        </p>
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
              {entity === "contact" ? "Contacts match by email or phone number." : "Records match by name."}
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
            {entity === "contact" && (
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
          <Button
            disabled={busy || missing.length > 0}
            onClick={() =>
              attempt(async () => {
                setDry(await ok(api.POST("/api/v1/imports/{id}/dry-run", { params: { path: { id: preview.import.id } }, body: plan() })));
              })
            }
          >
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
          <Label htmlFor="import-entity">What does the spreadsheet hold?</Label>
          <NativeSelect id="import-entity" value={entity} onChange={(e) => setEntity(e.target.value as EntityKey)}>
            {Object.values(ENTITIES).map((e) => (
              <option key={e.entity} value={e.entity}>
                {e.label}
              </option>
            ))}
          </NativeSelect>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="import-file">CSV file</Label>
          <Input
            id="import-file"
            type="file"
            accept=".csv,.tsv,.txt,text/csv,text/tab-separated-values"
            className="py-1.5"
            onChange={(e) => setFile(e.target.files?.[0] ?? null)}
          />
          <p className="text-xs text-muted-foreground">
            In Excel or Numbers choose File → Save As (or Export) → CSV UTF-8. In Google Sheets, File → Download → CSV. Up to 10 MB and
            50,000 rows.
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
                if (file.size > 10 * 1024 * 1024) throw new Error("The file is over 10 MB. Split it into smaller files.");
                const csv = await file.text();
                const p = await ok(api.POST("/api/v1/imports", { body: { entity, filename: file.name, csv } }));
                setPreview(p);
                setColumns(p.suggested);
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
