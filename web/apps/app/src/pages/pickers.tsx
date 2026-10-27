// Linking a record to others by searching for them: one contact, venue or
// organisation, or several talent.

import { useQuery } from "@tanstack/react-query";
import { Input, type RelationRenderer, Spinner } from "@onhold/ui";
import { X } from "lucide-react";
import { useState } from "react";
import { fetchList, useOffline } from "../data.ts";
import type { EntityName } from "../offline/store.ts";
import type { Named } from "../work.ts";

const PLURAL: Record<EntityName, string> = { contact: "contacts", organisation: "organisations", talent: "talent" };

/** A search box listing matching records; picking one calls `onPick`. */
export function RecordSearch({
  id,
  entity,
  onPick,
  exclude = [],
  placeholder,
}: {
  id?: string;
  entity: EntityName;
  onPick: (picked: Named) => void;
  exclude?: string[];
  placeholder?: string;
}) {
  const sync = useOffline();
  const [q, setQ] = useState("");
  const results = useQuery({
    queryKey: ["pick", entity, q],
    queryFn: async () => (await fetchList(sync, entity, { q, limit: 8 })).data,
    enabled: q.trim().length > 0,
  });
  const items = (results.data?.items ?? []).filter((r) => !exclude.includes(r.id));
  return (
    <div className="relative">
      <Input
        id={id}
        placeholder={placeholder ?? `Search ${PLURAL[entity]}…`}
        value={q}
        onChange={(e) => setQ(e.target.value)}
        autoComplete="off"
      />
      {q.trim() && (
        <ul className="absolute z-20 mt-1 max-h-56 w-full overflow-y-auto rounded-md border bg-card text-sm shadow-md">
          {results.isFetching && !results.data && (
            <li className="p-2">
              <Spinner />
            </li>
          )}
          {items.map((r) => (
            <li key={r.id}>
              <button
                type="button"
                className="w-full px-3 py-2 text-left hover:bg-accent"
                onClick={() => {
                  setQ("");
                  onPick({ id: r.id, name: String(r.name ?? "") });
                }}
              >
                {String(r.name ?? "")}
              </button>
            </li>
          ))}
          {results.data && items.length === 0 && <li className="p-2 text-muted-foreground">Nothing matches.</li>}
        </ul>
      )}
    </div>
  );
}

/** One linked record, or a search to choose one. */
export function RecordPicker({
  id,
  entity,
  value,
  onChange,
}: {
  id?: string;
  entity: EntityName;
  value: Named | null;
  onChange: (picked: Named | null) => void;
}) {
  if (value) {
    return (
      <div className="flex h-9 items-center justify-between rounded-md border px-3 text-sm">
        <span id={id}>{value.name}</span>
        <button type="button" aria-label={`Remove ${value.name}`} onClick={() => onChange(null)}>
          <X className="size-4" />
        </button>
      </div>
    );
  }
  return <RecordSearch id={id} entity={entity} onPick={onChange} />;
}

/** The relation picker for forms: the field says which kind of record it links to. */
export const RelationPicker: RelationRenderer = ({ field, id, value, onChange, record }) => (
  <RelationField
    id={id}
    entity={(field.relation_entity ?? "organisation") as EntityName}
    value={value}
    initialName={(record?.[field.key.replace(/_id$/, "_name")] as string | undefined) ?? null}
    onChange={onChange}
  />
);

function RelationField({
  id,
  entity,
  value,
  initialName,
  onChange,
}: {
  id: string;
  entity: EntityName;
  value: string | null;
  initialName: string | null;
  onChange: (id: string | null) => void;
}) {
  const [name, setName] = useState(initialName);
  return (
    <RecordPicker
      id={id}
      entity={entity}
      value={value ? { id: value, name: name ?? "…" } : null}
      onChange={(p) => {
        setName(p?.name ?? null);
        onChange(p?.id ?? null);
      }}
    />
  );
}

/** Several talent: chips to remove, and a search to add more. */
export function TalentPicker({
  id,
  value,
  onChange,
  disabled,
}: {
  id?: string;
  value: Named[];
  onChange: (talent: Named[]) => void;
  disabled?: boolean;
}) {
  return (
    <div className="flex flex-col gap-2">
      {value.length > 0 && (
        <ul className="flex flex-wrap gap-1.5">
          {value.map((t) => (
            <li key={t.id} className="flex items-center gap-1 rounded-full border bg-secondary px-2.5 py-0.5 text-sm">
              {t.name}
              {!disabled && (
                <button
                  type="button"
                  aria-label={`Remove ${t.name}`}
                  onClick={() => onChange(value.filter((v) => v.id !== t.id))}
                >
                  <X className="size-3.5" />
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      {!disabled && (
        <RecordSearch
          id={id}
          entity="talent"
          placeholder="Add talent…"
          exclude={value.map((v) => v.id)}
          onPick={(t) => onChange([...value, t])}
        />
      )}
    </div>
  );
}
