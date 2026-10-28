// Pitching: the workspace's own world regions, the statuses it gives
// organisations (each with a colour and what it means), and what it pitches;
// and on an organisation, what it is interested in and might be a match for.

import type { Schemas } from "@onhold/api-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { Alert, Button, Card, Checkbox, Input, Label, NativeSelect, type RecordJson, Spinner, StatusDot } from "@onhold/ui";
import { Plus, RotateCcw, Trash2, X } from "lucide-react";
import { useMemo, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";
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

const COLOURS: Colour[] = ["green", "amber", "red", "grey", "blue", "purple"];
const MEANINGS: { value: Meaning; label: string }[] = [
  { value: "booked", label: "Booked or worked with" },
  { value: "talking", label: "Talking or interested" },
  { value: "inactive", label: "Not active" },
  { value: "other", label: "Something else" },
];

function failure(e: unknown): string {
  return e instanceof ApiFailure || e instanceof Error ? e.message : "Something went wrong.";
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
  if (settings.isPending) {
    return (
      <div className="flex justify-center p-10">
        <Spinner className="size-6" />
      </div>
    );
  }
  if (settings.isError) return <Alert className="m-4">{settings.error.message}</Alert>;
  return <SettingsForm key={settings.data.version} saved={settings.data} />;
}

function SettingsForm({ saved }: { saved: View }) {
  const { workspace } = useSession();
  const admin = workspace.role === "owner" || workspace.role === "admin";
  const setup = useSetup();
  const qc = useQueryClient();
  const [regions, setRegions] = useState<Region[]>(saved.regions ?? []);
  const [statuses, setStatuses] = useState<Status[]>(saved.statuses ?? []);
  const [items, setItems] = useState<ItemKind[]>(saved.items ?? []);
  const [search, setSearch] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  const regionOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const r of regions) for (const c of r.countries) m.set(c, r.key);
    return m;
  }, [regions]);
  const q = search.trim().toLowerCase();
  const shown = q ? saved.countries.filter((c) => c.name.toLowerCase().includes(q) || c.code.toLowerCase() === q) : saved.countries;
  const unplaced = saved.countries.filter((c) => !regionOf.has(c.code)).length;

  const changed = () => setDone(false);
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
          body: { regions, statuses, items },
        }),
      );
      qc.setQueryData(["pitching-settings"], after);
      // Region and status choices on organisations come from these.
      void qc.invalidateQueries({ queryKey: ["schema"] });
      setDone(true);
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
                  { key: keyFor("status", xs.map((x) => x.key)), label: "", colour: "blue", meaning: "other" },
                ]);
              }}
            >
              <Plus /> Add a status
            </Button>
          </div>
        )}
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
                setRegions((xs) => [...xs, { key: keyFor(`region ${xs.length + 1}`, xs.map((x) => x.key)), name: "", countries: [] }]);
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
      {done && <Alert tone="success">Saved.</Alert>}
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
