// Your setup: plain tick boxes for what the workspace will use OnHold for.
// The answers decide which menus, pages and buttons it has and the words it
// uses, with a live summary and price estimate beside them. It can be sent
// again at any time; anything switched off is only hidden. Comments reach
// OnHold's team.

import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, Badge, Button, Card, Checkbox, Label, Spinner, Textarea, cn } from "@onhold/ui";
import { Info } from "lucide-react";
import { useEffect, useState } from "react";
import type { Schemas } from "@onhold/api-client";
import { api, ok } from "../api.ts";
import { useSession } from "../session.tsx";
import { type PriceList, estimate, price, setupOf } from "../setup.ts";

type Answers = Schemas["Answers"];
type Plan = Schemas["Plan"];

/** What each module and extra adds to the app, in plain words. */
const ADDS: Record<string, string> = {
  crm: "Contacts, organisations and your roster",
  calendar: "Calendar, with Google and Outlook connected",
  messaging: "Inbox for your email",
  finance: "Quotes, invoices, card payments and reports",
  documents: "Contracts, e-signatures and documents",
  events: "Events, tours and line-ups (coming soon)",
  portal: "A login for your artists or clients (coming soon)",
  team: "Assigning work to each other",
  commission: "Commission and artist statements",
  advanced: "Custom fields, pipelines and imports",
};

export function SetupSettingsPage() {
  const { workspace } = useSession();
  const canChange = workspace.role === "owner" || workspace.role === "admin";
  const qc = useQueryClient();
  const form = useQuery({ queryKey: ["setup"], queryFn: () => ok(api.GET("/api/v1/workspace/setup")) });
  const [answers, setAnswers] = useState<Answers | null>(null);
  const [comment, setComment] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  useEffect(() => {
    if (form.data && answers === null) setAnswers(form.data.answers);
  }, [form.data, answers]);
  const preview = useQuery({
    queryKey: ["setup-preview", answers],
    queryFn: () => ok(api.POST("/api/v1/workspace/setup/preview", { body: answers! })),
    enabled: !!answers,
    placeholderData: keepPreviousData,
  });

  if (form.isPending || !answers) return <Spinner />;
  if (form.isError) return <Alert>{form.error.message}</Alert>;
  const { questions, price_list, requests, done_at } = form.data;
  const prices = price_list as PriceList;
  const list = (key: string) => (answers[key as keyof Answers] as string[] | null | undefined) ?? [];

  const toggle = (key: string, choice: string, on: boolean, suggests: string[]) => {
    setMessage(null);
    setAnswers((a) => {
      if (!a) return a;
      if (key === "team") return { ...a, team: choice };
      const current = (a[key as keyof Answers] as string[] | undefined) ?? [];
      const next = { ...a, [key]: on ? [...current, choice] : current.filter((c) => c !== choice) };
      // Ticking a role ticks what it usually needs; anything can be unticked after.
      if (on && suggests.length > 0) next.features = [...new Set([...(next.features ?? []), ...suggests])];
      return next;
    });
  };

  const save = async () => {
    setBusy(true);
    setMessage(null);
    try {
      await ok(
        api.PUT("/api/v1/workspace/setup", {
          params: { header: { "If-Match": String(form.data.version) } },
          body: { answers, comment: comment.trim() || null },
        }),
      );
      setComment("");
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["workspace"] }),
        qc.invalidateQueries({ queryKey: ["setup"] }),
        // Field labels follow the workspace's words.
        qc.invalidateQueries({ queryKey: ["schema"] }),
      ]);
      setAnswers(null);
      setMessage({ ok: true, text: "Saved. Your menus now match your answers, and you can come back and change them any time." });
    } catch (err) {
      setMessage({ ok: false, text: err instanceof Error ? err.message : String(err) });
    } finally {
      setBusy(false);
    }
  };

  const plan: Plan | undefined = preview.data;
  const monthly = estimate(prices, { features: answers.features ?? [], team: answers.team });

  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-col gap-1">
        <h1 className="text-xl font-semibold">Your setup</h1>
        <p className="text-sm text-muted-foreground">
          Tick what you&apos;ll use OnHold for and we&apos;ll shape the app around it: only the menus and buttons you need, in your own words.
          Nothing is locked in. Change your answers whenever your work changes; anything you switch off is hidden, never deleted.
        </p>
      </div>
      {!canChange && <Alert tone="info">Only the workspace&apos;s owners and admins can change its setup.</Alert>}
      <div className="grid items-start gap-4 lg:grid-cols-[1fr_20rem]">
        <form
          className="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          {questions.map((q) => (
            <Card key={q.key} className="flex flex-col gap-3 p-4">
              <fieldset className="flex flex-col gap-3" disabled={!canChange}>
                <legend className="font-semibold">{q.title}</legend>
                {q.help && <p className="-mt-1 text-sm text-muted-foreground">{q.help}</p>}
                <div className="grid gap-2 sm:grid-cols-2">
                  {q.choices.map((c) => {
                    const id = `setup-${q.key}-${c.key}`;
                    const checked = q.multiple ? list(q.key).includes(c.key) : answers.team === c.key;
                    return (
                      <label
                        key={c.key}
                        htmlFor={id}
                        className={cn(
                          "flex min-h-11 cursor-pointer items-start gap-3 rounded-md border p-3 text-sm hover:bg-accent",
                          checked && "border-primary bg-accent",
                        )}
                      >
                        {q.multiple ? (
                          <Checkbox id={id} className="mt-0.5" checked={checked} onChange={(e) => toggle(q.key, c.key, e.target.checked, c.suggests)} />
                        ) : (
                          <input
                            id={id}
                            type="radio"
                            name={`setup-${q.key}`}
                            className="mt-0.5 size-4 accent-primary"
                            checked={checked}
                            onChange={() => toggle(q.key, c.key, true, [])}
                          />
                        )}
                        <span className="flex flex-col gap-0.5">
                          <span>{c.label}</span>
                          {c.hint && <span className="text-xs text-muted-foreground">{c.hint}</span>}
                        </span>
                      </label>
                    );
                  })}
                </div>
              </fieldset>
            </Card>
          ))}
          <Card className="flex flex-col gap-2 p-4">
            <Label htmlFor="setup-comment">Anything else?</Label>
            <p className="text-sm text-muted-foreground">
              Ask a question, or tell us about something you need that isn&apos;t here. It goes straight to the OnHold team.
            </p>
            <Textarea
              id="setup-comment"
              rows={4}
              maxLength={4000}
              disabled={!canChange}
              value={comment}
              onChange={(e) => setComment(e.target.value)}
              placeholder="e.g. Can I keep track of riders for each of my artists?"
            />
          </Card>
          {message && <Alert tone={message.ok ? "success" : "error"}>{message.text}</Alert>}
          {canChange && (
            // On a phone the summary sits below the form, so the price rides along with the button.
            <div className="sticky bottom-0 -mx-4 flex flex-wrap items-center gap-3 border-t bg-background px-4 py-3 sm:-mx-6 sm:px-6 lg:static lg:mx-0 lg:border-0 lg:bg-transparent lg:p-0">
              <span className="flex-1 text-sm lg:hidden">
                <span className="font-semibold">{price(monthly, prices.currency)}</span> a month{prices.placeholder ? " (example)" : ""}
              </span>
              <Button type="submit" disabled={busy}>
                {busy ? "Saving…" : done_at ? "Save changes" : "Set up my app"}
              </Button>
              {done_at && <span className="text-xs text-muted-foreground">Last changed {new Date(done_at).toLocaleDateString()}</span>}
            </div>
          )}
          {canChange && requests.length > 0 && (
            <Card className="flex flex-col gap-2 p-4">
              <h2 className="font-semibold">Your messages to OnHold</h2>
              <ul className="flex flex-col gap-2">
                {requests.map((r) => (
                  <li key={r.id} className="flex flex-col gap-1 rounded-md border p-3 text-sm">
                    <span className="whitespace-pre-wrap">{r.body}</span>
                    <span className="flex items-center gap-2 text-xs text-muted-foreground">
                      {new Date(r.created_at).toLocaleDateString()}
                      <Badge className="border">{r.resolved_at ? "Answered" : "With the OnHold team"}</Badge>
                    </span>
                  </li>
                ))}
              </ul>
            </Card>
          )}
        </form>
        <Summary plan={plan} prices={prices} monthly={monthly} />
      </div>
    </div>
  );
}

/** What the answers give, updated as boxes are ticked. */
function Summary({ plan, prices, monthly }: { plan: Plan | undefined; prices: PriceList; monthly: number }) {
  const words = setupOf(plan && { enabled_modules: plan.modules, extras: plan.extras, vocabulary: plan.vocabulary });
  const parts = plan ? [...plan.modules, ...plan.extras] : [];
  return (
    <Card className="flex flex-col gap-3 p-4 lg:sticky lg:top-4" aria-live="polite">
      <h2 className="font-semibold">Your app</h2>
      {!plan && <Spinner />}
      {plan && (
        <>
          <ul className="flex flex-col gap-1.5 text-sm">
            {plan.modules.includes("bookings") && (
              <li>
                {words.word("bookings", "enquiry", true)} and {words.word("bookings", "booking", true).toLowerCase()}
              </li>
            )}
            {parts
              .filter((p) => p !== "bookings")
              .map((p) => (
                <li key={p}>{p === "crm" ? `Contacts and ${words.word("crm", "talent", true).toLowerCase()}` : ADDS[p]}</li>
              ))}
          </ul>
          {plan.presets.length > 1 && (
            <p className="text-xs text-muted-foreground">Your roles are combined in one workspace; you switch nothing.</p>
          )}
        </>
      )}
      <div className="flex flex-col gap-1 border-t pt-3">
        <span className="text-sm text-muted-foreground">Estimated price</span>
        <span className="text-2xl font-semibold">
          {price(monthly, prices.currency)}
          <span className="text-sm font-normal text-muted-foreground"> a month</span>
        </span>
        {prices.placeholder && (
          <span className="flex items-center gap-1 text-xs text-muted-foreground">
            <Info className="size-3" /> Example prices while OnHold&apos;s pricing is finalised.
          </span>
        )}
        <span className="text-xs text-muted-foreground">Nothing is charged from this page.</span>
      </div>
    </Card>
  );
}
