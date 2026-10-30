// Talent's own calendars, busy times only. On a talent's page the agency
// sends a link; the talent (or each member of an act) opens it without an
// OnHold account and connects Google, Microsoft or a private iCal link.
// OnHold then reads when they are busy, never what they are doing, and
// the busy times count like marked availability.

import type { Schemas } from "@onhold/api-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useParams, useSearch } from "@tanstack/react-router";
import {
  Alert,
  Badge,
  Button,
  Card,
  Input,
  Label,
  Spinner,
  applyBrandTheme,
  readBranding,
  relativeTime,
} from "@onhold/ui";
import {
  CalendarCheck,
  Check,
  Copy,
  Link2,
  RefreshCw,
  Trash2,
} from "lucide-react";
import { useEffect, useState } from "react";
import { ApiFailure, api, ok } from "../api.ts";

type TalentCalendar = Schemas["TalentCalendarView"];

const PROVIDER: Record<string, string> = {
  google: "Google Calendar",
  microsoft: "Outlook",
  ical: "iCal link",
};

function failure(e: unknown): string {
  return e instanceof ApiFailure || e instanceof Error
    ? e.message
    : "Something went wrong.";
}

const LINK_GONE =
  "This link does not work any more. Links last two weeks; ask whoever sent it for a new one.";

/** On the talent's pages, "not found" means the link stopped working. */
function linkFailure(e: unknown): string {
  return e instanceof ApiFailure && e.status === 404 ? LINK_GONE : failure(e);
}

function CopyLink({ url }: { url: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="flex items-center gap-2">
      <Input
        readOnly
        value={url}
        aria-label="Link to send"
        onFocus={(e) => e.target.select()}
      />
      <Button
        variant="outline"
        size="sm"
        aria-label="Copy link"
        onClick={() => {
          void navigator.clipboard?.writeText(url).then(() => {
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          });
        }}
      >
        {copied ? <Check /> : <Copy />}
      </Button>
    </div>
  );
}

function StatusBadge({ c }: { c: TalentCalendar }) {
  if (c.status === "reconnect")
    return (
      <Badge className="border-destructive bg-destructive text-white">
        Needs connecting again
      </Badge>
    );
  if (c.status === "failing")
    return (
      <Badge className="border border-amber-500 text-amber-800 dark:text-amber-300">
        Not read lately
      </Badge>
    );
  return (
    <Badge className="border border-emerald-600 text-emerald-700 dark:text-emerald-300">
      Connected
    </Badge>
  );
}

export function TalentCalendarsCard({
  talentId,
  canWrite,
}: {
  talentId: string;
  canWrite: boolean;
}) {
  const qc = useQueryClient();
  const key = ["talent-calendars", talentId];
  // Calendars asked to be read now, by how they stood when asked.
  const [reading, setReading] = useState<Record<string, string>>({});
  const stood = (c: TalentCalendar) =>
    `${c.last_synced_at ?? ""}|${c.last_error_at ?? ""}`;
  const data = useQuery({
    queryKey: key,
    queryFn: () =>
      ok(
        api.GET("/api/v1/talent/{id}/calendars", {
          params: { path: { id: talentId } },
        }),
      ),
    refetchInterval: Object.keys(reading).length > 0 ? 2000 : false,
  });
  useEffect(() => {
    const ids = Object.keys(reading);
    if (!data.data || ids.length === 0) return;
    const done = ids.filter((id) => {
      const c = data.data.calendars.find((x) => x.id === id);
      return !c || stood(c) !== reading[id];
    });
    if (done.length === 0) return;
    setReading((r) =>
      Object.fromEntries(
        Object.entries(r).filter(([id]) => !done.includes(id)),
      ),
    );
    void qc.invalidateQueries({ queryKey: ["availability", talentId] });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data.data]);
  // Give up waiting after a minute; the worker may be busy.
  useEffect(() => {
    if (Object.keys(reading).length === 0) return;
    const t = window.setTimeout(() => setReading({}), 60_000);
    return () => window.clearTimeout(t);
  }, [reading]);
  const [link, setLink] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const act = async (call: () => Promise<unknown>) => {
    setError(null);
    try {
      await call();
    } catch (e) {
      setError(failure(e));
    } finally {
      void qc.invalidateQueries({ queryKey: key });
      void qc.invalidateQueries({ queryKey: ["availability", talentId] });
    }
  };
  const path = (id: string) => ({
    params: { path: { id: talentId, calendar_id: id } },
  });

  return (
    <section className="flex flex-col gap-3" aria-label="Their own calendars">
      <div className="flex items-center justify-between gap-2">
        <h2 className="font-semibold">Their own calendars</h2>
        {canWrite && (
          <Button
            variant="outline"
            size="sm"
            onClick={() =>
              void act(async () => {
                const made = await ok(
                  api.POST("/api/v1/talent/{id}/calendar-links", {
                    params: { path: { id: talentId } },
                  }),
                );
                setLink(made.url);
              })
            }
          >
            <Link2 /> Get a link to send
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">
        When they connect their calendar, OnHold sees only when they are busy,
        never what they are doing, and counts it like dates marked unavailable.
      </p>
      {link && (
        <Alert tone="success" className="flex flex-col gap-2">
          <span>
            Send this link to them, or to each member of an act. It works for
            two weeks, needs no OnHold account, and is shown only now.
          </span>
          <CopyLink url={link} />
        </Alert>
      )}
      {error && <Alert>{error}</Alert>}
      {data.isPending ? (
        <Spinner />
      ) : data.isError ? (
        <Alert>{data.error.message}</Alert>
      ) : (
        <>
          {data.data.calendars.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No calendars connected yet.
            </p>
          ) : (
            <ul className="divide-y rounded-md border">
              {data.data.calendars.map((c) => (
                <li
                  key={c.id}
                  className="flex flex-wrap items-center gap-2 px-3 py-2 text-sm"
                >
                  <CalendarCheck className="size-4 text-muted-foreground" />
                  <span className="flex min-w-0 flex-1 flex-col">
                    <span>
                      <span className="font-medium">
                        {c.person ?? c.account}
                      </span>
                      {" · "}
                      {PROVIDER[c.provider] ?? c.provider}
                    </span>
                    <span className="text-muted-foreground">
                      {c.person && `${c.account} · `}
                      {c.id in reading
                        ? "reading…"
                        : c.last_synced_at
                          ? `read ${relativeTime(c.last_synced_at)}`
                          : "not read yet"}
                      {c.status !== "active" &&
                        c.last_error &&
                        ` · ${c.last_error}`}
                    </span>
                  </span>
                  <StatusBadge c={c} />
                  {canWrite && (
                    <>
                      {c.status !== "reconnect" && (
                        <Button
                          variant="ghost"
                          size="icon"
                          aria-label={`Read ${c.person ?? c.account} now`}
                          disabled={c.id in reading}
                          onClick={() =>
                            void act(async () => {
                              await ok(
                                api.POST(
                                  "/api/v1/talent/{id}/calendars/{calendar_id}/sync",
                                  path(c.id),
                                ),
                              );
                              setReading((r) => ({ ...r, [c.id]: stood(c) }));
                            })
                          }
                        >
                          <RefreshCw
                            className={c.id in reading ? "animate-spin" : ""}
                          />
                        </Button>
                      )}
                      <Button
                        variant="ghost"
                        size="icon"
                        aria-label={`Disconnect ${c.person ?? c.account}`}
                        onClick={() => {
                          if (
                            !window.confirm(
                              `Stop reading ${c.person ?? c.account}'s calendar? Its busy times are forgotten.`,
                            )
                          )
                            return;
                          void act(() =>
                            ok(
                              api.DELETE(
                                "/api/v1/talent/{id}/calendars/{calendar_id}",
                                path(c.id),
                              ),
                            ),
                          );
                        }}
                      >
                        <Trash2 />
                      </Button>
                    </>
                  )}
                </li>
              ))}
            </ul>
          )}
          {data.data.calendars.some((c) => c.status === "reconnect") && (
            <p className="text-sm text-muted-foreground">
              A calendar that needs connecting again no longer counts its busy
              times. Send a new link: connecting the same calendar, or pasting
              the same iCal link, puts it right.
            </p>
          )}
          {data.data.links.length > 0 && (
            <p className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
              {data.data.links.length === 1
                ? "One link"
                : `${data.data.links.length} links`}{" "}
              sent, working until{" "}
              {new Date(
                Math.max(
                  ...data.data.links.map((l) => Date.parse(l.expires_at)),
                ),
              ).toLocaleDateString()}
              .
              {canWrite && (
                <Button
                  variant="link"
                  size="sm"
                  className="h-auto p-0"
                  onClick={() =>
                    void act(async () => {
                      for (const l of data.data.links) {
                        await ok(
                          api.DELETE(
                            "/api/v1/talent/{id}/calendar-links/{link_id}",
                            {
                              params: { path: { id: talentId, link_id: l.id } },
                            },
                          ),
                        );
                      }
                      setLink(null);
                    })
                  }
                >
                  Stop {data.data.links.length === 1 ? "it" : "them"} working
                </Button>
              )}
            </p>
          )}
        </>
      )}
    </section>
  );
}

// The talent's page, from their link -----------------------------------------------------------

const TOKEN_KEY = "onhold.calendar-connect";

function remember(token: string) {
  try {
    sessionStorage.setItem(TOKEN_KEY, token);
  } catch {
    // Without storage there is just no way back to the link after connecting.
  }
}

function remembered(): string | null {
  try {
    return sessionStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

function useBranding(branding: unknown, title: string | undefined) {
  useEffect(() => {
    // The keys the branding settings save, as the quote and signing pages read them.
    if (branding) applyBrandTheme(readBranding(branding));
    if (title) document.title = title;
  }, [branding, title]);
}

export function Header({
  workspace,
  branding,
  title,
}: {
  workspace: string;
  branding: unknown;
  title: string;
}) {
  const logo = readBranding(branding).logo;
  return (
    <header className="flex items-center gap-3">
      {logo && (
        <img src={logo} alt="" className="size-10 rounded object-contain" />
      )}
      <div>
        <p className="text-sm text-muted-foreground">{workspace}</p>
        <h1 className="text-2xl font-semibold">{title}</h1>
      </div>
    </header>
  );
}

export function ConnectCalendarPage() {
  const { token } = useParams({ from: "/connect-calendar/$token" });
  const page = useQuery({
    queryKey: ["calendar-connect", token],
    queryFn: () =>
      ok(
        api.GET("/api/v1/public/calendar-connect/{token}", {
          params: { path: { token } },
        }),
      ),
    retry: false,
  });
  const [person, setPerson] = useState("");
  const [ical, setIcal] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<Schemas["CalendarConnected"] | null>(null);
  useBranding(
    page.data?.branding,
    page.data && `Connect your calendar · ${page.data.workspace_name}`,
  );

  if (page.isPending) {
    return (
      <div className="flex h-dvh items-center justify-center">
        <Spinner className="size-6" />
      </div>
    );
  }
  if (page.isError) {
    return (
      <main className="mx-auto max-w-xl p-6">
        <Alert>{linkFailure(page.error)}</Alert>
      </main>
    );
  }
  const p = page.data;
  if (done) return <Connected result={done} again={() => setDone(null)} />;

  const start = async (provider: "google" | "microsoft") => {
    setError(null);
    setBusy(true);
    try {
      const { url } = await ok(
        api.POST("/api/v1/public/calendar-connect/{token}/start", {
          params: { path: { token } },
          body: { provider, person: person || null },
        }),
      );
      remember(token);
      window.location.assign(url);
    } catch (e) {
      setError(linkFailure(e));
      setBusy(false);
    }
  };

  return (
    <main className="mx-auto flex max-w-xl flex-col gap-6 p-4 sm:p-8">
      <Header
        workspace={p.workspace_name}
        branding={p.branding}
        title="Connect your calendar"
      />
      <p>
        {p.workspace_name} would like to see when{" "}
        <strong>{p.talent_name}</strong> is busy, so they do not book you when
        you cannot make it.
      </p>
      <Card className="flex flex-col gap-2 p-4 text-sm">
        <p className="font-medium">What they see</p>
        <p>
          Only the times you are busy. Never what you are doing, where, or with
          whom.
        </p>
        <p className="text-muted-foreground">
          They can stop it at any time, and so can you: remove OnHold from your
          Google or Microsoft account, or stop sharing the iCal link.
        </p>
      </Card>
      {p.connected.length > 0 && (
        <p className="text-sm text-muted-foreground">
          Already connected:{" "}
          {p.connected
            .map(
              (c) =>
                `${c.person ?? "a calendar"} (${PROVIDER[c.provider] ?? c.provider}${c.status === "reconnect" ? ", needs connecting again" : ""})`,
            )
            .join(", ")}
          .
        </p>
      )}
      <div className="flex flex-col gap-1.5">
        <Label htmlFor="connect-person">
          Your name (if you are one of several)
        </Label>
        <Input
          id="connect-person"
          value={person}
          maxLength={100}
          onChange={(e) => setPerson(e.target.value)}
          placeholder="Optional"
        />
      </div>
      {error && <Alert>{error}</Alert>}
      <div className="flex flex-col gap-2 sm:flex-row">
        {p.google && (
          <Button disabled={busy} onClick={() => void start("google")}>
            Connect Google Calendar
          </Button>
        )}
        {p.microsoft && (
          <Button
            disabled={busy}
            variant={p.google ? "outline" : "default"}
            onClick={() => void start("microsoft")}
          >
            Connect Outlook or Microsoft 365
          </Button>
        )}
      </div>
      <form
        className="flex flex-col gap-2"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          setBusy(true);
          try {
            setDone(
              await ok(
                api.POST("/api/v1/public/calendar-connect/{token}/ical", {
                  params: { path: { token } },
                  body: { url: ical, person: person || null },
                }),
              ),
            );
            setIcal("");
            void page.refetch();
          } catch (err) {
            setError(linkFailure(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <Label htmlFor="connect-ical">
          Or paste a private iCal link (Apple, Proton, Fastmail and others)
        </Label>
        <div className="flex gap-2">
          <Input
            id="connect-ical"
            required
            value={ical}
            onChange={(e) => setIcal(e.target.value)}
            placeholder="webcal://… or https://….ics"
          />
          <Button type="submit" variant="outline" disabled={busy}>
            {busy ? <Spinner /> : "Connect"}
          </Button>
        </div>
        <p className="text-xs text-muted-foreground">
          In Apple Calendar: share the calendar as a public calendar and copy
          its link. In Google Calendar: Settings, your calendar, "Secret address
          in iCal format".
        </p>
      </form>
    </main>
  );
}

function Connected({
  result,
  again,
}: {
  result: Schemas["CalendarConnected"];
  again?: () => void;
}) {
  return (
    <main className="mx-auto flex max-w-xl flex-col gap-6 p-4 sm:p-8">
      <Header
        workspace={result.workspace_name}
        branding={result.branding}
        title="Your calendar is connected"
      />
      <Alert tone="success">
        {result.workspace_name} now sees when {result.talent_name} is busy in{" "}
        {result.account} ({PROVIDER[result.provider] ?? result.provider}
        ), and nothing else. It updates every few minutes.
      </Alert>
      {again && (
        <Button variant="outline" onClick={again}>
          Connect another calendar
        </Button>
      )}
    </main>
  );
}

/** Where Google or Microsoft send the talent back to. */
export function ConnectCalendarReturnPage() {
  const search = useSearch({ from: "/connect-calendar" });
  const [result, setResult] = useState<Schemas["CalendarConnected"] | null>(
    null,
  );
  const [error, setError] = useState<string | null>(
    search.error
      ? "The calendar was not connected: permission was not given. You can try again from your link."
      : null,
  );
  useBranding(
    result?.branding,
    result ? `Calendar connected · ${result.workspace_name}` : undefined,
  );
  useEffect(() => {
    if (!search.code || !search.state) {
      if (!search.error)
        setError(
          "This page is opened by Google or Microsoft after you connect. Open your link to start.",
        );
      return;
    }
    const body = { code: search.code, state: search.state };
    // Tidy the address bar so a reload does not use the code twice.
    window.history.replaceState(null, "", "/connect-calendar");
    ok(api.POST("/api/v1/public/calendar-connect/finish", { body }))
      .then(setResult)
      .catch((e: unknown) => setError(linkFailure(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const token = remembered();
  const back = token
    ? () => window.location.assign(`/connect-calendar/${token}`)
    : undefined;
  if (result) return <Connected result={result} again={back} />;
  if (error) {
    return (
      <main className="mx-auto flex max-w-xl flex-col gap-4 p-6">
        <Alert>{error}</Alert>
        {back && (
          <Button variant="outline" onClick={back}>
            Back to your link
          </Button>
        )}
      </main>
    );
  }
  return (
    <div className="flex h-dvh items-center justify-center">
      <Spinner className="size-6" />
    </div>
  );
}
