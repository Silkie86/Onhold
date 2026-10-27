// A record's history: who created and changed it, field by field, and the
// messages sent to it, newest first.

import { Mail, Pencil, Plus, Trash2 } from "lucide-react";
import type { Schemas } from "@onhold/api-client";
import { type EntitySchema, formatValue } from "../format.ts";
import { Button } from "./button.tsx";
import { EmptyState, Spinner } from "./misc.tsx";

export type TimelineEntry = Schemas["TimelineEntry"];

const VERBS: Record<string, string> = { created: "created", updated: "changed", deleted: "deleted" };

/** "3 minutes ago", "yesterday", or a date for older entries. */
export function relativeTime(iso: string, now = Date.now(), locale?: string): string {
  const seconds = Math.round((new Date(iso).getTime() - now) / 1000);
  const rtf = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
  const abs = Math.abs(seconds);
  if (abs < 60) return rtf.format(seconds, "second");
  if (abs < 3600) return rtf.format(Math.round(seconds / 60), "minute");
  if (abs < 86400) return rtf.format(Math.round(seconds / 3600), "hour");
  if (abs < 7 * 86400) return rtf.format(Math.round(seconds / 86400), "day");
  return new Date(iso).toLocaleDateString(locale, { dateStyle: "medium" });
}

function describe(entry: TimelineEntry, schema: EntitySchema | undefined): string {
  if (entry.message) {
    const subject = entry.message.subject ? `“${entry.message.subject}”` : "A message";
    return `${subject} ${entry.message.status === "sent" ? "sent" : "failed"} to ${entry.message.to}`;
  }
  const [, verb = "changed"] = entry.action.split(".");
  const word = VERBS[verb] ?? verb;
  const about = entry.object_name ?? (schema?.label ?? entry.resource).toLowerCase();
  const what = entry.resource.replace(/_/g, " ");
  return entry.object_name ? `${what[0]?.toUpperCase()}${what.slice(1)} ${about} ${word}` : `${schema?.label ?? what} ${word}`;
}

export function Timeline({
  entries,
  schema,
  loading,
  hasMore,
  onMore,
  locale,
}: {
  entries: TimelineEntry[];
  schema?: EntitySchema;
  loading?: boolean;
  hasMore?: boolean;
  onMore?: () => void;
  locale?: string;
}) {
  const fields = new Map((schema?.fields ?? []).map((f) => [f.key, f]));
  if (!loading && entries.length === 0) return <EmptyState title="Nothing has happened yet" />;
  return (
    <div className="flex flex-col gap-1">
      <ol className="flex flex-col">
        {entries.map((e) => {
          const verb = e.action.split(".")[1];
          const Icon = e.message ? Mail : verb === "created" ? Plus : verb === "deleted" ? Trash2 : Pencil;
          return (
            <li key={e.id} className="flex gap-3 border-l py-2 pl-4 relative">
              <span className="absolute -left-2.5 top-2.5 flex size-5 items-center justify-center rounded-full border bg-background">
                <Icon className="size-3" />
              </span>
              <div className="flex min-w-0 flex-col gap-1">
                <p className="text-sm">
                  <span className="font-medium">{describe(e, schema)}</span>
                  {e.actor?.name && <span className="text-muted-foreground"> by {e.actor.name}</span>}
                  <span className="text-muted-foreground">
                    {" · "}
                    <time dateTime={e.at} title={new Date(e.at).toLocaleString(locale)}>
                      {relativeTime(e.at, Date.now(), locale)}
                    </time>
                  </span>
                </p>
                {e.changes.length > 0 && (
                  <ul className="text-xs text-muted-foreground">
                    {e.changes.map((c) => {
                      const f = fields.get(c.field);
                      const before = formatValue(f, c.before, locale) || "empty";
                      const after = formatValue(f, c.after, locale) || "empty";
                      return (
                        <li key={c.field} className="break-words">
                          {f?.label ?? c.field}: <s>{before}</s> → {after}
                        </li>
                      );
                    })}
                  </ul>
                )}
              </div>
            </li>
          );
        })}
      </ol>
      {loading && <Spinner className="m-2" />}
      {hasMore && !loading && (
        <Button variant="ghost" size="sm" className="self-start" onClick={onMore}>
          Show older
        </Button>
      )}
    </div>
  );
}
