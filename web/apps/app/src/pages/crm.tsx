// Contacts, organisations and talent: one list page and one record page for
// all three, drawn from each entity's schema.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import {
  Alert,
  type BulkAction,
  Button,
  DataTable,
  Dialog,
  EntityForm,
  type FieldIssue,
  Input,
  type RecordJson,
  type RelationRenderer,
  type SavedViewSummary,
  Spinner,
  type TableState,
  Timeline,
  type TimelineEntry,
  defaultTableState,
  stateFromView,
  viewConfig,
} from "@onhold/ui";
import { ArrowLeft, Plus, Trash2, X } from "lucide-react";
import { useEffect, useState } from "react";
import { ApiFailure, type EntitySchema, ENTITIES, api, entityByPath, ok } from "../api.ts";
import { useSession } from "../session.tsx";
import { TalentMembers } from "./members.tsx";

type Entity = (typeof ENTITIES)[keyof typeof ENTITIES];
type Page = { items: RecordJson[]; total: number };

// The three entities share their routes' shapes; one typed path stands in for each.
const P = (path: string) => `/api/v1/${path}` as "/api/v1/contacts";
const PI = (path: string) => `/api/v1/${path}/{id}` as "/api/v1/contacts/{id}";

export function useSchema(entity: string) {
  return useQuery({
    queryKey: ["schema", entity],
    queryFn: () => ok(api.GET("/api/v1/schema/{entity}", { params: { path: { entity } } })),
  });
}

export interface ListSearch {
  q?: string;
  sort?: string;
  tag?: string;
  offset?: number;
  limit?: number;
  view?: string;
}

export function validateListSearch(s: Record<string, unknown>): ListSearch {
  const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);
  const num = (v: unknown) => (typeof v === "number" ? v : typeof v === "string" && v ? Number(v) || undefined : undefined);
  return { q: str(s.q), sort: str(s.sort), tag: str(s.tag), offset: num(s.offset), limit: num(s.limit), view: str(s.view) };
}

function columnsKey(entity: string) {
  return `onhold.columns.${entity}`;
}

function storedColumns(entity: string): string[] | null {
  try {
    const raw = localStorage.getItem(columnsKey(entity));
    return raw ? (JSON.parse(raw) as string[]) : null;
  } catch {
    return null;
  }
}

export function failureIssues(e: unknown): FieldIssue[] | null {
  return e instanceof ApiFailure && e.issues.length > 0 ? e.issues : null;
}

function useEntity(): Entity {
  const { entity } = useParams({ strict: false }) as { entity?: string };
  const found = entityByPath(entity ?? "");
  if (!found) throw new Error(`no such list: ${entity}`);
  return found;
}

export function EntityListPage() {
  const entity = useEntity();
  const search = useSearch({ from: "/app/crm/$entity" });
  const navigate = useNavigate({ from: "/crm/$entity" });
  const qc = useQueryClient();
  const { workspace } = useSession();
  const schema = useSchema(entity.entity);
  const [creating, setCreating] = useState(false);
  const [columns, setColumns] = useState<string[] | null>(() => storedColumns(entity.entity));

  useEffect(() => setColumns(storedColumns(entity.entity)), [entity.entity]);

  const state: TableState = {
    ...defaultTableState,
    q: search.q ?? "",
    sort: search.sort ?? null,
    tag: search.tag ?? null,
    offset: search.offset ?? 0,
    limit: search.limit ?? 50,
    columns,
  };

  const list = useQuery({
    queryKey: ["list", entity.entity, state.q, state.sort, state.tag, state.offset, state.limit],
    queryFn: async () =>
      (await ok(
        api.GET(P(entity.path), {
          params: {
            query: {
              q: state.q || undefined,
              sort: state.sort ?? undefined,
              tag: state.tag ?? undefined,
              offset: state.offset,
              limit: state.limit,
            },
          },
        }),
      )) as unknown as Page,
    placeholderData: (prev) => prev,
  });
  const tags = useQuery({
    queryKey: ["tags", entity.entity],
    queryFn: () => ok(api.GET("/api/v1/tags", { params: { query: { entity: entity.entity } } })),
  });
  const views = useQuery({
    queryKey: ["views", entity.entity],
    queryFn: () => ok(api.GET("/api/v1/views", { params: { query: { entity: entity.entity } } })),
  });

  const setState = (s: TableState) => {
    if (s.columns !== columns) {
      setColumns(s.columns);
      try {
        if (s.columns) localStorage.setItem(columnsKey(entity.entity), JSON.stringify(s.columns));
        else localStorage.removeItem(columnsKey(entity.entity));
      } catch {
        // Not remembered in private browsing.
      }
    }
    void navigate({
      search: {
        q: s.q || undefined,
        sort: s.sort ?? undefined,
        tag: s.tag ?? undefined,
        offset: s.offset || undefined,
        limit: s.limit === 50 ? undefined : s.limit,
        view: search.view,
      },
      replace: true,
    });
  };

  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["list", entity.entity] });
    void qc.invalidateQueries({ queryKey: ["tags", entity.entity] });
  };
  const bulk = async (action: "delete" | "add_tags" | "remove_tags", ids: string[], tags: string[]) => {
    const res = await ok(api.POST(`/api/v1/${entity.path}/bulk` as "/api/v1/contacts/bulk", { body: { action, ids, tags } }));
    refresh();
    const failed = res.failed.length ? `; ${res.failed.length} could not be changed (${res.failed[0]?.message})` : "";
    return `${res.done} ${res.done === 1 ? "record" : "records"} updated${failed}.`;
  };
  const bulkActions: BulkAction[] = [
    { label: "Add tags", needsTags: true, run: (ids, t) => bulk("add_tags", ids, t) },
    { label: "Remove tags", needsTags: true, run: (ids, t) => bulk("remove_tags", ids, t) },
    {
      label: "Delete",
      destructive: true,
      run: async (ids) => {
        if (!window.confirm(`Delete ${ids.length} ${ids.length === 1 ? "record" : "records"}?`)) return;
        return bulk("delete", ids, []);
      },
    },
  ];

  const savedViews: SavedViewSummary[] = (views.data ?? []).map((v) => ({
    id: v.id,
    name: v.name,
    shared: v.shared,
    config: v.config as SavedViewSummary["config"],
  }));
  const canShare = ["owner", "admin"].includes(workspace.role);

  if (schema.isPending) return <Loading />;
  if (schema.isError) return <PageError error={schema.error} />;

  return (
    <div className="mx-auto flex max-w-7xl flex-col gap-4 p-4 sm:p-6">
      <h1 className="text-xl font-semibold">{schema.data.label_plural}</h1>
      <DataTable
        schema={schema.data}
        rows={list.data?.items ?? []}
        total={list.data?.total ?? 0}
        loading={list.isFetching}
        error={list.error?.message}
        state={state}
        onStateChange={setState}
        rowLink={(row, children) => (
          <Link to="/crm/$entity/$id" params={{ entity: entity.path, id: row.id }} className="font-medium hover:underline">
            {children}
          </Link>
        )}
        bulkActions={bulkActions}
        tags={tags.data}
        views={savedViews}
        activeViewId={search.view ?? null}
        onSelectView={(v) => {
          const next = v ? stateFromView(v.config) : { ...defaultTableState, columns: null };
          setColumns(next.columns);
          void navigate({
            search: {
              q: next.q || undefined,
              sort: next.sort ?? undefined,
              tag: next.tag ?? undefined,
              limit: next.limit === 50 ? undefined : next.limit,
              view: v?.id,
            },
          });
        }}
        onSaveView={async (name, shared) => {
          const view = await ok(
            api.POST("/api/v1/views", { body: { entity: entity.entity, name, shared, config: viewConfig(state) as never } }),
          );
          await qc.invalidateQueries({ queryKey: ["views", entity.entity] });
          void navigate({ search: { ...search, view: view.id } });
        }}
        canShareViews={canShare}
        toolbar={
          schema.data.can_write && (
            <Button onClick={() => setCreating(true)}>
              <Plus /> New {schema.data.label.toLowerCase()}
            </Button>
          )
        }
      />
      {creating && <CreateRecord entity={entity} schema={schema.data} onClose={() => setCreating(false)} />}
    </div>
  );
}

function duplicateQuery(entity: Entity, body: Record<string, unknown>): Record<string, unknown> {
  const name = (body.name as string | undefined) ?? "";
  if (entity.entity === "contact") return { name, emails: body.emails ?? [], phones: body.phones ?? [] };
  if (entity.entity === "organisation") return { name, tax_id: body.tax_id ?? null };
  return { name };
}

type Duplicate = { id: string; name: string; reasons: string[] };

function CreateRecord({
  entity,
  schema,
  initial,
  hide,
  onClose,
  onCreated,
}: {
  entity: Entity;
  schema: EntitySchema;
  initial?: Record<string, unknown>;
  hide?: string[];
  onClose: () => void;
  onCreated?: (record: RecordJson) => void;
}) {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const { workspace } = useSession();
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [duplicates, setDuplicates] = useState<{ list: Duplicate[]; body: Record<string, unknown> } | null>(null);
  const currency = useWorkspaceCurrency();

  const create = async (body: Record<string, unknown>) => {
    try {
      const record = (await ok(api.POST(P(entity.path), { body: body as never }))) as unknown as RecordJson;
      await qc.invalidateQueries({ queryKey: ["list", entity.entity] });
      onClose();
      if (onCreated) onCreated(record);
      else void navigate({ to: "/crm/$entity/$id", params: { entity: entity.path, id: record.id } });
    } catch (e) {
      const found = failureIssues(e);
      if (found) setIssues(found);
      else throw e;
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={`New ${schema.label.toLowerCase()}`} className="max-w-2xl">
      {duplicates ? (
        <div className="flex flex-col gap-4">
          <Alert tone="info">This may already be in {workspace.name}:</Alert>
          <ul className="flex flex-col gap-2">
            {duplicates.list.map((d) => (
              <li key={d.id} className="rounded-md border p-3 text-sm">
                <Link to="/crm/$entity/$id" params={{ entity: entity.path, id: d.id }} className="font-medium hover:underline" onClick={onClose}>
                  {d.name}
                </Link>
                <p className="text-muted-foreground">{d.reasons.join(", ")}</p>
              </li>
            ))}
          </ul>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setDuplicates(null)}>
              Back
            </Button>
            <Button onClick={() => void create(duplicates.body)}>Create anyway</Button>
          </div>
        </div>
      ) : (
        <EntityForm
          schema={schema}
          initial={initial}
          hide={hide}
          issues={issues}
          defaultCurrency={currency}
          renderRelation={OrganisationPicker}
          onCancel={onClose}
          onSubmit={async (body) => {
            setIssues([]);
            const query = duplicateQuery(entity, body);
            const list = (query.name as string)
              ? await ok(api.POST(`/api/v1/${entity.path}/duplicates` as "/api/v1/contacts/duplicates", { body: query as never }))
              : [];
            if (list.length > 0) setDuplicates({ list, body });
            else await create(body);
          }}
        />
      )}
    </Dialog>
  );
}

export function useWorkspaceCurrency(): string {
  const ws = useQuery({ queryKey: ["workspace"], queryFn: () => ok(api.GET("/api/v1/workspace")) });
  return ws.data?.default_currency?.trim() || "GBP";
}

/** Chooses a contact's organisation by searching for it. */
export const OrganisationPicker: RelationRenderer = ({ id, value, onChange, record }) => (
  <OrganisationSearch id={id} value={value} onChange={onChange} initialName={(record?.organisation_name as string) ?? null} />
);

function OrganisationSearch({
  id,
  value,
  onChange,
  initialName,
}: {
  id: string;
  value: string | null;
  onChange: (id: string | null) => void;
  initialName: string | null;
}) {
  const [name, setName] = useState(initialName);
  const [q, setQ] = useState("");
  const results = useQuery({
    queryKey: ["org-search", q],
    queryFn: () => ok(api.GET("/api/v1/organisations", { params: { query: { q, limit: 8 } } })),
    enabled: q.trim().length > 0,
  });
  if (value && name) {
    return (
      <div className="flex h-9 items-center justify-between rounded-md border px-3 text-sm">
        <span id={id}>{name}</span>
        <button type="button" aria-label="Remove organisation" onClick={() => onChange(null)}>
          <X className="size-4" />
        </button>
      </div>
    );
  }
  return (
    <div className="relative">
      <Input id={id} placeholder="Search organisations…" value={q} onChange={(e) => setQ(e.target.value)} autoComplete="off" />
      {q.trim() && (
        <ul className="absolute z-10 mt-1 max-h-56 w-full overflow-y-auto rounded-md border bg-card text-sm shadow-md">
          {results.isFetching && !results.data && (
            <li className="p-2">
              <Spinner />
            </li>
          )}
          {results.data?.items.map((o) => (
            <li key={o.id}>
              <button
                type="button"
                className="w-full px-3 py-2 text-left hover:bg-accent"
                onClick={() => {
                  setName(o.name);
                  setQ("");
                  onChange(o.id);
                }}
              >
                {o.name}
              </button>
            </li>
          ))}
          {results.data?.items.length === 0 && <li className="p-2 text-muted-foreground">No organisation matches.</li>}
        </ul>
      )}
    </div>
  );
}

export function EntityRecordPage() {
  const entity = useEntity();
  const { id } = useParams({ from: "/app/crm/$entity/$id" });
  const navigate = useNavigate();
  const qc = useQueryClient();
  const schema = useSchema(entity.entity);
  const currency = useWorkspaceCurrency();
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [saved, setSaved] = useState(false);
  const record = useQuery({
    queryKey: ["record", entity.entity, id],
    queryFn: async () => (await ok(api.GET(PI(entity.path), { params: { path: { id } } }))) as unknown as RecordJson,
  });
  const [before, setBefore] = useState<string | null>(null);
  const timeline = useQuery({
    queryKey: ["timeline", entity.entity, id, before],
    queryFn: () =>
      ok(
        api.GET(`/api/v1/${entity.path}/{id}/timeline` as "/api/v1/contacts/{id}/timeline", {
          params: { path: { id }, query: { before: before ?? undefined, limit: 50 } },
        }),
      ),
  });
  const [older, setOlder] = useState<TimelineEntry[]>([]);
  useEffect(() => {
    setOlder([]);
    setBefore(null);
  }, [id]);
  const entries = before ? [...older, ...(timeline.data?.entries ?? [])] : (timeline.data?.entries ?? []);

  const remove = useMutation({
    mutationFn: () => ok(api.DELETE(PI(entity.path), { params: { path: { id } } })),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ["list", entity.entity] });
      void navigate({ to: "/crm/$entity", params: { entity: entity.path } });
    },
  });

  if (schema.isPending || record.isPending) return <Loading />;
  if (schema.isError) return <PageError error={schema.error} />;
  if (record.isError) return <PageError error={record.error} />;
  const r = record.data;

  return (
    <div className="mx-auto flex max-w-7xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="ghost" size="icon" asChild aria-label={`Back to ${schema.data.label_plural}`}>
          <Link to="/crm/$entity" params={{ entity: entity.path }}>
            <ArrowLeft />
          </Link>
        </Button>
        <h1 className="min-w-0 flex-1 truncate text-xl font-semibold">{String(r.name ?? "")}</h1>
        {schema.data.can_write && (
          <Button
            variant="outline"
            onClick={() => {
              if (window.confirm(`Delete ${String(r.name)}?`)) remove.mutate();
            }}
          >
            <Trash2 /> Delete
          </Button>
        )}
      </div>
      {remove.error && <Alert>{remove.error.message}</Alert>}
      <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <section className="flex flex-col gap-6" aria-label="Details">
          {saved && <Alert tone="success">Saved.</Alert>}
          <EntityForm
            key={String(r.version)}
            schema={schema.data}
            record={r}
            issues={issues}
            defaultCurrency={currency}
            renderRelation={OrganisationPicker}
            onSubmit={async (body) => {
              setIssues([]);
              setSaved(false);
              if (Object.keys(body).length === 0) return;
              try {
                const updated = (await ok(
                  api.PATCH(PI(entity.path), {
                    params: { path: { id } },
                    headers: { "If-Match": `"${String(r.version)}"` },
                    body: body as never,
                  }),
                )) as unknown as RecordJson;
                qc.setQueryData(["record", entity.entity, id], updated);
                setSaved(true);
                void qc.invalidateQueries({ queryKey: ["timeline", entity.entity, id] });
                void qc.invalidateQueries({ queryKey: ["list", entity.entity] });
              } catch (e) {
                if (e instanceof ApiFailure && e.status === 412) {
                  throw new Error("Someone else changed this record while you were editing. Reload to see their changes.");
                }
                const found = failureIssues(e);
                if (found) setIssues(found);
                else throw e;
              }
            }}
          />
          {entity.entity === "talent" && <TalentMembers talentId={id} canWrite={schema.data.can_write} />}
          {entity.entity === "organisation" && <OrganisationContacts organisation={r} />}
        </section>
        <section aria-label="Activity" className="flex flex-col gap-3">
          <h2 className="font-semibold">Activity</h2>
          <Timeline
            entries={entries}
            schema={schema.data}
            loading={timeline.isFetching}
            hasMore={!!timeline.data?.next_before}
            onMore={() => {
              setOlder(entries);
              setBefore(timeline.data?.next_before ?? null);
            }}
          />
        </section>
      </div>
    </div>
  );
}

function OrganisationContacts({ organisation }: { organisation: RecordJson }) {
  const schema = useSchema("contact");
  const [adding, setAdding] = useState(false);
  const contacts = useQuery({
    queryKey: ["list", "contact", "org", organisation.id],
    queryFn: () =>
      ok(api.GET("/api/v1/contacts", { params: { query: { organisation_id: organisation.id, limit: 100 } } })),
  });
  return (
    <section className="flex flex-col gap-3" aria-label="People">
      <div className="flex items-center justify-between">
        <h2 className="font-semibold">People</h2>
        {schema.data?.can_write && (
          <Button variant="outline" size="sm" onClick={() => setAdding(true)}>
            <Plus /> Add contact
          </Button>
        )}
      </div>
      {contacts.isPending ? (
        <Spinner />
      ) : (contacts.data?.items.length ?? 0) === 0 ? (
        <p className="text-sm text-muted-foreground">Nobody here yet.</p>
      ) : (
        <ul className="divide-y rounded-md border">
          {contacts.data?.items.map((c) => (
            <li key={c.id} className="flex flex-wrap justify-between gap-2 px-3 py-2 text-sm">
              <Link to="/crm/$entity/$id" params={{ entity: "contacts", id: c.id }} className="font-medium hover:underline">
                {c.name}
              </Link>
              <span className="text-muted-foreground">{[c.role, c.email].filter(Boolean).join(" · ")}</span>
            </li>
          ))}
        </ul>
      )}
      {adding && schema.data && (
        <CreateRecord
          entity={ENTITIES.contact}
          schema={schema.data}
          initial={{ organisation_id: organisation.id }}
          hide={["organisation_id"]}
          onClose={() => setAdding(false)}
          onCreated={() => void contacts.refetch()}
        />
      )}
    </section>
  );
}

export function Loading() {
  return (
    <div className="flex justify-center p-12">
      <Spinner className="size-6" />
    </div>
  );
}

export function PageError({ error }: { error: Error }) {
  return (
    <div className="p-6">
      <Alert>{error instanceof ApiFailure && error.status === 404 ? "This record does not exist or was deleted." : error.message}</Alert>
    </div>
  );
}
