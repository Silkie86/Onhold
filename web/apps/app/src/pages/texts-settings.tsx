// Text messages: the workspace's own Twilio number for SMS and WhatsApp
// number (Meta's Cloud API), the addresses to give each provider for what
// comes in, and the text templates rules and replies send. Rules text only
// contacts who agreed to texts; nobody who replied STOP is texted.

import type { Schemas } from "@onhold/api-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, Badge, Button, Card, Input, Label, NativeSelect, Spinner, Textarea } from "@onhold/ui";
import { Copy, MessageSquare, Plus, Trash2, Unplug } from "lucide-react";
import { useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { useSession } from "../session.tsx";

type Channel = Schemas["MessagingChannel"];

function failure(e: unknown): string {
  return e instanceof ApiFailure || e instanceof Error ? e.message : "Something went wrong.";
}

const NAMES: Record<string, string> = { sms: "SMS", whatsapp: "WhatsApp" };

export function TextsSettingsPage() {
  const { workspace } = useSession();
  const admin = workspace.role === "owner" || workspace.role === "admin";
  const qc = useQueryClient();
  const numbers = useQuery({ queryKey: ["numbers"], queryFn: async () => (await ok(api.GET("/api/v1/inbox/numbers"))).channels });
  const [error, setError] = useState<string | null>(null);

  if (numbers.isPending) {
    return (
      <div className="flex justify-center p-10">
        <Spinner className="size-6" />
      </div>
    );
  }
  if (numbers.isError) return <Alert className="m-4">{numbers.error.message}</Alert>;
  const live = numbers.data;
  const has = (kind: string) => live.some((n) => n.kind === kind);
  const saved = (c: Channel) => qc.setQueryData<Channel[]>(["numbers"], [...live.filter((n) => n.kind !== c.kind), c]);

  const disconnect = async (c: Channel) => {
    if (!window.confirm(`Stop using ${c.sender}? Its conversations stay in the inbox.`)) return;
    setError(null);
    try {
      await ok(api.DELETE("/api/v1/inbox/numbers/{id}", { params: { path: { id: c.id } } }));
      qc.setQueryData<Channel[]>(["numbers"], live.filter((n) => n.id !== c.id));
    } catch (e) {
      setError(failure(e));
    }
  };

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-6 p-4 sm:p-6">
      <h1 className="text-xl font-semibold">Text messages</h1>
      <p className="text-sm text-muted-foreground">
        Text from your own SMS and WhatsApp numbers. Texts people send come into the inbox, linked to the contact with that number, and you answer them
        there. Rules and follow-ups text only contacts who agreed to texts (ticked on the contact), and anyone who replies STOP is not texted again
        until they reply START.
      </p>
      {error && <Alert>{error}</Alert>}
      {live.map((c) => (
        <Card key={c.id} className="flex flex-col gap-3 p-5 text-sm">
          <div className="flex flex-wrap items-center gap-2">
            <MessageSquare className="size-5" />
            <span className="font-medium">{c.display_name ? `${c.display_name} (${c.sender})` : c.sender}</span>
            <Badge className="border">{NAMES[c.kind] ?? c.kind}</Badge>
            {c.status === "active" ? (
              <Badge className="border-success bg-success text-success-foreground">Working</Badge>
            ) : (
              <Badge className="border border-warning text-warning">Needs attention</Badge>
            )}
          </div>
          {c.last_error && (
            <Alert>
              {c.last_error}
              {c.last_error_at && ` (${new Date(c.last_error_at).toLocaleString()})`}
            </Alert>
          )}
          {c.webhook_url && (
            <div className="flex flex-col gap-1.5">
              <p>
                {c.kind === "sms"
                  ? "In Twilio, set this number's \"A message comes in\" webhook (HTTP POST) to:"
                  : "In your Meta app's WhatsApp configuration, set the callback URL and verify token to these, and subscribe to messages:"}
              </p>
              <CopyLine label="Address" value={c.webhook_url} />
              {c.verify_token && <CopyLine label="Verify token" value={c.verify_token} />}
            </div>
          )}
          <p className="text-muted-foreground">Connected {new Date(c.connected_at).toLocaleDateString()}</p>
          {admin && (
            <div>
              <Button variant="outline" onClick={() => void disconnect(c)}>
                <Unplug /> Disconnect
              </Button>
            </div>
          )}
        </Card>
      ))}
      {admin && !has("sms") && <ConnectSms onSaved={saved} />}
      {admin && !has("whatsapp") && <ConnectWhatsapp onSaved={saved} />}
      {!admin && live.length === 0 && <Alert tone="info">No numbers yet. An owner or admin can connect them here.</Alert>}
      <TextTemplates canEdit={admin} />
    </div>
  );
}

function CopyLine({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center gap-2">
      <span className="w-24 shrink-0 text-muted-foreground">{label}</span>
      <code className="min-w-0 flex-1 truncate rounded bg-muted px-2 py-1 text-xs">{value}</code>
      <Button type="button" size="icon" variant="ghost" aria-label={`Copy ${label.toLowerCase()}`} onClick={() => void navigator.clipboard?.writeText(value)}>
        <Copy />
      </Button>
    </div>
  );
}

function ConnectSms({ onSaved }: { onSaved: (c: Channel) => void }) {
  const [sid, setSid] = useState("");
  const [token, setToken] = useState("");
  const [number, setNumber] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Card className="p-5 text-sm">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setError(null);
          try {
            onSaved(await ok(api.POST("/api/v1/inbox/numbers/sms", { body: { account_sid: sid.trim(), auth_token: token.trim(), number: number.trim() } })));
          } catch (err) {
            setError(failure(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <h2 className="font-medium">SMS through Twilio</h2>
        <p className="text-muted-foreground">From your Twilio console: the account SID and auth token, and a number in that account that can send texts.</p>
        <Label htmlFor="sms-sid">Account SID</Label>
        <Input id="sms-sid" required value={sid} onChange={(e) => setSid(e.target.value)} placeholder="AC…" autoComplete="off" />
        <Label htmlFor="sms-token">Auth token</Label>
        <Input id="sms-token" required type="password" value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" />
        <Label htmlFor="sms-number">Number</Label>
        <Input id="sms-number" required type="tel" value={number} onChange={(e) => setNumber(e.target.value)} placeholder="+44 7700 900123" />
        {error && <Alert>{error}</Alert>}
        <div>
          <Button type="submit" disabled={busy}>
            {busy ? <Spinner /> : <Plus />} Connect SMS
          </Button>
        </div>
      </form>
    </Card>
  );
}

function ConnectWhatsapp({ onSaved }: { onSaved: (c: Channel) => void }) {
  const [phoneId, setPhoneId] = useState("");
  const [token, setToken] = useState("");
  const [secret, setSecret] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Card className="p-5 text-sm">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setError(null);
          try {
            onSaved(
              await ok(
                api.POST("/api/v1/inbox/numbers/whatsapp", {
                  body: { phone_number_id: phoneId.trim(), access_token: token.trim(), app_secret: secret.trim() },
                }),
              ),
            );
          } catch (err) {
            setError(failure(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <h2 className="font-medium">WhatsApp</h2>
        <p className="text-muted-foreground">
          From Meta's WhatsApp Manager and your Meta app: the number's phone number ID, a system user's permanent access token, and the app secret.
        </p>
        <Label htmlFor="wa-phone">Phone number ID</Label>
        <Input id="wa-phone" required value={phoneId} onChange={(e) => setPhoneId(e.target.value)} autoComplete="off" />
        <Label htmlFor="wa-token">Access token</Label>
        <Input id="wa-token" required type="password" value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" />
        <Label htmlFor="wa-secret">App secret</Label>
        <Input id="wa-secret" required type="password" value={secret} onChange={(e) => setSecret(e.target.value)} autoComplete="off" />
        {error && <Alert>{error}</Alert>}
        <div>
          <Button type="submit" disabled={busy}>
            {busy ? <Spinner /> : <Plus />} Connect WhatsApp
          </Button>
        </div>
      </form>
    </Card>
  );
}

/** The SMS and WhatsApp templates rules and replies send. */
function TextTemplates({ canEdit }: { canEdit: boolean }) {
  const qc = useQueryClient();
  const list = useQuery({
    queryKey: ["templates", "texts"],
    queryFn: async () => {
      const [sms, whatsapp] = await Promise.all([
        ok(api.GET("/api/v1/templates", { params: { query: { kind: "sms" } } })),
        ok(api.GET("/api/v1/templates", { params: { query: { kind: "whatsapp" } } })),
      ]);
      return [...sms, ...whatsapp];
    },
  });
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["templates"] });
  };
  const archive = async (id: string) => {
    setError(null);
    try {
      await ok(api.DELETE("/api/v1/templates/{id}", { params: { path: { id } } }));
      refresh();
    } catch (e) {
      setError(failure(e));
    }
  };
  return (
    <section className="flex flex-col gap-3">
      <h2 className="text-lg font-semibold">Text templates</h2>
      <p className="text-sm text-muted-foreground">
        Rules and follow-ups text from these. A WhatsApp template stands for one Meta approved: give its name in Meta (and language, if not your
        workspace's), and one line per value it fills in.
      </p>
      {error && <Alert>{error}</Alert>}
      {list.isPending ? (
        <Spinner />
      ) : list.isError ? (
        <Alert>{list.error.message}</Alert>
      ) : list.data.length === 0 ? (
        <p className="text-sm text-muted-foreground">No text templates yet.</p>
      ) : (
        <ul className="divide-y rounded-md border text-sm">
          {list.data.map((t) => (
            <li key={t.id} className="flex items-start gap-2 p-3">
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-medium">{t.name}</span>
                  <Badge className="border">{NAMES[t.kind] ?? t.kind}</Badge>
                  <code className="text-xs text-muted-foreground">{t.key}</code>
                </div>
                {t.subject && <p className="text-xs text-muted-foreground">Meta template: {t.subject}</p>}
                <p className="whitespace-pre-wrap break-words text-muted-foreground">{t.body}</p>
              </div>
              {canEdit && (
                <Button size="icon" variant="ghost" aria-label={`Remove ${t.name}`} onClick={() => void archive(t.id)}>
                  <Trash2 />
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
      {canEdit &&
        (adding ? (
          <NewTextTemplate
            onDone={() => {
              setAdding(false);
              refresh();
            }}
            onCancel={() => setAdding(false)}
          />
        ) : (
          <div>
            <Button variant="outline" onClick={() => setAdding(true)}>
              <Plus /> New text template
            </Button>
          </div>
        ))}
    </section>
  );
}

function NewTextTemplate({ onDone, onCancel }: { onDone: () => void; onCancel: () => void }) {
  const [kind, setKind] = useState<"sms" | "whatsapp">("sms");
  const [name, setName] = useState("");
  const [key, setKey] = useState("");
  const [meta, setMeta] = useState("");
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const whatsapp = kind === "whatsapp";
  return (
    <Card className="p-5 text-sm">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setError(null);
          try {
            await ok(
              api.POST("/api/v1/templates", {
                body: { kind, key: key.trim(), name: name.trim(), subject: whatsapp ? meta.trim() : null, body },
              }),
            );
            onDone();
          } catch (err) {
            setError(failure(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <Label htmlFor="tt-kind">Kind</Label>
        <NativeSelect id="tt-kind" value={kind} onChange={(e) => setKind(e.target.value as "sms" | "whatsapp")}>
          <option value="sms">SMS</option>
          <option value="whatsapp">WhatsApp</option>
        </NativeSelect>
        <Label htmlFor="tt-name">Name</Label>
        <Input id="tt-name" required value={name} onChange={(e) => setName(e.target.value)} placeholder="Booking confirmed" />
        <Label htmlFor="tt-key">Key</Label>
        <Input id="tt-key" required value={key} onChange={(e) => setKey(e.target.value)} placeholder="booking.confirmed" pattern="[a-z0-9_]+(\.[a-z0-9_]+)*" />
        {whatsapp && (
          <>
            <Label htmlFor="tt-meta">Template name in Meta</Label>
            <Input id="tt-meta" required value={meta} onChange={(e) => setMeta(e.target.value)} placeholder="booking_confirmed en_GB" />
          </>
        )}
        <Label htmlFor="tt-body">{whatsapp ? "Values, one per line" : "Text"}</Label>
        <Textarea
          id="tt-body"
          required={!whatsapp}
          rows={4}
          value={body}
          onChange={(e) => setBody(e.target.value)}
          placeholder={whatsapp ? "{{ contact.name }}\n{{ booking.title }}" : "Hi {{ contact.name }}, your booking is confirmed."}
        />
        {error && <Alert>{error}</Alert>}
        <div className="flex gap-2">
          <Button type="submit" disabled={busy}>
            Save
          </Button>
          <Button type="button" variant="outline" onClick={onCancel}>
            Cancel
          </Button>
        </div>
      </form>
    </Card>
  );
}
