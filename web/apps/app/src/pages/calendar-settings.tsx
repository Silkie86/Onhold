// Calendar settings: how long holds last, and private links that put the
// workspace's bookings (or one talent's) in a phone or desktop calendar.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, Button, Input, Label, NativeSelect, Spinner } from "@onhold/ui";
import { Check, Copy, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { api, ok } from "../api.ts";
import { fetchList, fetchWorkspace, useOffline } from "../data.ts";
import { useSession } from "../session.tsx";

export function CalendarSettingsPage() {
  const { workspace } = useSession();
  const admin = workspace.role === "owner" || workspace.role === "admin";
  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-8 p-4 sm:p-6">
      <h1 className="text-xl font-semibold">Calendar</h1>
      {admin && <HoldPeriod />}
      <Feeds />
    </div>
  );
}

function HoldPeriod() {
  const sync = useOffline();
  const qc = useQueryClient();
  const ws = useQuery({ queryKey: ["workspace"], queryFn: async () => (await fetchWorkspace(sync)).data });
  const [days, setDays] = useState<string | null>(null);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  if (ws.isPending) return <Spinner />;
  if (ws.isError) return <Alert>{ws.error.message}</Alert>;
  const value = days ?? String(ws.data.hold_days);
  return (
    <section className="flex flex-col gap-3" aria-label="Holds">
      <h2 className="font-semibold">Holds</h2>
      <form
        className="flex flex-wrap items-end gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setMessage(null);
          try {
            const saved = await ok(
              api.PATCH("/api/v1/workspace", {
                headers: { "If-Match": `"${ws.data.version}"` },
                body: { hold_days: Number(value) },
              }),
            );
            qc.setQueryData(["workspace"], saved);
            setDays(null);
            setMessage({ ok: true, text: "Saved. New holds use this; existing ones keep their date." });
          } catch (err) {
            setMessage({ ok: false, text: err instanceof Error ? err.message : String(err) });
          }
        }}
      >
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="hold-days">A new hold lapses after</Label>
          <div className="flex items-center gap-2">
            <Input
              id="hold-days"
              type="number"
              min={0}
              max={365}
              className="w-24"
              value={value}
              onChange={(e) => setDays(e.target.value)}
            />
            <span className="text-sm">days</span>
          </div>
        </div>
        <Button type="submit" variant="outline" disabled={days === null}>
          Save
        </Button>
      </form>
      <p className="text-sm text-muted-foreground">0 means holds never lapse by themselves. A lapsed hold can be put back while the date is free.</p>
      {message && <Alert tone={message.ok ? "success" : "error"}>{message.text}</Alert>}
    </section>
  );
}

function Feeds() {
  const qc = useQueryClient();
  const sync = useOffline();
  const [name, setName] = useState("");
  const [talentId, setTalentId] = useState("");
  const [created, setCreated] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const feeds = useQuery({ queryKey: ["calendar-feeds"], queryFn: () => ok(api.GET("/api/v1/calendar/feeds")) });
  const talent = useQuery({
    queryKey: ["list", "talent", "calendar"],
    queryFn: async () => (await fetchList(sync, "talent", { limit: 200, sort: "name" })).data,
  });
  const refresh = () => void qc.invalidateQueries({ queryKey: ["calendar-feeds"] });
  return (
    <section className="flex flex-col gap-3" aria-label="Calendar links">
      <h2 className="font-semibold">Calendar links</h2>
      <p className="text-sm text-muted-foreground">
        Subscribe to a link in Google Calendar, Outlook or your phone to see bookings there. Links show holds, pending and
        confirmed bookings (never fees), and anyone with a link can read it, so keep it private and remove it when it is
        no longer needed.
      </p>
      <form
        className="flex flex-wrap items-end gap-2"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          setCreated(null);
          try {
            const made = await ok(
              api.POST("/api/v1/calendar/feeds", { body: { name, talent_id: talentId || null } }),
            );
            setCreated(made.url);
            setCopied(false);
            setName("");
            refresh();
          } catch (err) {
            setError(err instanceof Error ? err.message : String(err));
          }
        }}
      >
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="feed-name">Name</Label>
          <Input id="feed-name" required placeholder="My phone" value={name} onChange={(e) => setName(e.target.value)} />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="feed-talent">Bookings of</Label>
          <NativeSelect id="feed-talent" value={talentId} onChange={(e) => setTalentId(e.target.value)}>
            <option value="">All talent</option>
            {talent.data?.items.map((t) => (
              <option key={t.id} value={t.id}>
                {String(t.name)}
              </option>
            ))}
          </NativeSelect>
        </div>
        <Button type="submit">
          <Plus /> Make a link
        </Button>
      </form>
      {error && <Alert>{error}</Alert>}
      {created && (
        <Alert tone="success">
          <p className="mb-2">Here is the link. It is shown only now: copy it into your calendar app.</p>
          <div className="flex gap-2">
            <Input readOnly value={created} aria-label="New calendar link" onFocus={(e) => e.target.select()} />
            <Button
              variant="outline"
              size="icon"
              aria-label="Copy link"
              onClick={() => void navigator.clipboard?.writeText(created).then(() => setCopied(true))}
            >
              {copied ? <Check /> : <Copy />}
            </Button>
          </div>
        </Alert>
      )}
      {feeds.isPending ? (
        <Spinner />
      ) : (feeds.data?.length ?? 0) === 0 ? (
        <p className="text-sm text-muted-foreground">No links yet.</p>
      ) : (
        <ul className="divide-y rounded-md border">
          {feeds.data?.map((f) => (
            <li key={f.id} className="flex items-center gap-2 px-3 py-2 text-sm">
              <span className="flex min-w-0 flex-1 flex-col">
                <span className="font-medium">{f.name}</span>
                <span className="text-muted-foreground">
                  {f.talent_name ?? "All talent"} ·{" "}
                  {f.last_used_at
                    ? `last read ${new Date(f.last_used_at).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}`
                    : "not used yet"}
                </span>
              </span>
              <Button
                variant="ghost"
                size="icon"
                aria-label={`Remove ${f.name}`}
                onClick={async () => {
                  if (!window.confirm(`Stop the ${f.name} link working?`)) return;
                  await ok(api.DELETE("/api/v1/calendar/feeds/{id}", { params: { path: { id: f.id } } }));
                  refresh();
                }}
              >
                <Trash2 />
              </Button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
