// Follow-ups: ready-made chasers a workspace switches on in one click (an
// unanswered enquiry, a quote nobody opened, an overdue invoice, a reminder
// text before a booking). Each is ordinary rules and templates underneath,
// so its wording can be changed; switching it off removes the rules only.

import type { Schemas } from "@onhold/api-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, Card, Checkbox, Spinner } from "@onhold/ui";
import { useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { useSession } from "../session.tsx";
import { useSetup } from "../setup.ts";

type FollowUp = Schemas["FollowUp"];

function failure(e: unknown): string {
  return e instanceof ApiFailure || e instanceof Error ? e.message : "Something went wrong.";
}

export function FollowUpsSettingsPage() {
  const { workspace } = useSession();
  const admin = workspace.role === "owner" || workspace.role === "admin";
  const setup = useSetup();
  const qc = useQueryClient();
  const list = useQuery({ queryKey: ["follow-ups"], queryFn: () => ok(api.GET("/api/v1/follow-ups")) });
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const set = async (f: FollowUp, on: boolean) => {
    setBusy(f.key);
    setError(null);
    try {
      const after = await ok(api.PUT("/api/v1/follow-ups/{key}", { params: { path: { key: f.key } }, body: { on } }));
      qc.setQueryData<FollowUp[]>(["follow-ups"], (old) => old?.map((x) => (x.key === after.key ? after : x)));
    } catch (e) {
      setError(failure(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-6 p-4 sm:p-6">
      <h1 className="text-xl font-semibold">Follow-ups</h1>
      <p className="text-sm text-muted-foreground">
        Chasers that run by themselves. Switch one on and it starts with what happens from now on. Their emails and texts are templates you can
        reword; switching one off stops it and keeps the wording.
      </p>
      {!admin && <Alert tone="info">Only owners and admins can switch follow-ups on or off.</Alert>}
      {error && <Alert>{error}</Alert>}
      {list.isPending ? (
        <Spinner />
      ) : list.isError ? (
        <Alert>{list.error.message}</Alert>
      ) : (
        <Card className="divide-y text-sm">
          {list.data
            .filter((f) => f.on || setup.has(f.module))
            .map((f) => (
              <label key={f.key} className="flex items-start gap-3 p-4">
                <Checkbox
                  className="mt-0.5"
                  checked={f.on}
                  disabled={!admin || busy !== null}
                  onChange={(e) => void set(f, e.target.checked)}
                />
                <span className="flex flex-col gap-0.5">
                  <span className="font-medium">
                    {f.name}
                    {busy === f.key && <Spinner className="ml-2 inline size-3" />}
                  </span>
                  <span className="text-muted-foreground">{f.description}</span>
                </span>
              </label>
            ))}
        </Card>
      )}
    </div>
  );
}
