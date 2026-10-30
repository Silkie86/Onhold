// Enquiries: a board of the pipeline's stages, and each enquiry with its
// details, talent, the holds placed for it, its tasks and its history.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import {
  Alert,
  Button,
  Dialog,
  EntityForm,
  type FieldIssue,
  Input,
  Label,
  NativeSelect,
  Spinner,
  cn,
  formatMoney,
} from "@onhold/ui";
import { ArrowLeft, CalendarPlus, Plus, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { ApiFailure } from "../api.ts";
import {
  createRecord,
  deleteRecord,
  fetchBookings,
  fetchEnquiries,
  fetchRecord,
  fetchStages,
  updateRecord,
  useOffline,
  useSyncStatus,
} from "../data.ts";
import type { OfflineSync } from "../offline/sync.ts";
import { isLocalId } from "../offline/store.ts";
import type { Enquiry, Named } from "../work.ts";
import { BookingCard, NewBookingDialog, RecordTimeline } from "./bookings.tsx";
import { Loading, OfflineNote, PageError, failureIssues, useSchema, useWorkspaceCurrency } from "./crm.tsx";
import { RelationPicker, TalentPicker } from "./pickers.tsx";
import { RelatedTasks } from "./tasks.tsx";
import { RelatedConversations } from "./inbox.tsx";

/** The stages before the workspace's pipeline exists (it is made with the first enquiry). */
const DEFAULT_STAGES = [
  { key: "new", name: "New", outcome: "open" },
  { key: "checking", name: "Checking availability", outcome: "open" },
  { key: "quoted", name: "Quoted", outcome: "open" },
  { key: "won", name: "Won", outcome: "won" },
  { key: "lost", name: "Lost", outcome: "lost" },
];

function useStages() {
  const sync = useOffline();
  return useQuery({
    queryKey: ["enquiry-stages"],
    queryFn: async () => (await fetchStages(sync)).data ?? DEFAULT_STAGES,
  });
}

async function moveStage(sync: OfflineSync | null, enquiry: Enquiry, stage: string, lostReason?: string) {
  const body = { stage, ...(lostReason !== undefined && { lost_reason: lostReason || null }) };
  return (await updateRecord(sync, "enquiry", enquiry.id, body, enquiry.version)).record as unknown as Enquiry;
}

export function EnquiriesPage() {
  const qc = useQueryClient();
  const sync = useOffline();
  const status = useSyncStatus();
  const stages = useStages();
  const [q, setQ] = useState("");
  const [creating, setCreating] = useState(false);
  const [dragging, setDragging] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const list = useQuery({
    queryKey: ["enquiries", q],
    queryFn: () => fetchEnquiries(sync, { q, limit: 200 }),
    placeholderData: (prev) => prev,
  });

  const move = async (e: Enquiry, stage: string) => {
    if (e.stage === stage) return;
    setError(null);
    try {
      await moveStage(sync, e, stage);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
    await qc.invalidateQueries({ queryKey: ["enquiries"] });
  };

  if (stages.isPending) return <Loading />;
  if (stages.isError) return <PageError error={stages.error} />;
  const items = list.data?.data.items ?? [];

  return (
    <div className="flex flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="flex-1 text-xl font-semibold">Enquiries</h1>
        <Input
          aria-label="Search enquiries"
          placeholder="Search…"
          className="w-48"
          value={q}
          onChange={(e) => setQ(e.target.value)}
        />
        <Button onClick={() => setCreating(true)}>
          <Plus /> New enquiry
        </Button>
      </div>
      {(list.data?.offline || status?.online === false) && <OfflineNote />}
      {error && <Alert>{error}</Alert>}
      {list.isError && <Alert>{list.error.message}</Alert>}
      <div className="flex gap-3 overflow-x-auto pb-2" role="list" aria-label="Pipeline">
        {stages.data.map((s) => {
          const here = items.filter((e) => e.stage === s.key);
          return (
            <section
              key={s.key}
              role="listitem"
              aria-label={s.name}
              className={cn(
                "flex w-72 shrink-0 flex-col gap-2 rounded-lg border bg-muted/30 p-2",
                dragging && "outline-dashed outline-1 outline-muted-foreground/30",
              )}
              onDragOver={(e) => e.preventDefault()}
              onDrop={(e) => {
                e.preventDefault();
                const id = e.dataTransfer.getData("text/enquiry");
                const found = items.find((i) => i.id === id);
                setDragging(null);
                if (found) void move(found, s.key);
              }}
            >
              <h2 className="flex items-center justify-between px-1 text-sm font-medium">
                {s.name}
                <span className="text-xs text-muted-foreground">{here.length}</span>
              </h2>
              {here.map((e) => (
                <EnquiryCard
                  key={e.id}
                  enquiry={e}
                  stages={stages.data}
                  onMove={(stage) => void move(e, stage)}
                  onDragStart={() => setDragging(e.id)}
                />
              ))}
              {list.isPending && <Spinner />}
            </section>
          );
        })}
      </div>
      {creating && <NewEnquiryDialog onClose={() => setCreating(false)} />}
    </div>
  );
}

function EnquiryCard({
  enquiry: e,
  stages,
  onMove,
  onDragStart,
}: {
  enquiry: Enquiry;
  stages: { key: string; name: string }[];
  onMove: (stage: string) => void;
  onDragStart: () => void;
}) {
  return (
    <article
      draggable
      onDragStart={(ev) => {
        ev.dataTransfer.setData("text/enquiry", e.id);
        onDragStart();
      }}
      className="flex flex-col gap-1 rounded-md border bg-card p-3 text-sm shadow-xs"
    >
      <Link to="/enquiries/$id" params={{ id: e.id }} className="font-medium hover:underline">
        {e.title}
      </Link>
      <p className="text-muted-foreground">
        {[e.starts_on && new Date(e.starts_on).toLocaleDateString(undefined, { dateStyle: "medium" }), e.organisation_name ?? e.contact_name]
          .filter(Boolean)
          .join(" · ")}
      </p>
      {e.talent.length > 0 && <p className="truncate">{e.talent.map((t) => t.name).join(", ")}</p>}
      <div className="flex items-center justify-between gap-2">
        <span className="text-muted-foreground">{e.budget ? formatMoney(e.budget) : ""}</span>
        <NativeSelect
          aria-label={`Stage of ${e.title}`}
          className="h-7 w-auto text-xs"
          value={e.stage ?? ""}
          onChange={(ev) => onMove(ev.target.value)}
        >
          {stages.map((s) => (
            <option key={s.key} value={s.key}>
              {s.name}
            </option>
          ))}
        </NativeSelect>
      </div>
    </article>
  );
}

function NewEnquiryDialog({ onClose }: { onClose: () => void }) {
  const schema = useSchema("enquiry");
  const currency = useWorkspaceCurrency();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const sync = useOffline();
  const [talent, setTalent] = useState<Named[]>([]);
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="New enquiry" className="max-w-2xl">
      {schema.isPending ? (
        <Spinner />
      ) : schema.isError ? (
        <Alert>{schema.error.message}</Alert>
      ) : (
        <div className="flex flex-col gap-4">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="new-enquiry-talent">Talent</Label>
            <TalentPicker id="new-enquiry-talent" value={talent} onChange={setTalent} />
          </div>
          <EntityForm
            schema={schema.data}
            issues={issues}
            defaultCurrency={currency}
            renderRelation={RelationPicker}
            onCancel={onClose}
            onSubmit={async (body) => {
              setIssues([]);
              try {
                const { record } = await createRecord(sync, "enquiry", { ...body, talent_ids: talent.map((t) => t.id) });
                await qc.invalidateQueries({ queryKey: ["enquiries"] });
                onClose();
                if (record) void navigate({ to: "/enquiries/$id", params: { id: record.id } });
              } catch (err) {
                const found = failureIssues(err);
                if (found) setIssues(found);
                else throw err;
              }
            }}
          />
        </div>
      )}
    </Dialog>
  );
}

export function EnquiryPage() {
  const { id } = useParams({ from: "/app/enquiries/$id" });
  const qc = useQueryClient();
  const navigate = useNavigate();
  const sync = useOffline();
  const status = useSyncStatus();
  const schema = useSchema("enquiry");
  const stages = useStages();
  const currency = useWorkspaceCurrency();
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [saved, setSaved] = useState<"sent" | "queued" | null>(null);
  const [holding, setHolding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const enquiry = useQuery({
    queryKey: ["enquiry", id],
    queryFn: () => fetchRecord(sync, "enquiry", id),
  });
  const bookings = useQuery({
    queryKey: ["bookings", "enquiry", id],
    queryFn: async () => (await fetchBookings(sync, { enquiry_id: id, limit: 100 })).data,
  });
  // An enquiry made offline gets its real id once it reaches the server.
  const renamed = isLocalId(id) ? sync?.renamed.get(id) : undefined;
  useEffect(() => {
    if (renamed) void navigate({ to: "/enquiries/$id", params: { id: renamed }, replace: true });
  }, [renamed, navigate]);

  if (schema.isPending || enquiry.isPending || stages.isPending) return <Loading />;
  if (schema.isError) return <PageError error={schema.error} />;
  if (enquiry.isError) return <PageError error={enquiry.error} />;
  const e = enquiry.data.data as unknown as Enquiry;
  const offline = enquiry.data.offline || status?.online === false;

  const refresh = (next: Enquiry | null, queued = false) => {
    if (next) qc.setQueryData(["enquiry", id], { data: next, offline: queued && !navigator.onLine });
    void qc.invalidateQueries({ queryKey: ["enquiries"] });
    void qc.invalidateQueries({ queryKey: ["timeline", "enquiries", id] });
  };
  const save = async (body: Record<string, unknown>) => {
    const result = await updateRecord(sync, "enquiry", id, body, e.version);
    refresh(result.record as unknown as Enquiry | null, result.queued);
    return result;
  };
  const patch = async (body: Record<string, unknown>) => {
    setError(null);
    try {
      await save(body);
    } catch (err) {
      if (err instanceof ApiFailure && err.status === 412) {
        setError("Someone else changed this enquiry while you were looking at it. Reload to see their changes.");
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    }
  };

  return (
    <div className="mx-auto flex max-w-7xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="ghost" size="icon" asChild aria-label="Back to enquiries">
          <Link to="/enquiries">
            <ArrowLeft />
          </Link>
        </Button>
        <h1 className="min-w-0 flex-1 truncate text-xl font-semibold">{e.title}</h1>
        <NativeSelect
          aria-label="Stage"
          className="w-auto"
          value={e.stage ?? ""}
          onChange={(ev) => {
            const stage = ev.target.value;
            const lost = stages.data?.find((s) => s.key === stage)?.outcome === "lost";
            const reason = lost ? window.prompt("Why was it lost? (optional)") : undefined;
            if (reason === null) return;
            void moveStage(sync, e, stage, reason).then(
              (next) => refresh(next),
              (err: Error) => setError(err.message),
            );
          }}
        >
          {(stages.data ?? []).map((s) => (
            <option key={s.key} value={s.key}>
              {s.name}
            </option>
          ))}
        </NativeSelect>
        <Button
          variant="outline"
          onClick={async () => {
            if (!window.confirm(`Delete ${e.title}?`)) return;
            await deleteRecord(sync, "enquiry", id, e.version);
            await qc.invalidateQueries({ queryKey: ["enquiries"] });
            void navigate({ to: "/enquiries" });
          }}
        >
          <Trash2 /> Delete
        </Button>
      </div>
      {offline && <OfflineNote />}
      {error && <Alert>{error}</Alert>}
      {e.lost_reason && <Alert tone="info">Lost: {e.lost_reason}</Alert>}
      <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <section className="flex flex-col gap-6" aria-label="Details">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="enquiry-talent">Talent</Label>
            <TalentPicker
              id="enquiry-talent"
              value={e.talent}
              onChange={(talent) => void patch({ talent_ids: talent.map((t) => t.id) })}
            />
          </div>
          {saved === "sent" && <Alert tone="success">Saved.</Alert>}
          {saved === "queued" && <Alert tone="info">Saved on this device. It will be sent when you are back online.</Alert>}
          <EntityForm
            key={String(e.version)}
            schema={schema.data}
            record={e as never}
            issues={issues}
            defaultCurrency={currency}
            renderRelation={RelationPicker}
            onSubmit={async (body) => {
              setIssues([]);
              setSaved(null);
              if (Object.keys(body).length === 0) return;
              try {
                const result = await save(body);
                setSaved(result.queued ? "queued" : "sent");
              } catch (err) {
                const found = failureIssues(err);
                if (found) setIssues(found);
                else throw err;
              }
            }}
          />
          <section className="flex flex-col gap-3" aria-label="Holds and bookings">
            <div className="flex items-center justify-between">
              <h2 className="font-semibold">Holds and bookings</h2>
              <Button variant="outline" size="sm" onClick={() => setHolding(true)}>
                <CalendarPlus /> Place a hold
              </Button>
            </div>
            {bookings.isPending ? (
              <Spinner />
            ) : (bookings.data?.items.length ?? 0) === 0 ? (
              <p className="text-sm text-muted-foreground">No dates held yet.</p>
            ) : (
              <ul className="flex flex-col gap-2">
                {bookings.data?.items.map((b) => (
                  <li key={b.id}>
                    <BookingCard booking={b} />
                  </li>
                ))}
              </ul>
            )}
          </section>
        </section>
        <section aria-label="Tasks and activity" className="flex flex-col gap-6">
          <RelatedTasks entity="enquiry" id={id} />
          <RelatedConversations filter={{ enquiry_id: id }} />
          <RecordTimeline path="enquiries" id={id} schema={schema.data} offline={offline} />
        </section>
      </div>
      {holding && (
        <NewBookingDialog
          initial={{
            enquiry_id: e.id,
            title: e.title,
            talent: e.talent,
            day: e.starts_on ?? undefined,
          }}
          onClose={() => setHolding(false)}
          onCreated={() => void bookings.refetch()}
        />
      )}
    </div>
  );
}
