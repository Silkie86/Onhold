// Quotes: a booking's quotes with their lines, sending, revising and
// withdrawing them; and the page a buyer opens from the email to accept or
// decline without signing in.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useParams } from "@tanstack/react-router";
import type { Schemas } from "@onhold/api-client";
import {
  Alert,
  Badge,
  Button,
  Card,
  Dialog,
  type FieldIssue,
  Input,
  Label,
  MoneyInput,
  Spinner,
  Textarea,
  applyBranding,
  type Branding,
  cn,
  formatMoney,
} from "@onhold/ui";
import { Check, Copy, FilePlus2, Pencil, Plus, Send, Trash2, Undo2, X } from "lucide-react";
import { useEffect, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { fetchQuotes, useOffline, useSyncStatus } from "../data.ts";
import { isLocalId } from "../offline/store.ts";
import { type Booking, type Quote, formatWhen } from "../work.ts";
import { failureIssues, useWorkspaceCurrency } from "./crm.tsx";

type QuoteStatus = Schemas["QuoteStatus"];
type PublicQuote = Schemas["PublicQuote"];

const QUOTE_LABEL: Record<QuoteStatus, string> = {
  draft: "Draft",
  sent: "Sent",
  viewed: "Viewed",
  accepted: "Accepted",
  declined: "Declined",
  withdrawn: "Withdrawn",
  expired: "Expired",
};

const QUOTE_STYLE: Partial<Record<QuoteStatus, string>> = {
  sent: "border-sky-500 text-sky-800 dark:text-sky-300",
  viewed: "border-sky-500 bg-sky-50 text-sky-900 dark:bg-sky-950/40 dark:text-sky-200",
  accepted: "border-emerald-600 bg-emerald-600 text-white",
  declined: "border-destructive text-destructive",
};

function QuoteBadge({ status }: { status: QuoteStatus }) {
  return <Badge className={cn("border", QUOTE_STYLE[status])}>{QUOTE_LABEL[status]}</Badge>;
}

function failure(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function QuotesPanel({ booking, onBookingChanged }: { booking: Booking; onBookingChanged: () => void }) {
  const qc = useQueryClient();
  const sync = useOffline();
  const status = useSyncStatus();
  const [editing, setEditing] = useState<Quote | null>(null);
  const [sendingId, setSendingId] = useState<string | null>(null);
  const [links, setLinks] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const quotes = useQuery({
    queryKey: ["quotes", booking.id],
    queryFn: async () => (await fetchQuotes(sync, booking.id)).data,
  });
  // Quotes are made and sent on the server: offline they can be read, not changed.
  const offline = status?.online === false || isLocalId(booking.id);
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["quotes", booking.id] });
    void qc.invalidateQueries({ queryKey: ["timeline", "bookings", booking.id] });
  };
  const act = async (run: () => Promise<unknown>) => {
    setError(null);
    try {
      await run();
      refresh();
    } catch (e) {
      setError(failure(e));
    }
  };
  const open = !offline && ["hold", "pending", "confirmed"].includes(booking.status);
  // Always the latest copy, so its version is current.
  const sending = quotes.data?.items.find((q) => q.id === sendingId) ?? null;
  const saved = (q: Quote) =>
    qc.setQueryData(["quotes", booking.id], (page: typeof quotes.data) =>
      page && { ...page, items: page.items.map((x) => (x.id === q.id ? q : x)) },
    );

  return (
    <section className="flex flex-col gap-3" aria-label="Quotes">
      <div className="flex items-center justify-between">
        <h2 className="font-semibold">Quotes</h2>
        {open && (
          <Button
            variant="outline"
            size="sm"
            onClick={() =>
              void act(async () => {
                const q = await ok(api.POST("/api/v1/bookings/{id}/quotes", { params: { path: { id: booking.id } }, body: {} }));
                setEditing(q);
              })
            }
          >
            <Plus /> New quote
          </Button>
        )}
      </div>
      {offline && <p className="text-sm text-muted-foreground">Quotes can be made and sent when you are online.</p>}
      {error && <Alert>{error}</Alert>}
      {quotes.isPending ? (
        <Spinner />
      ) : (quotes.data?.items.length ?? 0) === 0 ? (
        <p className="text-sm text-muted-foreground">No quotes yet.</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {quotes.data?.items.map((q) => (
            <li key={q.id}>
              <Card className="flex flex-col gap-2 p-3 text-sm">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-medium">{q.reference}</span>
                  <QuoteBadge status={q.status} />
                  <span className="flex-1 text-right font-medium">{formatMoney(q.total)}</span>
                </div>
                <ul className="text-muted-foreground">
                  {q.line_items.map((l, i) => (
                    <li key={i}>
                      {l.quantity !== 1 && `${l.quantity} × `}
                      {l.description}
                    </li>
                  ))}
                </ul>
                <QuoteFacts quote={q} />
                {links[q.id] && <CopyLink url={links[q.id]!} />}
                <div className={cn("flex flex-wrap gap-2", offline && "hidden")}>
                  {q.status === "draft" && (
                    <>
                      <Button size="sm" variant="outline" onClick={() => setEditing(q)}>
                        <Pencil /> Edit
                      </Button>
                      <Button size="sm" onClick={() => setSendingId(q.id)}>
                        <Send /> Send
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() =>
                          void act(() => ok(api.DELETE("/api/v1/quotes/{id}", { params: { path: { id: q.id } } })))
                        }
                      >
                        <Trash2 /> Delete
                      </Button>
                    </>
                  )}
                  {(q.status === "sent" || q.status === "viewed") && (
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() =>
                        void act(() =>
                          ok(
                            api.POST("/api/v1/quotes/{id}/withdraw", {
                              params: { path: { id: q.id } },
                              headers: { "If-Match": `"${q.version}"` },
                            }),
                          ),
                        )
                      }
                    >
                      <Undo2 /> Withdraw
                    </Button>
                  )}
                  {open && ["sent", "viewed", "declined", "expired"].includes(q.status) && (
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() =>
                        void act(async () => {
                          const r = await ok(
                            api.POST("/api/v1/quotes/{id}/revise", {
                              params: { path: { id: q.id } },
                              headers: { "If-Match": `"${q.version}"` },
                            }),
                          );
                          setEditing(r);
                        })
                      }
                    >
                      <FilePlus2 /> Revise
                    </Button>
                  )}
                </div>
              </Card>
            </li>
          ))}
        </ul>
      )}
      {editing && (
        <QuoteEditor
          quote={editing}
          onClose={() => setEditing(null)}
          onSaved={(q) => {
            saved(q);
            setEditing(null);
            refresh();
          }}
        />
      )}
      {sending && (
        <SendDialog
          quote={sending}
          booking={booking}
          onClose={() => setSendingId(null)}
          onSent={(link) => {
            setLinks((l) => ({ ...l, [sending.id]: link }));
            setSendingId(null);
            refresh();
            onBookingChanged();
          }}
        />
      )}
    </section>
  );
}

function QuoteFacts({ quote: q }: { quote: Quote }) {
  const date = (iso: string) => new Date(iso).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
  const facts = [
    q.sent_at && `Sent to ${q.sent_to ?? "the buyer"} ${date(q.sent_at)}`,
    q.viewed_at && `opened ${date(q.viewed_at)}`,
    q.decided_at && `${q.status === "accepted" ? "accepted" : "declined"} by ${q.decided_name ?? "the buyer"} ${date(q.decided_at)}`,
    q.decline_reason && `“${q.decline_reason}”`,
    q.valid_until && q.status !== "accepted" && `valid until ${new Date(q.valid_until).toLocaleDateString(undefined, { dateStyle: "medium" })}`,
  ].filter(Boolean);
  return facts.length ? <p className="text-xs text-muted-foreground">{facts.join(" · ")}</p> : null;
}

function CopyLink({ url }: { url: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="flex gap-2">
      <Input readOnly value={url} aria-label="Link to the quote" onFocus={(e) => e.target.select()} />
      <Button
        variant="outline"
        size="icon"
        aria-label="Copy link"
        onClick={() => {
          void navigator.clipboard?.writeText(url).then(() => setCopied(true));
        }}
      >
        {copied ? <Check /> : <Copy />}
      </Button>
    </div>
  );
}

interface LineDraft {
  description: string;
  quantity: string;
  unit_minor: number | null;
}

/** Totals as the server works them out: quantity × unit, rounded to the minor unit. */
export function lineAmount(quantity: number, unitMinor: number): number {
  return Math.round(quantity * unitMinor);
}

function QuoteEditor({ quote, onClose, onSaved }: { quote: Quote; onClose: () => void; onSaved: (q: Quote) => void }) {
  const currency = useWorkspaceCurrency();
  const code = quote.currency || currency;
  const [lines, setLines] = useState<LineDraft[]>(
    quote.line_items.map((l) => ({ description: l.description, quantity: String(l.quantity), unit_minor: l.unit_minor })),
  );
  const [validUntil, setValidUntil] = useState(quote.valid_until ?? "");
  const [message, setMessage] = useState(quote.message ?? "");
  const [terms, setTerms] = useState(quote.terms ?? "");
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [error, setError] = useState<string | null>(null);
  const total = lines.reduce((sum, l) => sum + lineAmount(Number(l.quantity) || 0, l.unit_minor ?? 0), 0);
  const set = (i: number, patch: Partial<LineDraft>) => setLines((ls) => ls.map((l, j) => (j === i ? { ...l, ...patch } : l)));
  const issue = (prefix: string) => issues.filter((x) => x.field?.startsWith(prefix)).map((x) => x.message).join("; ");

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={`Quote ${quote.reference}`} className="max-w-2xl">
      <form
        className="flex flex-col gap-4"
        onSubmit={async (e) => {
          e.preventDefault();
          setIssues([]);
          setError(null);
          try {
            const q = await ok(
              api.PATCH("/api/v1/quotes/{id}", {
                params: { path: { id: quote.id } },
                headers: { "If-Match": `"${quote.version}"` },
                body: {
                  line_items: lines.map((l) => ({
                    description: l.description,
                    quantity: Number(l.quantity) || 0,
                    unit_minor: l.unit_minor ?? 0,
                  })),
                  valid_until: validUntil || null,
                  message: message || null,
                  terms: terms || null,
                },
              }),
            );
            onSaved(q);
          } catch (err) {
            const found = failureIssues(err);
            if (found) setIssues(found);
            else if (err instanceof ApiFailure && err.status === 412) setError("This quote changed meanwhile. Close and open it again.");
            else setError(failure(err));
          }
        }}
      >
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-1 text-sm font-medium">Lines</legend>
          {lines.map((l, i) => (
            <div key={i} className="grid grid-cols-[1fr_4.5rem_8rem_auto] items-start gap-2">
              <Input aria-label={`Line ${i + 1} description`} value={l.description} onChange={(e) => set(i, { description: e.target.value })} />
              <Input
                aria-label={`Line ${i + 1} quantity`}
                inputMode="decimal"
                value={l.quantity}
                onChange={(e) => set(i, { quantity: e.target.value })}
              />
              <MoneyInput
                aria-label={`Line ${i + 1} price`}
                value={l.unit_minor === null ? null : { minor: l.unit_minor, currency: code }}
                currency={code}
                defaultCurrency={code}
                onChange={(m) => set(i, { unit_minor: m?.minor ?? null })}
              />
              <Button variant="ghost" size="icon" aria-label={`Remove line ${i + 1}`} onClick={() => setLines((ls) => ls.filter((_, j) => j !== i))}>
                <X />
              </Button>
            </div>
          ))}
          {issue("line_items") && <p className="text-sm text-destructive">{issue("line_items")}</p>}
          <div className="flex items-center justify-between">
            <Button variant="outline" size="sm" onClick={() => setLines((ls) => [...ls, { description: "", quantity: "1", unit_minor: null }])}>
              <Plus /> Add a line
            </Button>
            <span className="text-sm font-medium">Total {formatMoney({ minor: total, currency: code })}</span>
          </div>
        </fieldset>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="quote-valid">Valid until</Label>
          <Input id="quote-valid" type="date" value={validUntil} onChange={(e) => setValidUntil(e.target.value)} />
          {issue("valid_until") && <p className="text-sm text-destructive">{issue("valid_until")}</p>}
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="quote-message">Message to the buyer</Label>
          <Textarea id="quote-message" value={message} onChange={(e) => setMessage(e.target.value)} />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="quote-terms">Terms</Label>
          <Textarea id="quote-terms" value={terms} onChange={(e) => setTerms(e.target.value)} />
        </div>
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit">Save quote</Button>
        </div>
      </form>
    </Dialog>
  );
}

function SendDialog({
  quote,
  booking,
  onClose,
  onSent,
}: {
  quote: Quote;
  booking: Booking;
  onClose: () => void;
  onSent: (link: string) => void;
}) {
  const [to, setTo] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={`Send ${quote.reference}`}>
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          setBusy(true);
          try {
            const sent = await ok(
              api.POST("/api/v1/quotes/{id}/send", {
                params: { path: { id: quote.id } },
                headers: { "If-Match": `"${quote.version}"` },
                body: { to: to.trim() || null },
              }),
            );
            onSent(sent.link);
          } catch (err) {
            setError(failure(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <p className="text-sm">
          The buyer gets an email with a link to view the quote and accept or decline it. Any other open quote for this
          booking is withdrawn.
        </p>
        <Label htmlFor="send-to">Send to</Label>
        <Input
          id="send-to"
          type="email"
          placeholder={booking.contact_name ? `${booking.contact_name}'s email` : "buyer@example.com"}
          value={to}
          onChange={(e) => setTo(e.target.value)}
        />
        <p className="text-xs text-muted-foreground">Leave blank to use the booking contact&apos;s email.</p>
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy}>
            <Send /> Send quote
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

// The buyer's page -----------------------------------------------------------------------

export function PublicQuotePage() {
  const { token } = useParams({ from: "/quote/$token" });
  const quote = useQuery({
    queryKey: ["public-quote", token],
    queryFn: () => ok(api.GET("/api/v1/public/quotes/{token}", { params: { path: { token } } })),
    retry: false,
  });
  const [answer, setAnswer] = useState<"accept" | "decline" | null>(null);
  useEffect(() => {
    const b = quote.data?.branding as { colours?: Branding; name?: string } | undefined;
    if (b?.colours) applyBranding(b.colours);
    if (quote.data) document.title = `Quote ${quote.data.reference} · ${quote.data.workspace_name}`;
  }, [quote.data]);

  if (quote.isPending) {
    return (
      <div className="flex h-dvh items-center justify-center">
        <Spinner className="size-6" />
      </div>
    );
  }
  if (quote.isError) {
    return (
      <main className="mx-auto max-w-xl p-6">
        <Alert>
          {quote.error instanceof ApiFailure && quote.error.status === 404
            ? "This quote link does not work. It may have been replaced by a newer one; ask whoever sent it."
            : quote.error.message}
        </Alert>
      </main>
    );
  }
  const q = quote.data;
  const logo = (q.branding as { logo_url?: string } | null)?.logo_url;
  return (
    <main className="mx-auto flex max-w-2xl flex-col gap-6 p-4 sm:p-8">
      <header className="flex items-center gap-3">
        {logo && <img src={logo} alt="" className="size-10 rounded object-contain" />}
        <div>
          <p className="text-sm text-muted-foreground">{q.workspace_name}</p>
          <h1 className="text-2xl font-semibold">Quote {q.reference}</h1>
        </div>
      </header>
      <Card className="flex flex-col gap-4 p-5">
        <div>
          <h2 className="text-lg font-medium">{q.booking_title}</h2>
          <p className="text-sm text-muted-foreground">
            {formatWhen(q.starts_at, q.ends_at, q.all_day)}
            {q.venue && ` · ${q.venue}`}
          </p>
          {q.talent.length > 0 && <p className="text-sm">{q.talent.join(", ")}</p>}
        </div>
        {q.message && <p className="whitespace-pre-line text-sm">{q.message}</p>}
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b text-left text-muted-foreground">
              <th className="py-1 font-normal">Item</th>
              <th className="py-1 text-right font-normal">Amount</th>
            </tr>
          </thead>
          <tbody>
            {q.line_items.map((l, i) => (
              <tr key={i} className="border-b">
                <td className="py-2">
                  {l.description}
                  {l.quantity !== 1 && (
                    <span className="text-muted-foreground">
                      {" "}
                      ({l.quantity} × {formatMoney(l.unit)})
                    </span>
                  )}
                </td>
                <td className="py-2 text-right">{formatMoney(l.amount)}</td>
              </tr>
            ))}
          </tbody>
          <tfoot>
            <tr>
              <td className="pt-3 font-medium">Total</td>
              <td className="pt-3 text-right text-lg font-semibold">{formatMoney(q.total)}</td>
            </tr>
          </tfoot>
        </table>
        {q.terms && (
          <details className="text-sm">
            <summary className="cursor-pointer font-medium">Terms</summary>
            <p className="mt-2 whitespace-pre-line">{q.terms}</p>
          </details>
        )}
        {q.valid_until && q.can_decide && (
          <p className="text-sm text-muted-foreground">
            You can accept this quote until {new Date(q.valid_until).toLocaleDateString(undefined, { dateStyle: "long" })}.
          </p>
        )}
      </Card>
      <Decision quote={q} token={token} answer={answer} setAnswer={setAnswer} onDone={() => void quote.refetch()} />
    </main>
  );
}

function Decision({
  quote: q,
  token,
  answer,
  setAnswer,
  onDone,
}: {
  quote: PublicQuote;
  token: string;
  answer: "accept" | "decline" | null;
  setAnswer: (a: "accept" | "decline" | null) => void;
  onDone: () => void;
}) {
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (q.status === "accepted") {
    return (
      <Alert tone="success">
        Accepted by {q.decided_name}
        {q.decided_at && ` on ${new Date(q.decided_at).toLocaleDateString(undefined, { dateStyle: "long" })}`}. Thank you —{" "}
        {q.workspace_name} will be in touch.
      </Alert>
    );
  }
  if (q.status === "declined") return <Alert tone="info">This quote was declined. Thank you for letting us know.</Alert>;
  if (!q.can_decide) {
    return <Alert tone="info">This quote can no longer be accepted. Ask {q.workspace_name} for an up-to-date one.</Alert>;
  }
  if (!answer) {
    return (
      <div className="flex flex-wrap gap-3">
        <Button size="lg" onClick={() => setAnswer("accept")}>
          <Check /> Accept quote
        </Button>
        <Button size="lg" variant="outline" onClick={() => setAnswer("decline")}>
          Decline
        </Button>
      </div>
    );
  }
  return (
    <Card className="p-5">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          setBusy(true);
          try {
            if (answer === "accept") {
              await ok(
                api.POST("/api/v1/public/quotes/{token}/accept", {
                  params: { path: { token } },
                  body: { name, email: email || null },
                }),
              );
            } else {
              await ok(
                api.POST("/api/v1/public/quotes/{token}/decline", {
                  params: { path: { token } },
                  body: { name, reason: reason || null },
                }),
              );
            }
            setAnswer(null);
            onDone();
          } catch (err) {
            setError(failure(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <h2 className="font-medium">{answer === "accept" ? "Accept this quote" : "Decline this quote"}</h2>
        <Label htmlFor="decider-name">Your full name</Label>
        <Input id="decider-name" required autoComplete="name" value={name} onChange={(e) => setName(e.target.value)} />
        {answer === "accept" ? (
          <>
            <Label htmlFor="decider-email">Your email (optional)</Label>
            <Input id="decider-email" type="email" autoComplete="email" value={email} onChange={(e) => setEmail(e.target.value)} />
            <p className="text-xs text-muted-foreground">
              Typing your name and accepting is your agreement to this quote and its terms. We record the time and your
              connection&apos;s address with it.
            </p>
          </>
        ) : (
          <>
            <Label htmlFor="decline-reason">Anything we should know? (optional)</Label>
            <Textarea id="decline-reason" value={reason} onChange={(e) => setReason(e.target.value)} />
          </>
        )}
        {error && <Alert>{error}</Alert>}
        <div className="flex gap-2">
          <Button type="submit" disabled={busy || !name.trim()}>
            {answer === "accept" ? "Accept" : "Decline"}
          </Button>
          <Button variant="ghost" onClick={() => setAnswer(null)}>
            Back
          </Button>
        </div>
      </form>
    </Card>
  );
}
