// OnHold's team: every client workspace, and a time-boxed, audited visit into
// one (to set its theme at onboarding, or to help). Superadmins with
// two-factor sign-in only; the server checks the same.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import type { Schemas } from "@onhold/api-client";
import { Alert, Badge, Button, Card, Checkbox, Input, Label, Spinner, cn } from "@onhold/ui";
import { useState } from "react";
import { api, ok, setWorkspace } from "../api.ts";
import { GoTo } from "../go-to.tsx";
import type { PriceList } from "../setup.ts";
import { useMe } from "../session.tsx";

export function OperatorPage() {
  const me = useMe();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [q, setQ] = useState("");
  const [visiting, setVisiting] = useState<string | null>(null);
  // Where a visit opens: the theme, or streamlining the client's app.
  const [goTo, setGoTo] = useState<"/settings/theme" | "/settings/streamline">("/settings/theme");
  const [reason, setReason] = useState("Onboarding: setting up their theme");
  const [error, setError] = useState<string | null>(null);
  const list = useQuery({
    queryKey: ["admin-workspaces"],
    queryFn: () => ok(api.GET("/api/v1/admin/workspaces")),
    enabled: !!me.data?.user.is_superadmin,
  });
  if (me.isPending) return <Spinner />;
  if (!me.data) return <GoTo to="/sign-in" redirect="/operator" />;
  if (!me.data.user.is_superadmin) {
    return <div className="mx-auto max-w-md p-6 text-sm">This page is for OnHold&apos;s team.</div>;
  }
  const shown = (list.data ?? []).filter((w) => `${w.name} ${w.slug}`.toLowerCase().includes(q.trim().toLowerCase()));
  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="flex-1 text-xl font-semibold">Client workspaces</h1>
        {me.data.workspaces.length > 0 && (
          <Button variant="outline" asChild>
            <Link to="/">Back to the app</Link>
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">
        Visit a workspace to set its theme or to help. A visit is time-boxed (an hour by default), shows a banner the whole time and is recorded
        in the client&apos;s audit log with your reason.
      </p>
      {!me.data.user.totp_enabled && <Alert>Turn on two-factor sign-in for your account first; OnHold&apos;s team tools need it.</Alert>}
      <Input aria-label="Find a workspace" placeholder="Find a workspace…" value={q} onChange={(e) => setQ(e.target.value)} />
      {list.isPending && <Spinner />}
      {list.isError && <Alert>{list.error.message}</Alert>}
      {error && <Alert>{error}</Alert>}
      <ul className="flex flex-col gap-2">
        {shown.map((w) => (
          <li key={w.id}>
            <Card className="flex flex-col gap-3 p-3">
              <div className="flex flex-wrap items-center gap-2">
                <span className="min-w-0 flex-1">
                  <span className="font-medium">{w.name}</span>{" "}
                  <span className="text-xs text-muted-foreground">
                    {w.slug} · {w.member_count} {w.member_count === 1 ? "member" : "members"}
                  </span>
                </span>
                <Badge className="border capitalize">{w.status}</Badge>
                <Badge className="border">{w.plan}</Badge>
                {w.open_requests > 0 && <Badge className="border bg-warning text-warning-foreground">{w.open_requests} new {w.open_requests === 1 ? "request" : "requests"}</Badge>}
                {visiting !== w.id && (
                  <>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => {
                        setGoTo("/settings/streamline");
                        setReason("Onboarding: streamlining their app");
                        setVisiting(w.id);
                      }}
                    >
                      Streamline
                    </Button>
                    <Button
                      size="sm"
                      onClick={() => {
                        setGoTo("/settings/theme");
                        setReason("Onboarding: setting up their theme");
                        setVisiting(w.id);
                      }}
                    >
                      Visit
                    </Button>
                  </>
                )}
              </div>
              <SetupLine w={w} />
              {visiting === w.id && (
                <form
                  className="flex flex-wrap items-end gap-2"
                  onSubmit={async (e) => {
                    e.preventDefault();
                    setError(null);
                    try {
                      await ok(api.POST("/api/v1/admin/impersonation", { body: { workspace_id: w.id, reason } }));
                      setWorkspace(w.id);
                      qc.removeQueries({ predicate: (x) => x.queryKey[0] !== "me" });
                      await qc.invalidateQueries({ queryKey: ["me"] });
                      void navigate({ to: goTo });
                    } catch (err) {
                      setError(err instanceof Error ? err.message : String(err));
                    }
                  }}
                >
                  <div className="flex min-w-48 flex-1 flex-col gap-1.5">
                    <Label htmlFor={`reason-${w.id}`}>Why (goes in their audit log)</Label>
                    <Input id={`reason-${w.id}`} value={reason} onChange={(e) => setReason(e.target.value)} required />
                  </div>
                  <Button type="button" variant="ghost" onClick={() => setVisiting(null)}>
                    Cancel
                  </Button>
                  <Button type="submit" disabled={!reason.trim()}>
                    Start visit
                  </Button>
                </form>
              )}
            </Card>
          </li>
        ))}
      </ul>
      <Requests />
      <PriceListEditor />
    </div>
  );
}

/** What the client set up, in a line: their roles, and what is switched on. */
function SetupLine({ w }: { w: Schemas["AdminWorkspaceView"] }) {
  const answers = (w.setup ?? null) as { features?: string[]; team?: string } | null;
  return (
    <p className="text-xs text-muted-foreground">
      {w.setup_at ? (
        <>
          Set up {new Date(w.setup_at).toLocaleDateString()}
          {w.presets.length > 0 && <> as {w.presets.join(" + ")}</>}
          {answers?.team && <> · team: {answers.team.replace("_", " ")}</>} · uses {w.enabled_modules.filter((m) => m !== "crm").join(", ") || "contacts only"}
          {w.extras.length > 0 && <> · {w.extras.join(", ")}</>}
        </>
      ) : (
        <>Setup form not sent yet{w.presets.length > 0 && <> (started as {w.presets.join(" + ")})</>}: everything is switched on.</>
      )}
    </p>
  );
}

/** What clients asked in the setup form, open ones first. */
function Requests() {
  const qc = useQueryClient();
  const requests = useQuery({ queryKey: ["admin-setup-requests"], queryFn: () => ok(api.GET("/api/v1/admin/setup-requests")) });
  const [error, setError] = useState<string | null>(null);
  return (
    <section className="flex flex-col gap-2" aria-label="Requests from clients">
      <h2 className="text-lg font-semibold">Requests from clients</h2>
      <p className="text-sm text-muted-foreground">Questions and requests clients left in their setup form.</p>
      {requests.isPending && <Spinner />}
      {requests.isError && <Alert>{requests.error.message}</Alert>}
      {error && <Alert>{error}</Alert>}
      {requests.data?.length === 0 && <p className="text-sm text-muted-foreground">Nothing yet.</p>}
      <ul className="flex flex-col gap-2">
        {requests.data?.map((r) => (
          <li key={r.id}>
            <Card className={cn("flex flex-col gap-2 p-3 text-sm", r.resolved_at && "opacity-60")}>
              <span className="whitespace-pre-wrap">{r.body}</span>
              <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                <span className="flex-1">
                  {r.workspace_name} · {r.author_name ?? "someone"}
                  {r.author_email && (
                    <>
                      {" "}
                      (<a className="underline" href={`mailto:${r.author_email}`}>{r.author_email}</a>)
                    </>
                  )}{" "}
                  · {new Date(r.created_at).toLocaleString()}
                </span>
                {r.resolved_at ? (
                  <Badge className="border">Done</Badge>
                ) : (
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={async () => {
                      setError(null);
                      try {
                        await ok(api.POST("/api/v1/admin/setup-requests/{id}/resolve", { params: { path: { id: r.id } } }));
                        await qc.invalidateQueries({ queryKey: ["admin-setup-requests"] });
                        await qc.invalidateQueries({ queryKey: ["admin-workspaces"] });
                      } catch (err) {
                        setError(err instanceof Error ? err.message : String(err));
                      }
                    }}
                  >
                    Mark done
                  </Button>
                )}
              </div>
            </Card>
          </li>
        ))}
      </ul>
    </section>
  );
}

const FEATURE_NAMES: Record<string, string> = {
  bookings: "Enquiries and bookings",
  calendar: "Calendar",
  email: "Email inbox",
  invoicing: "Quotes and invoices",
  documents: "Contracts and documents",
  events: "Events and tours",
  portal: "Artist and client logins",
  commission: "Commission",
  custom: "Custom fields and pipelines",
};
const TEAM_NAMES: Record<string, string> = { just_me: "Just me", small: "2 to 5 people", large: "6 or more" };

/** The prices the setup form's estimate uses, a month, in the list's currency. */
function PriceListEditor() {
  const qc = useQueryClient();
  const list = useQuery({ queryKey: ["admin-price-list"], queryFn: () => ok(api.GET("/api/v1/admin/price-list")) });
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  if (list.isPending) return <Spinner />;
  if (list.isError) return <Alert>{list.error.message}</Alert>;
  const p = list.data as PriceList;
  const money = (name: string, label: string, minor: number | undefined) => (
    <div key={name} className="flex flex-col gap-1.5">
      <Label htmlFor={`price-${name}`}>{label}</Label>
      <Input id={`price-${name}`} name={name} type="number" min={0} step="0.01" inputMode="decimal" defaultValue={((minor ?? 0) / 100).toString()} />
    </div>
  );
  return (
    <section className="flex flex-col gap-2" aria-label="Prices">
      <h2 className="text-lg font-semibold">Prices</h2>
      <p className="text-sm text-muted-foreground">
        What clients see as an estimate while they fill in their setup form: a base price, plus each feature they tick, plus their team size, a
        month. Nothing is charged from it.
      </p>
      <Card className="p-3">
        <form
          className="flex flex-col gap-3"
          onSubmit={async (e) => {
            e.preventDefault();
            setMessage(null);
            const f = new FormData(e.currentTarget);
            const minor = (k: string) => Math.max(0, Math.round(Number(f.get(k) ?? 0) * 100));
            const body = {
              currency: String(f.get("currency") ?? "GBP").trim().toUpperCase(),
              placeholder: f.get("placeholder") === "on",
              base: minor("base"),
              features: Object.fromEntries(Object.keys(FEATURE_NAMES).map((k) => [k, minor(`feature-${k}`)])),
              team: Object.fromEntries(Object.keys(TEAM_NAMES).map((k) => [k, minor(`team-${k}`)])),
            };
            try {
              await ok(api.PUT("/api/v1/admin/price-list", { body }));
              await qc.invalidateQueries({ queryKey: ["admin-price-list"] });
              setMessage({ ok: true, text: "Prices saved. Clients see them the next time they open their setup." });
            } catch (err) {
              setMessage({ ok: false, text: err instanceof Error ? err.message : String(err) });
            }
          }}
        >
          <div className="grid gap-3 sm:grid-cols-3">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="price-currency">Currency</Label>
              <Input id="price-currency" name="currency" maxLength={3} defaultValue={p.currency} />
            </div>
            {money("base", "Base price", p.base)}
          </div>
          <fieldset className="grid gap-3 sm:grid-cols-3">
            <legend className="mb-2 text-sm font-medium">Each feature</legend>
            {Object.entries(FEATURE_NAMES).map(([k, label]) => money(`feature-${k}`, label, p.features[k]))}
          </fieldset>
          <fieldset className="grid gap-3 sm:grid-cols-3">
            <legend className="mb-2 text-sm font-medium">Team size</legend>
            {Object.entries(TEAM_NAMES).map(([k, label]) => money(`team-${k}`, label, p.team[k]))}
          </fieldset>
          <label className="flex items-center gap-2 text-sm">
            <Checkbox name="placeholder" defaultChecked={!!p.placeholder} />
            Show as example prices (pricing not final)
          </label>
          {message && <Alert tone={message.ok ? "success" : "error"}>{message.text}</Alert>}
          <div>
            <Button type="submit">Save prices</Button>
          </div>
        </form>
      </Card>
    </section>
  );
}
