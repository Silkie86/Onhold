// Card payments: connecting the workspace's own Stripe account, so payers
// can pay invoices by card from their invoice link and the money goes
// straight to the client. Stripe sends the admin back here with `code`
// and `state`, which this page passes on to finish connecting.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { Alert, Badge, Button, Card, Spinner } from "@onhold/ui";
import { CreditCard, ExternalLink, Unplug } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { useSession } from "../session.tsx";

function failure(e: unknown): string {
  return e instanceof ApiFailure || e instanceof Error ? e.message : "Something went wrong.";
}

export function PaymentsSettingsPage() {
  const { workspace } = useSession();
  const admin = workspace.role === "owner" || workspace.role === "admin";
  const search = useSearch({ from: "/app/settings/payments" });
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [error, setError] = useState<string | null>(search.error ? (search.error_description ?? "Stripe did not connect.") : null);
  const [busy, setBusy] = useState(false);
  const finishing = useRef(false);
  const status = useQuery({
    queryKey: ["stripe-status"],
    queryFn: () => ok(api.GET("/api/v1/finance/stripe")),
    enabled: admin,
  });

  // Back from Stripe: finish once, then drop the code from the address bar.
  useEffect(() => {
    if (!admin || !search.code || !search.state || finishing.current) return;
    finishing.current = true;
    setBusy(true);
    ok(api.POST("/api/v1/finance/stripe/connect/finish", { body: { code: search.code, state: search.state } }))
      .then((s) => qc.setQueryData(["stripe-status"], s))
      .catch((e: unknown) => setError(failure(e)))
      .finally(() => {
        setBusy(false);
        void navigate({ to: "/settings/payments", search: {}, replace: true });
      });
  }, [admin, search.code, search.state, navigate, qc]);

  // Stripe sent back a refusal: keep the message, drop it from the address bar.
  useEffect(() => {
    if (search.error && !search.code) void navigate({ to: "/settings/payments", search: {}, replace: true });
  }, [search.error, search.code, navigate]);

  if (!admin) {
    return (
      <div className="mx-auto max-w-3xl p-4 sm:p-6">
        <Alert tone="info">Only owners and admins can set up card payments.</Alert>
      </div>
    );
  }
  if (status.isPending || busy) {
    return (
      <div className="flex justify-center p-10">
        <Spinner className="size-6" />
      </div>
    );
  }
  if (status.isError) return <Alert>{status.error.message}</Alert>;
  const s = status.data;

  const connect = async () => {
    setError(null);
    setBusy(true);
    try {
      const { url } = await ok(api.POST("/api/v1/finance/stripe/connect"));
      window.location.assign(url);
    } catch (e) {
      setError(failure(e));
      setBusy(false);
    }
  };
  const disconnect = async () => {
    if (!window.confirm("Disconnect Stripe? Payers will no longer be able to pay invoices by card.")) return;
    setError(null);
    setBusy(true);
    try {
      qc.setQueryData(["stripe-status"], await ok(api.DELETE("/api/v1/finance/stripe")));
    } catch (e) {
      setError(failure(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-6 p-4 sm:p-6">
      <h1 className="text-xl font-semibold">Card payments</h1>
      <p className="text-sm text-muted-foreground">
        Connect your Stripe account and the people you invoice can pay by card from their invoice link. Payments go straight to your Stripe account and
        are marked against the invoice here when Stripe confirms them. Refunds made in Stripe show up here too.
      </p>
      {error && <Alert>{error}</Alert>}
      {!s.available ? (
        <Alert tone="info">Card payments are not switched on for this OnHold server yet.</Alert>
      ) : s.connected ? (
        <Card className="flex flex-col gap-3 p-5 text-sm">
          <div className="flex flex-wrap items-center gap-2">
            <CreditCard className="size-5" />
            <span className="font-medium">{s.display_name ?? s.account_id}</span>
            {!s.livemode && <Badge className="border">Test mode</Badge>}
            {s.charges_enabled ? (
              <Badge className="border-success bg-success text-success-foreground">Taking payments</Badge>
            ) : (
              <Badge className="border border-warning text-warning">
                Not taking payments yet
              </Badge>
            )}
          </div>
          {!s.charges_enabled && (
            <p>
              Stripe needs more details before this account can take payments. Finish them in your{" "}
              <a className="underline" href="https://dashboard.stripe.com/" target="_blank" rel="noreferrer">
                Stripe dashboard <ExternalLink className="inline size-3" />
              </a>
              .
            </p>
          )}
          <p className="text-muted-foreground">
            {s.account_id}
            {s.default_currency && ` · ${s.default_currency}`}
            {s.connected_at && ` · connected ${new Date(s.connected_at).toLocaleDateString()}`}
          </p>
          <div>
            <Button variant="outline" onClick={disconnect}>
              <Unplug /> Disconnect
            </Button>
          </div>
        </Card>
      ) : (
        <Card className="flex flex-col gap-3 p-5 text-sm">
          {s.disconnected_at && <p>Stripe was disconnected on {new Date(s.disconnected_at).toLocaleDateString()}.</p>}
          <p>You will sign in to Stripe (or create an account) and come back here.</p>
          <div>
            <Button onClick={connect}>
              <CreditCard /> Connect Stripe
            </Button>
          </div>
        </Card>
      )}
    </div>
  );
}
