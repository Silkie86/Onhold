// Calendar settings: how long holds last, bookings written into a member's
// own Google or Outlook calendar, and private links that put the
// workspace's bookings (or one talent's) in a phone or desktop calendar.
// Google and Microsoft send the member back here with `code` and `state`,
// which this page passes on to finish connecting.

import type { Schemas } from "@onhold/api-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { Alert, Badge, Button, Input, Label, NativeSelect, Spinner, relativeTime } from "@onhold/ui";
import { Check, Copy, Plus, RefreshCw, Trash2, Unplug } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { fetchList, fetchWorkspace, useOffline } from "../data.ts";
import { useSession } from "../session.tsx";

export function CalendarSettingsPage() {
  const { workspace } = useSession();
  const admin = workspace.role === "owner" || workspace.role === "admin";
  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-8 p-4 sm:p-6">
      <h1 className="text-xl font-semibold">Calendar</h1>
      {admin && <HoldPeriod />}
      <OwnCalendars />
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

type Push = Schemas["CalendarPushView"];

function failure(e: unknown): string {
  return e instanceof ApiFailure || e instanceof Error ? e.message : "Something went wrong.";
}

function PushStatus({ p }: { p: Push }) {
  if (p.status === "active") return <Badge className="border-emerald-600 bg-emerald-600 text-white">Working</Badge>;
  if (p.status === "failing") return <Badge className="border border-amber-500 text-amber-800 dark:text-amber-300">Having trouble</Badge>;
  return <Badge className="border-destructive bg-destructive text-white">Needs connecting again</Badge>;
}

/** Bookings written into the member's own Google or Outlook calendar. */
function OwnCalendars() {
  const qc = useQueryClient();
  const sync = useOffline();
  const search = useSearch({ from: "/app/settings/calendar" });
  const navigate = useNavigate();
  const [talentId, setTalentId] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(
    search.error ? (search.error_description ?? "The calendar was not connected.") : null,
  );
  const [notice, setNotice] = useState<string | null>(null);
  const finishing = useRef(false);
  const pushes = useQuery({ queryKey: ["calendar-pushes"], queryFn: () => ok(api.GET("/api/v1/calendar/pushes")) });
  const talent = useQuery({
    queryKey: ["list", "talent", "calendar"],
    queryFn: async () => (await fetchList(sync, "talent", { limit: 200, sort: "name" })).data,
  });
  const refresh = () => void qc.invalidateQueries({ queryKey: ["calendar-pushes"] });

  // Back from Google or Microsoft: finish once, then drop the code from the address bar.
  useEffect(() => {
    if (!search.code || !search.state || finishing.current) return;
    finishing.current = true;
    setBusy(true);
    ok(api.POST("/api/v1/calendar/pushes/finish", { body: { code: search.code, state: search.state } }))
      .then((p) => {
        setNotice(`${p.account} is connected. Bookings will appear in an "OnHold" calendar there within a few minutes.`);
        refresh();
      })
      .catch((e: unknown) => setError(failure(e)))
      .finally(() => {
        setBusy(false);
        void navigate({ to: "/settings/calendar", search: {}, replace: true });
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [search.code, search.state]);

  // A refusal from the provider: keep the message, drop it from the address bar.
  useEffect(() => {
    if (search.error && !search.code) void navigate({ to: "/settings/calendar", search: {}, replace: true });
  }, [search.error, search.code, navigate]);

  const connect = async (provider: "google" | "microsoft", talent: string | null) => {
    setError(null);
    setBusy(true);
    try {
      const { url } = await ok(api.POST("/api/v1/calendar/pushes/start", { body: { provider, talent_id: talent } }));
      window.location.assign(url);
    } catch (e) {
      setError(failure(e));
      setBusy(false);
    }
  };
  const act = async (call: () => Promise<unknown>, done?: string) => {
    setError(null);
    setNotice(null);
    try {
      await call();
      if (done) setNotice(done);
      refresh();
    } catch (e) {
      setError(failure(e));
    }
  };

  if (pushes.isPending || busy) return <Spinner />;
  if (pushes.isError) return <Alert>{pushes.error.message}</Alert>;
  const { calendars, google, microsoft } = pushes.data;
  if (!google && !microsoft && calendars.length === 0) return null;
  return (
    <section className="flex flex-col gap-3" aria-label="Your own calendar">
      <h2 className="font-semibold">Your own calendar</h2>
      <p className="text-sm text-muted-foreground">
        Connect your Google or Outlook calendar and OnHold keeps the bookings in a calendar of its own there, updated as
        they change: holds and pending bookings as tentative, confirmed ones as busy, never fees. OnHold doesn&apos;t
        read or change your other events.
      </p>
      {error && <Alert>{error}</Alert>}
      {notice && <Alert tone="success">{notice}</Alert>}
      <div className="flex flex-wrap items-end gap-2">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="push-talent">Bookings of</Label>
          <NativeSelect id="push-talent" value={talentId} onChange={(e) => setTalentId(e.target.value)}>
            <option value="">All talent</option>
            {talent.data?.items.map((t) => (
              <option key={t.id} value={t.id}>
                {String(t.name)}
              </option>
            ))}
          </NativeSelect>
        </div>
        {google && (
          <Button variant="outline" onClick={() => void connect("google", talentId || null)}>
            Connect Google Calendar
          </Button>
        )}
        {microsoft && (
          <Button variant="outline" onClick={() => void connect("microsoft", talentId || null)}>
            Connect Outlook
          </Button>
        )}
      </div>
      {calendars.length > 0 && (
        <ul className="divide-y rounded-md border">
          {calendars.map((p) => (
            <li key={p.id} className="flex flex-wrap items-center gap-2 px-3 py-2 text-sm">
              <span className="flex min-w-0 flex-1 flex-col">
                <span className="font-medium">
                  {p.account} <PushStatus p={p} />
                </span>
                <span className="text-muted-foreground">
                  {p.provider === "google" ? "Google Calendar" : "Outlook"} · {p.talent_name ?? "All talent"} ·{" "}
                  {p.last_synced_at ? `up to date ${relativeTime(p.last_synced_at)}` : "being filled"}
                </span>
                {p.status !== "active" && p.last_error && <span className="text-destructive">{p.last_error}</span>}
              </span>
              {p.status === "reconnect" ? (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => void connect(p.provider === "google" ? "google" : "microsoft", p.talent_id ?? null)}
                >
                  Connect again
                </Button>
              ) : (
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={`Update ${p.account} now`}
                  onClick={() =>
                    void act(
                      () => ok(api.POST("/api/v1/calendar/pushes/{id}/sync", { params: { path: { id: p.id } } })),
                      "Updating now.",
                    )
                  }
                >
                  <RefreshCw />
                </Button>
              )}
              <Button
                variant="ghost"
                size="icon"
                aria-label={`Disconnect ${p.account}`}
                onClick={() => {
                  if (!window.confirm(`Stop writing bookings into ${p.account}? OnHold's calendar there is removed.`)) return;
                  void act(() => ok(api.DELETE("/api/v1/calendar/pushes/{id}", { params: { path: { id: p.id } } })));
                }}
              >
                <Unplug />
              </Button>
            </li>
          ))}
        </ul>
      )}
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
