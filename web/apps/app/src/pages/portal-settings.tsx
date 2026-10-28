// The agency's side of the talent portal: inviting a talent from their
// record, and what every talent may see and do (Settings → Talent portal).

import type { Schemas } from "@onhold/api-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, Button, Checkbox, Input, Label, Spinner } from "@onhold/ui";
import { MailPlus, UserRoundX } from "lucide-react";
import { useState } from "react";
import { api, ok } from "../api.ts";
import { useSession } from "../session.tsx";

type Settings = Schemas["PortalSettings"];

function failure(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** On a talent's page: who their portal opens for, and inviting them. */
export function PortalAccessCard({ talentId, canWrite }: { talentId: string; canWrite: boolean }) {
  const qc = useQueryClient();
  const key = ["portal-access", talentId];
  const access = useQuery({
    queryKey: key,
    queryFn: () => ok(api.GET("/api/v1/talent/{id}/portal", { params: { path: { id: talentId } } })),
  });
  const [email, setEmail] = useState("");
  const [inviting, setInviting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const act = async (call: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await call();
      setInviting(false);
      setEmail("");
    } catch (e) {
      setError(failure(e));
    } finally {
      setBusy(false);
      void qc.invalidateQueries({ queryKey: key });
    }
  };
  const a = access.data;
  return (
    <section className="flex flex-col gap-3" aria-label="Their portal">
      <div className="flex items-center justify-between gap-2">
        <h2 className="font-semibold">Their portal</h2>
        {canWrite && a && !a.user && !inviting && (
          <Button variant="outline" size="sm" onClick={() => setInviting(true)}>
            <MailPlus /> {a.invitation ? "Invite again" : "Invite"}
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">
        A free page, made for phones, where they see their own dates with times, travel and documents, confirm or decline
        dates and mark when they can&apos;t work. They see nothing else of your workspace.
      </p>
      {access.isPending ? (
        <Spinner />
      ) : access.isError ? (
        <Alert>{access.error.message}</Alert>
      ) : a?.user ? (
        <div className="flex flex-wrap items-center gap-2 rounded-md border px-3 py-2 text-sm">
          <span className="min-w-0 flex-1">
            Open for <strong>{a.user.name}</strong> <span className="text-muted-foreground">({a.user.email})</span>
          </span>
          {canWrite && (
            <Button
              variant="ghost"
              size="sm"
              disabled={busy}
              onClick={() => {
                if (window.confirm(`${a.user!.name} will no longer see these dates. Remove their access?`)) {
                  void act(() => ok(api.DELETE("/api/v1/talent/{id}/portal", { params: { path: { id: talentId } } })));
                }
              }}
            >
              <UserRoundX /> Remove access
            </Button>
          )}
        </div>
      ) : a?.invitation ? (
        <div className="flex flex-wrap items-center gap-2 rounded-md border px-3 py-2 text-sm">
          <span className="min-w-0 flex-1">
            Invitation sent to <strong>{a.invitation.email}</strong>, open until{" "}
            {new Date(a.invitation.expires_at).toLocaleDateString(undefined, { day: "numeric", month: "short" })}.
          </span>
          {canWrite && (
            <Button
              variant="ghost"
              size="sm"
              disabled={busy}
              onClick={() => void act(() => ok(api.DELETE("/api/v1/talent/{id}/portal", { params: { path: { id: talentId } } })))}
            >
              Cancel invitation
            </Button>
          )}
        </div>
      ) : (
        <p className="text-sm">Not invited yet.</p>
      )}
      {inviting && (
        <form
          className="flex flex-col gap-2 rounded-md border p-3"
          onSubmit={(e) => {
            e.preventDefault();
            void act(() => ok(api.POST("/api/v1/talent/{id}/portal", { params: { path: { id: talentId } }, body: { email } })));
          }}
        >
          <Label htmlFor={`portal-email-${talentId}`}>Their email</Label>
          <Input
            id={`portal-email-${talentId}`}
            type="email"
            required
            autoComplete="off"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
          />
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setInviting(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={busy}>
              Send invitation
            </Button>
          </div>
        </form>
      )}
      {error && <Alert>{error}</Alert>}
    </section>
  );
}

const SWITCHES: { key: keyof Settings; label: string; hint: string }[] = [
  { key: "answer_dates", label: "Confirm or decline dates", hint: "You get a task when they answer. The booking itself stays yours to move." },
  { key: "show_holds", label: "Show pencilled dates", hint: "Holds as well as dates being confirmed and confirmed ones." },
  { key: "show_fees", label: "Show fees", hint: "The fee on dates they play alone, and contracts and confirmations, which carry it." },
  { key: "show_travel", label: "Show travel", hint: "Flights, trains, rides and hotels on their dates; never what they cost." },
  { key: "show_documents", label: "Show documents", hint: "Contracts, advance sheets, riders and stage plots on their dates and record." },
  { key: "edit_availability", label: "Mark their own availability", hint: "Unavailable and tentative times, and connecting their own calendar." },
  { key: "show_money", label: "Show statements", hint: "What each settled date paid them, and what was paid out." },
];

export function PortalSettingsPage() {
  const { workspace } = useSession();
  const admin = workspace.role === "owner" || workspace.role === "admin";
  const qc = useQueryClient();
  const saved = useQuery({ queryKey: ["portal-settings"], queryFn: () => ok(api.GET("/api/v1/workspace/portal")) });
  const [draft, setDraft] = useState<Settings | null>(null);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  if (saved.isPending) return <Spinner />;
  if (saved.isError) return <Alert>{saved.error.message}</Alert>;
  const shown = draft ?? saved.data;
  return (
    <div className="mx-auto flex max-w-2xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-col gap-1">
        <h1 className="text-xl font-semibold">Talent portal</h1>
        <p className="text-sm text-muted-foreground">
          The people you represent get a free page of their own dates. Invite each one from their record under Talent.
          These switches set what all of them see and can do.
        </p>
      </div>
      {!admin && <Alert tone="info">Only owners and admins can change these.</Alert>}
      <ul className="flex flex-col divide-y rounded-lg border">
        {SWITCHES.map((s) => (
          <li key={s.key}>
            <label className="flex items-start gap-3 px-3 py-3">
              <Checkbox
                className="mt-0.5"
                disabled={!admin}
                checked={shown[s.key]}
                onChange={(e) => {
                  setMessage(null);
                  setDraft({ ...shown, [s.key]: e.target.checked });
                }}
              />
              <span className="flex flex-col gap-0.5">
                <span className="text-sm font-medium">{s.label}</span>
                <span className="text-xs text-muted-foreground">{s.hint}</span>
              </span>
            </label>
          </li>
        ))}
      </ul>
      {message && <Alert tone={message.ok ? "success" : "error"}>{message.text}</Alert>}
      {admin && (
        <div className="flex justify-end gap-2">
          {draft && (
            <Button variant="ghost" onClick={() => setDraft(null)}>
              Undo changes
            </Button>
          )}
          <Button
            disabled={!draft || busy}
            onClick={async () => {
              if (!draft) return;
              setBusy(true);
              try {
                const next = await ok(api.PUT("/api/v1/workspace/portal", { body: draft }));
                qc.setQueryData(["portal-settings"], next);
                setDraft(null);
                setMessage({ ok: true, text: "Saved. Talent see the change the next time they open their page." });
              } catch (e) {
                setMessage({ ok: false, text: failure(e) });
              } finally {
                setBusy(false);
              }
            }}
          >
            Save
          </Button>
        </div>
      )}
    </div>
  );
}
