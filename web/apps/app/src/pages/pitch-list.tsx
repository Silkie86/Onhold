// Pitching, the list: organisations to pitch something to, where each
// stands with it, and the "best to pitch first" ranking with every reason
// shown. Organisations resting after an answer, or waiting for one, come
// last and greyed, with why. Worked out on the server when asked.

import type { Schemas } from "@onhold/api-client";
import { Alert, Badge, Button, Card, Checkbox, Input, Label, NativeSelect, Spinner, StatusDot, useDebounced } from "@onhold/ui";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { useState } from "react";
import { api, ok } from "../api.ts";
import { fetchList, useOffline } from "../data.ts";
import { useSetup } from "../setup.ts";
import { usePitchingSettings } from "./pitching.tsx";

type Row = Schemas["PitchListRow"];
type PitchStatus = Schemas["PitchStatus"];
type ItemKind = Schemas["ItemKind"];
type Line = Schemas["RankLine"];
type View = Schemas["PitchingSettingsView"];

const PAGE = 50;

export const PITCH_STATUSES: { value: PitchStatus; label: string }[] = [
  { value: "booked", label: "Booked" },
  { value: "awaiting", label: "Awaiting an answer" },
  { value: "interested", label: "Interested" },
  { value: "tried", label: "Tried" },
  { value: "pitched", label: "Pitched, no answer" },
  { value: "match", label: "Potential match" },
  { value: "not_pitched", label: "Not pitched" },
];
const statusLabel = (s: PitchStatus) => PITCH_STATUSES.find((x) => x.value === s)?.label ?? s;

/** A row's status and region, by their keys in the pitching settings. */
function lookups(settings: View | undefined) {
  return {
    statusOf: (key: string | null | undefined) => settings?.statuses?.find((s) => s.key === key),
    regionName: (key: string | null | undefined) => settings?.regions?.find((r) => r.key === key)?.name,
  };
}

/** What can be pitched: the workspace's talent and, if it pitches them, its projects. */
function usePitchables(settings: View | undefined) {
  const sync = useOffline();
  const kinds = settings?.items ?? ["talent"];
  const talent = useQuery({
    queryKey: ["list", "talent", "pitchable"],
    queryFn: async () => (await fetchList(sync, "talent", { limit: 200 })).data,
    enabled: kinds.includes("talent"),
  });
  const projects = useQuery({
    queryKey: ["projects", "pick"],
    queryFn: () => ok(api.GET("/api/v1/projects", { params: { query: { limit: 200 } } })),
    enabled: kinds.includes("project"),
  });
  return {
    talent: kinds.includes("talent") ? (talent.data?.items ?? []).map((t) => ({ id: String(t.id), name: String(t.name) })) : [],
    projects: kinds.includes("project") ? (projects.data?.items ?? []).map((p) => ({ id: p.id, name: p.name })) : [],
  };
}

export function PitchListPage() {
  const search = useSearch({ from: "/app/pitching" });
  const settings = usePitchingSettings();
  const setup = useSetup();
  const navigate = useNavigate();
  const { talent, projects } = usePitchables(settings.data);
  const [q, setQ] = useState("");
  const [region, setRegion] = useState("");
  const [country, setCountry] = useState("");
  const [status, setStatus] = useState("");
  const [relationship, setRelationship] = useState("");
  const [pitchStatus, setPitchStatus] = useState<PitchStatus | "">("");
  const [hideResting, setHideResting] = useState(false);
  const [byName, setByName] = useState(false);
  const [offset, setOffset] = useState(0);
  const searched = useDebounced(q.trim(), 300);
  const item = search.kind && search.id ? `${search.kind}:${search.id}` : "";
  const query = {
    item_kind: search.kind,
    item_id: search.id,
    q: searched || undefined,
    region: region || undefined,
    country: country || undefined,
    status: status || undefined,
    relationship: relationship || undefined,
    pitch_status: pitchStatus || undefined,
    hide_resting: hideResting || undefined,
    sort: byName ? ("name" as const) : undefined,
    offset,
    limit: PAGE,
  };
  const list = useQuery({
    queryKey: ["pitch-list", query],
    queryFn: () => ok(api.GET("/api/v1/pitching/list", { params: { query } })),
    placeholderData: keepPreviousData,
  });
  // Any change of filter starts again at the top.
  const set =
    <T,>(f: (v: T) => void) =>
    (v: T) => {
      setOffset(0);
      f(v);
    };
  const talentWord = setup.word("crm", "talent", true);
  const { statusOf, regionName } = lookups(settings.data);
  const total = list.data?.total ?? 0;
  // One beyond the first few hundred, picked from its own page, still shows.
  const picked = list.data?.item;
  const withPicked = (items: { id: string; name: string }[], kind: ItemKind) =>
    picked && picked.kind === kind && !items.some((i) => i.id === picked.id) ? [...items, { id: picked.id, name: picked.name }] : items;
  const talentOptions = withPicked(talent, "talent");
  const projectOptions = withPicked(projects, "project");
  const regionCountries = (key: string) => settings.data?.regions?.find((r) => r.key === key)?.countries;

  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 className="text-xl font-semibold">Pitching list</h1>
        <div className="flex gap-1" role="group" aria-label="Order">
          <Button size="sm" variant={byName ? "outline" : "default"} aria-pressed={!byName} onClick={() => set(setByName)(false)}>
            Best first
          </Button>
          <Button size="sm" variant={byName ? "default" : "outline"} aria-pressed={byName} onClick={() => set(setByName)(true)}>
            A to Z
          </Button>
        </div>
      </div>
      <Card className="flex flex-col gap-3 p-4 text-sm">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="pitch-item">What you're pitching</Label>
          <NativeSelect
            id="pitch-item"
            className="max-w-md"
            value={item}
            onChange={(e) => {
              setOffset(0);
              const [kind, id] = e.target.value.split(":");
              void navigate({ to: "/pitching", search: kind && id ? { kind: kind as ItemKind, id } : {} });
            }}
          >
            <option value="">Nothing in particular</option>
            {talentOptions.length > 0 && (
              <optgroup label={talentWord}>
                {talentOptions.map((t) => (
                  <option key={t.id} value={`talent:${t.id}`}>
                    {t.name}
                  </option>
                ))}
              </optgroup>
            )}
            {projectOptions.length > 0 && (
              <optgroup label="Projects">
                {projectOptions.map((p) => (
                  <option key={p.id} value={`project:${p.id}`}>
                    {p.name}
                  </option>
                ))}
              </optgroup>
            )}
          </NativeSelect>
        </div>
        <div className="grid gap-2 sm:grid-cols-3">
          <Input aria-label="Search" placeholder="Search names and tags…" value={q} onChange={(e) => set(setQ)(e.target.value)} />
          <NativeSelect
            aria-label="Region"
            value={region}
            onChange={(e) => {
              const next = e.target.value;
              // A country outside the new region would hide everyone, unseen.
              if (next && country && !regionCountries(next)?.includes(country)) setCountry("");
              set(setRegion)(next);
            }}
          >
            <option value="">Every region</option>
            {settings.data?.regions?.map((r) => (
              <option key={r.key} value={r.key}>
                {r.name}
              </option>
            ))}
          </NativeSelect>
          <NativeSelect aria-label="Country" value={country} onChange={(e) => set(setCountry)(e.target.value)}>
            <option value="">Every country</option>
            {settings.data?.countries
              .filter((c) => !region || regionCountries(region)?.includes(c.code))
              .map((c) => (
                <option key={c.code} value={c.code}>
                  {c.name}
                </option>
              ))}
          </NativeSelect>
          <NativeSelect aria-label="Where things stand" value={pitchStatus} onChange={(e) => set(setPitchStatus)(e.target.value as PitchStatus | "")}>
            <option value="">However it stands</option>
            {PITCH_STATUSES.map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </NativeSelect>
          <NativeSelect aria-label="Status" value={status} onChange={(e) => set(setStatus)(e.target.value)}>
            <option value="">Any status</option>
            {settings.data?.statuses?.map((s) => (
              <option key={s.key} value={s.key}>
                {s.label}
              </option>
            ))}
          </NativeSelect>
          <NativeSelect aria-label="Client or prospect" value={relationship} onChange={(e) => set(setRelationship)(e.target.value)}>
            <option value="">Clients and prospects</option>
            <option value="client">Clients</option>
            <option value="prospect">Prospects</option>
          </NativeSelect>
        </div>
        <label className="flex items-center gap-2">
          <Checkbox checked={hideResting} onChange={(e) => set(setHideResting)(e.target.checked)} />
          Hide those resting or waiting for an answer
        </label>
      </Card>

      {list.isPending ? (
        <Spinner />
      ) : list.isError ? (
        <Alert>{list.error.message}</Alert>
      ) : list.data.items.length === 0 ? (
        <Card className="p-5 text-sm text-muted-foreground">
          Nobody matches. Organisations marked do not contact are never listed.
        </Card>
      ) : (
        <>
          {list.data.capped && (
            <Alert>There are more organisations than can be ranked at once. Narrow the list with a region, country or search to see them all.</Alert>
          )}
          <p className="text-sm text-muted-foreground" aria-live="polite">
            {total === 1 ? "1 organisation" : `${total} organisations`}
            {list.data.item ? ` for ${list.data.item.name}` : ""}
            {byName ? ", A to Z." : ", best to pitch first."}
          </p>
          <ul className="flex flex-col divide-y rounded-md border bg-card text-sm">
            {list.data.items.map((r) => (
              <PitchRow key={r.organisation_id} row={r} status={statusOf(r.status)} regionName={regionName(r.region)} />
            ))}
          </ul>
          {total > PAGE && (
            <div className="flex items-center justify-between gap-2 text-sm">
              <Button variant="outline" size="sm" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - PAGE))}>
                Previous
              </Button>
              <span className="text-muted-foreground">
                {offset + 1} to {Math.min(offset + PAGE, total)} of {total}
              </span>
              <Button variant="outline" size="sm" disabled={offset + PAGE >= total} onClick={() => setOffset(offset + PAGE)}>
                Next
              </Button>
            </div>
          )}
        </>
      )}
    </div>
  );
}

function points(l: Line) {
  return l.points > 0 ? `+${l.points}` : String(l.points);
}

function PitchRow({ row: r, status, regionName }: { row: Row; status: Schemas["Status"] | undefined; regionName: string | undefined }) {
  const where = [r.city, r.country_code, regionName].filter(Boolean).join(" · ");
  const name = (
    <Link to="/crm/$entity/$id" params={{ entity: "organisation", id: r.organisation_id }} className="font-medium hover:underline">
      {r.name}
    </Link>
  );
  return (
    <li className={`flex flex-col gap-1.5 px-3 py-2.5 ${r.resting ? "opacity-60" : ""}`}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="flex min-w-0 flex-wrap items-center gap-2">
          {status ? <StatusDot colour={status.colour} label={name} /> : name}
          <Badge>{statusLabel(r.pitch_status)}</Badge>
          {r.relationship === "client" && <Badge>Client</Badge>}
        </span>
        <span className="tabular-nums text-muted-foreground" title="Points from the reasons below">
          {r.score} {Math.abs(r.score) === 1 ? "point" : "points"}
        </span>
      </div>
      {where && <span className="text-muted-foreground">{where}</span>}
      {r.rest_reason && <span>{r.rest_reason}</span>}
      {(r.reasons.length > 0 || r.but) && (
        <ul className="flex flex-wrap gap-1.5" aria-label="Why">
          {r.reasons.map((l) => (
            <li key={l.label} className="rounded-full border px-2 py-0.5">
              {l.label} <span className="text-muted-foreground">{points(l)}</span>
            </li>
          ))}
          {r.but && (
            <li className="rounded-full border border-dashed px-2 py-0.5">
              But: {r.but.label} <span className="text-muted-foreground">{points(r.but)}</span>
            </li>
          )}
        </ul>
      )}
    </li>
  );
}

/** The top few to pitch this talent to, from the same ranking. */
export function WhoToPitchCard({ talentId }: { talentId: string }) {
  const list = useQuery({
    queryKey: ["pitch-list", "who", talentId],
    queryFn: () =>
      ok(
        api.GET("/api/v1/pitching/list", {
          params: { query: { item_kind: "talent", item_id: talentId, hide_resting: true, limit: 5 } },
        }),
      ),
    // Ranking means scoring every organisation; pitches logged here refresh it
    // themselves, so it is not run again on every visit to the talent.
    staleTime: 5 * 60_000,
    refetchOnWindowFocus: false,
  });
  const settings = usePitchingSettings();
  const { statusOf, regionName } = lookups(settings.data);
  if (list.isError) return null;
  return (
    <section className="flex flex-col gap-3" aria-label="Who to pitch">
      <div className="flex items-center justify-between gap-2">
        <h2 className="font-semibold">Who to pitch</h2>
        <Link to="/pitching" search={{ kind: "talent", id: talentId }} className="text-sm underline">
          See the whole list
        </Link>
      </div>
      <p className="text-sm text-muted-foreground">The best bets first, and why, leaving out those resting after an answer.</p>
      {list.isPending ? (
        <Spinner />
      ) : list.data.items.length === 0 ? (
        <p className="text-sm text-muted-foreground">Nobody to pitch to right now.</p>
      ) : (
        <ul className="flex flex-col divide-y rounded-md border bg-card text-sm">
          {list.data.items.map((r) => (
            <PitchRow key={r.organisation_id} row={r} status={statusOf(r.status)} regionName={regionName(r.region)} />
          ))}
        </ul>
      )}
    </section>
  );
}
