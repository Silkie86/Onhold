// A talent's marked time: holidays and other unavailable dates block
// confirming bookings; tentative time only warns. Busy times read from the
// talent's own calendars are listed too, and change only there.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { Schemas } from "@onhold/api-client";
import {
  Alert,
  Button,
  Checkbox,
  Input,
  Label,
  NativeSelect,
  Spinner,
} from "@onhold/ui";
import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { api, ok } from "../api.ts";
import {
  addDays,
  dayKey,
  formatWhen,
  fromLocalInput,
  startOfDay,
} from "../work.ts";

type Kind = Schemas["AvailabilityKind"];

const KIND_LABEL: Record<Kind, string> = {
  unavailable: "Unavailable",
  tentative: "Tentative",
  available_only: "Available only",
};

export function AvailabilityEditor({
  talentId,
  canWrite,
}: {
  talentId: string;
  canWrite: boolean;
}) {
  const qc = useQueryClient();
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const from = startOfDay(new Date()).toISOString();
  const list = useQuery({
    queryKey: ["availability", talentId],
    queryFn: () =>
      ok(
        api.GET("/api/v1/talent/{id}/availability", {
          params: { path: { id: talentId }, query: { from } },
        }),
      ),
  });
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["availability", talentId] });
    void qc.invalidateQueries({ queryKey: ["calendar"] });
  };
  return (
    <section className="flex flex-col gap-3" aria-label="Availability">
      <div className="flex items-center justify-between">
        <h2 className="font-semibold">Availability</h2>
        {canWrite && !adding && (
          <Button variant="outline" size="sm" onClick={() => setAdding(true)}>
            <Plus /> Mark dates
          </Button>
        )}
      </div>
      {adding && (
        <AvailabilityForm
          talentId={talentId}
          onDone={() => {
            setAdding(false);
            refresh();
          }}
          onCancel={() => setAdding(false)}
        />
      )}
      {error && <Alert>{error}</Alert>}
      {list.isPending ? (
        <Spinner />
      ) : (list.data?.length ?? 0) === 0 ? (
        <p className="text-sm text-muted-foreground">
          No dates marked from today on.
        </p>
      ) : (
        <ul className="divide-y rounded-md border">
          {list.data?.map((a) => (
            <li
              key={a.id}
              className="flex items-center gap-2 px-3 py-2 text-sm"
            >
              <span className="flex min-w-0 flex-1 flex-col">
                <span>
                  <span className="font-medium">{KIND_LABEL[a.kind]}</span> ·{" "}
                  {formatWhen(a.starts_at, a.ends_at, a.all_day)}
                </span>
                {a.note && (
                  <span className="text-muted-foreground">{a.note}</span>
                )}
              </span>
              {/* Busy time from their own calendar changes there. */}
              {canWrite && a.source !== "external" && (
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={`Remove ${KIND_LABEL[a.kind].toLowerCase()} ${formatWhen(a.starts_at, a.ends_at, a.all_day)}`}
                  onClick={async () => {
                    setError(null);
                    try {
                      await ok(
                        api.DELETE("/api/v1/availability/{id}", {
                          params: { path: { id: a.id } },
                        }),
                      );
                      refresh();
                    } catch (e) {
                      setError(e instanceof Error ? e.message : String(e));
                    }
                  }}
                >
                  <Trash2 />
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function AvailabilityForm({
  talentId,
  onDone,
  onCancel,
}: {
  talentId: string;
  onDone: () => void;
  onCancel: () => void;
}) {
  const today = dayKey(new Date());
  const [kind, setKind] = useState<Kind>("unavailable");
  const [allDay, setAllDay] = useState(true);
  const [start, setStart] = useState(today);
  const [end, setEnd] = useState(today);
  const [startTime, setStartTime] = useState(`${today}T09:00`);
  const [endTime, setEndTime] = useState(`${today}T17:00`);
  const [note, setNote] = useState("");
  const [error, setError] = useState<string | null>(null);
  return (
    <form
      className="flex flex-col gap-3 rounded-md border p-3"
      onSubmit={async (e) => {
        e.preventDefault();
        setError(null);
        // Whole days run from the first day's start to the start of the day after the last.
        const startsAt = allDay
          ? new Date(`${start}T00:00`).toISOString()
          : fromLocalInput(startTime);
        const endsAt = allDay
          ? addDays(new Date(`${end}T00:00`), 1).toISOString()
          : fromLocalInput(endTime);
        if (!startsAt || !endsAt) {
          setError("Choose when it starts and ends.");
          return;
        }
        try {
          await ok(
            api.POST("/api/v1/talent/{id}/availability", {
              params: { path: { id: talentId } },
              body: {
                kind,
                starts_at: startsAt,
                ends_at: endsAt,
                all_day: allDay,
                note: note || null,
              },
            }),
          );
          onDone();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      }}
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="availability-kind">Mark as</Label>
          <NativeSelect
            id="availability-kind"
            value={kind}
            onChange={(e) => setKind(e.target.value as Kind)}
          >
            <option value="unavailable">Unavailable (blocks bookings)</option>
            <option value="tentative">Tentative (warns only)</option>
            <option value="available_only">Available only</option>
          </NativeSelect>
        </div>
        <label className="flex items-center gap-2 self-end pb-2 text-sm">
          <Checkbox
            checked={allDay}
            onChange={(e) => setAllDay(e.target.checked)}
          />{" "}
          Whole days
        </label>
        {allDay ? (
          <>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="availability-from">First day</Label>
              <Input
                id="availability-from"
                type="date"
                required
                value={start}
                onChange={(e) => setStart(e.target.value)}
              />
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="availability-to">Last day</Label>
              <Input
                id="availability-to"
                type="date"
                required
                min={start}
                value={end}
                onChange={(e) => setEnd(e.target.value)}
              />
            </div>
          </>
        ) : (
          <>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="availability-from">From</Label>
              <Input
                id="availability-from"
                type="datetime-local"
                required
                value={startTime}
                onChange={(e) => setStartTime(e.target.value)}
              />
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="availability-to">Until</Label>
              <Input
                id="availability-to"
                type="datetime-local"
                required
                value={endTime}
                onChange={(e) => setEndTime(e.target.value)}
              />
            </div>
          </>
        )}
      </div>
      <div className="flex flex-col gap-1.5">
        <Label htmlFor="availability-note">Note</Label>
        <Input
          id="availability-note"
          placeholder="Holiday, recording, …"
          value={note}
          onChange={(e) => setNote(e.target.value)}
        />
      </div>
      {error && <Alert>{error}</Alert>}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit">Save</Button>
      </div>
    </form>
  );
}
