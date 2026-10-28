// Accounting: the workspace's books in Xero or QuickBooks Online. What the
// workspace issues from a chosen day on is sent there, what is paid there is
// read back, and invoices from before that day can be imported as past
// ones. Xero and QuickBooks send the person back here with `code`, `state`
// (and QuickBooks' `realmId`), which this page passes on to finish
// connecting.

import type { Schemas } from "@onhold/api-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import {
  Alert,
  Badge,
  Button,
  Input,
  Label,
  NativeSelect,
  Spinner,
  relativeTime,
} from "@onhold/ui";
import { BookOpenCheck, Download, RefreshCw, Unplug } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { useSession } from "../session.tsx";

type Connection = Schemas["AccountingConnection"];
type Provider = "xero" | "quickbooks";

const NAMES: Record<string, string> = {
  xero: "Xero",
  quickbooks: "QuickBooks",
};

function failure(e: unknown): string {
  return e instanceof ApiFailure || e instanceof Error
    ? e.message
    : "Something went wrong.";
}

function Status({ c }: { c: Connection }) {
  if (c.status === "active")
    return (
      <Badge className="border-emerald-600 bg-emerald-600 text-white">
        Working
      </Badge>
    );
  if (c.status === "failing")
    return (
      <Badge className="border border-amber-500 text-amber-800 dark:text-amber-300">
        Having trouble
      </Badge>
    );
  return (
    <Badge className="border-destructive bg-destructive text-white">
      Needs connecting again
    </Badge>
  );
}

export function AccountingSettingsPage() {
  const { workspace } = useSession();
  const manage =
    workspace.role === "owner" ||
    workspace.role === "admin" ||
    workspace.role === "finance";
  const search = useSearch({ from: "/app/settings/accounting" });
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [error, setError] = useState<string | null>(
    search.error
      ? (search.error_description ?? "The books were not connected.")
      : null,
  );
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const finishing = useRef(false);
  const accounting = useQuery({
    queryKey: ["accounting"],
    queryFn: () => ok(api.GET("/api/v1/accounting")),
  });
  const refresh = () => void qc.invalidateQueries({ queryKey: ["accounting"] });

  // Back from Xero or QuickBooks: finish once, then drop the code from the address bar.
  useEffect(() => {
    if (!search.code || !search.state || finishing.current) return;
    finishing.current = true;
    setBusy(true);
    ok(
      api.POST("/api/v1/accounting/finish", {
        body: {
          code: search.code,
          state: search.state,
          realm_id: search.realmId ?? null,
        },
      }),
    )
      .then((c) => {
        setNotice(
          `${c.tenant_name} is connected. Choose where sales and payments go to start sending.`,
        );
        refresh();
      })
      .catch((e: unknown) => setError(failure(e)))
      .finally(() => {
        setBusy(false);
        void navigate({
          to: "/settings/accounting",
          search: {},
          replace: true,
        });
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [search.code, search.state]);

  // A refusal from the provider: keep the message, drop it from the address bar.
  useEffect(() => {
    if (search.error && !search.code)
      void navigate({ to: "/settings/accounting", search: {}, replace: true });
  }, [search.error, search.code, navigate]);

  const connect = async (provider: Provider) => {
    setError(null);
    setBusy(true);
    try {
      const { url } = await ok(
        api.POST("/api/v1/accounting/start", { body: { provider } }),
      );
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

  let body;
  if (accounting.isPending || busy) body = <Spinner />;
  else if (accounting.isError) body = <Alert>{accounting.error.message}</Alert>;
  else {
    const { connection: c, xero, quickbooks } = accounting.data;
    if (!c) {
      body =
        !xero && !quickbooks ? (
          <Alert tone="info">
            This server is not set up to connect to Xero or QuickBooks yet.
          </Alert>
        ) : !manage ? (
          <Alert tone="info">
            No books are connected. An owner, admin or finance member can
            connect them.
          </Alert>
        ) : (
          <div className="flex flex-wrap gap-2">
            {xero && (
              <Button variant="outline" onClick={() => void connect("xero")}>
                Connect Xero
              </Button>
            )}
            {quickbooks && (
              <Button
                variant="outline"
                onClick={() => void connect("quickbooks")}
              >
                Connect QuickBooks
              </Button>
            )}
          </div>
        );
    } else {
      const name = NAMES[c.provider] ?? c.provider;
      body = (
        <>
          <div className="flex flex-wrap items-center gap-2 rounded-md border px-3 py-2 text-sm">
            <span className="flex min-w-0 flex-1 flex-col">
              <span className="font-medium">
                {c.tenant_name} <Status c={c} />
              </span>
              <span className="text-muted-foreground">
                {name} · sending what is issued from{" "}
                {new Date(`${c.start_date}T00:00:00`).toLocaleDateString(
                  undefined,
                  { dateStyle: "medium" },
                )}{" "}
                ·{" "}
                {!c.ready
                  ? "waiting for the choices below"
                  : c.last_synced_at
                    ? `up to date ${relativeTime(c.last_synced_at)}`
                    : "starting"}
              </span>
              {c.status !== "active" && c.last_error && (
                <span className="text-destructive">{c.last_error}</span>
              )}
            </span>
            {manage && c.status === "reconnect" ? (
              <Button
                variant="outline"
                size="sm"
                onClick={() => void connect(c.provider as Provider)}
              >
                Connect again
              </Button>
            ) : (
              c.ready && (
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label="Send now"
                  onClick={() =>
                    void act(
                      () => ok(api.POST("/api/v1/accounting/sync")),
                      "Sending now, and checking what was paid.",
                    )
                  }
                >
                  <RefreshCw />
                </Button>
              )
            )}
            {manage && (
              <Button
                variant="ghost"
                size="icon"
                aria-label={`Disconnect ${name}`}
                onClick={() => {
                  if (
                    !window.confirm(
                      `Stop sending to ${c.tenant_name} in ${name}? What is there already stays.`,
                    )
                  )
                    return;
                  void act(
                    () => ok(api.DELETE("/api/v1/accounting")),
                    `${name} is disconnected.`,
                  );
                }}
              >
                <Unplug />
              </Button>
            )}
          </div>
          {manage && (
            <Choices
              c={c}
              onSaved={(done) => void act(async () => undefined, done)}
            />
          )}
          {c.refused > 0 && <Refused name={name} />}
          {manage && c.ready && <PastInvoices c={c} name={name} act={act} />}
        </>
      );
    }
  }

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-6 p-4 sm:p-6">
      <h1 className="text-xl font-semibold">Accounting</h1>
      <p className="text-sm text-muted-foreground">
        Connect Xero or QuickBooks Online and OnHold sends what you issue there:
        invoices, credit notes and the payments recorded against them, with the
        client as a contact. What is paid in the books is marked paid here too.
        Nothing issued before the day you connect is sent; it can be imported
        from the books instead.
      </p>
      {error && <Alert>{error}</Alert>}
      {notice && <Alert tone="success">{notice}</Alert>}
      {body}
    </div>
  );
}

/** Where sales and payments go, and which tax rate each tax code is. */
function Choices({
  c,
  onSaved,
}: {
  c: Connection;
  onSaved: (done: string) => void;
}) {
  const qc = useQueryClient();
  const options = useQuery({
    queryKey: ["accounting-options", c.id],
    queryFn: () => ok(api.GET("/api/v1/accounting/options")),
    enabled: c.status !== "reconnect",
    staleTime: 5 * 60_000,
  });
  const [sales, setSales] = useState<string | null>(null);
  const [bank, setBank] = useState<string | null>(null);
  const [start, setStart] = useState<string | null>(null);
  const [taxes, setTaxes] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  if (c.status === "reconnect") return null;
  if (options.isPending) return <Spinner />;
  if (options.isError) return <Alert>{options.error.message}</Alert>;
  const o = options.data;
  const salesValue = sales ?? c.sales_account ?? "";
  const bankValue = bank ?? c.bank_account ?? "";
  const startValue = start ?? c.start_date;
  const taxValue = (id: string) =>
    taxes[id] ?? o.tax_codes.find((t) => t.tax_code_id === id)?.remote ?? "";
  const qbo = c.provider === "quickbooks";
  return (
    <form
      className="flex flex-col gap-3"
      aria-label="Where things go in the books"
      onSubmit={async (e) => {
        e.preventDefault();
        setError(null);
        setSaving(true);
        try {
          const saved = await ok(
            api.PUT("/api/v1/accounting/settings", {
              body: {
                sales_account: salesValue || null,
                bank_account: bankValue || null,
                start_date: startValue,
                tax_codes: o.tax_codes.map((t) => ({
                  tax_code_id: t.tax_code_id,
                  remote: taxValue(t.tax_code_id) || null,
                })),
              },
            }),
          );
          setSales(null);
          setBank(null);
          setStart(null);
          setTaxes({});
          void qc.invalidateQueries({ queryKey: ["accounting-options"] });
          onSaved(
            saved.ready ? "Saved. Sending starts within a minute." : "Saved.",
          );
        } catch (err) {
          setError(failure(err));
        } finally {
          setSaving(false);
        }
      }}
    >
      <h2 className="font-semibold">Where things go</h2>
      {o.warnings.map((w) => (
        <Alert key={w} tone="info">
          {w}
        </Alert>
      ))}
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="acc-sales">
            {qbo ? "Sales go to the item" : "Sales go to the account"}
          </Label>
          <NativeSelect
            id="acc-sales"
            required
            value={salesValue}
            onChange={(e) => setSales(e.target.value)}
          >
            <option value="">Choose…</option>
            {o.sales.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </NativeSelect>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="acc-bank">Payments are paid into</Label>
          <NativeSelect
            id="acc-bank"
            value={bankValue}
            onChange={(e) => setBank(e.target.value)}
          >
            <option value="">
              {qbo ? "Undeposited funds" : "Don't send payments"}
            </option>
            {o.banks.map((b) => (
              <option key={b.id} value={b.id}>
                {b.name}
              </option>
            ))}
          </NativeSelect>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="acc-start">Send what is issued from</Label>
          <Input
            id="acc-start"
            type="date"
            required
            value={startValue}
            onChange={(e) => setStart(e.target.value)}
          />
        </div>
      </div>
      {o.tax_codes.length > 0 && (
        <div className="flex flex-col gap-2">
          <h3 className="text-sm font-medium">Tax codes</h3>
          {c.automatic_tax && (
            <p className="text-sm text-muted-foreground">
              QuickBooks works out the sales tax itself; say only whether each
              code is taxable.
            </p>
          )}
          {o.tax_codes.map((t) => (
            <div
              key={t.tax_code_id}
              className="flex flex-wrap items-center gap-2 text-sm"
            >
              <Label
                htmlFor={`acc-tax-${t.tax_code_id}`}
                className="min-w-40 flex-1"
              >
                {t.name} ({t.rate_bps / 100}%)
              </Label>
              <NativeSelect
                id={`acc-tax-${t.tax_code_id}`}
                value={taxValue(t.tax_code_id)}
                onChange={(e) =>
                  setTaxes({ ...taxes, [t.tax_code_id]: e.target.value })
                }
              >
                <option value="">
                  {qbo ? "Choose…" : "The account's default"}
                </option>
                {o.taxes.map((x) => (
                  <option key={x.id} value={x.id}>
                    {x.name}
                  </option>
                ))}
              </NativeSelect>
            </div>
          ))}
        </div>
      )}
      {error && <Alert>{error}</Alert>}
      <div>
        <Button type="submit" disabled={saving || !salesValue}>
          Save
        </Button>
      </div>
    </form>
  );
}

/** What the books refused, so it can be put right and sent again. */
function Refused({ name }: { name: string }) {
  const failures = useQuery({
    queryKey: ["accounting", "failures"],
    queryFn: () => ok(api.GET("/api/v1/accounting/failures")),
  });
  const what = (kind: string) =>
    kind === "invoice"
      ? "Invoice"
      : kind === "allocation"
        ? "Payment"
        : kind === "credit"
          ? "Credit"
          : "Contact";
  return (
    <section className="flex flex-col gap-2" aria-label="Refused">
      <h2 className="font-semibold">Refused by {name}</h2>
      <p className="text-sm text-muted-foreground">
        Put these right in OnHold or in {name}; they are tried again when they
        change, and on the next send.
      </p>
      {failures.isPending ? (
        <Spinner />
      ) : failures.isError ? (
        <Alert>{failures.error.message}</Alert>
      ) : (
        <ul className="divide-y rounded-md border">
          {failures.data.map((f) => (
            <li
              key={`${f.kind}-${f.local_id}`}
              className="flex flex-col px-3 py-2 text-sm"
            >
              <span className="font-medium">
                {what(f.kind)} {f.reference ?? ""}
              </span>
              <span className="text-destructive">{f.error ?? "Refused"}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Importing the books' invoices from before the start day as past invoices. */
function PastInvoices({
  c,
  name,
  act,
}: {
  c: Connection;
  name: string;
  act: (call: () => Promise<unknown>, done?: string) => Promise<void>;
}) {
  const running = c.import.status === "running";
  const qc = useQueryClient();
  // While importing, look again now and then.
  useEffect(() => {
    if (!running) return;
    const t = window.setInterval(
      () => void qc.invalidateQueries({ queryKey: ["accounting"] }),
      15_000,
    );
    return () => window.clearInterval(t);
  }, [running, qc]);
  return (
    <section className="flex flex-col gap-2" aria-label="Past invoices">
      <h2 className="font-semibold">Past invoices</h2>
      <p className="text-sm text-muted-foreground">
        Bring in the invoices issued in {name} before{" "}
        {new Date(`${c.start_date}T00:00:00`).toLocaleDateString(undefined, {
          dateStyle: "medium",
        })}
        , with what is still owed, matched to clients by name. They count in
        reports but are never sent back.
      </p>
      {c.import.status === "done" && (
        <Alert tone="success">
          {c.import.imported} past invoices imported.
        </Alert>
      )}
      {c.import.status === "failed" && (
        <Alert>{c.import.error ?? "The import stopped."}</Alert>
      )}
      <div>
        <Button
          variant="outline"
          disabled={running}
          onClick={() =>
            void act(
              () => ok(api.POST("/api/v1/accounting/import")),
              "Importing. This can take a few minutes.",
            )
          }
        >
          {running ? <BookOpenCheck /> : <Download />}
          {running
            ? `Importing (${c.import.imported} so far)`
            : c.import.status === "none"
              ? "Import past invoices"
              : "Import again"}
        </Button>
      </div>
    </section>
  );
}
