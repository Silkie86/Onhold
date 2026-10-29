// Contacts, organisations and talent: one list page and one record page for
// all three, drawn from each entity's schema.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import {
  Alert,
  type BulkAction,
  Button,
  CopyButton,
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
import { ArrowLeft, CloudOff, Plus, Trash2, X } from "lucide-react";
import { useEffect, useState } from "react";
import { ApiFailure, type EntitySchema, ENTITIES, api, entityByPath, ok } from "../api.ts";
import {
  createRecord,
  deleteRecord,
  fetchList,
  fetchRecord,
  fetchSchema,
  fetchTags,
  fetchWorkspace,
  findDuplicates,
  updateRecord,
  useOffline,
  useSyncStatus,
} from "../data.ts";
import { isNetworkError } from "../offline/sync.ts";
import { isLocalId } from "../offline/store.ts";
import { useSession } from "../session.tsx";
import { AvailabilityEditor } from "./availability.tsx";
import { TalentMembers } from "./members.tsx";
import { useSetup } from "../setup.ts";
import { RelatedConversations } from "./inbox.tsx";
import { TalentCalendarsCard } from "./talent-calendars.tsx";
import { PortalAccessCard } from "./portal-settings.tsx";
import { dialable, phonesOf } from "../phones.ts";
import { OrganisationPitching } from "./pitching.tsx";

type Entity = (typeof ENTITIES)[keyof typeof ENTITIES];

export function useSchema(entity: string) {
  const sync = useOffline();
  return useQuery({
    queryKey: ["schema", entity],
    queryFn: async () => (await fetchSchema(sync, entity)).data,
  });
}

/** Says the page shows what is saved on this device, because the app is offline. */
export function OfflineNote({ children }: { children?: React.ReactNode }) {
  return (
    <Alert tone="info">
      <span className="flex items-center gap-2">
        <CloudOff className="size-4 shrink-0" />
        {children ?? "You are offline. This is the copy saved on this device; changes you make are sent when you reconnect."}
      </span>
    </Alert>
  );
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
  const sync = useOffline();
  const status = useSyncStatus();
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
    queryFn: () =>
      fetchList(sync, entity.entity, {
        q: state.q,
        sort: state.sort,
        tag: state.tag,
        offset: state.offset,
        limit: state.limit,
      }),
    placeholderData: (prev) => prev,
  });
  const tags = useQuery({
    queryKey: ["tags", entity.entity],
    queryFn: async () => (await fetchTags(sync, entity.entity)).data,
  });
  const views = useQuery({
    queryKey: ["views", entity.entity],
    queryFn: async () => {
      try {
        return await ok(api.GET("/api/v1/views", { params: { query: { entity: entity.entity } } }));
      } catch (e) {
        // Saved views live on the server; offline the list simply has none.
        if (sync && isNetworkError(e)) return [];
        throw e;
      }
    },
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
    if (list.data?.offline) throw new Error("Changing many records at once needs a connection.");
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
      {(list.data?.offline || status?.online === false) && <OfflineNote />}
      <DataTable
        schema={schema.data}
        rows={list.data?.data.items ?? []}
        total={list.data?.data.total ?? 0}
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
  const sync = useOffline();
  const { workspace } = useSession();
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [duplicates, setDuplicates] = useState<{ list: Duplicate[]; body: Record<string, unknown> } | null>(null);
  const currency = useWorkspaceCurrency();

  const create = async (body: Record<string, unknown>) => {
    try {
      const record = (await createRecord(sync, entity.entity, body)).record as RecordJson;
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
            const list = (query.name as string) ? await findDuplicates(sync, entity.entity, query) : [];
            if (list.length > 0) setDuplicates({ list, body });
            else await create(body);
          }}
        />
      )}
    </Dialog>
  );
}

export function useWorkspaceCurrency(): string {
  const sync = useOffline();
  const ws = useQuery({ queryKey: ["workspace"], queryFn: async () => (await fetchWorkspace(sync)).data });
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
  const sync = useOffline();
  const [name, setName] = useState(initialName);
  const [q, setQ] = useState("");
  const results = useQuery({
    queryKey: ["org-search", q],
    queryFn: async () => (await fetchList(sync, "organisation", { q, limit: 8 })).data,
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
                  setName(String(o.name));
                  setQ("");
                  onChange(o.id);
                }}
              >
                {String(o.name)}
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
  // OnHold's team may have streamlined these away; email conversations need the inbox too.
  const setup = useSetup();
  const conversations = setup.has("messaging") && setup.shows("section:records.conversations");
  const { id } = useParams({ from: "/app/crm/$entity/$id" });
  const navigate = useNavigate();
  const qc = useQueryClient();
  const sync = useOffline();
  const status = useSyncStatus();
  const schema = useSchema(entity.entity);
  const currency = useWorkspaceCurrency();
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [saved, setSaved] = useState<"sent" | "queued" | null>(null);
  const record = useQuery({
    queryKey: ["record", entity.entity, id],
    queryFn: () => fetchRecord(sync, entity.entity, id),
    // Enquiries and bookings move organisations along on the server, so a
    // copy from a moment ago may be a version behind.
    refetchOnMount: "always",
  });
  // The details form starts again when the record changes, except after a
  // change to the pitching lists, which would throw away what is typed.
  const [formKey, setFormKey] = useState<string | null>(null);
  useEffect(() => setFormKey(null), [id]);
  // A record created offline gets its real id once it reaches the server.
  const renamed = isLocalId(id) ? sync?.renamed.get(id) : undefined;
  useEffect(() => {
    if (renamed) void navigate({ to: "/crm/$entity/$id", params: { entity: entity.path, id: renamed }, replace: true });
  }, [renamed, entity.path, navigate]);
  const [before, setBefore] = useState<string | null>(null);
  const timeline = useQuery({
    queryKey: ["timeline", entity.entity, id, before],
    queryFn: () =>
      ok(
        api.GET(`/api/v1/${entity.path}/{id}/timeline` as "/api/v1/contacts/{id}/timeline", {
          params: { path: { id }, query: { before: before ?? undefined, limit: 50 } },
        }),
      ),
    enabled: !isLocalId(id) && !record.data?.offline,
    retry: (count, e) => !isNetworkError(e) && count < 2,
  });
  const [older, setOlder] = useState<TimelineEntry[]>([]);
  useEffect(() => {
    setOlder([]);
    setBefore(null);
  }, [id]);
  const entries = before ? [...older, ...(timeline.data?.entries ?? [])] : (timeline.data?.entries ?? []);

  const remove = useMutation({
    mutationFn: () => deleteRecord(sync, entity.entity, id, Number(record.data?.data.version ?? 0) || undefined),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ["list", entity.entity] });
      void navigate({ to: "/crm/$entity", params: { entity: entity.path } });
    },
  });

  if (schema.isPending || record.isPending) return <Loading />;
  if (schema.isError) return <PageError error={schema.error} />;
  if (record.isError) return <PageError error={record.error} />;
  const r = record.data.data;
  const offline = record.data.offline || (timeline.isError && isNetworkError(timeline.error));

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
      {(record.data.offline || status?.online === false) && <OfflineNote />}
      <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <section className="flex flex-col gap-6" aria-label="Details">
          {saved === "sent" && <Alert tone="success">Saved.</Alert>}
          {saved === "queued" && (
            <Alert tone="info">Saved on this device. It will be sent when you are back online.</Alert>
          )}
          {entity.entity === "contact" && <QuickCopy contact={r} />}
          <EntityForm
            key={`${id}:${formKey ?? String(r.version)}`}
            schema={schema.data}
            record={r}
            issues={issues}
            defaultCurrency={currency}
            renderRelation={OrganisationPicker}
            onSubmit={async (body) => {
              setIssues([]);
              setSaved(null);
              if (Object.keys(body).length === 0) return;
              try {
                const result = await updateRecord(sync, entity.entity, id, body, Number(r.version ?? 0));
                qc.setQueryData(["record", entity.entity, id], { data: result.record, offline: result.queued && !navigator.onLine });
                setSaved(result.queued ? "queued" : "sent");
                setFormKey(null);
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
          {entity.entity === "talent" &&
            (offline || isLocalId(id) ? (
              <p className="text-sm text-muted-foreground">Members can be seen and changed when you are online.</p>
            ) : (
              <>
                {setup.shows("section:talent.members") && <TalentMembers talentId={id} canWrite={schema.data.can_write} />}
                {setup.shows("section:talent.availability") && <AvailabilityEditor talentId={id} canWrite={schema.data.can_write} />}
                {setup.shows("section:talent.calendars") && <TalentCalendarsCard talentId={id} canWrite={schema.data.can_write} />}
                {setup.shows("section:talent.portal") && <PortalAccessCard talentId={id} canWrite={schema.data.can_write} />}
              </>
            ))}
          {entity.entity === "organisation" && <OrganisationContacts organisation={r} />}
          {entity.entity === "organisation" &&
            setup.has("pitching") &&
            (offline || isLocalId(id) || record.data.queued ? (
              <p className="text-sm text-muted-foreground">
                {record.data.queued ? "Pitching shows once your changes here are sent." : "Pitching shows when you are online."}
              </p>
            ) : (
              <OrganisationPitching
                organisation={r}
                canWrite={schema.data.can_write}
                onSaved={(after) => {
                  setFormKey((k) => k ?? String(r.version));
                  qc.setQueryData(["record", entity.entity, id], { data: after, offline: false });
                  void qc.invalidateQueries({ queryKey: ["timeline", entity.entity, id] });
                }}
              />
            ))}
          {!offline && !isLocalId(id) && conversations && entity.entity === "contact" && <RelatedConversations filter={{ contact_id: id }} />}
          {!offline && !isLocalId(id) && conversations && entity.entity === "organisation" && (
            <RelatedConversations filter={{ organisation_id: id }} />
          )}
        </section>
        <section aria-label="Activity" className="flex flex-col gap-3">
          <h2 className="font-semibold">Activity</h2>
          {offline || isLocalId(id) ? (
            <p className="text-sm text-muted-foreground">Activity shows when you are online.</p>
          ) : (
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
          )}
        </section>
      </div>
    </div>
  );
}

function OrganisationContacts({ organisation }: { organisation: RecordJson }) {
  const sync = useOffline();
  const schema = useSchema("contact");
  const [adding, setAdding] = useState(false);
  const contacts = useQuery({
    queryKey: ["list", "contact", "org", organisation.id],
    queryFn: async () => (await fetchList(sync, "contact", { organisation_id: organisation.id, limit: 100 })).data,
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
            <li key={c.id} className="flex flex-wrap items-center justify-between gap-2 px-3 py-2 text-sm">
              <Link to="/crm/$entity/$id" params={{ entity: "contacts", id: c.id }} className="font-medium hover:underline">
                {String(c.name)}
              </Link>
              <span className="flex flex-wrap items-center gap-1 text-muted-foreground">
                {c.role ? <span>{String(c.role)}</span> : null}
                {c.email ? (
                  <span className="inline-flex items-center">
                    {c.role ? "· " : ""}
                    {String(c.email)}
                    <CopyButton value={String(c.email)} label={`${String(c.name)}'s email`} />
                  </span>
                ) : null}
                {firstPhone(c) && (
                  <span className="inline-flex items-center">
                    {firstPhone(c)}
                    <CopyButton value={firstPhone(c) ?? ""} label={`${String(c.name)}'s phone number`} />
                  </span>
                )}
              </span>
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

/** A contact's first number, with its country code when it can be told. */
function firstPhone(c: RecordJson): string | null {
  return phonesOf(c)[0] ?? null;
}

/**
 * A contact's numbers, with their country code when the server worked them
 * out from these numbers (a change still on this device has none yet).
 */


/** A contact's emails, numbers and links, each a tap away from the clipboard. */
function QuickCopy({ contact }: { contact: RecordJson }) {
  const emails = (contact.emails as string[] | undefined) ?? [];
  const phones = phonesOf(contact);
  const links = (contact.links as string[] | undefined) ?? [];
  if (emails.length + phones.length + links.length === 0) return null;
  const row = (kind: string, value: string, href: string) => (
    <li key={`${kind}:${value}`} className="flex min-w-0 items-center gap-1">
      <a href={href} className="min-w-0 truncate hover:underline" {...(kind === "link" ? { target: "_blank", rel: "noreferrer" } : {})}>
        {kind === "link" ? value.replace(/^https?:\/\//, "") : value}
      </a>
      <CopyButton value={value} label={kind === "link" ? "link" : kind === "email" ? "email address" : "phone number"} />
    </li>
  );
  return (
    <section aria-label="Quick copy" className="flex flex-col gap-1 rounded-md border p-3 text-sm">
      <ul className="flex flex-col gap-0.5">
        {emails.map((e) => row("email", e, `mailto:${e}`))}
        {phones.map((p) => row("phone", p, `tel:${dialable(p)}`))}
        {links.map((l) => row("link", l, l))}
      </ul>
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
