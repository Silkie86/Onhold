// OnHold's team: every client workspace, and a time-boxed, audited visit into
// one (to set its theme at onboarding, or to help). Superadmins with
// two-factor sign-in only; the server checks the same.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { Alert, Badge, Button, Card, Input, Label, Spinner } from "@onhold/ui";
import { useState } from "react";
import { api, ok, setWorkspace } from "../api.ts";
import { GoTo } from "../go-to.tsx";
import { useMe } from "../session.tsx";

export function OperatorPage() {
  const me = useMe();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [q, setQ] = useState("");
  const [visiting, setVisiting] = useState<string | null>(null);
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
                {visiting !== w.id && (
                  <Button size="sm" onClick={() => setVisiting(w.id)}>
                    Visit
                  </Button>
                )}
              </div>
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
                      void navigate({ to: "/settings/theme" });
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
    </div>
  );
}
