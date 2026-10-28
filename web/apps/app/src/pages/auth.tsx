// Signing in (password, emailed link, Google or Microsoft), the second
// factor, accepting an invitation, and signing in the desktop and mobile
// apps by approving their code here in the browser.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { Alert, Button, Card, Input, Label, Spinner, applyBrandTheme, clearBranding, readBranding, relativeTime } from "@onhold/ui";
import { type FormEvent, type ReactNode, useEffect, useLayoutEffect, useRef, useState } from "react";
import { ApiFailure, api, ok, setCsrfToken, setWorkspace } from "../api.ts";
import { GoTo } from "../go-to.tsx";
import { isNetworkError } from "../offline/sync.ts";
import { APP_NAME, IS_APP, deviceName, openInBrowser, saveAppToken, serverUrl, setServerUrl, validServerUrl } from "../platform.ts";
import { useMe } from "../session.tsx";

const ERRORS: Record<string, string> = {
  oidc_failed: "Signing in with that provider did not work. Try again or use your email.",
  oidc_no_account: "No OnHold account uses that address. Ask for an invitation first.",
};

/** Only paths on this site, so a link cannot send people elsewhere after signing in. */
export function safeRedirect(target: string | undefined): string {
  return target && target.startsWith("/") && !target.startsWith("//") ? target : "/";
}

function AuthCard({ title, logo, children }: { title: string; logo?: string | null; children: ReactNode }) {
  return (
    <div className="flex min-h-dvh items-center justify-center bg-muted/40 p-4">
      <Card className="w-full max-w-sm p-6">
        {logo && <img src={logo} alt="" className="mb-4 h-10 w-auto max-w-48 object-contain brand-logo" />}
        <div className="mb-6 flex items-center gap-2">
          {!logo && <img src="/favicon.svg" alt="" className="size-7" />}
          <h1 className="text-lg font-semibold">{title}</h1>
        </div>
        {children}
      </Card>
    </div>
  );
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function SignInPage() {
  return IS_APP ? <AppSignIn /> : <BrowserSignIn />;
}

function BrowserSignIn() {
  const { redirect, error } = useSearch({ from: "/sign-in" });
  const navigate = useNavigate();
  const qc = useQueryClient();
  const me = useMe();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(error ? (ERRORS[error] ?? error) : null);
  const [linkSent, setLinkSent] = useState(false);
  const providers = useQuery({
    queryKey: ["oidc-providers"],
    queryFn: () => ok(api.GET("/api/v1/auth/oidc/providers")),
  });
  const target = safeRedirect(redirect);

  useEffect(() => {
    if (me.data?.session.mfa_verified) void navigate({ to: target, replace: true });
  }, [me.data, navigate, target]);

  const signIn = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setProblem(null);
    try {
      const session = await ok(api.POST("/api/v1/auth/login", { body: { email, password } }));
      setCsrfToken(session.csrf_token);
      await qc.refetchQueries({ queryKey: ["me"], type: "all" });
      if (session.mfa_required) await navigate({ to: "/auth/mfa", search: { redirect: target } });
      else await navigate({ to: target });
    } catch (err) {
      setProblem(message(err));
    } finally {
      setBusy(false);
    }
  };

  const sendLink = async () => {
    if (!email) {
      setProblem("Enter your email address first.");
      return;
    }
    setBusy(true);
    setProblem(null);
    try {
      await ok(api.POST("/api/v1/auth/magic-link", { body: { email } }));
      setLinkSent(true);
    } catch (err) {
      setProblem(message(err));
    } finally {
      setBusy(false);
    }
  };

  if (linkSent) {
    return (
      <AuthCard title="Check your email">
        <p className="text-sm">
          If {email} has an OnHold account, a sign-in link is on its way. It works once and expires soon.
        </p>
      </AuthCard>
    );
  }

  return (
    <AuthCard title="Sign in to OnHold">
      <form className="flex flex-col gap-4" onSubmit={signIn}>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="email">Email</Label>
          <Input id="email" type="email" autoComplete="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="password">Password</Label>
          <Input
            id="password"
            type="password"
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </div>
        {problem && <Alert>{problem}</Alert>}
        <Button type="submit" disabled={busy || !password}>
          {busy && <Spinner />}
          Sign in
        </Button>
        <Button variant="outline" disabled={busy} onClick={() => void sendLink()}>
          Email me a sign-in link
        </Button>
        {(providers.data ?? []).map((p) => (
          <Button key={p.name} variant="outline" asChild>
            <a href={`/api/v1/auth/oidc/${p.name}/start?redirect=${encodeURIComponent(target)}`}>
              Continue with {p.display_name}
            </a>
          </Button>
        ))}
      </form>
    </AuthCard>
  );
}

export function MagicLinkPage() {
  const { token, redirect } = useSearch({ from: "/auth/magic" });
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [problem, setProblem] = useState<string | null>(null);
  const used = useRef(false);

  useEffect(() => {
    // The token works once; React may run effects twice in development.
    if (used.current) return;
    used.current = true;
    (async () => {
      try {
        const session = await ok(api.POST("/api/v1/auth/magic-link/consume", { body: { token: token ?? "" } }));
        setCsrfToken(session.csrf_token);
        await qc.refetchQueries({ queryKey: ["me"], type: "all" });
        const target = safeRedirect(redirect);
        if (session.mfa_required) await navigate({ to: "/auth/mfa", search: { redirect: target }, replace: true });
        else await navigate({ to: target, replace: true });
      } catch (e) {
        setProblem(message(e));
      }
    })();
  }, [token, redirect, navigate, qc]);

  return (
    <AuthCard title="Signing you in">
      {problem ? (
        <div className="flex flex-col gap-4">
          <Alert>{problem}</Alert>
          <Link to="/sign-in" className="text-sm text-primary underline">
            Back to sign in
          </Link>
        </div>
      ) : (
        <Spinner />
      )}
    </AuthCard>
  );
}

export function MfaPage() {
  const { redirect } = useSearch({ from: "/auth/mfa" });
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [code, setCode] = useState("");
  const [recovery, setRecovery] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const verify = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setProblem(null);
    try {
      const body = recovery ? { recovery_code: code.trim() } : { code: code.replace(/\s/g, "") };
      await ok(api.POST("/api/v1/auth/mfa/verify", { body }));
      await qc.refetchQueries({ queryKey: ["me"], type: "all" });
      await navigate({ to: safeRedirect(redirect), replace: true });
    } catch (err) {
      setProblem(message(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <AuthCard title="Two-step verification">
      <form className="flex flex-col gap-4" onSubmit={verify}>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="code">{recovery ? "Recovery code" : "Code from your authenticator app"}</Label>
          <Input
            id="code"
            autoFocus
            autoComplete="one-time-code"
            inputMode={recovery ? "text" : "numeric"}
            value={code}
            onChange={(e) => setCode(e.target.value)}
          />
        </div>
        {problem && <Alert>{problem}</Alert>}
        <Button type="submit" disabled={busy || !code.trim()}>
          {busy && <Spinner />}
          Verify
        </Button>
        <Button variant="link" onClick={() => setRecovery((r) => !r)}>
          {recovery ? "Use an authenticator code" : "Use a recovery code instead"}
        </Button>
      </form>
    </AuthCard>
  );
}

export function AcceptInvitationPage() {
  const { token } = useSearch({ from: "/invitations/accept" });
  const navigate = useNavigate();
  const qc = useQueryClient();
  const me = useMe();
  const preview = useQuery({
    queryKey: ["invitation", token],
    queryFn: () => ok(api.GET("/api/v1/invitations/lookup", { params: { query: { token: token ?? "" } } })),
    retry: false,
  });
  const brand = preview.data ? readBranding(preview.data.branding) : null;
  // The page wears the inviting workspace's look.
  const look = JSON.stringify(brand ? { colour: brand.colour, theme: brand.theme } : null);
  useLayoutEffect(() => {
    const l = JSON.parse(look) as Parameters<typeof applyBrandTheme>[0] | null;
    if (!l) return;
    applyBrandTheme(l);
    return () => clearBranding();
  }, [look]);
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const accept = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setProblem(null);
    try {
      const signedIn = !!me.data;
      const res = await ok(
        api.POST("/api/v1/invitations/accept", {
          body: signedIn ? { token: token ?? "" } : { token: token ?? "", name, password },
        }),
      );
      if (res.session) setCsrfToken(res.session.csrf_token);
      setWorkspace(res.workspace_id);
      await qc.refetchQueries({ queryKey: ["me"], type: "all" });
      await navigate({ to: "/", replace: true });
    } catch (err) {
      if (err instanceof ApiFailure && err.code === "account_exists") {
        setProblem("You already have an account. Sign in first, then open the invitation link again.");
      } else {
        setProblem(message(err));
      }
    } finally {
      setBusy(false);
    }
  };

  if (preview.isPending || me.isPending) {
    return (
      <AuthCard title="Invitation">
        <Spinner />
      </AuthCard>
    );
  }
  if (preview.isError) {
    return (
      <AuthCard title="Invitation">
        <Alert>This invitation is not valid any more. Ask whoever invited you to send a new one.</Alert>
      </AuthCard>
    );
  }
  const inv = preview.data;
  return (
    <AuthCard title={`Join ${inv.workspace_name}`} logo={brand?.logo}>
      <form className="flex flex-col gap-4" onSubmit={accept}>
        <p className="text-sm text-muted-foreground">
          You have been invited as {inv.role === "admin" || inv.role === "agent" ? "an" : "a"} {inv.role}, using {inv.email}.
        </p>
        {me.data ? (
          <p className="text-sm">Signed in as {me.data.user.email}.</p>
        ) : (
          <>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="name">Your name</Label>
              <Input id="name" autoComplete="name" required value={name} onChange={(e) => setName(e.target.value)} />
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="new-password">Choose a password</Label>
              <Input
                id="new-password"
                type="password"
                autoComplete="new-password"
                required
                minLength={12}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">At least 12 characters. A short sentence works well.</p>
            </div>
          </>
        )}
        {problem && <Alert>{problem}</Alert>}
        <Button type="submit" disabled={busy}>
          {busy && <Spinner />}
          Accept and continue
        </Button>
        {!me.data && (
          <Link
            to="/sign-in"
            search={{ redirect: `/invitations/accept?token=${encodeURIComponent(token ?? "")}` }}
            className="text-center text-sm text-primary underline"
          >
            I already have an account
          </Link>
        )}
      </form>
    </AuthCard>
  );
}

// The desktop and mobile apps ---------------------------------------------------------------

type AppSignInState =
  | { step: "start" }
  | { step: "waiting"; code: string; url: string; expiresAt: string }
  | { step: "stopped"; message: string };

/** In the apps: show a code, open the browser, and wait for it to be approved there. */
function AppSignIn() {
  const { redirect } = useSearch({ from: "/sign-in" });
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [state, setState] = useState<AppSignInState>({ step: "start" });
  const [busy, setBusy] = useState(false);
  const [server, setServer] = useState(serverUrl());
  const [editingServer, setEditingServer] = useState(!serverUrl());
  const [serverInput, setServerInput] = useState(serverUrl());
  const [problem, setProblem] = useState<string | null>(null);
  const cancelled = useRef(false);
  useEffect(() => {
    cancelled.current = false;
    return () => {
      cancelled.current = true;
    };
  }, []);

  const saveServer = (e: FormEvent) => {
    e.preventDefault();
    const url = validServerUrl(serverInput);
    if (!url) {
      setProblem("Enter the address you use OnHold at, starting with https://.");
      return;
    }
    setServerUrl(url);
    setServer(url);
    setEditingServer(false);
    setProblem(null);
  };

  const poll = async (pollToken: string, intervalMs: number) => {
    while (!cancelled.current) {
      await new Promise((r) => setTimeout(r, intervalMs));
      if (cancelled.current) return;
      try {
        const session = await ok(api.POST("/api/v1/auth/app/token", { body: { poll_token: pollToken } }));
        await saveAppToken(session.token);
        await qc.refetchQueries({ queryKey: ["me"], type: "all" });
        await navigate({ to: safeRedirect(redirect), replace: true });
        return;
      } catch (e) {
        if (e instanceof ApiFailure && e.code === "authorization_pending") continue;
        // No connection for a moment: keep waiting.
        if (isNetworkError(e)) continue;
        const why =
          e instanceof ApiFailure && e.code === "access_denied"
            ? "The sign-in was refused in the browser."
            : e instanceof ApiFailure && e.code === "expired_token"
              ? "The code expired before it was approved. Start again."
              : message(e);
        setState({ step: "stopped", message: why });
        return;
      }
    }
  };

  const start = async () => {
    setBusy(true);
    setProblem(null);
    try {
      const started = await ok(api.POST("/api/v1/auth/app/start", { body: { device_name: await deviceName() } }));
      setState({ step: "waiting", code: started.user_code, url: started.verification_url, expiresAt: started.expires_at });
      void openInBrowser(started.verification_url).catch(() => undefined);
      void poll(started.poll_token, started.interval_seconds * 1000);
    } catch (e) {
      setProblem(isNetworkError(e) ? `Could not reach ${server}. Check the address and your connection.` : message(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <AuthCard title={`Sign in to ${APP_NAME}`}>
      {editingServer ? (
        <form className="flex flex-col gap-4" onSubmit={saveServer}>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="server">Your OnHold address</Label>
            <Input
              id="server"
              inputMode="url"
              placeholder="https://yourcompany.onhold.app"
              value={serverInput}
              onChange={(e) => setServerInput(e.target.value)}
            />
          </div>
          {problem && <Alert>{problem}</Alert>}
          <Button type="submit">Continue</Button>
        </form>
      ) : state.step === "waiting" ? (
        <div className="flex flex-col gap-4 text-sm">
          <p>Your browser is open at the OnHold website. Sign in there if you need to, then enter this code:</p>
          <p className="text-center font-mono text-3xl font-semibold tracking-widest" aria-label="Sign-in code">
            {state.code}
          </p>
          <p className="text-muted-foreground">
            The code works until {new Date(state.expiresAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}.
            This screen moves on by itself once you approve.
          </p>
          <div className="flex items-center gap-2 text-muted-foreground">
            <Spinner /> Waiting for approval…
          </div>
          <Button variant="outline" onClick={() => void openInBrowser(state.url)}>
            Open the website again
          </Button>
        </div>
      ) : (
        <div className="flex flex-col gap-4 text-sm">
          {state.step === "stopped" && <Alert>{state.message}</Alert>}
          {problem && <Alert>{problem}</Alert>}
          <p>You sign in with your browser, the same way you do on the website, and approve this device there.</p>
          <Button disabled={busy} onClick={() => void start()}>
            {busy && <Spinner />}
            Sign in with your browser
          </Button>
          <p className="text-xs text-muted-foreground">
            Server: {server}{" "}
            <button type="button" className="text-primary underline" onClick={() => setEditingServer(true)}>
              Change
            </button>
          </p>
        </div>
      )}
    </AuthCard>
  );
}

/** On the website: approve (or refuse) an app that is waiting with a code. */
export function LinkAppPage() {
  const me = useMe();
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [found, setFound] = useState<{
    code: string;
    device_name: string;
    ip?: string | null;
    user_agent?: string | null;
    created_at: string;
  } | null>(null);
  const [done, setDone] = useState<"approved" | "refused" | null>(null);

  if (me.isPending) {
    return (
      <AuthCard title="Sign in an app">
        <Spinner />
      </AuthCard>
    );
  }
  if (!me.data?.session.mfa_verified) {
    return <GoTo to="/sign-in" redirect="/link-app" />;
  }

  const lookUp = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setProblem(null);
    try {
      const pending = await ok(api.GET("/api/v1/auth/app/lookup", { params: { query: { code } } }));
      setFound({ ...pending, code });
    } catch (err) {
      setProblem(message(err));
    } finally {
      setBusy(false);
    }
  };

  const decide = async (approve: boolean) => {
    if (!found) return;
    setBusy(true);
    setProblem(null);
    try {
      await ok(api.POST("/api/v1/auth/app/decide", { body: { code: found.code, approve } }));
      setDone(approve ? "approved" : "refused");
    } catch (err) {
      setProblem(message(err));
    } finally {
      setBusy(false);
    }
  };

  if (done) {
    return (
      <AuthCard title={done === "approved" ? "App signed in" : "Sign-in refused"}>
        <p className="text-sm">
          {done === "approved"
            ? `${found?.device_name} is now signed in as ${me.data.user.email}. You can go back to the app.`
            : "The app was not signed in. If you did not start this sign-in yourself, you can ignore it."}
        </p>
      </AuthCard>
    );
  }

  return (
    <AuthCard title="Sign in an app">
      {found ? (
        <div className="flex flex-col gap-4 text-sm">
          <p>
            Sign in <strong>{found.device_name}</strong> as {me.data.user.email}?
          </p>
          <p className="text-muted-foreground">
            Asked {relativeTime(found.created_at)}
            {found.ip ? ` from ${found.ip}` : ""}. Only approve a sign-in you started yourself just now: whoever has
            that device will be able to see and change your workspaces.
          </p>
          {problem && <Alert>{problem}</Alert>}
          <div className="flex gap-2">
            <Button variant="outline" className="flex-1" disabled={busy} onClick={() => void decide(false)}>
              Refuse
            </Button>
            <Button className="flex-1" disabled={busy} onClick={() => void decide(true)}>
              Approve
            </Button>
          </div>
        </div>
      ) : (
        <form className="flex flex-col gap-4" onSubmit={lookUp}>
          <p className="text-sm">Enter the code the OnHold app is showing.</p>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="app-code">Code</Label>
            <Input
              id="app-code"
              autoFocus
              autoComplete="off"
              autoCapitalize="characters"
              placeholder="ABCD-EFGH"
              className="font-mono uppercase tracking-widest"
              value={code}
              onChange={(e) => setCode(e.target.value)}
            />
          </div>
          {problem && <Alert>{problem}</Alert>}
          <Button type="submit" disabled={busy || code.replace(/[^a-z]/gi, "").length < 8}>
            {busy && <Spinner />}
            Continue
          </Button>
        </form>
      )}
    </AuthCard>
  );
}
