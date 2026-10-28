// Travel on a booking (flights, trains, rides, hotels), and the money check
// that says whether the booking pays once travel, buyouts, costs and
// commission are counted.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { Schemas } from "@onhold/api-client";
import { Alert, Badge, Button, Dialog, Input, Label, MoneyInput, NativeSelect, Spinner, Textarea, cn, formatMoney } from "@onhold/ui";
import { AlertTriangle, BedDouble, Bus, Car, Pencil, Plane, Plus, Ship, Train, Trash2 } from "lucide-react";
import { type ReactNode, useState } from "react";
import { api, ok } from "../api.ts";
import { fetchSaved, useOffline, useSyncStatus } from "../data.ts";
import { isLocalId } from "../offline/store.ts";
import { type Booking, fromZonedInput, timeZones, toZonedInput, zonedTime } from "../work.ts";
import { failureIssues, useWorkspaceCurrency } from "./crm.tsx";

type Travel = Schemas["TravelView"];
type TravelInput = Schemas["TravelInput"];
type MoneyCheck = Schemas["MoneyCheck"];
type MoneyLine = Schemas["MoneyLineView"];
type Money = Schemas["Money"];

export const TRAVEL_KINDS: Record<string, string> = {
  flight: "Flight",
  train: "Train",
  bus: "Bus",
  car: "Car",
  ferry: "Ferry",
  taxi: "Taxi or transfer",
  hotel: "Hotel",
  other: "Other",
};

const ICONS: Record<string, ReactNode> = {
  flight: <Plane className="size-4" />,
  train: <Train className="size-4" />,
  bus: <Bus className="size-4" />,
  car: <Car className="size-4" />,
  taxi: <Car className="size-4" />,
  ferry: <Ship className="size-4" />,
  hotel: <BedDouble className="size-4" />,
};

export const COST_LABELS: Record<string, string> = {
  travel: "Travel",
  accommodation: "Accommodation",
  ground: "Ground transport",
  per_diem: "Per diems",
  crew: "Crew",
  production: "Production",
  visa: "Visas and permits",
  marketing: "Marketing",
  other: "Other",
};

export const INCOME_LABELS: Record<string, string> = {
  buyout: "Buyout",
  merchandise: "Merchandise",
  other: "Other income",
};

function failure(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function useOnline(): boolean {
  return useSyncStatus()?.online !== false;
}

/** "LHR London Heathrow" from a code and a place, either optional. */
function place(code?: string | null, name?: string | null): string {
  return [code, name].filter(Boolean).join(" ");
}

/** A travel item's heading: "Flight · British Airways BA 1442". */
export function travelTitle(t: Travel): string {
  const what = [t.carrier, t.kind === "hotel" ? null : t.number].filter(Boolean).join(" ");
  return [TRAVEL_KINDS[t.kind] ?? t.kind, what].filter(Boolean).join(" · ");
}

/** One travel item, readable on a phone: times on each end in that place's clock. */
export function TravelLine({ item: t, actions }: { item: Travel; actions?: ReactNode }) {
  const cancelled = t.status === "cancelled";
  return (
    <li className={cn("flex gap-3 rounded-md border p-3", cancelled && "opacity-60")}>
      <span className="mt-0.5 text-muted-foreground" aria-hidden>
        {ICONS[t.kind] ?? <Plane className="size-4" />}
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-1 text-sm">
        <div className="flex flex-wrap items-center gap-2">
          <span className={cn("font-medium", cancelled && "line-through")}>{travelTitle(t)}</span>
          {t.status === "planned" && <Badge>To book</Badge>}
          {t.status === "cancelled" && <Badge>Cancelled</Badge>}
          {t.paid_by === "buyer" && <Badge>Provided</Badge>}
        </div>
        {t.kind === "hotel" ? (
          <div>
            {t.to_place && <div className="text-muted-foreground">{t.to_place}</div>}
            <div>
              In {zonedTime(t.starts_at, t.start_tz, true)}
              {t.ends_at && t.end_tz && <> · out {zonedTime(t.ends_at, t.end_tz, true)}</>}
            </div>
          </div>
        ) : (
          <div className="grid grid-cols-[auto_1fr] gap-x-3">
            <span className="tabular-nums">{zonedTime(t.starts_at, t.start_tz, true)}</span>
            <span className="truncate">{place(t.from_code, t.from_place) || "Departure"}</span>
            {t.ends_at && t.end_tz && (
              <>
                <span className="tabular-nums">{zonedTime(t.ends_at, t.end_tz, true)}</span>
                <span className="truncate">{place(t.to_code, t.to_place) || "Arrival"}</span>
              </>
            )}
          </div>
        )}
        <div className="flex flex-wrap gap-x-3 text-muted-foreground">
          {t.reference && <span>Ref {t.reference}</span>}
          {t.travellers.length > 0 && <span>{t.travellers.join(", ")}</span>}
          {t.cost && <span>{formatMoney(t.cost)}</span>}
        </div>
        {t.notes && <p className="whitespace-pre-wrap text-muted-foreground">{t.notes}</p>}
      </div>
      {actions}
    </li>
  );
}

/** A booking's travel, with adding and changing on the spot. */
export function BookingTravelPanel({ booking }: { booking: Booking }) {
  const sync = useOffline();
  const online = useOnline();
  const qc = useQueryClient();
  const local = isLocalId(booking.id);
  const [editing, setEditing] = useState<Travel | "new" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const travel = useQuery({
    queryKey: ["travel", booking.id],
    enabled: !local,
    queryFn: async () =>
      (
        await fetchSaved(sync, `travel:${booking.id}`, () =>
          ok(api.GET("/api/v1/bookings/{id}/travel", { params: { path: { id: booking.id } } })),
        )
      ).data,
  });
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["travel", booking.id] });
    void qc.invalidateQueries({ queryKey: ["money", booking.id] });
  };
  const canChange = online && !local;
  const items = travel.data ?? [];
  return (
    <section className="flex flex-col gap-3" aria-label="Travel">
      <div className="flex items-center gap-2">
        <h2 className="flex-1 font-semibold">Travel</h2>
        {canChange && (
          <Button size="sm" variant="outline" onClick={() => setEditing("new")}>
            <Plus /> Add travel
          </Button>
        )}
      </div>
      {error && <Alert>{error}</Alert>}
      {travel.isPending && !local ? (
        <Spinner />
      ) : items.length === 0 ? (
        <p className="text-sm text-muted-foreground">No flights, trains or hotels yet.</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {items.map((t) => (
            <TravelLine
              key={t.id}
              item={t}
              actions={
                canChange && (
                  <div className="flex flex-col gap-1">
                    <Button size="icon" variant="ghost" aria-label={`Change ${travelTitle(t)}`} onClick={() => setEditing(t)}>
                      <Pencil />
                    </Button>
                    <Button
                      size="icon"
                      variant="ghost"
                      aria-label={`Remove ${travelTitle(t)}`}
                      onClick={async () => {
                        if (!window.confirm(`Remove ${travelTitle(t)}?`)) return;
                        setError(null);
                        try {
                          await ok(api.DELETE("/api/v1/travel/{id}", { params: { path: { id: t.id } } }));
                          refresh();
                        } catch (e) {
                          setError(failure(e));
                        }
                      }}
                    >
                      <Trash2 />
                    </Button>
                  </div>
                )
              }
            />
          ))}
        </ul>
      )}
      {editing && (
        <TravelDialog
          booking={booking}
          item={editing === "new" ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            refresh();
          }}
        />
      )}
    </section>
  );
}

function TravelDialog({
  booking,
  item,
  onClose,
  onSaved,
}: {
  booking: Booking;
  item: Travel | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const currency = useWorkspaceCurrency();
  const bookingZone = booking.timezone;
  const [kind, setKind] = useState(item?.kind ?? "flight");
  const [status, setStatus] = useState(item?.status ?? "planned");
  const [carrier, setCarrier] = useState(item?.carrier ?? "");
  const [number, setNumber] = useState(item?.number ?? "");
  const [reference, setReference] = useState(item?.reference ?? "");
  const [fromCode, setFromCode] = useState(item?.from_code ?? "");
  const [fromPlace, setFromPlace] = useState(item?.from_place ?? "");
  const [toCode, setToCode] = useState(item?.to_code ?? "");
  const [toPlace, setToPlace] = useState(item?.to_place ?? "");
  const [startTz, setStartTz] = useState(item?.start_tz ?? bookingZone);
  const [endTz, setEndTz] = useState(item?.end_tz ?? item?.start_tz ?? bookingZone);
  const [start, setStart] = useState(toZonedInput(item?.starts_at ?? booking.starts_at, item?.start_tz ?? bookingZone));
  const [end, setEnd] = useState(item?.ends_at ? toZonedInput(item.ends_at, item.end_tz ?? bookingZone) : "");
  const [travellers, setTravellers] = useState((item?.travellers ?? []).join(", "));
  const [notes, setNotes] = useState(item?.notes ?? "");
  const [cost, setCost] = useState<Money | null>(item?.cost ?? null);
  const [paidBy, setPaidBy] = useState(item?.paid_by ?? "us");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const hotel = kind === "hotel";
  const moving = !hotel;

  const save = async () => {
    setError(null);
    const startsAt = fromZonedInput(start, startTz);
    if (!startsAt) {
      setError(hotel ? "Give the check-in time." : "Give the departure time.");
      return;
    }
    const body: TravelInput = {
      kind,
      status,
      carrier: carrier || null,
      number: moving ? number || null : null,
      reference: reference || null,
      from_code: moving ? fromCode || null : null,
      from_place: moving ? fromPlace || null : null,
      to_code: moving ? toCode || null : null,
      to_place: toPlace || null,
      starts_at: startsAt,
      start_tz: startTz,
      ends_at: end ? fromZonedInput(end, endTz) : null,
      end_tz: end ? endTz : null,
      travellers: travellers
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean),
      notes: notes || null,
      cost,
      paid_by: paidBy,
    };
    setBusy(true);
    try {
      if (item) {
        await ok(
          api.PUT("/api/v1/travel/{id}", {
            params: { path: { id: item.id } },
            headers: { "If-Match": `"${item.version}"` },
            body,
          }),
        );
      } else {
        await ok(api.POST("/api/v1/bookings/{id}/travel", { params: { path: { id: booking.id } }, body }));
      }
      onSaved();
    } catch (e) {
      const issues = failureIssues(e);
      setError(issues ? issues.map((i) => i.message && `${i.field}: ${i.message}`).join(". ") : failure(e));
    } finally {
      setBusy(false);
    }
  };

  const zones = (
    <datalist id="travel-zones">
      {timeZones().map((z) => (
        <option key={z} value={z} />
      ))}
    </datalist>
  );
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={item ? "Change travel" : "Add travel"} className="max-w-xl">
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        {zones}
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="What" id="travel-kind">
            <NativeSelect id="travel-kind" value={kind} onChange={(e) => setKind(e.target.value)}>
              {Object.entries(TRAVEL_KINDS).map(([k, label]) => (
                <option key={k} value={k}>
                  {label}
                </option>
              ))}
            </NativeSelect>
          </Field>
          <Field label="Status" id="travel-status">
            <NativeSelect id="travel-status" value={status} onChange={(e) => setStatus(e.target.value)}>
              <option value="planned">To book</option>
              <option value="booked">Booked</option>
              <option value="cancelled">Cancelled</option>
            </NativeSelect>
          </Field>
          <Field label={hotel ? "Hotel" : "Airline or company"} id="travel-carrier">
            <Input id="travel-carrier" value={carrier} onChange={(e) => setCarrier(e.target.value)} />
          </Field>
          {moving ? (
            <Field label={kind === "flight" ? "Flight number" : "Number"} id="travel-number">
              <Input id="travel-number" value={number} placeholder={kind === "flight" ? "BA 1442" : ""} onChange={(e) => setNumber(e.target.value)} />
            </Field>
          ) : (
            <Field label="Address" id="travel-address">
              <Input id="travel-address" value={toPlace} onChange={(e) => setToPlace(e.target.value)} />
            </Field>
          )}
        </div>
        {moving && (
          <div className="grid gap-3 sm:grid-cols-2">
            <fieldset className="flex flex-col gap-2">
              <legend className="mb-1 text-sm font-medium">From</legend>
              {kind === "flight" && (
                <Input aria-label="Departure airport code" placeholder="Airport code, e.g. LHR" maxLength={3} value={fromCode} onChange={(e) => setFromCode(e.target.value.toUpperCase())} />
              )}
              <Input aria-label="Departure place" placeholder={kind === "flight" ? "Airport and terminal" : "Station or address"} value={fromPlace} onChange={(e) => setFromPlace(e.target.value)} />
              <Input aria-label="Departure time" type="datetime-local" value={start} onChange={(e) => setStart(e.target.value)} />
              <Input aria-label="Departure time zone" list="travel-zones" value={startTz} onChange={(e) => setStartTz(e.target.value)} />
            </fieldset>
            <fieldset className="flex flex-col gap-2">
              <legend className="mb-1 text-sm font-medium">To</legend>
              {kind === "flight" && (
                <Input aria-label="Arrival airport code" placeholder="Airport code, e.g. BCN" maxLength={3} value={toCode} onChange={(e) => setToCode(e.target.value.toUpperCase())} />
              )}
              <Input aria-label="Arrival place" placeholder={kind === "flight" ? "Airport and terminal" : "Station or address"} value={toPlace} onChange={(e) => setToPlace(e.target.value)} />
              <Input aria-label="Arrival time" type="datetime-local" value={end} onChange={(e) => setEnd(e.target.value)} />
              <Input aria-label="Arrival time zone" list="travel-zones" value={endTz} onChange={(e) => setEndTz(e.target.value)} />
            </fieldset>
          </div>
        )}
        {hotel && (
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Check-in" id="travel-in">
              <Input id="travel-in" type="datetime-local" value={start} onChange={(e) => setStart(e.target.value)} />
            </Field>
            <Field label="Check-out" id="travel-out">
              <Input id="travel-out" type="datetime-local" value={end} onChange={(e) => setEnd(e.target.value)} />
            </Field>
            <Field label="Time zone" id="travel-tz">
              <Input
                id="travel-tz"
                list="travel-zones"
                value={startTz}
                onChange={(e) => {
                  setStartTz(e.target.value);
                  setEndTz(e.target.value);
                }}
              />
            </Field>
          </div>
        )}
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="Booking reference" id="travel-ref">
            <Input id="travel-ref" value={reference} onChange={(e) => setReference(e.target.value)} />
          </Field>
          <Field label={hotel ? "Guests" : "Travellers"} id="travel-people">
            <Input id="travel-people" value={travellers} placeholder="Names, separated by commas" onChange={(e) => setTravellers(e.target.value)} />
          </Field>
          <Field label="Cost" id="travel-cost">
            <MoneyInput id="travel-cost" value={cost} defaultCurrency={currency} onChange={setCost} />
          </Field>
          <Field label="Who pays" id="travel-paid">
            <NativeSelect id="travel-paid" value={paidBy} onChange={(e) => setPaidBy(e.target.value)}>
              <option value="us">We pay (a cost against the fee)</option>
              <option value="buyer">The client provides it</option>
            </NativeSelect>
          </Field>
        </div>
        <Field label="Notes" id="travel-notes">
          <Textarea id="travel-notes" rows={2} value={notes} placeholder="Seats, baggage, pick-up point" onChange={(e) => setNotes(e.target.value)} />
        </Field>
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy}>
            {busy && <Spinner />} Save
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

function Field({ label, id, children }: { label: string; id: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      {children}
    </div>
  );
}

// The money check ------------------------------------------------------------------------------

/** Whether a booking pays: fee and buyouts less commission and costs, with warnings. */
export function BookingMoneyPanel({ booking }: { booking: Booking }) {
  const sync = useOffline();
  const online = useOnline();
  const qc = useQueryClient();
  const local = isLocalId(booking.id);
  const [adding, setAdding] = useState<"income" | "cost" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const check = useQuery({
    queryKey: ["money", booking.id, booking.version],
    enabled: !local,
    queryFn: async () =>
      (
        await fetchSaved(sync, `money:${booking.id}`, () =>
          ok(api.GET("/api/v1/bookings/{id}/money", { params: { path: { id: booking.id } } })),
        )
      ).data,
  });
  const refresh = () => void qc.invalidateQueries({ queryKey: ["money", booking.id] });
  const canChange = online && !local;
  const m = check.data;
  const remove = async (l: MoneyLine) => {
    if (!window.confirm(`Remove ${l.description}?`)) return;
    setError(null);
    try {
      await ok(api.DELETE("/api/v1/money-lines/{id}", { params: { path: { id: l.id } } }));
      refresh();
    } catch (e) {
      setError(failure(e));
    }
  };
  return (
    <section className="flex flex-col gap-3" aria-label="Does it pay">
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="flex-1 font-semibold">Does it pay?</h2>
        {canChange && (
          <>
            <Button size="sm" variant="outline" onClick={() => setAdding("income")}>
              <Plus /> Buyout or income
            </Button>
            <Button size="sm" variant="outline" onClick={() => setAdding("cost")}>
              <Plus /> Cost
            </Button>
          </>
        )}
      </div>
      {error && <Alert>{error}</Alert>}
      {check.isPending && !local ? <Spinner /> : m ? <MoneySummary check={m} onRemove={canChange ? remove : undefined} /> : null}
      {adding && (
        <MoneyLineDialog
          booking={booking}
          direction={adding}
          currency={m?.currency}
          onClose={() => setAdding(null)}
          onSaved={() => {
            setAdding(null);
            refresh();
          }}
        />
      )}
    </section>
  );
}

/** The sums and warnings of a money check, as a short statement. */
export function MoneySummary({ check: m, onRemove }: { check: MoneyCheck; onRemove?: (l: MoneyLine) => void }) {
  const lineOf = (id?: string | null) => m.lines.find((l) => l.id === id);
  const row = (label: ReactNode, amount: Money, opts: { minus?: boolean; strong?: boolean; note?: ReactNode; removable?: MoneyLine } = {}) => (
    <div className={cn("flex items-baseline gap-2 py-1", opts.strong && "border-t pt-2 font-semibold")}>
      <span className="min-w-0 flex-1">
        {label}
        {opts.note && <span className="block text-xs text-muted-foreground">{opts.note}</span>}
      </span>
      <span className="tabular-nums">
        {opts.minus && amount.minor > 0 ? "−" : ""}
        {formatMoney(amount)}
      </span>
      {onRemove && (
        <span className="w-8">
          {opts.removable && (
            <Button size="icon" variant="ghost" className="size-7" aria-label={`Remove ${opts.removable.description}`} onClick={() => onRemove(opts.removable!)}>
              <Trash2 />
            </Button>
          )}
        </span>
      )}
    </div>
  );
  const loss = m.net.minor < 0;
  return (
    <div className="flex flex-col gap-2">
      {m.warnings.map((w) => (
        <div
          key={w.code}
          role={w.level === "loss" ? "alert" : "status"}
          className={cn(
            "flex items-start gap-2 rounded-md border p-2 text-sm",
            w.level === "loss" ? "border-destructive/40 bg-destructive/10 text-destructive" : "border-amber-500/40 bg-amber-500/10",
          )}
        >
          <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden />
          {w.message}
        </div>
      ))}
      <div className="rounded-md border p-3 text-sm">
        {m.income.map((i, n) => (
          <div key={`i${n}`}>
            {row(i.description, i.amount, {
              note: [i.category === "fee" ? null : INCOME_LABELS[i.category], i.original && formatMoney(i.original)].filter(Boolean).join(" · "),
              removable: lineOf(i.source_id),
            })}
          </div>
        ))}
        {m.commission.map((c, n) => <div key={`c${n}`}>{row(c.description, c.amount, { minus: true })}</div>)}
        {m.costs.map((c, n) => (
          <div key={`k${n}`}>
            {row(c.description, c.amount, {
              minus: true,
              note: [lineOf(c.source_id) ? COST_LABELS[c.category] : null, c.original && formatMoney(c.original)].filter(Boolean).join(" · "),
              removable: lineOf(c.source_id),
            })}
          </div>
        ))}
        <div className={cn(loss && "text-destructive")}>{row("Left after costs and commission", m.net, { strong: true })}</div>
      </div>
    </div>
  );
}

function MoneyLineDialog({
  booking,
  direction,
  currency,
  onClose,
  onSaved,
}: {
  booking: Booking;
  direction: "income" | "cost";
  currency?: string;
  onClose: () => void;
  onSaved: () => void;
}) {
  const fallback = useWorkspaceCurrency();
  const labels = direction === "income" ? INCOME_LABELS : COST_LABELS;
  const [category, setCategory] = useState(direction === "income" ? "buyout" : "per_diem");
  const [description, setDescription] = useState("");
  const [amount, setAmount] = useState<Money | null>(null);
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    setError(null);
    if (!amount) {
      setError("Give the amount.");
      return;
    }
    try {
      await ok(
        api.POST("/api/v1/bookings/{id}/money", {
          params: { path: { id: booking.id } },
          body: { direction, category, description: description || labels[category] || category, amount },
        }),
      );
      onSaved();
    } catch (e) {
      setError(failure(e));
    }
  };
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title={direction === "income" ? "Add a buyout or income" : "Add a cost"}>
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <Field label="Kind" id="line-category">
          <NativeSelect id="line-category" value={category} onChange={(e) => setCategory(e.target.value)}>
            {Object.entries(labels).map(([k, label]) => (
              <option key={k} value={k}>
                {label}
              </option>
            ))}
          </NativeSelect>
        </Field>
        <Field label="Description" id="line-description">
          <Input
            id="line-description"
            value={description}
            placeholder={direction === "income" ? "Hotel buyout, catering buyout" : "Per diems for four, backline hire"}
            onChange={(e) => setDescription(e.target.value)}
          />
        </Field>
        <Field label="Amount" id="line-amount">
          <MoneyInput id="line-amount" value={amount} defaultCurrency={currency ?? fallback} onChange={setAmount} />
        </Field>
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit">Add</Button>
        </div>
      </form>
    </Dialog>
  );
}
