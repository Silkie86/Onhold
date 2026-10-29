// Pitching: the workspace's own world regions, the statuses it gives
// organisations (each with a colour and what it means), what it pitches, and
// the actions and answers it logs; and on an organisation, what it is
// interested in and might be a match for, and its pitch log.

import type { Schemas } from "@onhold/api-client";
import { useInfiniteQuery, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import {
  Alert,
  Badge,
  Button,
  Card,
  Checkbox,
  Dialog,
  Input,
  Label,
  NativeSelect,
  type RecordJson,
  Spinner,
  StatusDot,
  Textarea,
} from "@onhold/ui";
import { Pencil, Plus, RotateCcw, Send, Trash2, X } from "lucide-react";
import { useMemo, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
import { fetchList, useOffline } from "../data.ts";
import { dialable, phonesOf } from "../phones.ts";
import { openInBrowser } from "../platform.ts";
import { useSession } from "../session.tsx";
import { useSetup } from "../setup.ts";
import { RecordSearch } from "./pickers.tsx";
import { keyFrom } from "./settings.tsx";

type View = Schemas["PitchingSettingsView"];
type Region = Schemas["Region"];
type Status = Schemas["Status"];
type ItemKind = Schemas["ItemKind"];
type ItemRef = Schemas["ItemRef"];
type Colour = Status["colour"];
type Meaning = Status["meaning"];
type Outcome = Schemas["PitchOutcome"];
type ActionType = Schemas["PitchAction"];
type OutcomeTag = Schemas["OutcomeTag"];
type Pitch = Schemas["PitchView"];
type PitchKind = Schemas["PitchKind"];
type PitchChannel = Schemas["PitchChannel"];
type PitchTemplate = Schemas["PitchTemplate"];
type SendWith = Schemas["PitchSendWith"];
type Unlogged = Schemas["UnloggedMail"];
type FollowUpStep = Schemas["FollowUpStep"];
type Due = Schemas["DueFollowUp"];

const COLOURS: Colour[] = ["green", "amber", "red", "grey", "blue", "purple"];
const MEANINGS: { value: Meaning; label: string }[] = [
  { value: "booked", label: "Booked or worked with" },
  { value: "talking", label: "Talking or interested" },
  { value: "inactive", label: "Not active" },
  { value: "other", label: "Something else" },
];

const TAGS: { value: OutcomeTag; label: string; colour: Colour }[] = [
  { value: "good", label: "Good news", colour: "green" },
  { value: "wait", label: "Try again later", colour: "amber" },
  { value: "no", label: "No for now", colour: "red" },
  { value: "never", label: "Never", colour: "grey" },
];

export const SEND_WITH: { value: SendWith; label: string; hint: string }[] = [
  {
    value: "onhold",
    label: "Send from OnHold",
    hint: "Logged by itself. With your own mailbox connected it goes from there and the answers come into OnHold too.",
  },
  { value: "mail_app", label: "Open in my email app", hint: "The pitch opens ready to send in your own email app." },
  { value: "copy", label: "Copy", hint: "Copy the pitch to paste wherever you like." },
];

export const CHANNELS: { value: PitchChannel; label: string }[] = [
  { value: "email", label: "Email" },
  { value: "phone", label: "Phone" },
  { value: "text", label: "Text" },
  { value: "whatsapp", label: "WhatsApp" },
  { value: "social", label: "Social media" },
  { value: "in_person", label: "In person" },
  { value: "other", label: "Other" },
];

function failure(e: unknown): string {
  return e instanceof ApiFailure || e instanceof Error ? e.message : "Something went wrong.";
}

/**
 * A key for a new status or region. It carries the time, so it never
 * matches one removed earlier, which organisations may still hold.
 */
export function newKey(prefix: string, taken: string[], now = Date.now()): string {
  return keyFor(`${prefix}_${now.toString(36)}`, taken);
}

/** A key from a name, unlike any already taken: "North America" → `north_america`. */
export function keyFor(name: string, taken: string[]): string {
  const base = keyFrom(name).slice(0, 34) || "item";
  let key = base;
  for (let n = 2; taken.includes(key); n++) key = `${base}_${n}`;
  return key;
}

/** The workspace's pitching settings; they change rarely. */
export function usePitchingSettings(enabled = true) {
  return useQuery({
    queryKey: ["pitching-settings"],
    queryFn: () => ok(api.GET("/api/v1/pitching/settings")),
    staleTime: 5 * 60_000,
    enabled,
  });
}

export function PitchingSettingsPage() {
  const settings = usePitchingSettings();
  // Kept here: the form starts again from what was saved.
  const [saved, setSaved] = useState(false);
  if (settings.isPending) {
    return (
      <div className="flex justify-center p-10">
        <Spinner className="size-6" />
      </div>
    );
  }
  if (settings.isError) return <Alert className="m-4">{settings.error.message}</Alert>;
  return (
    <SettingsForm
      key={settings.data.version}
      saved={settings.data}
      justSaved={saved}
      onSaved={() => setSaved(true)}
      onChange={() => setSaved(false)}
    />
  );
}

function SettingsForm({
  saved,
  justSaved,
  onSaved,
  onChange,
}: {
  saved: View;
  justSaved: boolean;
  onSaved: () => void;
  onChange: () => void;
}) {
  const { workspace } = useSession();
  const admin = workspace.role === "owner" || workspace.role === "admin";
  const setup = useSetup();
  const qc = useQueryClient();
  const [regions, setRegions] = useState<Region[]>(saved.regions ?? []);
  const [statuses, setStatuses] = useState<Status[]>(saved.statuses ?? []);
  const [items, setItems] = useState<ItemKind[]>(saved.items ?? []);
  const [outcomes, setOutcomes] = useState<Outcome[]>(saved.outcomes ?? []);
  const [actions, setActions] = useState<ActionType[]>(saved.actions ?? []);
  const [templates, setTemplates] = useState<PitchTemplate[]>(saved.templates ?? []);
  const [sendWith, setSendWith] = useState<SendWith>(saved.send_with ?? "onhold");
  const [followUps, setFollowUps] = useState<FollowUpStep[]>(saved.follow_ups ?? []);
  const [giveUp, setGiveUp] = useState(saved.give_up_after_days ?? 14);
  const [search, setSearch] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const regionOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const r of regions) for (const c of r.countries) m.set(c, r.key);
    return m;
  }, [regions]);
  const q = search.trim().toLowerCase();
  const shown = q ? saved.countries.filter((c) => c.name.toLowerCase().includes(q) || c.code.toLowerCase() === q) : saved.countries;
  const unplaced = saved.countries.filter((c) => !regionOf.has(c.code)).length;

  const changed = () => {
    if (justSaved) onChange();
  };
  const moveCountry = (code: string, to: string) => {
    changed();
    setRegions((rs) =>
      rs.map((r) => {
        const without = r.countries.filter((c) => c !== code);
        return r.key === to ? { ...r, countries: [...without, code] } : { ...r, countries: without };
      }),
    );
  };
  const resetRegions = () => {
    if (!window.confirm("Go back to OnHold's world regions? Your own regions are replaced when you save.")) return;
    changed();
    const names: Record<string, string> = {
      europe: "Europe",
      north_central_america: "North and Central America",
      south_america: "South America",
      oceania: "Oceania",
      asia: "Asia",
      africa: "Africa",
    };
    setRegions(
      Object.entries(names).map(([key, name]) => ({
        key,
        name,
        countries: saved.countries.filter((c) => c.default_region === key).map((c) => c.code),
      })),
    );
  };

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      const after = await ok(
        api.PUT("/api/v1/pitching/settings", {
          headers: { "If-Match": `"${saved.version}"` },
          body: { regions, statuses, items, outcomes, actions, templates, send_with: sendWith, follow_ups: followUps, give_up_after_days: giveUp },
        }),
      );
      onSaved();
      qc.setQueryData(["pitching-settings"], after);
      // Region and status choices on organisations come from these.
      void qc.invalidateQueries({ queryKey: ["schema"] });
    } catch (e) {
      if (e instanceof ApiFailure && e.status === 412) setError("Someone else changed these while you were editing. Reload to see their changes.");
      else setError(failure(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-6 p-4 sm:p-6">
      <h1 className="text-xl font-semibold">Pitching</h1>
      <p className="text-sm text-muted-foreground">
        How you sort the venues, brands and buyers you pitch to. Organisations are placed in a region from their country, and you can pick another
        on any of them. Statuses show as a coloured dot; bookings and enquiries move an organisation along by themselves.
      </p>
      {!admin && <Alert tone="info">Only owners and admins can change these.</Alert>}

      <Card className="flex flex-col gap-4 p-5 text-sm">
        <h2 className="font-semibold">What you pitch</h2>
        <label className="flex items-center gap-2">
          <Checkbox
            checked={items.includes("talent")}
            disabled={!admin}
            onChange={(e) => {
              changed();
              setItems((xs) => (e.target.checked ? [...xs, "talent"] : xs.filter((x) => x !== "talent")));
            }}
          />
          {setup.word("crm", "talent", true)}
        </label>
        <label className="flex items-center gap-2">
          <Checkbox
            checked={items.includes("project")}
            disabled={!admin}
            onChange={(e) => {
              changed();
              setItems((xs) => (e.target.checked ? [...xs, "project"] : xs.filter((x) => x !== "project")));
            }}
          />
          Tours and projects
        </label>
      </Card>

      <Card className="flex flex-col gap-4 p-5 text-sm">
        <h2 className="font-semibold">Statuses</h2>
        <p className="text-muted-foreground">
          What each colour means to you. "Booked" and "Talking" are the ones a confirmed booking or a new enquiry sets by themselves.
        </p>
        <ul className="flex flex-col gap-3">
          {statuses.map((s, i) => (
            <li key={s.key} className="flex flex-wrap items-end gap-2">
              <div className="flex min-w-40 flex-1 flex-col gap-1">
                <Label htmlFor={`status-${s.key}`}>Label</Label>
                <Input
                  id={`status-${s.key}`}
                  value={s.label}
                  maxLength={60}
                  disabled={!admin}
                  onChange={(e) => {
                    changed();
                    setStatuses((xs) => xs.map((x, j) => (j === i ? { ...x, label: e.target.value } : x)));
                  }}
                />
              </div>
              <div className="flex flex-col gap-1">
                <Label htmlFor={`colour-${s.key}`}>
                  <StatusDot colour={s.colour} label="Colour" />
                </Label>
                <NativeSelect
                  id={`colour-${s.key}`}
                  value={s.colour}
                  disabled={!admin}
                  onChange={(e) => {
                    changed();
                    setStatuses((xs) => xs.map((x, j) => (j === i ? { ...x, colour: e.target.value as Colour } : x)));
                  }}
                >
                  {COLOURS.map((c) => (
                    <option key={c} value={c}>
                      {c[0]?.toUpperCase() + c.slice(1)}
                    </option>
                  ))}
                </NativeSelect>
              </div>
              <div className="flex flex-col gap-1">
                <Label htmlFor={`meaning-${s.key}`}>Means</Label>
                <NativeSelect
                  id={`meaning-${s.key}`}
                  value={s.meaning}
                  disabled={!admin}
                  onChange={(e) => {
                    changed();
                    setStatuses((xs) => xs.map((x, j) => (j === i ? { ...x, meaning: e.target.value as Meaning } : x)));
                  }}
                >
                  {MEANINGS.map((m) => (
                    <option key={m.value} value={m.value}>
                      {m.label}
                    </option>
                  ))}
                </NativeSelect>
              </div>
              {admin && (
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={`Remove ${s.label || "status"}`}
                  onClick={() => {
                    changed();
                    setStatuses((xs) => xs.filter((_, j) => j !== i));
                  }}
                >
                  <Trash2 />
                </Button>
              )}
            </li>
          ))}
        </ul>
        {admin && statuses.length < 12 && (
          <div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                changed();
                setStatuses((xs) => [
                  ...xs,
                  { key: newKey("status", xs.map((x) => x.key)), label: "", colour: "blue", meaning: "other" },
                ]);
              }}
            >
              <Plus /> Add a status
            </Button>
          </div>
        )}
      </Card>

      <Card className="flex flex-col gap-4 p-5 text-sm">
        <h2 className="font-semibold">Answers</h2>
        <p className="text-muted-foreground">
          What people say back when you pitch. What each one means decides when to try them again.
        </p>
        <ul className="flex flex-col gap-3">
          {outcomes.map((o, i) => (
            <li key={o.key} className="flex flex-wrap items-end gap-2">
              <div className="flex min-w-40 flex-1 flex-col gap-1">
                <Label htmlFor={`outcome-${o.key}`}>Answer</Label>
                <Input
                  id={`outcome-${o.key}`}
                  value={o.label}
                  maxLength={60}
                  disabled={!admin}
                  onChange={(e) => {
                    changed();
                    setOutcomes((xs) => xs.map((x, j) => (j === i ? { ...x, label: e.target.value } : x)));
                  }}
                />
              </div>
              <div className="flex flex-col gap-1">
                <Label htmlFor={`tag-${o.key}`}>Means</Label>
                <NativeSelect
                  id={`tag-${o.key}`}
                  value={o.tag}
                  disabled={!admin}
                  onChange={(e) => {
                    changed();
                    setOutcomes((xs) => xs.map((x, j) => (j === i ? { ...x, tag: e.target.value as OutcomeTag } : x)));
                  }}
                >
                  {TAGS.map((t) => (
                    <option key={t.value} value={t.value}>
                      {t.label}
                    </option>
                  ))}
                </NativeSelect>
              </div>
              {admin && outcomes.length > 1 && (
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={`Remove ${o.label || "answer"}`}
                  onClick={() => {
                    changed();
                    setOutcomes((xs) => xs.filter((_, j) => j !== i));
                  }}
                >
                  <Trash2 />
                </Button>
              )}
            </li>
          ))}
        </ul>
        {admin && outcomes.length < 20 && (
          <div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                changed();
                setOutcomes((xs) => [...xs, { key: newKey("answer", xs.map((x) => x.key)), label: "", tag: "wait" }]);
              }}
            >
              <Plus /> Add an answer
            </Button>
          </div>
        )}
      </Card>

      <Card className="flex flex-col gap-4 p-5 text-sm">
        <h2 className="font-semibold">Actions</h2>
        <p className="text-muted-foreground">The things you do to win people over, to log on each organisation.</p>
        <ul className="flex flex-col gap-2">
          {actions.map((a, i) => (
            <li key={a.key} className="flex items-center gap-2">
              <Input
                aria-label="Action"
                value={a.label}
                maxLength={60}
                disabled={!admin}
                onChange={(e) => {
                  changed();
                  setActions((xs) => xs.map((x, j) => (j === i ? { ...x, label: e.target.value } : x)));
                }}
              />
              {admin && actions.length > 1 && (
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={`Remove ${a.label || "action"}`}
                  disabled={templates.some((t) => t.action === a.key) || followUps.some((f) => f.action === a.key)}
                  title={
                    templates.some((t) => t.action === a.key)
                      ? "A pitch template logs this; change the template first."
                      : followUps.some((f) => f.action === a.key)
                        ? "A follow-up logs this; change the follow-up first."
                        : undefined
                  }
                  onClick={() => {
                    changed();
                    setActions((xs) => xs.filter((_, j) => j !== i));
                  }}
                >
                  <Trash2 />
                </Button>
              )}
            </li>
          ))}
        </ul>
        {admin && actions.length < 20 && (
          <div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                changed();
                setActions((xs) => [...xs, { key: newKey("action", xs.map((x) => x.key)), label: "" }]);
              }}
            >
              <Plus /> Add an action
            </Button>
          </div>
        )}
      </Card>

      <Card className="flex flex-col gap-4 p-5 text-sm">
        <h2 className="font-semibold">Sending pitches</h2>
        <p className="text-muted-foreground">How pitches go out unless a member picks their own way. Nothing is sent without someone pressing send.</p>
        <div className="flex flex-col gap-2">
          {SEND_WITH.map((w) => (
            <label key={w.value} className="flex items-start gap-2">
              <input
                type="radio"
                name="send-with"
                className="mt-1"
                checked={sendWith === w.value}
                disabled={!admin}
                onChange={() => {
                  changed();
                  setSendWith(w.value);
                }}
              />
              <span className="flex flex-col">
                <span className="font-medium">{w.label}</span>
                <span className="text-muted-foreground">{w.hint}</span>
              </span>
            </label>
          ))}
        </div>
      </Card>

      <Card className="flex flex-col gap-4 p-5 text-sm">
        <h2 className="font-semibold">Pitch templates</h2>
        <p className="text-muted-foreground">
          The wording you pitch with. These fill themselves in: {(saved.placeholders ?? []).map((p) => `{${p.name}}`).join(" ")}. You can change the words on
          each pitch before it goes.
        </p>
        <details className="text-muted-foreground">
          <summary className="cursor-pointer">What each one means</summary>
          <ul className="mt-1 grid gap-x-4 sm:grid-cols-2">
            {(saved.placeholders ?? []).map((p) => (
              <li key={p.name}>
                <code>{`{${p.name}}`}</code> {p.means}
              </li>
            ))}
          </ul>
        </details>
        <ul className="flex flex-col gap-4">
          {templates.map((t, i) => {
            const set = (patch: Partial<PitchTemplate>) => {
              changed();
              setTemplates((xs) => xs.map((x, j) => (j === i ? { ...x, ...patch } : x)));
            };
            return (
              <li key={t.key} className="flex flex-col gap-2 rounded-md border p-3">
                <div className="flex flex-wrap items-end gap-2">
                  <div className="flex min-w-40 flex-1 flex-col gap-1">
                    <Label htmlFor={`template-${t.key}-name`}>Name</Label>
                    <Input id={`template-${t.key}-name`} value={t.name} maxLength={80} disabled={!admin} onChange={(e) => set({ name: e.target.value })} />
                  </div>
                  <div className="flex w-48 flex-col gap-1">
                    <Label htmlFor={`template-${t.key}-action`}>Logs as</Label>
                    <NativeSelect id={`template-${t.key}-action`} value={t.action} disabled={!admin} onChange={(e) => set({ action: e.target.value })}>
                      {!actions.some((a) => a.key === t.action) && <option value={t.action}>Choose an action</option>}
                      {actions.map((a) => (
                        <option key={a.key} value={a.key}>
                          {a.label || "Unnamed action"}
                        </option>
                      ))}
                    </NativeSelect>
                  </div>
                  {admin && (
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label={`Remove ${t.name || "template"}`}
                      onClick={() => {
                        changed();
                        setTemplates((xs) => xs.filter((_, j) => j !== i));
                        // A follow-up that sent it becomes a reminder.
                        setFollowUps((fs) => fs.map((f) => (f.template_key === t.key ? { ...f, template_key: null } : f)));
                      }}
                    >
                      <Trash2 />
                    </Button>
                  )}
                </div>
                <div className="flex flex-col gap-1">
                  <Label htmlFor={`template-${t.key}-subject`}>Subject</Label>
                  <Input id={`template-${t.key}-subject`} value={t.subject} maxLength={300} disabled={!admin} onChange={(e) => set({ subject: e.target.value })} />
                </div>
                <div className="flex flex-col gap-1">
                  <Label htmlFor={`template-${t.key}-body`}>Message</Label>
                  <Textarea
                    id={`template-${t.key}-body`}
                    value={t.body}
                    maxLength={5000}
                    rows={7}
                    disabled={!admin}
                    onChange={(e) => set({ body: e.target.value })}
                  />
                </div>
              </li>
            );
          })}
        </ul>
        {templates.length === 0 && <p className="text-muted-foreground">No templates. Pitches start from a blank message.</p>}
        {admin && templates.length < 30 && (
          <div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                changed();
                setTemplates((xs) => [
                  ...xs,
                  { key: newKey("template", xs.map((x) => x.key)), name: "", action: actions[0]?.key ?? "", subject: "", body: "Hi {firstName},\n\n" },
                ]);
              }}
            >
              <Plus /> Add a template
            </Button>
          </div>
        )}
      </Card>

      <Card className="flex flex-col gap-4 p-5 text-sm">
        <h2 className="font-semibold">Follow-ups</h2>
        <p className="text-muted-foreground">
          What to do next when a pitch gets no answer, each so many days after the last thing done. With a template it's a message to send; without,
          a reminder. An answer stops them. They show under Follow-ups due, and nothing goes without you sending it.
        </p>
        <ol className="flex flex-col gap-3">
          {followUps.map((f, i) => {
            const set = (patch: Partial<FollowUpStep>) => {
              changed();
              setFollowUps((xs) => xs.map((x, j) => (j === i ? { ...x, ...patch } : x)));
            };
            return (
              <li key={i} className="flex flex-wrap items-end gap-2 rounded-md border p-3">
                <span className="flex w-full items-center justify-between">
                  <span className="font-medium">Follow-up {i + 1}</span>
                  {admin && (
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label={`Remove follow-up ${i + 1}`}
                      onClick={() => {
                        changed();
                        setFollowUps((xs) => xs.filter((_, j) => j !== i));
                      }}
                    >
                      <Trash2 />
                    </Button>
                  )}
                </span>
                <div className="flex w-24 flex-col gap-1">
                  <Label htmlFor={`follow-up-${i}-days`}>Days after</Label>
                  <Input
                    id={`follow-up-${i}-days`}
                    type="number"
                    min={1}
                    max={365}
                    value={f.after_days}
                    disabled={!admin}
                    onChange={(e) => set({ after_days: Number(e.target.value) })}
                  />
                </div>
                <div className="flex w-36 flex-col gap-1">
                  <Label htmlFor={`follow-up-${i}-channel`}>How</Label>
                  <NativeSelect
                    id={`follow-up-${i}-channel`}
                    value={f.channel}
                    disabled={!admin}
                    onChange={(e) => set({ channel: e.target.value as PitchChannel })}
                  >
                    {CHANNELS.map((c) => (
                      <option key={c.value} value={c.value}>
                        {c.label}
                      </option>
                    ))}
                  </NativeSelect>
                </div>
                <div className="flex w-44 flex-col gap-1">
                  <Label htmlFor={`follow-up-${i}-template`}>Send</Label>
                  <NativeSelect
                    id={`follow-up-${i}-template`}
                    value={f.template_key ?? ""}
                    disabled={!admin}
                    onChange={(e) => {
                      const t = templates.find((x) => x.key === e.target.value);
                      set({ template_key: t?.key ?? null, ...(t && { action: t.action }) });
                    }}
                  >
                    <option value="">A reminder only</option>
                    {templates.map((t) => (
                      <option key={t.key} value={t.key}>
                        {t.name || "Unnamed template"}
                      </option>
                    ))}
                  </NativeSelect>
                </div>
                <div className="flex w-44 flex-col gap-1">
                  <Label htmlFor={`follow-up-${i}-action`}>Logs as</Label>
                  <NativeSelect id={`follow-up-${i}-action`} value={f.action} disabled={!admin} onChange={(e) => set({ action: e.target.value })}>
                    {!actions.some((a) => a.key === f.action) && <option value={f.action}>Choose an action</option>}
                    {actions.map((a) => (
                      <option key={a.key} value={a.key}>
                        {a.label || "Unnamed action"}
                      </option>
                    ))}
                  </NativeSelect>
                </div>
              </li>
            );
          })}
        </ol>
        {followUps.length === 0 && <p className="text-muted-foreground">No follow-ups: pitches wait for an answer.</p>}
        {admin && followUps.length < 10 && (
          <div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                changed();
                const t = templates.find((x) => x.action !== templates[0]?.action) ?? null;
                setFollowUps((xs) => [
                  ...xs,
                  { after_days: 7, channel: "email", action: t?.action ?? actions[0]?.key ?? "", template_key: t?.key ?? null },
                ]);
              }}
            >
              <Plus /> Add a follow-up
            </Button>
          </div>
        )}
        <label className="flex flex-wrap items-center gap-2">
          After the last one, ask what came of it after
          <Input
            type="number"
            aria-label="Days after the last follow-up"
            className="w-20"
            min={0}
            max={365}
            value={giveUp}
            disabled={!admin}
            onChange={(e) => {
              changed();
              setGiveUp(Number(e.target.value));
            }}
          />
          days (0 never asks).
        </label>
      </Card>

      <Card className="flex flex-col gap-4 p-5 text-sm">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 className="font-semibold">Regions</h2>
          {admin && (
            <Button variant="ghost" size="sm" onClick={resetRegions}>
              <RotateCcw /> OnHold's world regions
            </Button>
          )}
        </div>
        <p className="text-muted-foreground">
          Rename them, add your own (say, "UK" apart from the rest of Europe) and choose which countries go in each.
        </p>
        <ul className="flex flex-col gap-2">
          {regions.map((r, i) => (
            <li key={r.key} className="flex items-center gap-2">
              <Input
                aria-label="Region name"
                value={r.name}
                maxLength={60}
                disabled={!admin}
                onChange={(e) => {
                  changed();
                  setRegions((xs) => xs.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)));
                }}
              />
              <span className="w-24 shrink-0 text-muted-foreground">
                {r.countries.length} {r.countries.length === 1 ? "country" : "countries"}
              </span>
              {admin && regions.length > 1 && (
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={`Remove ${r.name || "region"}`}
                  onClick={() => {
                    changed();
                    setRegions((xs) => xs.filter((_, j) => j !== i));
                  }}
                >
                  <Trash2 />
                </Button>
              )}
            </li>
          ))}
        </ul>
        {admin && regions.length < 30 && (
          <div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                changed();
                setRegions((xs) => [...xs, { key: newKey("region", xs.map((x) => x.key)), name: "", countries: [] }]);
              }}
            >
              <Plus /> Add a region
            </Button>
          </div>
        )}
        <div className="flex flex-col gap-2">
          <Label htmlFor="country-search">Countries</Label>
          <Input id="country-search" placeholder="Search countries…" value={search} onChange={(e) => setSearch(e.target.value)} />
          {unplaced > 0 && (
            <p className="text-muted-foreground">
              {unplaced} {unplaced === 1 ? "country is" : "countries are"} in no region; organisations there show no region unless you pick one.
            </p>
          )}
          <ul className="max-h-96 divide-y overflow-y-auto rounded-md border">
            {shown.map((c) => (
              <li key={c.code} className="flex items-center justify-between gap-2 px-3 py-1.5">
                <span className="min-w-0 truncate">{c.name}</span>
                <NativeSelect
                  aria-label={`Region for ${c.name}`}
                  className="w-48"
                  value={regionOf.get(c.code) ?? ""}
                  disabled={!admin}
                  onChange={(e) => moveCountry(c.code, e.target.value)}
                >
                  <option value="">No region</option>
                  {regions.map((r) => (
                    <option key={r.key} value={r.key}>
                      {r.name || "Unnamed region"}
                    </option>
                  ))}
                </NativeSelect>
              </li>
            ))}
            {shown.length === 0 && <li className="px-3 py-2 text-muted-foreground">No country matches.</li>}
          </ul>
        </div>
      </Card>

      {error && <Alert>{error}</Alert>}
      {justSaved && <Alert tone="success">Saved.</Alert>}
      {admin && (
        <div>
          <Button onClick={() => void save()} disabled={busy}>
            {busy && <Spinner className="size-4" />} Save
          </Button>
        </div>
      )}
    </div>
  );
}

/**
 * On an organisation: which of the workspace's talent or projects it is
 * interested in, and which might suit it. Changed online only.
 */
export function OrganisationPitching({
  organisation,
  canWrite,
  onSaved,
}: {
  organisation: RecordJson;
  canWrite: boolean;
  onSaved: (after: RecordJson) => void;
}) {
  const settings = usePitchingSettings();
  const kinds: ItemKind[] = settings.data?.items ?? ["talent"];
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const interested = (organisation.interested_in as ItemRef[] | undefined) ?? [];
  const matches = (organisation.potential_matches as ItemRef[] | undefined) ?? [];

  const save = async (list: "interested_in" | "potential_matches", next: ItemRef[]) => {
    setBusy(true);
    setError(null);
    try {
      const after = await ok(
        api.PATCH("/api/v1/organisations/{id}", {
          params: { path: { id: String(organisation.id) } },
          headers: { "If-Match": `"${String(organisation.version)}"` },
          body: { [list]: next.map((x) => ({ kind: x.kind, id: x.id })) },
        }),
      );
      onSaved(after as unknown as RecordJson);
    } catch (e) {
      if (e instanceof ApiFailure && e.status === 412) setError("Someone else changed this organisation. Reload to see their changes.");
      else setError(failure(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="flex flex-col gap-3" aria-label="Pitching">
      <h2 className="font-semibold">Pitching</h2>
      {error && <Alert>{error}</Alert>}
      <ItemList
        title="Interested in"
        hint="What they have asked about or booked before."
        items={interested}
        kinds={kinds}
        canWrite={canWrite && !busy}
        onChange={(next) => void save("interested_in", next)}
      />
      <ItemList
        title="Potential matches"
        hint="What you think would suit them."
        items={matches}
        kinds={kinds}
        canWrite={canWrite && !busy}
        onChange={(next) => void save("potential_matches", next)}
      />
      <PitchLog organisationId={String(organisation.id)} items={uniqueItems([...interested, ...matches])} canWrite={canWrite} />
    </section>
  );
}

function ItemList({
  title,
  hint,
  items,
  kinds,
  canWrite,
  onChange,
}: {
  title: string;
  hint: string;
  items: ItemRef[];
  kinds: ItemKind[];
  canWrite: boolean;
  onChange: (next: ItemRef[]) => void;
}) {
  const setup = useSetup();
  const [kind, setKind] = useState<ItemKind>(kinds[0] ?? "talent");
  const add = (picked: ItemRef) => {
    if (items.some((x) => x.kind === picked.kind && x.id === picked.id)) return;
    onChange([...items, picked]);
  };
  return (
    <Card className="flex flex-col gap-2 p-4 text-sm">
      <div>
        <h3 className="font-medium">{title}</h3>
        <p className="text-muted-foreground">{hint}</p>
      </div>
      {items.length === 0 ? (
        <p className="text-muted-foreground">Nothing yet.</p>
      ) : (
        <ul className="flex flex-wrap gap-2">
          {items.map((x) => (
            <li key={`${x.kind}:${x.id}`} className="inline-flex items-center gap-1 rounded-full border px-2.5 py-0.5">
              {x.kind === "talent" ? (
                <Link to="/crm/$entity/$id" params={{ entity: "talent", id: x.id }} className="hover:underline">
                  {x.name}
                </Link>
              ) : (
                <Link to="/projects/$id" params={{ id: x.id }} className="hover:underline">
                  {x.name}
                </Link>
              )}
              {canWrite && (
                <button type="button" aria-label={`Remove ${x.name}`} onClick={() => onChange(items.filter((y) => y !== x))}>
                  <X className="size-3.5" />
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      {canWrite && items.length < 50 && (
        <div className="flex flex-wrap gap-2">
          {kinds.length > 1 && (
            <NativeSelect aria-label={`What to add to ${title.toLowerCase()}`} className="w-40" value={kind} onChange={(e) => setKind(e.target.value as ItemKind)}>
              <option value="talent">{setup.word("crm", "talent", true)}</option>
              <option value="project">Projects</option>
            </NativeSelect>
          )}
          <div className="min-w-48 flex-1">
            {(kinds.length > 1 ? kind : kinds[0]) === "project" ? (
              <ProjectSearch exclude={items.map((x) => x.id)} onPick={(p) => add({ kind: "project", id: p.id, name: p.name })} />
            ) : (
              <RecordSearch
                entity="talent"
                exclude={items.map((x) => x.id)}
                placeholder={`Add ${setup.word("crm", "talent", true).toLowerCase()}…`}
                onPick={(p) => add({ kind: "talent", id: p.id, name: p.name })}
              />
            )}
          </div>
        </div>
      )}
    </Card>
  );
}

/** Projects have no search on the server; the workspace's are few, so filter here. */
function ProjectSearch({ exclude, onPick }: { exclude: string[]; onPick: (p: { id: string; name: string }) => void }) {
  const [q, setQ] = useState("");
  const projects = useQuery({
    queryKey: ["projects", "pick"],
    queryFn: () => ok(api.GET("/api/v1/projects", { params: { query: { limit: 200 } } })),
    enabled: q.trim().length > 0,
  });
  const needle = q.trim().toLowerCase();
  const items = (projects.data?.items ?? []).filter((p) => !exclude.includes(p.id) && p.name.toLowerCase().includes(needle)).slice(0, 8);
  return (
    <div className="relative">
      <Input placeholder="Add a project…" value={q} onChange={(e) => setQ(e.target.value)} autoComplete="off" />
      {needle && (
        <ul className="absolute z-20 mt-1 max-h-56 w-full overflow-y-auto rounded-md border bg-card text-sm shadow-md">
          {projects.isFetching && !projects.data && (
            <li className="p-2">
              <Spinner />
            </li>
          )}
          {items.map((p) => (
            <li key={p.id}>
              <button
                type="button"
                className="w-full px-3 py-2 text-left hover:bg-accent"
                onClick={() => {
                  setQ("");
                  onPick({ id: p.id, name: p.name });
                }}
              >
                {p.name}
              </button>
            </li>
          ))}
          {projects.data && items.length === 0 && <li className="p-2 text-muted-foreground">No project matches.</li>}
        </ul>
      )}
    </div>
  );
}

/** A date picker's value ("2026-10-28") for a moment, in local time. */
export function dayOf(iso: string): string {
  const d = new Date(iso);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/**
 * When a logged entry happened, from the day picked: now for today, so the
 * log keeps its order, and midday for another day.
 */
export function happenedAt(day: string, now = new Date()): string {
  if (day === dayOf(now.toISOString())) return now.toISOString();
  return new Date(`${day}T12:00:00`).toISOString();
}

const TAG_COLOUR = Object.fromEntries(TAGS.map((t) => [t.value, t.colour])) as Record<OutcomeTag, Colour>;
const LOG_PAGE = 50;

/** Each item once, in the order first seen. */
export function uniqueItems(items: ItemRef[]): ItemRef[] {
  const seen = new Set<string>();
  return items.filter((x) => {
    const key = `${x.kind}:${x.id}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/** When a follow-up is due, from today. */
export function dueLabel(dueOn: string, today: string): string {
  const days = Math.round((Date.parse(`${dueOn}T00:00:00Z`) - Date.parse(`${today}T00:00:00Z`)) / 86_400_000);
  if (days === 0) return "Due today";
  if (days === -1) return "Due yesterday";
  if (days < 0) return `${-days} days overdue`;
  if (days === 1) return "Due tomorrow";
  return `Due in ${days} days`;
}

/** What a follow-up is, in a few words. */
export function dueWhat(d: Due, templates: PitchTemplate[]): string {
  if (d.kind === "date") return "The follow-up date you set";
  if (d.kind === "no_answer") return "No answer after the follow-ups: what came of it?";
  const how = CHANNELS.find((c) => c.value === d.channel)?.label ?? "Follow up";
  const t = templates.find((x) => x.key === d.template_key);
  const step = d.steps > 1 ? `Follow-up ${d.step} of ${d.steps}` : "Follow-up";
  return t ? `${step}: ${how}, "${t.name}"` : `${step}: ${how} (${d.action_label ?? "reminder"})`;
}

/** Everything due to follow up, soonest first, with what to do about each. */
export function FollowUpsDuePage() {
  const settings = usePitchingSettings();
  const [ahead, setAhead] = useState(0);
  const due = useQuery({
    queryKey: ["follow-ups", "due", ahead],
    queryFn: () => ok(api.GET("/api/v1/pitching/follow-ups", { params: { query: { days_ahead: ahead } } })),
  });
  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 className="text-xl font-semibold">Follow-ups due</h1>
        <NativeSelect aria-label="Show" className="w-48" value={ahead} onChange={(e) => setAhead(Number(e.target.value))}>
          <option value={0}>Due now</option>
          <option value={7}>And the next 7 days</option>
          <option value={30}>And the next 30 days</option>
        </NativeSelect>
      </div>
      <p className="text-sm text-muted-foreground">
        Pitches with no answer yet, by the follow-ups in{" "}
        <Link to="/settings/pitching" className="underline">
          Settings → Pitching
        </Link>
        , and organisations given a follow-up date.
      </p>
      {due.isPending || settings.isPending ? (
        <Spinner />
      ) : due.isError ? (
        <Alert>{due.error.message}</Alert>
      ) : due.data.length === 0 ? (
        <Card className="p-5 text-sm text-muted-foreground">Nothing to follow up. Pitches waiting on an answer show here when their next step is due.</Card>
      ) : (
        <ul className="flex flex-col gap-2">
          {due.data.map((d) => (
            <DueRow key={`${d.organisation_id}:${d.kind}`} due={d} settings={settings.data} />
          ))}
        </ul>
      )}
    </div>
  );
}

function DueRow({ due: d, settings }: { due: Due; settings: View | undefined }) {
  const qc = useQueryClient();
  const sync = useOffline();
  const [pitching, setPitching] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const people = useQuery({
    queryKey: ["list", "contact", "org", d.organisation_id],
    queryFn: async () => (await fetchList(sync, "contact", { organisation_id: d.organisation_id, limit: 100 })).data,
    enabled: pitching,
  });
  const today = dayOf(new Date().toISOString());
  const overdue = d.due_on < today;
  const done = () => {
    void qc.invalidateQueries({ queryKey: ["follow-ups"] });
    void qc.invalidateQueries({ queryKey: ["pitches", d.organisation_id] });
  };
  const run = async (call: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await call();
      done();
    } catch (e) {
      setError(failure(e));
    } finally {
      setBusy(false);
    }
  };
  // A reminder done: logged as its action.
  const logStep = () =>
    run(() =>
      ok(
        api.POST("/api/v1/pitches", {
          body: {
            organisation_id: d.organisation_id,
            kind: "action",
            type_key: d.action ?? "",
            channel: d.channel ?? null,
            contact_id: d.contact_id ?? null,
            item: d.item ? { kind: d.item.kind, id: d.item.id } : null,
          },
        }),
      ),
    );
  const logOutcome = (o: Outcome) =>
    run(() =>
      ok(
        api.POST("/api/v1/pitches", {
          body: {
            organisation_id: d.organisation_id,
            kind: "outcome",
            type_key: o.key,
            contact_id: d.contact_id ?? null,
            item: d.item ? { kind: d.item.kind, id: d.item.id } : null,
          },
        }),
      ),
    );
  // A date set by hand, done: cleared.
  const clearDate = () =>
    run(async () => {
      const org = await ok(api.GET("/api/v1/organisations/{id}", { params: { path: { id: d.organisation_id } } }));
      await ok(
        api.PATCH("/api/v1/organisations/{id}", {
          params: { path: { id: d.organisation_id } },
          headers: { "If-Match": `"${String((org as RecordJson).version)}"` },
          body: { follow_up_on: null },
        }),
      );
    });

  return (
    <li className="flex flex-col gap-2 rounded-md border bg-card p-3 text-sm">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div className="flex min-w-0 flex-col gap-0.5">
          <Link to="/crm/$entity/$id" params={{ entity: "organisations", id: d.organisation_id }} className="font-medium underline">
            {d.organisation_name}
          </Link>
          <span>{dueWhat(d, settings?.templates ?? [])}</span>
          <span className="text-muted-foreground">
            {[d.contact_name, d.item?.name, d.last_action_at && `last done ${new Date(d.last_action_at).toLocaleDateString()}`]
              .filter(Boolean)
              .join(" · ")}
          </span>
        </div>
        <Badge className={overdue ? "text-destructive" : undefined}>{dueLabel(d.due_on, today)}</Badge>
      </div>
      {error && <Alert>{error}</Alert>}
      <div className="flex flex-wrap items-center gap-2">
        {d.kind === "step" && d.template_key && (
          <Button size="sm" disabled={busy || !settings} onClick={() => setPitching(true)}>
            <Send /> Write it
          </Button>
        )}
        {d.kind === "step" && (
          <Button size="sm" variant={d.template_key ? "outline" : "default"} disabled={busy} onClick={() => void logStep()}>
            {d.template_key ? "I've sent it" : `Done: log "${d.action_label ?? "it"}"`}
          </Button>
        )}
        {d.kind === "no_answer" &&
          (settings?.outcomes ?? []).map((o) => (
            <Button key={o.key} variant="outline" size="sm" disabled={busy} onClick={() => void logOutcome(o)}>
              <StatusDot colour={TAG_COLOUR[o.tag]} label={o.label} />
            </Button>
          ))}
        {d.kind === "date" && (
          <Button size="sm" variant="outline" disabled={busy} onClick={() => void clearDate()}>
            Done: clear the date
          </Button>
        )}
      </div>
      {pitching && settings && people.data && (
        <PitchDialog
          organisationId={d.organisation_id}
          settings={settings}
          people={people.data.items.map(person)}
          items={d.item ? [d.item] : []}
          initial={{ template: d.template_key ?? null, contact: d.contact_id ?? null, item: d.item ?? null }}
          onClose={() => setPitching(false)}
          onLogged={() => {
            setPitching(false);
            done();
          }}
        />
      )}
    </li>
  );
}

/**
 * Whether an answered action still waits for what they said: no answer has
 * been logged since theirs came in.
 */
export function awaitingOutcome(entry: Pitch, entries: Pitch[]): boolean {
  if (entry.kind !== "action" || !entry.answered_at) return false;
  const at = new Date(entry.answered_at).getTime();
  return !entries.some((e) => e.kind === "outcome" && new Date(e.happened_at).getTime() >= at);
}

/** An organisation's pitch log: what was done and the answers given, latest first. */
function PitchLog({ organisationId, items, canWrite }: { organisationId: string; items: ItemRef[]; canWrite: boolean }) {
  const settings = usePitchingSettings();
  const qc = useQueryClient();
  const sync = useOffline();
  const log = useInfiniteQuery({
    queryKey: ["pitches", organisationId],
    initialPageParam: 0,
    queryFn: ({ pageParam }) =>
      ok(api.GET("/api/v1/organisations/{id}/pitches", { params: { path: { id: organisationId }, query: { offset: pageParam, limit: LOG_PAGE } } })),
    getNextPageParam: (last, pages) => {
      const shown = pages.reduce((n, p) => n + p.items.length, 0);
      return shown < last.total && last.items.length > 0 ? shown : undefined;
    },
  });
  const entries = log.data?.pages.flatMap((p) => p.items) ?? [];
  const total = log.data?.pages.at(-1)?.total ?? 0;
  const people = useQuery({
    queryKey: ["list", "contact", "org", organisationId],
    queryFn: async () => (await fetchList(sync, "contact", { organisation_id: organisationId, limit: 100 })).data,
  });
  const [editing, setEditing] = useState<Pitch | PitchKind | null>(null);
  // The Pitch dialog: open, from a follow-up's template if one.
  const [pitching, setPitching] = useState<false | { initial?: Due }>(false);
  const next = useQuery({
    queryKey: ["follow-ups", "organisation", organisationId],
    queryFn: () => ok(api.GET("/api/v1/pitching/follow-ups", { params: { query: { organisation_id: organisationId } } })),
  });
  const [error, setError] = useState<string | null>(null);
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["pitches", organisationId] });
    void qc.invalidateQueries({ queryKey: ["follow-ups"] });
    // Sent mail logged or removed elsewhere comes and goes from "In your
    // Sent mail"; not asked again for those who can't read the inbox.
    if (qc.getQueryState(["unlogged", organisationId])?.status !== "error")
      void qc.invalidateQueries({ queryKey: ["unlogged", organisationId] });
  };
  const remove = async (p: Pitch) => {
    if (!window.confirm(`Remove "${p.type_label ?? "this entry"}" from the log?`)) return;
    setError(null);
    try {
      await ok(api.DELETE("/api/v1/pitches/{id}", { params: { path: { id: p.id } } }));
      refresh();
    } catch (e) {
      setError(failure(e));
    }
  };
  // What they said in their answer, logged at the time it came in (never
  // later than now, should a clock run ahead), by the person who answered
  // when they still work here.
  const [saying, setSaying] = useState<string | null>(null);
  const said = async (p: Pitch, outcome: Outcome) => {
    if (saying || !p.answered_at) return;
    setSaying(p.id);
    setError(null);
    const at = new Date(Math.min(new Date(p.answered_at).getTime(), Date.now())).toISOString();
    const contact = p.contact_id && people.data?.items.some((c) => String(c.id) === p.contact_id) ? p.contact_id : null;
    try {
      await ok(
        api.POST("/api/v1/pitches", {
          params: { header: { "Idempotency-Key": crypto.randomUUID() } },
          body: {
            organisation_id: organisationId,
            kind: "outcome",
            type_key: outcome.key,
            happened_at: at,
            channel: p.answer?.channel ?? p.channel ?? null,
            contact_id: contact,
            item: p.item ? { kind: p.item.kind, id: p.item.id } : null,
          },
        }),
      );
      refresh();
    } catch (e) {
      setError(failure(e));
    } finally {
      setSaying(null);
    }
  };
  const channel = (c: PitchChannel | null | undefined) => CHANNELS.find((x) => x.value === c)?.label;

  return (
    <Card className="flex flex-col gap-3 p-4 text-sm">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div>
          <h3 className="font-medium">Pitch log</h3>
          <p className="text-muted-foreground">What you have done and what they said.</p>
        </div>
        {canWrite && settings.data && (
          <div className="flex flex-wrap gap-2">
            <Button size="sm" disabled={!people.data} onClick={() => setPitching({})}>
              <Send /> Pitch
            </Button>
            <Button variant="outline" size="sm" onClick={() => setEditing("action")}>
              <Plus /> Log an action
            </Button>
            <Button variant="outline" size="sm" onClick={() => setEditing("outcome")}>
              <Plus /> Log an answer
            </Button>
          </div>
        )}
      </div>
      {next.data?.map((d) => (
        <p key={d.kind} className="flex flex-wrap items-center gap-2 rounded-md bg-muted px-3 py-2">
          <span className="font-medium">{dueLabel(d.due_on, dayOf(new Date().toISOString()))}:</span>
          <span>{dueWhat(d, settings.data?.templates ?? [])}</span>
          {canWrite && d.kind === "step" && d.template_key && settings.data && (
            <Button size="sm" variant="outline" disabled={!people.data} onClick={() => setPitching({ initial: d })}>
              <Send /> Write it
            </Button>
          )}
        </p>
      ))}
      {error && <Alert>{error}</Alert>}
      {canWrite && settings.data && <InSent organisationId={organisationId} settings={settings.data} onChanged={refresh} />}
      {log.isPending ? (
        <Spinner />
      ) : log.isError ? (
        <Alert>{log.error.message}</Alert>
      ) : entries.length === 0 ? (
        <p className="text-muted-foreground">Nothing logged yet.</p>
      ) : (
        <ul className="divide-y rounded-md border">
          {entries.map((p) => (
            <li key={p.id} className="flex items-start justify-between gap-2 px-3 py-2">
              <div className="flex min-w-0 flex-col gap-0.5">
                <span className="flex flex-wrap items-center gap-2">
                  {p.kind === "outcome" ? (
                    <StatusDot
                      colour={p.tag ? TAG_COLOUR[p.tag] : "grey"}
                      label={<span className="font-medium">{p.type_label ?? "An answer no longer listed"}</span>}
                    />
                  ) : (
                    <span className="font-medium">{p.type_label ?? "An action no longer listed"}</span>
                  )}
                  {p.item && <Badge>{p.item.name}</Badge>}
                </span>
                <span className="text-muted-foreground">
                  {[new Date(p.happened_at).toLocaleDateString(), channel(p.channel), p.contact_name].filter(Boolean).join(" · ")}
                </span>
                {p.note && <span className="break-words whitespace-pre-line">{p.note}</span>}
                {p.message && (
                  <span className="text-muted-foreground">
                    {p.source === "spotted" ? (
                      "From your Sent mail."
                    ) : p.message.status === "failed" ? (
                      <span className="text-destructive">Not sent.</span>
                    ) : p.message.status === "sent" ? (
                      "Sent from OnHold."
                    ) : (
                      "Sending…"
                    )}{" "}
                    <Link to="/inbox" search={{ thread: p.message.thread_id }} className="underline">
                      Open the conversation
                    </Link>
                  </span>
                )}
                {p.answered_at && (
                  <span className="text-muted-foreground">
                    They answered on {new Date(p.answered_at).toLocaleDateString()}.{" "}
                    {p.answer && (
                      <Link to="/inbox" search={{ thread: p.answer.thread_id }} className="underline">
                        Read their answer
                      </Link>
                    )}
                  </span>
                )}
                {canWrite && settings.data && awaitingOutcome(p, entries) && (settings.data.outcomes ?? []).length > 0 && (
                  <span className="flex flex-wrap items-center gap-1.5 pt-1" role="group" aria-label="What did they say?">
                    <span className="text-muted-foreground">What did they say?</span>
                    {(settings.data.outcomes ?? []).map((o) => (
                      <Button key={o.key} variant="outline" size="sm" disabled={saying !== null} onClick={() => void said(p, o)}>
                        <StatusDot colour={TAG_COLOUR[o.tag]} label={o.label} />
                      </Button>
                    ))}
                  </span>
                )}
              </div>
              {canWrite && (
                <span className="flex shrink-0">
                  <Button variant="ghost" size="icon" aria-label="Edit" onClick={() => setEditing(p)}>
                    <Pencil />
                  </Button>
                  <Button variant="ghost" size="icon" aria-label="Remove" onClick={() => void remove(p)}>
                    <Trash2 />
                  </Button>
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
      {log.hasNextPage && (
        <Button variant="ghost" size="sm" className="self-start" disabled={log.isFetchingNextPage} onClick={() => void log.fetchNextPage()}>
          {log.isFetchingNextPage && <Spinner className="size-4" />} Show older ({total - entries.length} more)
        </Button>
      )}
      {pitching && settings.data && (
        <PitchDialog
          organisationId={organisationId}
          settings={settings.data}
          people={(people.data?.items ?? []).map(person)}
          items={items}
          initial={
            pitching.initial && {
              template: pitching.initial.template_key ?? null,
              contact: pitching.initial.contact_id ?? null,
              item: pitching.initial.item ?? null,
            }
          }
          onClose={() => setPitching(false)}
          onLogged={() => {
            setPitching(false);
            refresh();
          }}
        />
      )}
      {editing && settings.data && (
        <LogDialog
          organisationId={organisationId}
          entry={typeof editing === "string" ? null : editing}
          kind={typeof editing === "string" ? editing : editing.kind}
          settings={settings.data}
          people={(people.data?.items ?? []).map((c) => ({ id: String(c.id), name: String(c.name) }))}
          items={items}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            refresh();
          }}
        />
      )}
    </Card>
  );
}

/**
 * Email to the organisation that went from someone's own mailbox, outside
 * OnHold, and isn't in the log: log each as a pitch or set it aside. Shown
 * only to those who can read the inbox.
 */
function InSent({ organisationId, settings, onChanged }: { organisationId: string; settings: View; onChanged: () => void }) {
  const mail = useQuery({
    queryKey: ["unlogged", organisationId],
    queryFn: () => ok(api.GET("/api/v1/organisations/{id}/pitches/unlogged", { params: { path: { id: organisationId } } })),
    retry: false,
  });
  const actions = settings.actions ?? [];
  const [chosen, setChosen] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  if (!mail.data || mail.data.length === 0 || actions.length === 0) return null;

  const act = async (m: Unlogged, call: () => Promise<unknown>) => {
    setBusy(m.message_id);
    setError(null);
    try {
      await call();
      void mail.refetch();
      onChanged();
    } catch (e) {
      setError(failure(e));
    } finally {
      setBusy(null);
    }
  };
  const log = (m: Unlogged) =>
    act(m, () =>
      ok(
        api.POST("/api/v1/pitches", {
          body: {
            organisation_id: organisationId,
            kind: "action",
            type_key: chosen[m.message_id] ?? actions[0]?.key ?? "",
            message_id: m.message_id,
            happened_at: new Date(Math.min(new Date(m.sent_at).getTime(), Date.now())).toISOString(),
            channel: "email",
            contact_id: m.contact_id ?? null,
          },
        }),
      ),
    );
  const setAside = (m: Unlogged) =>
    act(m, () => ok(api.POST("/api/v1/pitches/not-a-pitch", { body: { message_id: m.message_id } })));

  return (
    <section aria-label="In your Sent mail" className="flex flex-col gap-2 rounded-md border border-dashed p-3">
      <div>
        <h4 className="font-medium">In your Sent mail</h4>
        <p className="text-muted-foreground">Email to them that isn't in the log yet. Log it, or set it aside.</p>
      </div>
      {error && <Alert>{error}</Alert>}
      <ul className="flex flex-col gap-2">
        {mail.data.map((m) => (
          <li key={m.message_id} className="flex flex-wrap items-center justify-between gap-2">
            <span className="flex min-w-0 flex-col">
              <Link to="/inbox" search={{ thread: m.thread_id }} className="truncate font-medium underline">
                {m.subject || "(no subject)"}
              </Link>
              <span className="text-muted-foreground">
                {[new Date(m.sent_at).toLocaleDateString(), m.contact_name ?? m.to.join(", ")].filter(Boolean).join(" · ")}
              </span>
            </span>
            <span className="flex flex-wrap items-center gap-2">
              <NativeSelect
                aria-label="Log it as"
                className="w-40"
                value={chosen[m.message_id] ?? actions[0]?.key ?? ""}
                onChange={(e) => setChosen({ ...chosen, [m.message_id]: e.target.value })}
              >
                {actions.map((a) => (
                  <option key={a.key} value={a.key}>
                    {a.label}
                  </option>
                ))}
              </NativeSelect>
              <Button size="sm" disabled={busy !== null} onClick={() => void log(m)}>
                Log
              </Button>
              <Button variant="ghost" size="sm" disabled={busy !== null} onClick={() => void setAside(m)}>
                Not a pitch
              </Button>
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}

const itemKey = (x: ItemRef | null) => (x ? `${x.kind}:${x.id}` : "");

type Person = { id: string; name: string; email: string | null; phone: string | null };

function person(c: RecordJson): Person {
  return { id: String(c.id), name: String(c.name), email: ((c.emails as string[] | undefined) ?? [])[0] ?? null, phone: phonesOf(c)[0] ?? null };
}

/** The placeholders (of those `known`) still in a pitch, to fill in by hand. */
export function unfilled(text: string, known: string[]): string[] {
  const names = new Set(known);
  return [...new Set([...text.matchAll(/\{([A-Za-z]+)\}/g)].map((m) => m[1] ?? "").filter((n) => names.has(n)))];
}

/** A pitch opened in the sender's own email app. */
export function mailtoLink(to: string | null, subject: string, body: string): string {
  const q = new URLSearchParams({ subject, body }).toString().replace(/\+/g, "%20");
  return `mailto:${to ? encodeURIComponent(to).replace(/%40/g, "@") : ""}?${q}`;
}

/** A pitch opened in WhatsApp to an international number, or to pick a chat. */
export function whatsappLink(phone: string | null, text: string): string {
  const digits = phone?.startsWith("+") ? dialable(phone).replace(/\D/g, "") : "";
  return `https://wa.me/${digits}?text=${encodeURIComponent(text)}`;
}

/** A pitch opened as a text message (`?&body=` works on iPhones and Android alike). */
export function smsLink(phone: string | null, text: string): string {
  return `sms:${phone ? dialable(phone) : ""}?&body=${encodeURIComponent(text)}`;
}

type Way = "mail_app" | "copy" | "text" | "whatsapp";
const WAY_CHANNEL: Record<Way, PitchChannel | ""> = { mail_app: "email", copy: "", text: "text", whatsapp: "whatsapp" };

/**
 * Pitching someone: a template filled in for them and what is pitched,
 * changed as need be, then sent from OnHold (logged by itself), or opened
 * in their email app, copied, or opened as a text or WhatsApp, and logged
 * with one tap once sent.
 */
function PitchDialog({
  organisationId,
  settings,
  people,
  items,
  initial,
  onClose,
  onLogged,
}: {
  organisationId: string;
  settings: View;
  people: Person[];
  items: ItemRef[];
  /** A follow-up's template, person and item, to start from. */
  initial?: { template: string | null; contact: string | null; item: ItemRef | null };
  onClose: () => void;
  onLogged: () => void;
}) {
  const setup = useSetup();
  const qc = useQueryClient();
  const templates = settings.templates ?? [];
  const me = useQuery({ queryKey: ["pitching-me"], queryFn: () => ok(api.GET("/api/v1/pitching/me")) });
  const [contact, setContact] = useState(
    () => (initial?.contact && people.some((p) => p.id === initial.contact) ? initial.contact : (people.find((p) => p.email) ?? people[0])?.id) ?? "",
  );
  const [item, setItem] = useState<ItemRef | null>(initial ? initial.item : (items[0] ?? null));
  const [template, setTemplate] = useState(initial ? (initial.template ?? "") : (templates[0]?.key ?? ""));
  const [subject, setSubject] = useState("");
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // After it was opened elsewhere or copied: how, to log it once sent.
  const [asked, setAsked] = useState<Way | null>(null);
  const [askedChannel, setAskedChannel] = useState<PitchChannel | "">("");
  const who = people.find((p) => p.id === contact) ?? null;
  const chosen = templates.find((t) => t.key === template) ?? null;
  // What it logs: the template's action, or the workspace's first.
  const action = chosen?.action ?? settings.actions?.[0]?.key ?? "";
  const choices = item && !items.some((x) => itemKey(x) === itemKey(item)) ? [item, ...items] : items;

  const draft = useQuery({
    queryKey: ["pitch-draft", organisationId, template, contact, itemKey(item)],
    enabled: template !== "",
    // Fetched again only when who, what or the template changes, so edits
    // stay; and dropped once the dialog closes, so the next one is fresh.
    staleTime: Infinity,
    gcTime: 0,
    refetchOnWindowFocus: false,
    queryFn: () =>
      ok(
        api.POST("/api/v1/organisations/{id}/pitches/draft", {
          params: { path: { id: organisationId } },
          body: { template_key: template, contact_id: contact || null, item: item ? { kind: item.kind, id: item.id } : null },
        }),
      ),
  });
  // A new draft replaces the words; changing who or what rewrites it.
  const [drafted, setDrafted] = useState<unknown>(null);
  if (draft.data && draft.data !== drafted) {
    setDrafted(draft.data);
    setSubject(draft.data.subject);
    setBody(draft.data.body);
  }

  const usual: SendWith = me.data?.send_with ?? me.data?.workspace_send_with ?? settings.send_with ?? "onhold";
  const left = unfilled(`${subject}\n${body}`, (settings.placeholders ?? []).map((p) => p.name));
  const text = subject ? `${subject}\n\n${body}` : body;
  const ready = body.trim() !== "" && !draft.isFetching;

  const send = async () => {
    if (!who) return;
    setBusy(true);
    setError(null);
    try {
      await ok(
        api.POST("/api/v1/pitches/send", {
          body: {
            organisation_id: organisationId,
            contact_id: who.id,
            item: item ? { kind: item.kind, id: item.id } : null,
            template_key: template || null,
            type_key: action,
            subject,
            body,
          },
        }),
      );
      // It shows among the organisation's and the person's conversations.
      void qc.invalidateQueries({ queryKey: ["threads"] });
      onLogged();
    } catch (e) {
      setError(failure(e));
      setBusy(false);
    }
  };
  const elsewhere = async (way: Way) => {
    setError(null);
    try {
      if (way === "copy") {
        if (!navigator.clipboard) throw new Error("This browser can't copy here. Select the words and copy them instead.");
        await navigator.clipboard.writeText(text);
      } else {
        await openInBrowser(
          way === "mail_app" ? mailtoLink(who?.email ?? null, subject, body) : way === "text" ? smsLink(who?.phone ?? null, text) : whatsappLink(who?.phone ?? null, text),
        );
      }
    } catch (e) {
      setError(e instanceof Error && e.message ? e.message : "That didn't work. Copy the words instead.");
      return;
    }
    setAsked(way);
    setAskedChannel(WAY_CHANNEL[way]);
  };
  const logIt = async () => {
    setBusy(true);
    setError(null);
    try {
      await ok(
        api.POST("/api/v1/pitches", {
          body: {
            organisation_id: organisationId,
            contact_id: contact || null,
            kind: "action",
            type_key: action,
            channel: askedChannel || null,
            item: item ? { kind: item.kind, id: item.id } : null,
            template_key: template || null,
            note: subject.trim().slice(0, 2000) || null,
          },
        }),
      );
      onLogged();
    } catch (e) {
      setError(failure(e));
      setBusy(false);
    }
  };
  const setUsual = async (v: SendWith | "") => {
    try {
      const after = await ok(api.PUT("/api/v1/pitching/me", { body: { send_with: v || null } }));
      qc.setQueryData(["pitching-me"], after);
    } catch (e) {
      setError(failure(e));
    }
  };

  const ways: { way: SendWith | Way; label: string; hidden?: boolean }[] = [
    { way: "onhold", label: "Send from OnHold", hidden: !who?.email },
    { way: "mail_app", label: "Open in my email app" },
    { way: "copy", label: "Copy" },
    { way: "text", label: "Text", hidden: !who?.phone },
    { way: "whatsapp", label: "WhatsApp", hidden: !who?.phone },
  ];
  const go = (way: SendWith | Way) => (way === "onhold" ? void send() : void elsewhere(way));
  const primary = ways.find((w) => w.way === usual && !w.hidden) ?? ways.find((w) => !w.hidden && w.way !== "onhold") ?? ways[1];

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="Pitch">
      <div className="flex flex-col gap-3 text-sm">
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1">
            <Label htmlFor="pitch-contact">To</Label>
            <NativeSelect id="pitch-contact" value={contact} onChange={(e) => setContact(e.target.value)}>
              {people.length === 0 && <option value="">No one at this organisation yet</option>}
              {people.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                  {p.email ? ` (${p.email})` : ""}
                </option>
              ))}
            </NativeSelect>
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="pitch-template">Template</Label>
            <NativeSelect
              id="pitch-template"
              value={template}
              onChange={(e) => {
                setTemplate(e.target.value);
                if (e.target.value === "") {
                  setSubject("");
                  setBody("");
                  // Picking a template again fills it in again.
                  setDrafted(null);
                }
              }}
            >
              {templates.map((t) => (
                <option key={t.key} value={t.key}>
                  {t.name}
                </option>
              ))}
              <option value="">A blank message</option>
            </NativeSelect>
          </div>
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="pitch-item">What you pitch</Label>
          {choices.length > 0 && (
            <NativeSelect
              id="pitch-item"
              value={itemKey(item)}
              onChange={(e) => setItem(choices.find((x) => itemKey(x) === e.target.value) ?? null)}
            >
              <option value="">Nothing in particular</option>
              {choices.map((x) => (
                <option key={itemKey(x)} value={itemKey(x)}>
                  {x.name}
                </option>
              ))}
            </NativeSelect>
          )}
          <RecordSearch
            id={choices.length > 0 ? undefined : "pitch-item"}
            entity="talent"
            exclude={choices.filter((x) => x.kind === "talent").map((x) => x.id)}
            placeholder={`Or find ${setup.word("crm", "talent", true).toLowerCase()}…`}
            onPick={(p) => setItem({ kind: "talent", id: p.id, name: p.name })}
          />
        </div>
        {draft.isError && <Alert>{draft.error.message}</Alert>}
        <div className="flex flex-col gap-1">
          <Label htmlFor="pitch-subject">Subject</Label>
          <Input id="pitch-subject" value={subject} maxLength={1000} onChange={(e) => setSubject(e.target.value)} />
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="pitch-body">Message</Label>
          <Textarea id="pitch-body" value={body} rows={10} onChange={(e) => setBody(e.target.value)} />
          {draft.isFetching && <Spinner className="size-4" />}
        </div>
        {left.length > 0 && (
          <Alert tone="info">
            Fill in {left.map((n) => `{${n}}`).join(", ")} before it goes: there was nothing to fill {left.length === 1 ? "it" : "them"} with.
          </Alert>
        )}
        {error && <Alert>{error}</Alert>}
        {asked ? (
          <div className="flex flex-col gap-2 rounded-md border bg-muted/40 p-3" role="status">
            <span className="font-medium">Did you send it?</span>
            {asked === "copy" && (
              <NativeSelect aria-label="How you sent it" value={askedChannel} onChange={(e) => setAskedChannel(e.target.value as PitchChannel | "")}>
                <option value="">How you sent it (optional)</option>
                {CHANNELS.map((c) => (
                  <option key={c.value} value={c.value}>
                    {c.label}
                  </option>
                ))}
              </NativeSelect>
            )}
            <div className="flex flex-wrap gap-2">
              <Button size="sm" disabled={busy} onClick={() => void logIt()}>
                {busy && <Spinner className="size-4" />} Yes, log it
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setAsked(null)}>
                Not yet
              </Button>
            </div>
          </div>
        ) : (
          <div className="flex flex-col gap-2">
            <div className="flex flex-wrap gap-2">
              {primary && (
                <Button
                  disabled={busy || !ready || (primary.way === "onhold" && left.length > 0)}
                  onClick={() => go(primary.way)}
                >
                  {busy && <Spinner className="size-4" />} {primary.way === "onhold" ? "Send" : primary.label}
                </Button>
              )}
              {ways
                .filter((w) => !w.hidden && w !== primary)
                .map((w) => (
                  <Button
                    key={w.way}
                    variant="outline"
                    disabled={busy || !ready || (w.way === "onhold" && left.length > 0)}
                    onClick={() => go(w.way)}
                  >
                    {w.label}
                  </Button>
                ))}
            </div>
            {primary?.way === "onhold" && (
              <p className="text-muted-foreground">
                {me.data?.mailbox
                  ? `Sent from ${me.data.mailbox}, then logged here; answers arrive in the inbox.`
                  : "Sent by OnHold in your workspace's name, then logged here. Answers go to your own email; connect it under Email accounts to see them in OnHold."}
              </p>
            )}
            {!who?.email && people.length > 0 && <p className="text-muted-foreground">They have no email address, so it cannot be sent from OnHold.</p>}
          </div>
        )}
        <div className="flex flex-wrap items-center justify-between gap-2 border-t pt-3">
          <label className="flex items-center gap-2 text-muted-foreground">
            Usually I
            <NativeSelect aria-label="How you usually pitch" className="w-64" value={me.data?.send_with ?? ""} onChange={(e) => void setUsual(e.target.value as SendWith | "")}>
              <option value="">Do as the workspace does</option>
              {SEND_WITH.map((w) => (
                <option key={w.value} value={w.value}>
                  {w.label}
                </option>
              ))}
            </NativeSelect>
          </label>
          <Button variant="ghost" onClick={onClose}>
            Close
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

function LogDialog({
  organisationId,
  entry,
  kind,
  settings,
  people,
  items,
  onClose,
  onSaved,
}: {
  organisationId: string;
  entry: Pitch | null;
  kind: PitchKind;
  settings: View;
  people: { id: string; name: string }[];
  items: ItemRef[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const setup = useSetup();
  const types = kind === "action" ? (settings.actions ?? []) : (settings.outcomes ?? []);
  // An entry starts as it is; a new one with likely choices.
  const [start] = useState(() => ({
    type: entry ? entry.type_key : (types[0]?.key ?? ""),
    channel: (entry ? (entry.channel ?? "") : kind === "action" ? "email" : "") as PitchChannel | "",
    contact: entry ? (entry.contact_id ?? "") : people.length === 1 ? (people[0]?.id ?? "") : "",
    item: entry ? (entry.item ?? null) : null,
    day: dayOf(entry?.happened_at ?? new Date().toISOString()),
    note: entry?.note ?? "",
  }));
  const [type, setType] = useState(start.type);
  const [channel, setChannel] = useState<PitchChannel | "">(start.channel);
  const [contact, setContact] = useState(start.contact);
  const [item, setItem] = useState<ItemRef | null>(start.item);
  const [day, setDay] = useState(start.day);
  const [note, setNote] = useState(start.note);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const choices = item && !items.some((x) => x.kind === item.kind && x.id === item.id) ? [item, ...items] : items;
  // A type since removed from the settings stays on the entry until changed.
  const typeChoices = entry && !types.some((t) => t.key === entry.type_key) ? [{ key: entry.type_key, label: "No longer listed" }, ...types] : types;

  const save = async () => {
    setBusy(true);
    setError(null);
    const body = {
      contact_id: contact || null,
      type_key: type,
      channel: channel || null,
      item: item ? { kind: item.kind, id: item.id } : null,
      note: note.trim() || null,
      happened_at: happenedAt(day),
    };
    try {
      if (entry) {
        // Only what was changed, so what the form cannot show (such as an
        // act since deleted) stays as it is.
        await ok(
          api.PATCH("/api/v1/pitches/{id}", {
            params: { path: { id: entry.id } },
            headers: { "If-Match": `"${entry.version}"` },
            body: {
              ...(contact !== start.contact && { contact_id: body.contact_id }),
              ...(type !== start.type && { type_key: body.type_key }),
              ...(channel !== start.channel && { channel: body.channel }),
              ...(itemKey(item) !== itemKey(start.item) && { item: body.item }),
              ...(note !== start.note && { note: body.note }),
              ...(day !== start.day && { happened_at: body.happened_at }),
            },
          }),
        );
      } else {
        await ok(api.POST("/api/v1/pitches", { body: { ...body, organisation_id: organisationId, kind } }));
      }
      onSaved();
    } catch (e) {
      if (e instanceof ApiFailure && e.status === 412) setError("Someone else changed this entry. Close and open it again to see their changes.");
      else setError(failure(e));
      setBusy(false);
    }
  };

  const title = entry ? "Edit the entry" : kind === "action" ? "Log an action" : "Log an answer";
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={title}>
      <form
        className="flex flex-col gap-3 text-sm"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <div className="flex flex-col gap-1">
          <Label htmlFor="log-type">{kind === "action" ? "What you did" : "What they said"}</Label>
          <NativeSelect id="log-type" value={type} onChange={(e) => setType(e.target.value)}>
            {typeChoices.map((t) => (
              <option key={t.key} value={t.key}>
                {t.label}
              </option>
            ))}
          </NativeSelect>
        </div>
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1">
            <Label htmlFor="log-day">When</Label>
            <Input id="log-day" type="date" value={day} max={dayOf(new Date().toISOString())} required onChange={(e) => setDay(e.target.value)} />
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="log-channel">How</Label>
            <NativeSelect id="log-channel" value={channel} onChange={(e) => setChannel(e.target.value as PitchChannel | "")}>
              <option value="">Not saying</option>
              {CHANNELS.map((c) => (
                <option key={c.value} value={c.value}>
                  {c.label}
                </option>
              ))}
            </NativeSelect>
          </div>
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="log-contact">With</Label>
          <NativeSelect id="log-contact" value={contact} onChange={(e) => setContact(e.target.value)}>
            <option value="">The organisation</option>
            {entry?.contact_id && !people.some((p) => p.id === entry.contact_id) && (
              <option value={entry.contact_id}>{entry.contact_name ?? "Someone no longer here"}</option>
            )}
            {people.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </NativeSelect>
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="log-item">{kind === "action" ? "What you pitched" : "About"}</Label>
          {choices.length > 0 && (
            <NativeSelect
              id="log-item"
              value={item ? `${item.kind}:${item.id}` : ""}
              onChange={(e) => setItem(choices.find((x) => `${x.kind}:${x.id}` === e.target.value) ?? null)}
            >
              <option value="">Nothing in particular</option>
              {choices.map((x) => (
                <option key={`${x.kind}:${x.id}`} value={`${x.kind}:${x.id}`}>
                  {x.name}
                </option>
              ))}
            </NativeSelect>
          )}
          <RecordSearch
            id={choices.length > 0 ? undefined : "log-item"}
            entity="talent"
            exclude={choices.filter((x) => x.kind === "talent").map((x) => x.id)}
            placeholder={`Or find ${setup.word("crm", "talent", true).toLowerCase()}…`}
            onPick={(p) => setItem({ kind: "talent", id: p.id, name: p.name })}
          />
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="log-note">Note</Label>
          <Textarea id="log-note" value={note} maxLength={2000} rows={3} onChange={(e) => setNote(e.target.value)} />
        </div>
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy || !type}>
            {busy && <Spinner className="size-4" />} {entry ? "Save" : "Log it"}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
