// Email accounts: each member connects their own mailbox (Gmail, Outlook
// and Microsoft 365, or any other by IMAP and SMTP) so its mail shows in
// the inbox and replies go out from it. Google and Microsoft send the
// member back here with `code` and `state`, which this page passes on to
// finish connecting.

import type { Schemas } from "@onhold/api-client";
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { Alert, Badge, Button, Card, Checkbox, Input, Label, NativeSelect, Spinner, cn, relativeTime } from "@onhold/ui";
import { Mail, RefreshCw, Server, Unplug } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { useSession } from "../session.tsx";
import { useMailSetup } from "./inbox.tsx";
import { KeepMailCard } from "./mail-device.tsx";

type Mailbox = Schemas["IntegrationView"];
type Security = "tls" | "starttls";

function failure(e: unknown): string {
  return e instanceof ApiFailure || e instanceof Error ? e.message : "Something went wrong.";
}

/** Servers for mail providers people often use, to save looking them up. */
export const PRESETS: { name: string; imap: string; imapPort: number; smtp: string; smtpPort: number; smtpSecurity: Security; note?: string }[] = [
  { name: "iCloud Mail", imap: "imap.mail.me.com", imapPort: 993, smtp: "smtp.mail.me.com", smtpPort: 587, smtpSecurity: "starttls", note: "Use an app-specific password from your Apple Account." },
  { name: "Yahoo Mail", imap: "imap.mail.yahoo.com", imapPort: 993, smtp: "smtp.mail.yahoo.com", smtpPort: 465, smtpSecurity: "tls", note: "Use an app password from Yahoo's account security page." },
  { name: "Fastmail", imap: "imap.fastmail.com", imapPort: 993, smtp: "smtp.fastmail.com", smtpPort: 465, smtpSecurity: "tls", note: "Use an app password from Fastmail's settings." },
  { name: "Zoho Mail", imap: "imap.zoho.com", imapPort: 993, smtp: "smtp.zoho.com", smtpPort: 465, smtpSecurity: "tls" },
  { name: "IONOS", imap: "imap.ionos.co.uk", imapPort: 993, smtp: "smtp.ionos.co.uk", smtpPort: 465, smtpSecurity: "tls" },
];

const PROVIDER_NAMES: Record<string, string> = { google: "Gmail", microsoft: "Outlook", imap: "IMAP" };

function StatusBadge({ m }: { m: Mailbox }) {
  if (m.status === "active") return <Badge className="border-emerald-700 bg-emerald-700 text-white">Working</Badge>;
  if (m.status === "failing") return <Badge className="border border-amber-500 text-amber-800 dark:text-amber-300">Having trouble</Badge>;
  return <Badge className="border-destructive bg-destructive text-destructive-foreground">Needs connecting again</Badge>;
}

export function EmailSettingsPage() {
  const { me, workspace } = useSession();
  const manager = workspace.role === "owner" || workspace.role === "admin";
  const search = useSearch({ from: "/app/settings/email" });
  const navigate = useNavigate();
  const qc = useQueryClient();
  const setup = useMailSetup();
  const [error, setError] = useState<string | null>(
    search.error ? (search.error_description ?? "The mailbox was not connected.") : null,
  );
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [imapFor, setImapFor] = useState<Mailbox | "new" | null>(null);
  const [sharedMailbox, setSharedMailbox] = useState("");
  const finishing = useRef(false);
  const refresh = () => void qc.invalidateQueries({ queryKey: ["mailboxes"] });

  // Back from Google or Microsoft: finish once, then drop the code from the address bar.
  useEffect(() => {
    if (!search.code || !search.state || finishing.current) return;
    finishing.current = true;
    setBusy(true);
    ok(api.POST("/api/v1/inbox/mailboxes/connect/finish", { body: { code: search.code, state: search.state } }))
      .then((m) => {
        setNotice(`${m.mailbox} is connected. Its mail from the last few weeks will show in the inbox shortly.`);
        refresh();
      })
      .catch((e: unknown) => setError(failure(e)))
      .finally(() => {
        setBusy(false);
        void navigate({ to: "/settings/email", search: {}, replace: true });
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [search.code, search.state]);

  // A refusal from the provider: keep the message, drop it from the address bar.
  useEffect(() => {
    if (search.error && !search.code) void navigate({ to: "/settings/email", search: {}, replace: true });
  }, [search.error, search.code, navigate]);

  if (setup.isPending || busy) {
    return (
      <div className="flex justify-center p-10">
        <Spinner className="size-6" />
      </div>
    );
  }
  if (setup.isError) return <Alert className="m-4">{setup.error.message}</Alert>;
  const s = setup.data;
  const canWrite = workspace.role !== "viewer";

  const connect = async (provider: "google" | "microsoft", shared?: string) => {
    setError(null);
    setBusy(true);
    try {
      const { url } = await ok(
        api.POST("/api/v1/inbox/mailboxes/connect", { body: { provider, shared_mailbox: shared?.trim() || null } }),
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

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-6 p-4 sm:p-6">
      <h1 className="text-xl font-semibold">Email accounts</h1>
      <p className="text-sm text-muted-foreground">
        Connect your mailbox and its mail shows in the inbox, linked to the contacts, enquiries and bookings it is about. Replies you write in OnHold go
        out from your own address. Your mailbox is private to you unless you share it with the team.
      </p>
      {error && <Alert>{error}</Alert>}
      {notice && <Alert tone="success">{notice}</Alert>}
      <KeepMailCard />

      {s.mailboxes.length > 0 && (
        <section aria-label="Connected mailboxes" className="flex flex-col gap-3">
          {s.mailboxes.map((m) => {
            const mine = m.owner_id === me.user.id;
            return (
              <Card key={m.id} className="flex flex-col gap-3 p-4 text-sm">
                <div className="flex flex-wrap items-center gap-2">
                  <Mail className="size-5" />
                  <span className="font-medium">{m.display_name ? `${m.display_name} <${m.mailbox}>` : m.mailbox}</span>
                  <Badge className="border">{PROVIDER_NAMES[m.provider] ?? m.provider}</Badge>
                  <StatusBadge m={m} />
                  {m.shared && <Badge className="border">Shared with the team</Badge>}
                </div>
                <p className="text-muted-foreground">
                  {m.last_synced_at ? `Last read ${relativeTime(m.last_synced_at)}` : "Not read yet"}
                  {!mine && " · connected by another member"}
                </p>
                {m.status !== "active" && m.last_error && (
                  <Alert tone={m.status === "reconnect" ? "error" : "info"}>
                    {m.status === "reconnect"
                      ? `OnHold can no longer reach this mailbox (${m.last_error}). Connect it again to carry on; nothing already here is lost.`
                      : `The last tries failed (${m.last_error}). OnHold keeps trying.`}
                  </Alert>
                )}
                {mine && (
                  <div className="flex flex-wrap items-center gap-2">
                    {m.status === "reconnect" &&
                      (m.provider === "imap" ? (
                        <Button size="sm" onClick={() => setImapFor(m)}>
                          Connect again
                        </Button>
                      ) : (
                        <Button size="sm" onClick={() => connect(m.provider as "google" | "microsoft", m.account === m.mailbox ? undefined : m.mailbox)}>
                          Connect again
                        </Button>
                      ))}
                    {m.status !== "reconnect" && (
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() => act(() => ok(api.POST("/api/v1/inbox/mailboxes/{id}/sync", { params: { path: { id: m.id } } })), "Reading it now.")}
                      >
                        <RefreshCw /> Check now
                      </Button>
                    )}
                    <label className="flex items-center gap-1.5">
                      <Checkbox
                        checked={m.shared}
                        onChange={(e) =>
                          act(() =>
                            ok(
                              api.PATCH("/api/v1/inbox/mailboxes/{id}", {
                                params: { path: { id: m.id } },
                                body: { shared: e.target.checked },
                              }),
                            ),
                          )
                        }
                      />
                      Share with the team
                    </label>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => {
                        if (!window.confirm(`Disconnect ${m.mailbox}? Its conversations stay in OnHold; no new mail is read.`)) return;
                        void act(() => ok(api.DELETE("/api/v1/inbox/mailboxes/{id}", { params: { path: { id: m.id } } })), `${m.mailbox} is disconnected.`);
                      }}
                    >
                      <Unplug /> Disconnect
                    </Button>
                  </div>
                )}
                {!mine && m.status !== "active" && <p className="text-xs text-muted-foreground">Only the member who connected it can connect it again.</p>}
                {!mine && manager && (
                  <Button
                    size="sm"
                    variant="outline"
                    className="self-start"
                    onClick={() => {
                      if (!window.confirm(`Disconnect ${m.mailbox} for everyone?`)) return;
                      void act(() => ok(api.DELETE("/api/v1/inbox/mailboxes/{id}", { params: { path: { id: m.id } } })), `${m.mailbox} is disconnected.`);
                    }}
                  >
                    <Unplug /> Disconnect
                  </Button>
                )}
              </Card>
            );
          })}
        </section>
      )}

      {canWrite && (
        <section aria-label="Connect a mailbox" className="flex flex-col gap-3">
          <h2 className="font-semibold">Connect a mailbox</h2>
          <div className="flex flex-wrap gap-2">
            {s.google && (
              <Button onClick={() => connect("google")}>
                <Mail /> Gmail or Google Workspace
              </Button>
            )}
            {s.microsoft && (
              <Button onClick={() => connect("microsoft")}>
                <Mail /> Outlook or Microsoft 365
              </Button>
            )}
            <Button variant="outline" onClick={() => setImapFor("new")}>
              <Server /> Another provider (IMAP)
            </Button>
          </div>
          {!s.google && !s.microsoft && (
            <p className="text-xs text-muted-foreground">Gmail and Outlook sign-in are not switched on for this OnHold server yet; any mailbox works by IMAP.</p>
          )}
          {s.microsoft && (
            <details className="text-sm">
              <summary className="cursor-pointer">A Microsoft 365 shared mailbox, such as bookings@…</summary>
              <form
                className="mt-2 flex flex-wrap items-end gap-2"
                onSubmit={(e) => {
                  e.preventDefault();
                  void connect("microsoft", sharedMailbox);
                }}
              >
                <div className="flex flex-1 flex-col gap-1">
                  <Label htmlFor="shared-mailbox">Shared mailbox address</Label>
                  <Input id="shared-mailbox" type="email" required value={sharedMailbox} onChange={(e) => setSharedMailbox(e.target.value)} />
                </div>
                <Button type="submit">Connect</Button>
              </form>
              <p className="mt-1 text-xs text-muted-foreground">
                You sign in as yourself; your account must already have access to the shared mailbox. It is shared with the team from the start.
              </p>
            </details>
          )}
        </section>
      )}

      {imapFor && (
        <ImapForm
          existing={imapFor === "new" ? null : imapFor}
          onClose={() => setImapFor(null)}
          onConnected={(m) => {
            setImapFor(null);
            setNotice(`${m.mailbox} is connected. Its mail from the last few weeks will show in the inbox shortly.`);
            refresh();
          }}
        />
      )}
    </div>
  );
}

interface ServerFields {
  host: string;
  port: string;
  security: Security;
}

function ImapForm({ existing, onClose, onConnected }: { existing: Mailbox | null; onClose: () => void; onConnected: (m: Mailbox) => void }) {
  const saved = existing?.servers as { imap?: { host: string; port: number; security: Security }; smtp?: { host: string; port: number; security: Security }; username?: string } | null | undefined;
  const [address, setAddress] = useState(existing?.mailbox ?? "");
  const [name, setName] = useState(existing?.display_name ?? "");
  const [username, setUsername] = useState(saved?.username ?? "");
  const [password, setPassword] = useState("");
  const [imap, setImap] = useState<ServerFields>({
    host: saved?.imap?.host ?? "",
    port: String(saved?.imap?.port ?? 993),
    security: saved?.imap?.security ?? "tls",
  });
  const [smtp, setSmtp] = useState<ServerFields>({
    host: saved?.smtp?.host ?? "",
    port: String(saved?.smtp?.port ?? 465),
    security: saved?.smtp?.security ?? "tls",
  });
  const [note, setNote] = useState<string | undefined>();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const preset = (n: string) => {
    const p = PRESETS.find((x) => x.name === n);
    if (!p) return;
    setImap({ host: p.imap, port: String(p.imapPort), security: "tls" });
    setSmtp({ host: p.smtp, port: String(p.smtpPort), security: p.smtpSecurity });
    setNote(p.note);
  };
  const server = (label: string, v: ServerFields, set: (f: ServerFields) => void, ports: number[]) => (
    <fieldset className="flex flex-col gap-2 rounded-md border p-3">
      <legend className="px-1 text-sm font-medium">{label}</legend>
      <div className="flex flex-wrap gap-2">
        <Input aria-label={`${label} server`} placeholder="mail.example.com" required className="min-w-48 flex-1" value={v.host} onChange={(e) => set({ ...v, host: e.target.value })} />
        <NativeSelect
          aria-label={`${label} port`}
          className="w-auto"
          value={v.port}
          onChange={(e) => {
            const port = e.target.value;
            // The usual protection for each port.
            set({ ...v, port, security: port === "993" || port === "465" ? "tls" : "starttls" });
          }}
        >
          {ports.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </NativeSelect>
        <NativeSelect aria-label={`${label} encryption`} className="w-auto" value={v.security} onChange={(e) => set({ ...v, security: e.target.value as Security })}>
          <option value="tls">SSL/TLS</option>
          <option value="starttls">STARTTLS</option>
        </NativeSelect>
      </div>
    </fieldset>
  );

  return (
    <Card className="flex flex-col gap-3 p-4">
      <h2 className="font-semibold">{existing ? `Connect ${existing.mailbox} again` : "Connect by IMAP and SMTP"}</h2>
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          setBusy(true);
          try {
            const m = await ok(
              api.POST("/api/v1/inbox/mailboxes/imap", {
                body: {
                  address,
                  display_name: name.trim() || null,
                  username: username.trim() || null,
                  password,
                  imap: { host: imap.host.trim(), port: Number(imap.port), security: imap.security },
                  smtp: { host: smtp.host.trim(), port: Number(smtp.port), security: smtp.security },
                },
              }),
            );
            onConnected(m);
          } catch (err) {
            setError(failure(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        {!existing && (
          <>
            <Label htmlFor="imap-preset">Provider</Label>
            <NativeSelect id="imap-preset" defaultValue="" onChange={(e) => preset(e.target.value)}>
              <option value="">Another provider: I'll enter the servers</option>
              {PRESETS.map((p) => (
                <option key={p.name} value={p.name}>
                  {p.name}
                </option>
              ))}
            </NativeSelect>
          </>
        )}
        <Label htmlFor="imap-address">Email address</Label>
        <Input id="imap-address" type="email" required readOnly={Boolean(existing)} value={address} onChange={(e) => setAddress(e.target.value)} />
        <Label htmlFor="imap-name">Name on messages you send</Label>
        <Input id="imap-name" placeholder="Optional" value={name} onChange={(e) => setName(e.target.value)} />
        {server("Incoming mail (IMAP)", imap, setImap, [993, 143])}
        {server("Outgoing mail (SMTP)", smtp, setSmtp, [465, 587, 25, 2525])}
        <Label htmlFor="imap-username">Username</Label>
        <Input id="imap-username" placeholder="Usually your email address" value={username} onChange={(e) => setUsername(e.target.value)} />
        <Label htmlFor="imap-password">Password</Label>
        <Input id="imap-password" type="password" required autoComplete="off" value={password} onChange={(e) => setPassword(e.target.value)} />
        <p className={cn("text-xs text-muted-foreground", note && "text-foreground")}>
          {note ?? "Many providers want an app password here rather than your usual one."} OnHold tries both servers before saving and keeps the
          password encrypted.
        </p>
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button type="button" variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy}>
            {busy ? <Spinner /> : null} Connect
          </Button>
        </div>
      </form>
    </Card>
  );
}
