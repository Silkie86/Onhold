// A record's form, drawn from its entity's schema: core fields first, then
// custom fields by group. The server is the validator; its per-field
// problems are shown under the fields they name.

import { Plus, Trash2 } from "lucide-react";
import { type ReactNode, useId, useMemo, useState } from "react";
import {
  type EntitySchema,
  type Money,
  type RecordJson,
  type SchemaField,
  fieldValue,
  moneyInputText,
  parseMoneyInput,
} from "../format.ts";
import { cn } from "../lib.ts";
import { Button } from "./button.tsx";
import { Checkbox, Input, Label, NativeSelect, Textarea } from "./inputs.tsx";
import { Alert, Spinner } from "./misc.tsx";

/** A problem the server found, by field (`emails`, `custom.budget`, `default_fee.currency`). */
export interface FieldIssue {
  field?: string | null;
  message: string;
}

/** Draws the picker for a relation field, e.g. a contact's organisation. */
export type RelationRenderer = (props: {
  field: SchemaField;
  id: string;
  value: string | null;
  onChange: (id: string | null) => void;
  record?: RecordJson;
}) => ReactNode;

export interface EntityFormProps {
  schema: EntitySchema;
  /** The record being edited; absent to create one. */
  record?: RecordJson;
  /** Values to start a new record with, by field key. */
  initial?: Record<string, unknown>;
  /** The request body: every field for a new record, only changes for an edit. */
  onSubmit: (body: Record<string, unknown>) => Promise<void>;
  onCancel?: () => void;
  issues?: FieldIssue[];
  submitLabel?: string;
  /** For money fields that do not fix their currency. */
  defaultCurrency?: string;
  renderRelation?: RelationRenderer;
  /** Fields not to show (e.g. ones a page edits elsewhere). */
  hide?: string[];
}

type Values = Record<string, unknown>;

type Address = Record<"label" | "line1" | "line2" | "city" | "region" | "postcode" | "country", string | undefined>;

const ADDRESS_PARTS: [keyof Address, string][] = [
  ["label", "Label"],
  ["line1", "Address line 1"],
  ["line2", "Address line 2"],
  ["city", "City"],
  ["region", "County / state"],
  ["postcode", "Postcode"],
  ["country", "Country"],
];

const LIST_TYPES = new Set(["tags", "email_list", "phone_list", "url_list", "multi_select", "address_list"]);

function emptyValue(field: SchemaField): unknown {
  if (LIST_TYPES.has(field.field_type)) return [];
  if (field.field_type === "boolean") return false;
  if (field.field_type === "select" && field.required) return field.options[0]?.value ?? null;
  return field.default_value ?? null;
}

function startValues(schema: EntitySchema, record?: RecordJson, initial?: Values): Values {
  const values: Values = {};
  for (const f of schema.fields) {
    const v = record ? fieldValue(record, f.key) : initial?.[f.key];
    values[f.key] = v === undefined || (v === null && !record) ? emptyValue(f) : v;
  }
  return values;
}

/** `datetime-local` text for an ISO instant, in the browser's timezone. */
function toLocalInput(iso: unknown): string {
  if (typeof iso !== "string" || !iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function listText(value: unknown, bySpaces = false): string {
  return Array.isArray(value) ? value.join(bySpaces ? " " : ", ") : "";
}

/** Items split on commas, or on spaces for web addresses, which may hold commas but never spaces. */
export function parseList(text: string, bySpaces = false): string[] {
  return text
    .split(bySpaces ? /\s+/ : /[,\n]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
}

/** The request body for `values`: custom fields under `custom`, edits only what changed. */
export function formBody(schema: EntitySchema, values: Values, record?: RecordJson): Record<string, unknown> {
  const body: Record<string, unknown> = {};
  const custom: Record<string, unknown> = {};
  for (const f of schema.fields) {
    if (f.read_only) continue;
    let v = values[f.key];
    if (v === "") v = null;
    if (record && same(v, fieldValue(record, f.key))) continue;
    if (!record && (v === null || (Array.isArray(v) && v.length === 0))) continue;
    if (f.key.startsWith("custom.")) custom[f.key.slice(7)] = v;
    else body[f.key] = v;
  }
  if (Object.keys(custom).length > 0) body.custom = custom;
  return body;
}

function issuesFor(issues: FieldIssue[] | undefined, key: string): string[] {
  return (issues ?? [])
    .filter((i) => i.field && (i.field === key || i.field.startsWith(`${key}.`) || i.field.startsWith(`${key}[`)))
    .map((i) => i.message);
}

export function EntityForm(props: EntityFormProps) {
  const { schema, record } = props;
  const [values, setValues] = useState<Values>(() => startValues(schema, record, props.initial));
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const formId = useId();
  const editable = schema.can_write;

  const groups = useMemo(() => {
    const hidden = new Set(props.hide ?? []);
    const shown = schema.fields.filter((f) => !hidden.has(f.key) && !(f.read_only && !record));
    const out = new Map<string, SchemaField[]>();
    for (const f of shown) {
      const group = f.group ?? (f.core ? "" : "More");
      out.set(group, [...(out.get(group) ?? []), f]);
    }
    return [...out.entries()];
  }, [schema, props.hide, record]);

  // Problems that name no field shown here go above the buttons.
  const shownKeys = groups.flatMap(([, fs]) => fs.map((f) => f.key));
  const general = (props.issues ?? []).filter(
    (i) => !i.field || !shownKeys.some((k) => issuesFor([i], k).length > 0),
  );

  const set = (key: string, value: unknown) => setValues((v) => ({ ...v, [key]: value }));

  const submit = async () => {
    setSubmitting(true);
    setError(null);
    try {
      await props.onSubmit(formBody(schema, values, record));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  const control = (f: SchemaField, id: string): ReactNode => {
    const value = values[f.key];
    const disabled = !editable || f.read_only;
    const invalid = issuesFor(props.issues, f.key).length > 0 || undefined;
    const common = { id, disabled, "aria-invalid": invalid, "aria-describedby": `${id}-help` };
    switch (f.field_type) {
      case "long_text":
        return <Textarea {...common} value={String(value ?? "")} onChange={(e) => set(f.key, e.target.value)} />;
      case "number":
      case "percent":
        return (
          <Input
            {...common}
            type="number"
            step="any"
            min={f.field_type === "percent" ? 0 : undefined}
            max={f.field_type === "percent" ? 100 : undefined}
            value={value === null || value === undefined ? "" : String(value)}
            onChange={(e) => set(f.key, e.target.value === "" ? null : Number(e.target.value))}
          />
        );
      case "money":
        return (
          <MoneyInput
            {...common}
            value={(value as Money | null) ?? null}
            currency={f.currency ?? undefined}
            defaultCurrency={props.defaultCurrency ?? "GBP"}
            onChange={(m) => set(f.key, m)}
          />
        );
      case "currency":
        return (
          <Input
            {...common}
            maxLength={3}
            className="w-24 uppercase"
            value={String(value ?? "")}
            onChange={(e) => set(f.key, e.target.value.toUpperCase())}
          />
        );
      case "date":
        return <Input {...common} type="date" value={String(value ?? "")} onChange={(e) => set(f.key, e.target.value || null)} />;
      case "datetime":
        return (
          <Input
            {...common}
            type="datetime-local"
            value={toLocalInput(value)}
            onChange={(e) => set(f.key, e.target.value ? new Date(e.target.value).toISOString() : null)}
          />
        );
      case "boolean":
        return <Checkbox {...common} checked={value === true} onChange={(e) => set(f.key, e.target.checked)} />;
      case "select":
        return (
          <NativeSelect {...common} value={String(value ?? "")} onChange={(e) => set(f.key, e.target.value || null)}>
            {!f.required && <option value="">—</option>}
            {f.options.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </NativeSelect>
        );
      case "multi_select": {
        const chosen = Array.isArray(value) ? (value as string[]) : [];
        return (
          <div className="flex flex-wrap gap-x-4 gap-y-1" id={id} role="group">
            {f.options.map((o) => (
              <label key={o.value} className="flex items-center gap-2 text-sm">
                <Checkbox
                  disabled={disabled}
                  checked={chosen.includes(o.value)}
                  onChange={(e) =>
                    set(f.key, e.target.checked ? [...chosen, o.value] : chosen.filter((c) => c !== o.value))
                  }
                />
                {o.label}
              </label>
            ))}
          </div>
        );
      }
      case "tags":
      case "email_list":
      case "phone_list":
        return <ListInput {...common} value={value} onChange={(list) => set(f.key, list)} />;
      case "url_list":
        return <ListInput {...common} bySpaces value={value} onChange={(list) => set(f.key, list)} />;
      case "address_list":
        return (
          <AddressList id={id} disabled={disabled} value={(value as Address[] | null) ?? []} onChange={(a) => set(f.key, a)} />
        );
      case "relation":
        if (props.renderRelation) {
          return props.renderRelation({
            field: f,
            id,
            value: (value as string | null) ?? null,
            onChange: (v) => set(f.key, v),
            record,
          });
        }
        return <Input {...common} value={String(value ?? "")} onChange={(e) => set(f.key, e.target.value || null)} />;
      case "file":
        return <p className="text-sm text-muted-foreground">Files can be attached once documents arrive.</p>;
      default:
        return (
          <Input
            {...common}
            type={f.field_type === "email" ? "email" : f.field_type === "url" ? "url" : f.field_type === "phone" ? "tel" : "text"}
            value={String(value ?? "")}
            onChange={(e) => set(f.key, e.target.value)}
          />
        );
    }
  };

  return (
    <form
      className="flex flex-col gap-6"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
      noValidate
    >
      {groups.map(([group, fields]) => (
        <fieldset key={group || "core"} className="grid gap-4 sm:grid-cols-2">
          {group && <legend className="col-span-full mb-1 text-sm font-semibold">{group}</legend>}
          {fields.map((f) => {
            const id = `${formId}-${f.key}`;
            const wide = ["long_text", "address_list", "multi_select"].includes(f.field_type);
            const problems = issuesFor(props.issues, f.key);
            return (
              <div key={f.key} className={cn("flex flex-col gap-1.5", wide && "sm:col-span-2", f.field_type === "boolean" && "flex-row-reverse items-center justify-end gap-2")}>
                <Label htmlFor={id}>
                  {f.label}
                  {f.required && <span className="text-destructive"> *</span>}
                </Label>
                {control(f, id)}
                <div id={`${id}-help`} className={cn(f.field_type === "boolean" && "hidden")}>
                  {f.help_text && <p className="text-xs text-muted-foreground">{f.help_text}</p>}
                  {problems.map((m) => (
                    <p key={m} className="text-xs text-destructive">
                      {m}
                    </p>
                  ))}
                </div>
              </div>
            );
          })}
        </fieldset>
      ))}
      {general.map((i) => (
        <Alert key={`${i.field}-${i.message}`}>{i.field ? `${i.field}: ${i.message}` : i.message}</Alert>
      ))}
      {error && <Alert>{error}</Alert>}
      {editable && (
        <div className="flex justify-end gap-2">
          {props.onCancel && (
            <Button variant="ghost" onClick={props.onCancel}>
              Cancel
            </Button>
          )}
          <Button type="submit" disabled={submitting}>
            {submitting && <Spinner />}
            {props.submitLabel ?? (record ? "Save" : "Create")}
          </Button>
        </div>
      )}
    </form>
  );
}

export function MoneyInput({
  id,
  value,
  currency,
  defaultCurrency,
  onChange,
  disabled,
  ...rest
}: {
  id?: string;
  value: Money | null;
  /** Fixed by the field; otherwise chosen beside the amount. */
  currency?: string;
  defaultCurrency: string;
  onChange: (m: Money | null) => void;
  disabled?: boolean;
  "aria-invalid"?: boolean;
  "aria-describedby"?: string;
}) {
  const [text, setText] = useState(moneyInputText(value));
  const code = currency ?? value?.currency ?? defaultCurrency;
  const [chosen, setChosen] = useState(code);
  const update = (t: string, c: string) => {
    const minor = parseMoneyInput(t, c);
    onChange(minor === null ? null : { minor, currency: c });
  };
  return (
    <div className="flex gap-2">
      <Input
        id={id}
        inputMode="decimal"
        disabled={disabled}
        value={text}
        onChange={(e) => {
          setText(e.target.value);
          update(e.target.value, currency ?? chosen);
        }}
        {...rest}
      />
      {currency ? (
        <span className="self-center text-sm text-muted-foreground">{currency}</span>
      ) : (
        <Input
          aria-label="Currency"
          className="w-20 uppercase"
          maxLength={3}
          disabled={disabled}
          value={chosen}
          onChange={(e) => {
            const c = e.target.value.toUpperCase();
            setChosen(c);
            if (c.length === 3) update(text, c);
          }}
        />
      )}
    </div>
  );
}

function ListInput({
  value,
  onChange,
  bySpaces = false,
  ...rest
}: {
  id: string;
  value: unknown;
  onChange: (list: string[]) => void;
  bySpaces?: boolean;
  disabled?: boolean;
  "aria-invalid"?: boolean;
  "aria-describedby"?: string;
}) {
  const [text, setText] = useState(listText(value, bySpaces));
  return (
    <Input
      {...rest}
      placeholder={bySpaces ? "Separate with spaces" : "Separate with commas"}
      value={text}
      onChange={(e) => {
        setText(e.target.value);
        onChange(parseList(e.target.value, bySpaces));
      }}
    />
  );
}

function AddressList({
  id,
  value,
  onChange,
  disabled,
}: {
  id: string;
  value: Address[];
  onChange: (a: Address[]) => void;
  disabled?: boolean;
}) {
  const setPart = (i: number, part: keyof Address, text: string) =>
    onChange(value.map((a, j) => (j === i ? { ...a, [part]: text || undefined } : a)));
  return (
    <div id={id} className="flex flex-col gap-3">
      {value.map((a, i) => (
        // Addresses have no ids; their position is their identity while editing.
        <div key={i} className="grid gap-2 rounded-md border p-3 sm:grid-cols-2">
          {ADDRESS_PARTS.map(([part, label]) => (
            <Input
              key={part}
              aria-label={`${label} (address ${i + 1})`}
              placeholder={label}
              disabled={disabled}
              value={a[part] ?? ""}
              onChange={(e) => setPart(i, part, e.target.value)}
            />
          ))}
          {!disabled && (
            <Button variant="ghost" size="sm" className="justify-self-start" onClick={() => onChange(value.filter((_, j) => j !== i))}>
              <Trash2 /> Remove
            </Button>
          )}
        </div>
      ))}
      {!disabled && (
        <Button variant="outline" size="sm" className="self-start" onClick={() => onChange([...value, {} as Address])}>
          <Plus /> Add address
        </Button>
      )}
    </div>
  );
}
