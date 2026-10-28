// Email signatures: yours, under what you send from the inbox and the
// quotes, invoices and signing requests you send; and the workspace's,
// used when a member has none and under emails rules send.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, Button, Card, Label, Spinner, Textarea } from "@onhold/ui";
import { useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";

function failure(e: unknown): string {
  return e instanceof ApiFailure || e instanceof Error ? e.message : "Something went wrong.";
}

function SignatureForm({
  id,
  label,
  hint,
  saved,
  save,
}: {
  id: string;
  label: string;
  hint: string;
  saved: string | null | undefined;
  save: (signature: string | null) => Promise<unknown>;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  const value = draft ?? saved ?? "";
  return (
    <form
      className="flex flex-col gap-2"
      onSubmit={async (e) => {
        e.preventDefault();
        setMessage(null);
        try {
          await save(value.trim() ? value : null);
          setDraft(null);
          setMessage({ ok: true, text: value.trim() ? "Saved." : "Removed." });
        } catch (err) {
          setMessage({ ok: false, text: failure(err) });
        }
      }}
    >
      <Label htmlFor={id}>{label}</Label>
      <Textarea
        id={id}
        rows={4}
        maxLength={2000}
        placeholder={"Sam Lee\nBookings, The Agency\n07700 900123"}
        value={value}
        onChange={(e) => setDraft(e.target.value)}
      />
      <p className="text-sm text-muted-foreground">{hint}</p>
      <div>
        <Button type="submit" variant="outline" disabled={draft === null}>
          Save
        </Button>
      </div>
      {message && <Alert tone={message.ok ? "success" : "error"}>{message.text}</Alert>}
    </form>
  );
}

export function SignatureCard() {
  const qc = useQueryClient();
  const sigs = useQuery({ queryKey: ["signatures"], queryFn: () => ok(api.GET("/api/v1/signatures")) });
  if (sigs.isPending) return <Spinner />;
  if (sigs.isError) return <Alert>{sigs.error.message}</Alert>;
  const s = sigs.data;
  const done = (next: typeof s) => qc.setQueryData(["signatures"], next);
  // The workspace's signature is part of the workspace, whose version moves.
  const doneWorkspace = (next: typeof s) => {
    done(next);
    void qc.invalidateQueries({ queryKey: ["workspace"] });
  };
  return (
    <Card className="flex flex-col gap-5 p-4" aria-label="Signatures">
      <h2 className="font-semibold">Signature</h2>
      {s.is_member && (
        <SignatureForm
          id="my-signature"
          label="Your signature"
          hint="Added under what you send from the inbox, and under the quotes, invoices and signing requests you send. Plain text."
          saved={s.mine}
          save={async (signature) => done(await ok(api.PUT("/api/v1/signatures/mine", { body: { signature } })))}
        />
      )}
      {s.can_edit_workspace ? (
        <SignatureForm
          id="workspace-signature"
          label="Workspace signature"
          hint="Used for members who have not written their own, and under emails your rules send."
          saved={s.workspace}
          save={async (signature) =>
            doneWorkspace(await ok(api.PUT("/api/v1/signatures/workspace", { body: { signature } })))
          }
        />
      ) : (
        s.workspace &&
        !s.mine && <p className="text-sm text-muted-foreground">Until you write your own, the workspace&apos;s signature is used.</p>
      )}
    </Card>
  );
}
