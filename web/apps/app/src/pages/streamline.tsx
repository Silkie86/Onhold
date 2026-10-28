// Streamline (OnHold's team, on a visit): hide menu items, parts of pages
// and optional fields a client does not need, on top of their setup. It
// survives the client sending their setup form again, deletes nothing, and
// required fields always show.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, Button, Card, Checkbox, Spinner } from "@onhold/ui";
import { useEffect, useState } from "react";
import { api, ok } from "../api.ts";
import { useMe } from "../session.tsx";
import { SECTIONS } from "../setup.ts";
import { NAV } from "../shell.tsx";

const ENTITY_NAMES: Record<string, string> = {
  contact: "Contacts",
  organisation: "Organisations",
  talent: "Talent",
  enquiry: "Enquiries",
  booking: "Bookings",
};

export function StreamlinePage() {
  const me = useMe();
  const qc = useQueryClient();
  const options = useQuery({ queryKey: ["streamline"], queryFn: () => ok(api.GET("/api/v1/workspace/streamline")) });
  const [hidden, setHidden] = useState<Set<string> | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  useEffect(() => {
    if (options.data && hidden === null) setHidden(new Set(options.data.hidden));
  }, [options.data, hidden]);

  if (!me.data?.user.is_superadmin) return <div className="mx-auto max-w-md p-6 text-sm">This page is for OnHold&apos;s team.</div>;
  if (options.isPending || !hidden) return <Spinner />;
  if (options.isError) return <Alert>{options.error.message}</Alert>;

  const toggle = (key: string, show: boolean) => {
    setMessage(null);
    setHidden((h) => {
      const next = new Set(h);
      if (show) next.delete(key);
      else next.add(key);
      return next;
    });
  };
  const row = (key: string, label: string) => (
    <label key={key} className="flex min-h-10 cursor-pointer items-center gap-3 rounded-md border px-3 py-2 text-sm hover:bg-accent">
      <Checkbox checked={!hidden.has(key)} onChange={(e) => toggle(key, e.target.checked)} aria-label={`Show ${label}`} />
      <span className={hidden.has(key) ? "text-muted-foreground line-through" : undefined}>{label}</span>
    </label>
  );
  const entities = [...new Set(options.data.fields.map((f) => f.entity))];

  return (
    <div className="mx-auto flex max-w-4xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-col gap-1">
        <h1 className="text-xl font-semibold">Streamline this app</h1>
        <p className="text-sm text-muted-foreground">
          Untick anything this client doesn&apos;t need. It disappears from their menus, pages and forms, on top of what their setup form switched
          on, and stays hidden when they send the form again. Nothing is deleted, and required fields always show. Tick it again to bring it back.
        </p>
      </div>
      <Card className="flex flex-col gap-3 p-4">
        <h2 className="font-semibold">Menu</h2>
        {NAV.map((g) => (
          <fieldset key={g.group} className="flex flex-col gap-2">
            <legend className="mb-1 text-xs font-medium text-muted-foreground">{g.group}</legend>
            <div className="grid gap-2 sm:grid-cols-2">{g.items.map((i) => row(`nav:${i.to}`, i.label))}</div>
          </fieldset>
        ))}
      </Card>
      <Card className="flex flex-col gap-3 p-4">
        <h2 className="font-semibold">Parts of pages</h2>
        <div className="grid gap-2 sm:grid-cols-2">{SECTIONS.map((s) => row(s.key, s.label))}</div>
      </Card>
      <Card className="flex flex-col gap-3 p-4">
        <h2 className="font-semibold">Fields</h2>
        <p className="-mt-1 text-sm text-muted-foreground">Optional fields only; names, dates and amounts that documents need always show.</p>
        {entities.map((e) => (
          <fieldset key={e} className="flex flex-col gap-2">
            <legend className="mb-1 text-xs font-medium text-muted-foreground">{ENTITY_NAMES[e] ?? e}</legend>
            <div className="grid gap-2 sm:grid-cols-2">
              {options.data.fields.filter((f) => f.entity === e).map((f) => row(`field:${f.entity}.${f.key}`, f.label))}
            </div>
          </fieldset>
        ))}
      </Card>
      {message && <Alert tone={message.ok ? "success" : "error"}>{message.text}</Alert>}
      <div className="flex flex-wrap gap-2">
        <Button
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            setMessage(null);
            try {
              await ok(
                api.PUT("/api/v1/workspace/streamline", {
                  params: { header: { "If-Match": String(options.data.version) } },
                  body: { hidden: [...hidden] },
                }),
              );
              await Promise.all([
                qc.invalidateQueries({ queryKey: ["workspace"] }),
                qc.invalidateQueries({ queryKey: ["schema"] }),
                qc.invalidateQueries({ queryKey: ["streamline"] }),
              ]);
              setHidden(null);
              setMessage({ ok: true, text: "Saved. Their app now shows only what's ticked." });
            } catch (err) {
              setMessage({ ok: false, text: err instanceof Error ? err.message : String(err) });
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy ? "Saving…" : "Save"}
        </Button>
        <Button variant="outline" disabled={busy || hidden.size === 0} onClick={() => setHidden(new Set())}>
          Show everything again
        </Button>
      </div>
    </div>
  );
}
