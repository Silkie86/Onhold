// Sending from the workspace's own domain: an owner or admin adds the
// domain, then adds the DNS records it shows, by hand (each with a copy
// button) or in one step at their DNS provider through Domain Connect when
// it offers that. The provider sends people back here with `checked`, and
// `error` when they did not approve. Once the records are found, the
// workspace's email goes out from an address at the domain.

import type { Schemas } from "@onhold/api-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import {
  Alert,
  Badge,
  Button,
  Card,
  Checkbox,
  Input,
  Label,
  Spinner,
  relativeTime,
} from "@onhold/ui";
import { Check, Copy, Globe, RefreshCw, Trash2, Wand2 } from "lucide-react";
import { useEffect, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { useSession } from "../session.tsx";

type Domain = Schemas["SenderDomainView"];
type DnsRecord = Schemas["RecordView"];

const KEY = ["sender-domains"];

function failure(e: unknown): string {
  return e instanceof ApiFailure || e instanceof Error
    ? e.message
    : "Something went wrong.";
}

// After "Check now" or a return from the DNS provider the result arrives
// within a minute or two, whatever the domain's status was.
let watchUntil = 0;
function watch() {
  watchUntil = Date.now() + 2 * 60_000;
}

export function useSenderDomains(enabled = true) {
  return useQuery({
    queryKey: KEY,
    queryFn: () => ok(api.GET("/api/v1/inbox/domains")),
    enabled,
    // Checks run in the background while records are being added.
    refetchInterval: (q) =>
      Date.now() < watchUntil
        ? 10_000
        : q.state.data?.domains.some((d) => d.status === "pending")
          ? 20_000
          : false,
  });
}

function StatusBadge({ d }: { d: Domain }) {
  if (d.sending)
    return (
      <Badge className="border-emerald-600 bg-emerald-600 text-white">
        Sending from it
      </Badge>
    );
  if (d.status === "verified")
    return (
      <Badge className="border border-emerald-600 text-emerald-700 dark:text-emerald-300">
        Verified
      </Badge>
    );
  if (d.status === "failed")
    return (
      <Badge className="border-destructive bg-destructive text-white">
        Records not found
      </Badge>
    );
  return (
    <Badge className="border border-amber-500 text-amber-800 dark:text-amber-300">
      Waiting for its records
    </Badge>
  );
}

function CopyButton({ text, label }: { text: string; label: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <Button
      size="sm"
      variant="ghost"
      aria-label={`Copy ${label}`}
      onClick={() => {
        void navigator.clipboard?.writeText(text).then(() => {
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        });
      }}
    >
      {copied ? <Check /> : <Copy />}
    </Button>
  );
}

function RecordRow({ r, optional }: { r: DnsRecord; optional?: boolean }) {
  return (
    <li className="flex flex-col gap-1 rounded-md border p-3 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <Badge className="border font-mono">{r.type}</Badge>
        <span className="text-muted-foreground">{r.purpose}</span>
        {optional ? (
          <span className="text-xs text-muted-foreground">Recommended</span>
        ) : r.found ? (
          <span className="flex items-center gap-1 text-xs text-emerald-700 dark:text-emerald-300">
            <Check className="size-3" /> Found
          </span>
        ) : (
          <span className="text-xs text-amber-800 dark:text-amber-300">
            Not found yet
          </span>
        )}
      </div>
      <div className="grid grid-cols-[5rem_1fr_auto] items-center gap-x-2 gap-y-1">
        <span className="text-xs text-muted-foreground">Name</span>
        <span className="flex flex-col">
          <code className="break-all">{r.host}</code>
          {r.name !== r.host && (
            <span className="text-xs text-muted-foreground">
              In full: {r.name}
            </span>
          )}
        </span>
        <CopyButton text={r.host} label={`${r.purpose} name`} />
        <span className="text-xs text-muted-foreground">Value</span>
        <code className="break-all">{r.value}</code>
        <CopyButton text={r.value} label={`${r.purpose} value`} />
        {r.priority != null && (
          <>
            <span className="text-xs text-muted-foreground">Priority</span>
            <code>{r.priority}</code>
            <span />
          </>
        )}
      </div>
    </li>
  );
}

function DomainCard({
  d,
  otherSending,
  act,
}: {
  d: Domain;
  otherSending: boolean;
  act: (call: () => Promise<unknown>, done?: string) => Promise<void>;
}) {
  const [localPart, setLocalPart] = useState(d.local_part);
  useEffect(() => setLocalPart(d.local_part), [d.local_part]);
  // No If-Match: background checks change the version, and these are simple
  // settings where the last choice should win.
  const patch = (body: Schemas["SenderDomainPatch"], done?: string) =>
    act(
      () =>
        ok(
          api.PATCH("/api/v1/inbox/domains/{id}", {
            params: { path: { id: d.id } },
            body,
          }),
        ),
      done,
    );
  // An unverified domain cannot take over from one that is already sending.
  const canChoose = d.status === "verified" || d.in_use || !otherSending;
  const automatic = async () => {
    const { url } = await ok(
      api.POST("/api/v1/inbox/domains/{id}/automatic-setup", {
        params: { path: { id: d.id } },
      }),
    );
    window.location.assign(url);
  };

  return (
    <Card className="flex flex-col gap-3 p-4 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <Globe className="size-5" />
        <span className="font-medium">{d.domain}</span>
        <StatusBadge d={d} />
      </div>

      {d.status !== "verified" && (
        <>
          <p>
            Add these records where the domain's DNS is managed (often where you
            bought it). They let mail providers check that email from {d.domain}{" "}
            really comes from you, so it lands in inboxes rather than spam.
          </p>
          {d.automatic_setup && (
            <div className="flex flex-wrap items-center gap-2 rounded-md bg-muted p-3">
              <span>{d.automatic_setup} can add them for you.</span>
              <Button size="sm" onClick={() => void act(automatic)}>
                <Wand2 /> Set up automatically
              </Button>
            </div>
          )}
          <ul className="flex flex-col gap-2" aria-label="DNS records to add">
            {d.records.map((r) => (
              <RecordRow key={`${r.type}-${r.host}`} r={r} />
            ))}
          </ul>
          <p className="text-xs text-muted-foreground">
            Records can take a few minutes, sometimes a few hours, to be seen.
            OnHold keeps looking
            {d.checked_at ? `; last looked ${relativeTime(d.checked_at)}` : ""}.
            {d.remove_after &&
              ` If they are not found by ${new Date(d.remove_after).toLocaleDateString()}, the domain is removed and can be added again.`}
          </p>
          {d.status === "failed" && (
            <Alert>
              The records were not found when OnHold last looked. Check each one
              against your DNS settings (many providers want only the name
              shown, without the domain on the end), then press Check now.
            </Alert>
          )}
        </>
      )}

      {d.dmarc_record && (
        <details>
          <summary className="cursor-pointer">
            Also recommended: a DMARC record
          </summary>
          <p className="my-2 text-muted-foreground">
            {d.domain} has no DMARC policy. Gmail and Yahoo expect one from
            anyone sending in volume. This one changes nothing about how your
            mail is handled; it tells receivers the domain is looked after.
          </p>
          <ul>
            <RecordRow r={d.dmarc_record} optional />
          </ul>
        </details>
      )}

      <form
        className="flex flex-wrap items-end gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          void patch(
            { local_part: localPart },
            "The sending address is saved.",
          );
        }}
      >
        <div className="flex flex-col gap-1">
          <Label htmlFor={`local-${d.id}`}>Send from</Label>
          <div className="flex items-center gap-1">
            <Input
              id={`local-${d.id}`}
              className="w-40"
              value={localPart}
              onChange={(e) => setLocalPart(e.target.value)}
              required
            />
            <span className="text-muted-foreground">@{d.domain}</span>
          </div>
        </div>
        {localPart.trim().toLowerCase() !== d.local_part && (
          <Button type="submit" size="sm" variant="outline">
            Save
          </Button>
        )}
      </form>
      <p className="text-xs text-muted-foreground">
        Replies still go to the member who wrote, or to the address you set for
        each email.
      </p>

      <div className="flex flex-wrap items-center gap-3">
        {canChoose && (
          <label className="flex items-center gap-1.5">
            <Checkbox
              checked={d.in_use}
              onChange={(e) => void patch({ in_use: e.target.checked })}
            />
            {d.status === "verified"
              ? "Send the workspace's email from this address"
              : "Send from this address once it is verified"}
          </label>
        )}
        {d.status !== "verified" && (
          <Button
            size="sm"
            variant="outline"
            onClick={() =>
              void act(
                () =>
                  ok(
                    api.POST("/api/v1/inbox/domains/{id}/check", {
                      params: { path: { id: d.id } },
                    }),
                  ).then(watch),
                "Looking for the records now. This page updates within a minute.",
              )
            }
          >
            <RefreshCw /> Check now
          </Button>
        )}
        <Button
          size="sm"
          variant="outline"
          onClick={() => {
            const then = d.sending
              ? " Email goes out from OnHold's address again."
              : "";
            if (!window.confirm(`Remove ${d.domain}?${then}`)) return;
            void act(
              () =>
                ok(
                  api.DELETE("/api/v1/inbox/domains/{id}", {
                    params: { path: { id: d.id } },
                  }),
                ),
              `${d.domain} is removed.`,
            );
          }}
        >
          <Trash2 /> Remove
        </Button>
      </div>
    </Card>
  );
}

export function SendingDomainPage() {
  const { workspace } = useSession();
  const manager = workspace.role === "owner" || workspace.role === "admin";
  const search = useSearch({ from: "/app/settings/email/domain" });
  const navigate = useNavigate();
  const qc = useQueryClient();
  const domains = useSenderDomains(manager);
  const [error, setError] = useState<string | null>(
    search.error
      ? "The records were not added at your DNS provider. You can try again, or add them by hand."
      : null,
  );
  const [notice, setNotice] = useState<string | null>(null);
  const [domain, setDomain] = useState("");
  const [adding, setAdding] = useState(false);

  // Back from the DNS provider: look for the records now, then tidy the address bar.
  useEffect(() => {
    if (!search.checked && !search.error) return;
    const id = search.checked;
    if (id && !search.error) {
      setNotice(
        "Your DNS provider has the records. OnHold is checking them now; this usually takes a minute or two.",
      );
      void ok(
        api.POST("/api/v1/inbox/domains/{id}/check", {
          params: { path: { id } },
        }),
      )
        .then(watch)
        .catch(() => undefined)
        .finally(() => void qc.invalidateQueries({ queryKey: KEY }));
    }
    void navigate({ to: "/settings/email/domain", search: {}, replace: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [search.checked, search.error]);

  const act = async (call: () => Promise<unknown>, done?: string) => {
    setError(null);
    setNotice(null);
    try {
      await call();
      if (done) setNotice(done);
    } catch (e) {
      setError(failure(e));
    } finally {
      void qc.invalidateQueries({ queryKey: KEY });
    }
  };

  if (!manager) {
    return (
      <div className="mx-auto max-w-3xl p-4 sm:p-6">
        <Alert>
          Only the workspace's owners and admins can choose the domain email is
          sent from.
        </Alert>
      </div>
    );
  }
  if (domains.isPending) {
    return (
      <div className="flex justify-center p-10">
        <Spinner className="size-6" />
      </div>
    );
  }
  if (domains.isError)
    return <Alert className="m-4">{domains.error.message}</Alert>;
  const s = domains.data;

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-6 p-4 sm:p-6">
      <div className="flex flex-col gap-1">
        <Link
          to="/settings/email"
          search={{}}
          className="text-sm text-muted-foreground underline"
        >
          Email accounts
        </Link>
        <h1 className="text-xl font-semibold">Send from your own domain</h1>
      </div>
      <p className="text-sm text-muted-foreground">
        Invitations, quotes, invoices, automated emails and replies sent without
        a connected mailbox go out from OnHold's address in your name. Add your
        domain and they come from an address of yours instead, such as
        bookings@youragency.com.
      </p>
      {error && <Alert>{error}</Alert>}
      {notice && <Alert tone="success">{notice}</Alert>}
      {!s.available && (
        <Alert tone="info">
          Sending from your own domain is not switched on for this OnHold server
          yet.
        </Alert>
      )}

      {s.domains.map((d) => (
        <DomainCard
          key={d.id}
          d={d}
          otherSending={s.domains.some((o) => o.id !== d.id && o.sending)}
          act={act}
        />
      ))}

      {s.available && (
        <form
          className="flex flex-wrap items-end gap-2"
          aria-label="Add a domain"
          onSubmit={(e) => {
            e.preventDefault();
            setAdding(true);
            void act(async () => {
              const d = await ok(
                api.POST("/api/v1/inbox/domains", { body: { domain } }),
              );
              setDomain("");
              setNotice(`${d.domain} is added. Now add its records.`);
            }).finally(() => setAdding(false));
          }}
        >
          <div className="flex flex-1 flex-col gap-1">
            <Label htmlFor="new-domain">
              {s.domains.length ? "Add another domain" : "Your domain"}
            </Label>
            <Input
              id="new-domain"
              placeholder="youragency.com"
              required
              value={domain}
              onChange={(e) => setDomain(e.target.value)}
            />
          </div>
          <Button type="submit" disabled={adding}>
            {adding ? <Spinner /> : <Globe />} Add domain
          </Button>
        </form>
      )}
    </div>
  );
}
