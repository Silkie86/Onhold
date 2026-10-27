// Workspace configuration: the custom fields each kind of record has, and
// the pipelines records move through.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { Schemas } from "@onhold/api-client";
import { Alert, Badge, Button, Checkbox, Dialog, type FieldIssue, Input, Label, NativeSelect, Spinner, Textarea } from "@onhold/ui";
import { ArrowDown, ArrowUp, Archive, Pencil, Plus, Trash2 } from "lucide-react";
import { type FormEvent, type ReactNode, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { useSession } from "../session.tsx";
import { failureIssues } from "./crm.tsx";

type Field = Schemas["FieldView"];
type FieldType = Schemas["FieldType"];
type Pipeline = Schemas["PipelineView"];
type Outcome = Schemas["Outcome"];

const FIELD_ENTITIES = [
  { entity: "contact", label: "Contacts" },
  { entity: "organisation", label: "Organisations" },
  { entity: "talent", label: "Talent" },
];

// Bookings and enquiries join when they arrive; `thing` is the engines' test entity.
const PIPELINE_ENTITIES = [{ entity: "thing", label: "Things (test records)" }];

const TYPES: { value: FieldType; label: string }[] = [
  { value: "text", label: "Text" },
  { value: "long_text", label: "Long text" },
  { value: "number", label: "Number" },
  { value: "money", label: "Money" },
  { value: "date", label: "Date" },
  { value: "datetime", label: "Date and time" },
  { value: "boolean", label: "Yes / no" },
  { value: "select", label: "Choice" },
  { value: "multi_select", label: "Several choices" },
  { value: "url", label: "Web address" },
  { value: "email", label: "Email" },
  { value: "phone", label: "Phone" },
];

/** A label as a key: lower case, letters, digits and underscores. */
export function keyFrom(label: string): string {
  const key = label
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .slice(0, 50);
  return /^[a-z]/.test(key) ? key : key ? `f_${key}` : "";
}

function canConfigure(role: string) {
  return role === "owner" || role === "admin";
}

function Tabs<T extends string>({ value, onChange, items }: { value: T; onChange: (v: T) => void; items: { entity: T; label: string }[] }) {
  return (
    <div role="tablist" className="flex gap-1 overflow-x-auto border-b">
      {items.map((i) => (
        <button
          key={i.entity}
          role="tab"
          type="button"
          aria-selected={value === i.entity}
          onClick={() => onChange(i.entity)}
          className={`border-b-2 px-3 py-2 text-sm whitespace-nowrap ${value === i.entity ? "border-primary font-medium" : "border-transparent text-muted-foreground"}`}
        >
          {i.label}
        </button>
      ))}
    </div>
  );
}

function Row({ children }: { children: ReactNode }) {
  return <div className="flex flex-col gap-1.5">{children}</div>;
}

function issueText(issues: FieldIssue[], field: string): string | undefined {
  return issues.find((i) => i.field === field || i.field?.startsWith(`${field}.`) || i.field?.startsWith(`${field}[`))?.message;
}

export function FieldsPage() {
  const { workspace } = useSession();
  const qc = useQueryClient();
  const [entity, setEntity] = useState("contact");
  const [editing, setEditing] = useState<Field | "new" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const fields = useQuery({
    queryKey: ["fields", entity],
    queryFn: () => ok(api.GET("/api/v1/custom-fields", { params: { query: { entity } } })),
  });
  const writable = canConfigure(workspace.role);
  const refresh = async () => {
    await qc.invalidateQueries({ queryKey: ["fields", entity] });
    await qc.invalidateQueries({ queryKey: ["schema", entity] });
  };
  const archive = async (f: Field) => {
    if (!window.confirm(`Hide “${f.label}”? Values already entered are kept and come back if you restore it.`)) return;
    try {
      await ok(api.DELETE("/api/v1/custom-fields/{id}", { params: { path: { id: f.id } } }));
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex items-center justify-between gap-2">
        <h1 className="text-xl font-semibold">Custom fields</h1>
        {writable && (
          <Button onClick={() => setEditing("new")}>
            <Plus /> Add field
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">
        Extra details your team records. They appear in forms and as table columns straight away.
      </p>
      <Tabs value={entity} onChange={setEntity} items={FIELD_ENTITIES} />
      {error && <Alert>{error}</Alert>}
      {fields.isPending && <Spinner />}
      {fields.data?.length === 0 && <p className="py-6 text-center text-sm text-muted-foreground">No custom fields yet.</p>}
      {fields.data && fields.data.length > 0 && (
        <ul className="divide-y rounded-md border">
          {fields.data.map((f) => (
            <li key={f.id} className="flex flex-wrap items-center justify-between gap-2 px-3 py-2">
              <div className="min-w-0">
                <p className="font-medium">
                  {f.label} {f.required && <Badge>Required</Badge>} {f.searchable && <Badge>Searchable</Badge>}
                </p>
                <p className="text-xs text-muted-foreground">
                  {TYPES.find((t) => t.value === f.field_type)?.label ?? f.field_type} · <code>{f.key}</code>
                  {f.group && ` · ${f.group}`}
                </p>
              </div>
              {writable && (
                <div className="flex gap-1">
                  <Button variant="ghost" size="icon" aria-label={`Edit ${f.label}`} onClick={() => setEditing(f)}>
                    <Pencil />
                  </Button>
                  <Button variant="ghost" size="icon" aria-label={`Hide ${f.label}`} onClick={() => void archive(f)}>
                    <Archive />
                  </Button>
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
      {editing && (
        <FieldDialog
          entity={entity}
          field={editing === "new" ? undefined : editing}
          onClose={() => setEditing(null)}
          onSaved={async () => {
            setEditing(null);
            await refresh();
          }}
        />
      )}
    </div>
  );
}

function optionsText(f?: Field): string {
  return (f?.validation.options ?? []).map((o) => o.label).join("\n");
}

function FieldDialog({ entity, field, onClose, onSaved }: { entity: string; field?: Field; onClose: () => void; onSaved: () => Promise<void> }) {
  const [label, setLabel] = useState(field?.label ?? "");
  const [key, setKey] = useState(field?.key ?? "");
  const [keyTouched, setKeyTouched] = useState(false);
  const [type, setType] = useState<FieldType>(field?.field_type ?? "text");
  const [required, setRequired] = useState(field?.required ?? false);
  const [searchable, setSearchable] = useState(field?.searchable ?? false);
  const [group, setGroup] = useState(field?.group ?? "");
  const [help, setHelp] = useState(field?.help_text ?? "");
  const [options, setOptions] = useState(optionsText(field));
  const [currency, setCurrency] = useState(field?.validation.currency ?? "");
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const choice = type === "select" || type === "multi_select";

  const validation = () => {
    const v: Schemas["Validation"] = { ...(field?.validation ?? {}), options: [] };
    if (choice) {
      const existing = field?.validation.options ?? [];
      v.options = options
        .split("\n")
        .map((l) => l.trim())
        .filter(Boolean)
        .map((l) => ({ label: l, value: existing.find((o) => o.label === l)?.value ?? keyFrom(l) }));
    }
    if (type === "money") v.currency = currency.trim().toUpperCase() || null;
    return v;
  };

  const save = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setIssues([]);
    setError(null);
    try {
      if (field) {
        await ok(
          api.PATCH("/api/v1/custom-fields/{id}", {
            params: { path: { id: field.id } },
            headers: { "If-Match": `"${field.version}"` },
            body: { label, required, searchable, group: group || null, help_text: help || null, validation: validation() },
          }),
        );
      } else {
        await ok(
          api.POST("/api/v1/custom-fields", {
            body: {
              entity,
              key,
              label,
              field_type: type,
              required,
              searchable,
              group: group || null,
              help_text: help || null,
              validation: validation(),
            },
          }),
        );
      }
      await onSaved();
    } catch (err) {
      const found = failureIssues(err);
      if (found) setIssues(found);
      else setError(err instanceof ApiFailure && err.status === 412 ? "Someone else changed this field. Close and try again." : String((err as Error).message));
    } finally {
      setBusy(false);
    }
  };

  const problem = (f: string) => {
    const m = issueText(issues, f);
    return m && <p className="text-xs text-destructive">{m}</p>;
  };
  const shown = new Set(["label", "key", "field_type", "validation", "group", "help_text"]);
  const other = issues.filter((i) => !i.field || ![...shown].some((s) => i.field?.startsWith(s)));

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={field ? `Edit ${field.label}` : "Add a field"}>
      <form className="flex flex-col gap-4" onSubmit={save}>
        <Row>
          <Label htmlFor="field-label">Label</Label>
          <Input
            id="field-label"
            autoFocus
            required
            value={label}
            onChange={(e) => {
              setLabel(e.target.value);
              if (!field && !keyTouched) setKey(keyFrom(e.target.value));
            }}
          />
          {problem("label")}
        </Row>
        {!field && (
          <>
            <Row>
              <Label htmlFor="field-key">Key (for templates and the API)</Label>
              <Input
                id="field-key"
                value={key}
                onChange={(e) => {
                  setKeyTouched(true);
                  setKey(e.target.value);
                }}
              />
              {problem("key")}
            </Row>
            <Row>
              <Label htmlFor="field-type">Type</Label>
              <NativeSelect id="field-type" value={type} onChange={(e) => setType(e.target.value as FieldType)}>
                {TYPES.map((t) => (
                  <option key={t.value} value={t.value}>
                    {t.label}
                  </option>
                ))}
              </NativeSelect>
            </Row>
          </>
        )}
        {choice && (
          <Row>
            <Label htmlFor="field-options">Choices, one per line</Label>
            <Textarea id="field-options" value={options} onChange={(e) => setOptions(e.target.value)} />
            {problem("validation")}
          </Row>
        )}
        {type === "money" && (
          <Row>
            <Label htmlFor="field-currency">Currency (leave empty to choose per record)</Label>
            <Input id="field-currency" maxLength={3} className="w-24 uppercase" value={currency} onChange={(e) => setCurrency(e.target.value)} />
            {problem("validation")}
          </Row>
        )}
        <div className="grid gap-4 sm:grid-cols-2">
          <Row>
            <Label htmlFor="field-group">Section (optional)</Label>
            <Input id="field-group" placeholder="e.g. Hospitality" value={group} onChange={(e) => setGroup(e.target.value)} />
          </Row>
          <Row>
            <Label htmlFor="field-help">Help text (optional)</Label>
            <Input id="field-help" value={help} onChange={(e) => setHelp(e.target.value)} />
          </Row>
        </div>
        <label className="flex items-center gap-2 text-sm">
          <Checkbox checked={required} onChange={(e) => setRequired(e.target.checked)} />
          Required
        </label>
        <label className="flex items-center gap-2 text-sm">
          <Checkbox checked={searchable} onChange={(e) => setSearchable(e.target.checked)} />
          Searchable (search finds records by this field)
        </label>
        {other.map((i) => (
          <Alert key={`${i.field}${i.message}`}>{i.field ? `${i.field}: ${i.message}` : i.message}</Alert>
        ))}
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy || !label.trim() || (!field && !key)}>
            {busy && <Spinner />}
            {field ? "Save" : "Add field"}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

export function PipelinesPage() {
  const { workspace } = useSession();
  const qc = useQueryClient();
  const [entity, setEntity] = useState("thing");
  const [editing, setEditing] = useState<Pipeline | "new" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pipelines = useQuery({
    queryKey: ["pipelines", entity],
    queryFn: () => ok(api.GET("/api/v1/pipelines", { params: { query: { entity } } })),
  });
  const writable = canConfigure(workspace.role);
  const remove = async (p: Pipeline) => {
    if (!window.confirm(`Delete the ${p.name} pipeline?`)) return;
    try {
      await ok(api.DELETE("/api/v1/pipelines/{id}", { params: { path: { id: p.id } } }));
      await qc.invalidateQueries({ queryKey: ["pipelines", entity] });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex items-center justify-between gap-2">
        <h1 className="text-xl font-semibold">Pipelines</h1>
        {writable && (
          <Button onClick={() => setEditing("new")}>
            <Plus /> Add pipeline
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">The stages a record moves through, in order. Enquiries and bookings get theirs as they arrive.</p>
      <Tabs value={entity} onChange={setEntity} items={PIPELINE_ENTITIES} />
      {error && <Alert>{error}</Alert>}
      {pipelines.isPending && <Spinner />}
      {pipelines.data?.length === 0 && <p className="py-6 text-center text-sm text-muted-foreground">No pipelines yet.</p>}
      {pipelines.data?.map((p) => (
        <div key={p.id} className="flex flex-col gap-2 rounded-md border p-3">
          <div className="flex items-center justify-between gap-2">
            <p className="font-medium">
              {p.name} {p.is_default && <Badge>Default</Badge>}
            </p>
            {writable && (
              <div className="flex gap-1">
                <Button variant="ghost" size="icon" aria-label={`Edit ${p.name}`} onClick={() => setEditing(p)}>
                  <Pencil />
                </Button>
                <Button variant="ghost" size="icon" aria-label={`Delete ${p.name}`} onClick={() => void remove(p)}>
                  <Trash2 />
                </Button>
              </div>
            )}
          </div>
          <ol className="flex flex-wrap gap-2 text-sm">
            {p.stages
              .filter((s) => !s.archived)
              .map((s) => (
                <li key={s.id} className="rounded-full border px-3 py-0.5">
                  {s.name}
                  {s.outcome !== "open" && <span className="text-muted-foreground"> ({s.outcome})</span>}
                </li>
              ))}
          </ol>
        </div>
      ))}
      {editing && (
        <PipelineDialog
          entity={entity}
          pipeline={editing === "new" ? undefined : editing}
          onClose={() => setEditing(null)}
          onSaved={async () => {
            setEditing(null);
            await qc.invalidateQueries({ queryKey: ["pipelines", entity] });
          }}
        />
      )}
    </div>
  );
}

type StageDraft = { key: string; name: string; outcome: Outcome; keyTouched: boolean };

function PipelineDialog({
  entity,
  pipeline,
  onClose,
  onSaved,
}: {
  entity: string;
  pipeline?: Pipeline;
  onClose: () => void;
  onSaved: () => Promise<void>;
}) {
  const [name, setName] = useState(pipeline?.name ?? "");
  const [isDefault, setIsDefault] = useState(pipeline?.is_default ?? false);
  const [stages, setStages] = useState<StageDraft[]>(
    pipeline?.stages
      .filter((s) => !s.archived)
      .map((s) => ({ key: s.key, name: s.name, outcome: s.outcome, keyTouched: true })) ?? [
      { key: "new", name: "New", outcome: "open", keyTouched: false },
      { key: "won", name: "Won", outcome: "won", keyTouched: false },
      { key: "lost", name: "Lost", outcome: "lost", keyTouched: false },
    ],
  );
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const kept = new Map((pipeline?.stages ?? []).map((s) => [s.key, s]));

  const update = (i: number, patch: Partial<StageDraft>) => setStages(stages.map((s, j) => (j === i ? { ...s, ...patch } : s)));
  const move = (i: number, by: number) => {
    const next = [...stages];
    const [s] = next.splice(i, 1);
    if (s) next.splice(i + by, 0, s);
    setStages(next);
  };

  const save = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setIssues([]);
    setError(null);
    // Keep each kept stage's other settings (rules on moving, SLAs).
    const body = stages.map((s) => {
      const old = kept.get(s.key);
      return {
        key: s.key,
        name: s.name,
        outcome: s.outcome,
        allowed_next: old?.allowed_next ?? null,
        required_fields: old?.required_fields ?? [],
        sla_days: old?.sla_days ?? null,
        colour: old?.colour ?? null,
      };
    });
    try {
      if (pipeline) {
        await ok(
          api.PATCH("/api/v1/pipelines/{id}", {
            params: { path: { id: pipeline.id } },
            headers: { "If-Match": `"${pipeline.version}"` },
            body: { name, stages: body, make_default: isDefault && !pipeline.is_default },
          }),
        );
      } else {
        await ok(api.POST("/api/v1/pipelines", { body: { entity, name, is_default: isDefault, stages: body } }));
      }
      await onSaved();
    } catch (err) {
      const found = failureIssues(err);
      if (found) setIssues(found);
      else setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={pipeline ? `Edit ${pipeline.name}` : "Add a pipeline"} className="max-w-2xl">
      <form className="flex flex-col gap-4" onSubmit={save}>
        <Row>
          <Label htmlFor="pipeline-name">Name</Label>
          <Input id="pipeline-name" autoFocus required value={name} onChange={(e) => setName(e.target.value)} />
        </Row>
        <label className="flex items-center gap-2 text-sm">
          <Checkbox checked={isDefault} disabled={pipeline?.is_default} onChange={(e) => setIsDefault(e.target.checked)} />
          New records start in this pipeline
        </label>
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-1 text-sm font-medium">Stages, in order</legend>
          {stages.map((s, i) => (
            // Stages are edited in place; the position is the identity until saved.
            <div key={i} className="flex flex-wrap items-start gap-2 rounded-md border p-2">
              <div className="flex min-w-40 flex-1 flex-col gap-1">
                <Input
                  aria-label={`Stage ${i + 1} name`}
                  value={s.name}
                  onChange={(e) => update(i, { name: e.target.value, key: s.keyTouched ? s.key : keyFrom(e.target.value) })}
                />
                {issueText(issues, `stages[${i}]`) && <p className="text-xs text-destructive">{issueText(issues, `stages[${i}]`)}</p>}
              </div>
              <NativeSelect
                aria-label={`Stage ${i + 1} outcome`}
                className="w-auto"
                value={s.outcome}
                onChange={(e) => update(i, { outcome: e.target.value as Outcome })}
              >
                <option value="open">In progress</option>
                <option value="won">Won</option>
                <option value="lost">Lost</option>
              </NativeSelect>
              <div className="flex">
                <Button variant="ghost" size="icon" aria-label="Move up" disabled={i === 0} onClick={() => move(i, -1)}>
                  <ArrowUp />
                </Button>
                <Button variant="ghost" size="icon" aria-label="Move down" disabled={i === stages.length - 1} onClick={() => move(i, 1)}>
                  <ArrowDown />
                </Button>
                <Button variant="ghost" size="icon" aria-label="Remove stage" onClick={() => setStages(stages.filter((_, j) => j !== i))}>
                  <Trash2 />
                </Button>
              </div>
            </div>
          ))}
          <Button
            variant="outline"
            size="sm"
            className="self-start"
            onClick={() => setStages([...stages, { key: "", name: "", outcome: "open", keyTouched: false }])}
          >
            <Plus /> Add stage
          </Button>
        </fieldset>
        {issues
          .filter((i) => !i.field?.startsWith("stages["))
          .map((i) => (
            <Alert key={`${i.field}${i.message}`}>{i.field ? `${i.field}: ${i.message}` : i.message}</Alert>
          ))}
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy || !name.trim() || stages.length === 0}>
            {busy && <Spinner />}
            Save
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
