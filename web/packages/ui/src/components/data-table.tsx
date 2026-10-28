// A list of records drawn from an entity's schema: server-side search, sort,
// tag filter and paging, a column chooser, saved views and bulk actions.
// It holds no data itself; the page fetches with `state` and passes rows in.

import { ArrowDown, ArrowUp, ChevronLeft, ChevronRight, Columns3, Search } from "lucide-react";
import { type ReactNode, useEffect, useId, useMemo, useState } from "react";
import { type EntitySchema, type RecordJson, type SchemaField, fieldValue, formatValue } from "../format.ts";
import { cn } from "../lib.ts";
import { Button } from "./button.tsx";
import { Dialog } from "./dialog.tsx";
import { Checkbox, Input, Label, NativeSelect } from "./inputs.tsx";
import { Menu, MenuCheckboxItem, MenuContent, MenuLabel, MenuTrigger } from "./menu.tsx";
import { Alert, Badge, EmptyState, Spinner, StatusDot } from "./misc.tsx";

export interface TableState {
  q: string;
  /** A field's `sort` key, `-` first for descending; `null` for the default. */
  sort: string | null;
  tag: string | null;
  offset: number;
  limit: number;
  /** Field keys shown, in order; `null` for the schema's defaults. */
  columns: string[] | null;
}

export const defaultTableState: TableState = { q: "", sort: null, tag: null, offset: 0, limit: 50, columns: null };

/** What a saved view stores: everything about the table except the paging. */
export type ViewConfig = Pick<TableState, "q" | "sort" | "tag" | "columns" | "limit">;

export function viewConfig(state: TableState): ViewConfig {
  const { q, sort, tag, columns, limit } = state;
  return { q, sort, tag, columns, limit };
}

export function stateFromView(config: Partial<ViewConfig>): TableState {
  return { ...defaultTableState, ...config, offset: 0 };
}

export interface SavedViewSummary {
  id: string;
  name: string;
  shared: boolean;
  config: Partial<ViewConfig>;
}

export interface BulkAction {
  label: string;
  destructive?: boolean;
  /** Asks for tags before running. */
  needsTags?: boolean;
  run: (ids: string[], tags: string[]) => Promise<string | void>;
}

export interface DataTableProps<Row extends RecordJson> {
  schema: EntitySchema;
  rows: Row[];
  total: number;
  loading?: boolean;
  error?: string | null;
  state: TableState;
  onStateChange: (state: TableState) => void;
  /** Wraps a row's name in a link to the record. */
  rowLink?: (row: Row, children: ReactNode) => ReactNode;
  bulkActions?: BulkAction[];
  tags?: { tag: string; count: number }[];
  views?: SavedViewSummary[];
  activeViewId?: string | null;
  onSelectView?: (view: SavedViewSummary | null) => void;
  onSaveView?: (name: string, shared: boolean) => Promise<void>;
  canShareViews?: boolean;
  toolbar?: ReactNode;
  locale?: string;
}

export function defaultColumns(schema: EntitySchema): string[] {
  return schema.fields.filter((f) => f.in_list).map((f) => f.key);
}

function useDebounced<T>(value: T, ms: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return debounced;
}

function Cell({ field, row, locale }: { field: SchemaField; row: RecordJson; locale?: string }) {
  const value = fieldValue(row, field.key);
  if (field.field_type === "tags" && Array.isArray(value)) {
    return (
      <span className="flex flex-wrap gap-1">
        {value.map((t) => (
          <Badge key={String(t)}>{String(t)}</Badge>
        ))}
      </span>
    );
  }
  if (field.field_type === "relation" && field.key.endsWith("_id")) {
    // The API sends the related record's name beside its id.
    const name = row[`${field.key.slice(0, -3)}_name`];
    if (typeof name === "string") return <>{name}</>;
  }
  if (field.field_type === "select" && typeof value === "string") {
    const option = field.options.find((o) => o.value === value);
    if (option?.colour) return <StatusDot colour={option.colour} label={option.label} />;
  }
  if (
    (field.field_type === "email_list" || field.field_type === "phone_list" || field.field_type === "url_list") &&
    Array.isArray(value)
  ) {
    const [first, ...rest] = value as string[];
    return (
      <>
        {first}
        {rest.length > 0 && <span className="text-muted-foreground"> +{rest.length}</span>}
      </>
    );
  }
  return <>{formatValue(field, value, locale)}</>;
}

export function DataTable<Row extends RecordJson>(props: DataTableProps<Row>) {
  const { schema, rows, total, state, onStateChange, locale } = props;
  const [search, setSearch] = useState(state.q);
  const debounced = useDebounced(search, 300);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ tone: "error" | "success"; text: string } | null>(null);
  const [tagPrompt, setTagPrompt] = useState<BulkAction | null>(null);
  const [saving, setSaving] = useState(false);
  const searchId = useId();

  useEffect(() => {
    if (debounced !== state.q) onStateChange({ ...state, q: debounced, offset: 0 });
    // Only the debounced text should trigger a new search.
  }, [debounced]);
  useEffect(() => setSearch(state.q), [state.q]);
  // A new page or filter clears the selection.
  useEffect(() => setSelected(new Set()), [state.offset, state.q, state.tag, state.sort]);

  const byKey = useMemo(() => new Map(schema.fields.map((f) => [f.key, f])), [schema]);
  const columns = (state.columns ?? defaultColumns(schema))
    .map((k) => byKey.get(k))
    .filter((f): f is SchemaField => !!f);
  const nameField = columns.find((f) => f.key === "name") ?? columns[0];
  const canBulk = schema.can_write && (props.bulkActions?.length ?? 0) > 0;
  const pageIds = rows.map((r) => r.id);
  const allSelected = pageIds.length > 0 && pageIds.every((id) => selected.has(id));

  const sortBy = (field: SchemaField) => {
    if (!field.sort) return;
    const current = state.sort;
    const next = current === field.sort ? `-${field.sort}` : current === `-${field.sort}` ? null : field.sort;
    onStateChange({ ...state, sort: next, offset: 0 });
  };

  const toggleColumn = (key: string, on: boolean) => {
    const current = state.columns ?? defaultColumns(schema);
    const next = on ? [...current, key] : current.filter((k) => k !== key);
    onStateChange({ ...state, columns: next });
  };

  const runBulk = async (action: BulkAction, tags: string[] = []) => {
    setBusy(action.label);
    setNotice(null);
    try {
      const message = await action.run([...selected], tags);
      setSelected(new Set());
      if (message) setNotice({ tone: "success", text: message });
    } catch (e) {
      setNotice({ tone: "error", text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(null);
    }
  };

  const from = total === 0 ? 0 : state.offset + 1;
  const to = Math.min(state.offset + rows.length, total);

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative min-w-48 flex-1">
          <Search className="pointer-events-none absolute top-2.5 left-2.5 size-4 text-muted-foreground" />
          <label htmlFor={searchId} className="sr-only">
            Search {schema.label_plural.toLowerCase()}
          </label>
          <Input
            id={searchId}
            type="search"
            className="pl-8"
            placeholder={`Search ${schema.label_plural.toLowerCase()}…`}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
        </div>
        {props.tags && props.tags.length > 0 && (
          <NativeSelect
            aria-label="Filter by tag"
            className="w-auto"
            value={state.tag ?? ""}
            onChange={(e) => onStateChange({ ...state, tag: e.target.value || null, offset: 0 })}
          >
            <option value="">All tags</option>
            {props.tags.map((t) => (
              <option key={t.tag} value={t.tag}>
                {t.tag} ({t.count})
              </option>
            ))}
          </NativeSelect>
        )}
        {props.views && props.onSelectView && (
          <NativeSelect
            aria-label="Saved view"
            className="w-auto"
            value={props.activeViewId ?? ""}
            onChange={(e) => props.onSelectView?.(props.views?.find((v) => v.id === e.target.value) ?? null)}
          >
            <option value="">All {schema.label_plural.toLowerCase()}</option>
            {props.views.map((v) => (
              <option key={v.id} value={v.id}>
                {v.name}
                {v.shared ? " (shared)" : ""}
              </option>
            ))}
          </NativeSelect>
        )}
        {props.onSaveView && (
          <Button variant="outline" onClick={() => setSaving(true)}>
            Save view
          </Button>
        )}
        <Menu>
          <MenuTrigger asChild>
            <Button variant="outline" aria-label="Choose columns">
              <Columns3 />
              <span className="hidden sm:inline">Columns</span>
            </Button>
          </MenuTrigger>
          <MenuContent className="max-h-80 overflow-y-auto">
            <MenuLabel>Columns</MenuLabel>
            {schema.fields.map((f) => (
              <MenuCheckboxItem
                key={f.key}
                checked={columns.some((c) => c.key === f.key)}
                onCheckedChange={(on) => toggleColumn(f.key, on === true)}
                onSelect={(e) => e.preventDefault()}
              >
                {f.label}
              </MenuCheckboxItem>
            ))}
          </MenuContent>
        </Menu>
        {props.toolbar}
      </div>

      {canBulk && selected.size > 0 && (
        <div className="flex flex-wrap items-center gap-2 rounded-md bg-muted px-3 py-2 text-sm" role="toolbar">
          <span>{selected.size} selected</span>
          {props.bulkActions?.map((a) => (
            <Button
              key={a.label}
              size="sm"
              variant={a.destructive ? "destructive" : "outline"}
              disabled={busy !== null}
              onClick={() => (a.needsTags ? setTagPrompt(a) : runBulk(a))}
            >
              {busy === a.label && <Spinner />}
              {a.label}
            </Button>
          ))}
          <Button size="sm" variant="ghost" onClick={() => setSelected(new Set())}>
            Clear
          </Button>
        </div>
      )}
      {notice && <Alert tone={notice.tone}>{notice.text}</Alert>}
      {props.error && <Alert>{props.error}</Alert>}

      <div className="overflow-x-auto rounded-md border">
        <table className="w-full text-left text-sm">
          <thead className="bg-muted/50 text-xs text-muted-foreground">
            <tr>
              {canBulk && (
                <th className="w-10 px-3 py-2">
                  <Checkbox
                    aria-label="Select all on this page"
                    checked={allSelected}
                    onChange={(e) => setSelected(e.target.checked ? new Set(pageIds) : new Set())}
                  />
                </th>
              )}
              {columns.map((f) => {
                const dir = state.sort === f.sort ? "asc" : state.sort === `-${f.sort}` ? "desc" : null;
                return (
                  <th
                    key={f.key}
                    className="px-3 py-2 font-medium whitespace-nowrap"
                    aria-sort={dir === "asc" ? "ascending" : dir === "desc" ? "descending" : undefined}
                  >
                    {f.sort ? (
                      <button
                        type="button"
                        className="inline-flex items-center gap-1 hover:text-foreground"
                        onClick={() => sortBy(f)}
                      >
                        {f.label}
                        {dir === "asc" && <ArrowUp className="size-3" />}
                        {dir === "desc" && <ArrowDown className="size-3" />}
                      </button>
                    ) : (
                      f.label
                    )}
                  </th>
                );
              })}
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.id} className={cn("border-t hover:bg-muted/40", selected.has(row.id) && "bg-accent/50")}>
                {canBulk && (
                  <td className="px-3 py-2">
                    <Checkbox
                      aria-label="Select row"
                      checked={selected.has(row.id)}
                      onChange={(e) => {
                        const next = new Set(selected);
                        if (e.target.checked) next.add(row.id);
                        else next.delete(row.id);
                        setSelected(next);
                      }}
                    />
                  </td>
                )}
                {columns.map((f) => (
                  <td key={f.key} className="max-w-72 truncate px-3 py-2">
                    {f === nameField && props.rowLink ? (
                      props.rowLink(row, <Cell field={f} row={row} locale={locale} />)
                    ) : (
                      <Cell field={f} row={row} locale={locale} />
                    )}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
        {props.loading && rows.length === 0 && (
          <div className="flex justify-center p-8">
            <Spinner />
          </div>
        )}
        {!props.loading && rows.length === 0 && (
          <EmptyState title={state.q || state.tag ? "Nothing matches" : `No ${schema.label_plural.toLowerCase()} yet`} />
        )}
      </div>

      <div className="flex flex-wrap items-center justify-between gap-2 text-sm text-muted-foreground">
        <span aria-live="polite">
          {from}–{to} of {total}
          {props.loading && rows.length > 0 && <Spinner className="ml-2 inline" />}
        </span>
        <div className="flex items-center gap-2">
          <NativeSelect
            aria-label="Rows per page"
            className="w-auto"
            value={state.limit}
            onChange={(e) => onStateChange({ ...state, limit: Number(e.target.value), offset: 0 })}
          >
            {[25, 50, 100, 250].map((n) => (
              <option key={n} value={n}>
                {n} per page
              </option>
            ))}
          </NativeSelect>
          <Button
            variant="outline"
            size="icon"
            aria-label="Previous page"
            disabled={state.offset === 0}
            onClick={() => onStateChange({ ...state, offset: Math.max(0, state.offset - state.limit) })}
          >
            <ChevronLeft />
          </Button>
          <Button
            variant="outline"
            size="icon"
            aria-label="Next page"
            disabled={state.offset + state.limit >= total}
            onClick={() => onStateChange({ ...state, offset: state.offset + state.limit })}
          >
            <ChevronRight />
          </Button>
        </div>
      </div>

      {tagPrompt && (
        <TagPrompt
          action={tagPrompt}
          onClose={() => setTagPrompt(null)}
          onRun={(tags) => {
            const a = tagPrompt;
            setTagPrompt(null);
            return runBulk(a, tags);
          }}
        />
      )}
      {saving && props.onSaveView && (
        <SaveView
          canShare={props.canShareViews ?? false}
          onClose={() => setSaving(false)}
          onSave={async (name, shared) => {
            await props.onSaveView?.(name, shared);
            setSaving(false);
          }}
        />
      )}
    </div>
  );
}

function TagPrompt({ action, onClose, onRun }: { action: BulkAction; onClose: () => void; onRun: (tags: string[]) => void }) {
  const [text, setText] = useState("");
  const tags = text
    .split(",")
    .map((t) => t.trim())
    .filter(Boolean);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={action.label}>
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          if (tags.length > 0) onRun(tags);
        }}
      >
        <Label htmlFor="bulk-tags">Tags, separated by commas</Label>
        <Input id="bulk-tags" autoFocus value={text} onChange={(e) => setText(e.target.value)} />
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={tags.length === 0}>
            {action.label}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

function SaveView({
  canShare,
  onClose,
  onSave,
}: {
  canShare: boolean;
  onClose: () => void;
  onSave: (name: string, shared: boolean) => Promise<void>;
}) {
  const [name, setName] = useState("");
  const [shared, setShared] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="Save view" description="Keep these columns, filters and sort.">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          try {
            await onSave(name.trim(), shared);
          } catch (err) {
            setError(err instanceof Error ? err.message : String(err));
          }
        }}
      >
        <Label htmlFor="view-name">Name</Label>
        <Input id="view-name" autoFocus required value={name} onChange={(e) => setName(e.target.value)} />
        {canShare && (
          <label className="flex items-center gap-2 text-sm">
            <Checkbox checked={shared} onChange={(e) => setShared(e.target.checked)} />
            Share with everyone in the workspace
          </label>
        )}
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={!name.trim()}>
            Save
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
