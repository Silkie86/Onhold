// The apps' sync page: whether the device is up to date, the changes still
// waiting to be sent, and the ones the server would not take as they were.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { Alert, Badge, Button, Card, EmptyState, type RecordJson, relativeTime } from "@onhold/ui";
import { CloudOff, RefreshCw } from "lucide-react";
import { useEffect } from "react";
import { ENTITIES } from "../api.ts";
import { useOffline, useSyncStatus } from "../data.ts";
import type { OutboxItem } from "../offline/store.ts";
import { Loading } from "./crm.tsx";

const OPS: Record<OutboxItem["op"], string> = { create: "New", update: "Edited", delete: "Deleted" };

function show(v: unknown): string {
  if (v === null || v === undefined || v === "") return "(empty)";
  if (Array.isArray(v)) return v.length ? v.map(show).join(", ") : "(empty)";
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

/** The fields this change touches, with both sides when they differ. */
function differences(item: OutboxItem): { field: string; mine: string; theirs: string }[] {
  const server = (item.server ?? {}) as RecordJson;
  const rows: { field: string; mine: string; theirs: string }[] = [];
  for (const [key, value] of Object.entries(item.body)) {
    if (key === "custom" && value && typeof value === "object") {
      for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
        rows.push({ field: k, mine: show(v), theirs: show(server.custom?.[k]) });
      }
    } else {
      rows.push({ field: key.replace(/_/g, " "), mine: show(value), theirs: show(server[key]) });
    }
  }
  return rows;
}

export function SyncPage() {
  const sync = useOffline();
  const status = useSyncStatus();
  const qc = useQueryClient();
  const queued = useQuery({
    queryKey: ["outbox", sync?.ws],
    queryFn: () => sync?.queued() ?? Promise.resolve([]),
    enabled: !!sync,
  });
  // The queue changes as the engine works through it.
  useEffect(() => {
    if (!sync) return;
    return sync.subscribe(() => void qc.invalidateQueries({ queryKey: ["outbox", sync.ws] }));
  }, [sync, qc]);

  if (!sync || !status) {
    return (
      <div className="mx-auto max-w-3xl p-6">
        <Alert tone="info">Keeping a copy for offline use is part of the desktop and mobile apps.</Alert>
      </div>
    );
  }
  if (queued.isPending) return <Loading />;
  const items = queued.data ?? [];
  const attention = items.filter((i) => i.state !== "pending");
  const waiting = items.filter((i) => i.state === "pending");

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-4 p-4 sm:p-6">
      <h1 className="text-xl font-semibold">Sync</h1>
      <Card className="flex flex-wrap items-center justify-between gap-3 p-4">
        <div className="flex items-center gap-3 text-sm">
          {status.online ? <RefreshCw className="size-5" /> : <CloudOff className="size-5" />}
          <div>
            <p className="font-medium">{status.online ? "Online" : "Offline"}</p>
            <p className="text-muted-foreground">
              {status.lastSync ? `Up to date as of ${relativeTime(status.lastSync)}` : "Not downloaded yet"}
            </p>
          </div>
        </div>
        <Button variant="outline" disabled={status.syncing || !status.online} onClick={() => void sync.syncNow()}>
          <RefreshCw className={status.syncing ? "animate-spin" : undefined} /> Sync now
        </Button>
      </Card>

      {attention.length > 0 && (
        <section aria-label="Needs attention" className="flex flex-col gap-3">
          <h2 className="font-semibold">Needs your decision</h2>
          {attention.map((item) => (
            <ChangeCard key={item.seq} item={item}>
              {item.state === "conflict" ? (
                <>
                  <p className="text-sm">Someone else changed this while you were offline.</p>
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="text-left text-muted-foreground">
                        <th className="py-1 font-normal">Field</th>
                        <th className="py-1 font-normal">Yours</th>
                        <th className="py-1 font-normal">Now on the server</th>
                      </tr>
                    </thead>
                    <tbody>
                      {differences(item).map((d) => (
                        <tr key={d.field} className="border-t">
                          <td className="py-1 pr-2 capitalize">{d.field}</td>
                          <td className="py-1 pr-2 font-medium">{d.mine}</td>
                          <td className="py-1">{d.theirs}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                  <div className="flex flex-wrap justify-end gap-2">
                    <Button variant="outline" onClick={() => void sync.resolve(item.seq as number, "theirs")}>
                      Use theirs
                    </Button>
                    <Button onClick={() => void sync.resolve(item.seq as number, "mine")}>Keep mine</Button>
                  </div>
                </>
              ) : (
                <>
                  <Alert>{item.error ?? "The server did not accept this change."}</Alert>
                  <div className="flex justify-end">
                    <Button variant="outline" onClick={() => void sync.resolve(item.seq as number, "theirs")}>
                      Discard this change
                    </Button>
                  </div>
                </>
              )}
            </ChangeCard>
          ))}
        </section>
      )}

      <section aria-label="Waiting to send" className="flex flex-col gap-3">
        <h2 className="font-semibold">Waiting to send</h2>
        {waiting.length === 0 ? (
          <EmptyState title="Nothing waiting">Every change made on this device has reached the server.</EmptyState>
        ) : (
          waiting.map((item) => <ChangeCard key={item.seq} item={item} />)
        )}
      </section>
    </div>
  );
}

function ChangeCard({ item, children }: { item: OutboxItem; children?: React.ReactNode }) {
  const name = String(item.body.name ?? item.server?.name ?? "A record");
  const entity = ENTITIES[item.entity];
  return (
    <Card className="flex flex-col gap-3 p-4">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <Badge>{OPS[item.op]}</Badge>
        {item.op === "delete" ? (
          <span className="font-medium">{name}</span>
        ) : (
          <Link to="/crm/$entity/$id" params={{ entity: entity.path, id: item.id }} className="font-medium hover:underline">
            {name}
          </Link>
        )}
        <span className="text-muted-foreground">
          {entity.label.toLowerCase()} · {relativeTime(item.createdAt)}
        </span>
      </div>
      {children}
    </Card>
  );
}
