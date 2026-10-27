// Signing in (password, emailed link, Google or Microsoft), the second
// factor, and accepting an invitation.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { Alert, Button, Card, Input, Label, Spinner } from "@onhold/ui";
import { type FormEvent, type ReactNode, useEffect, useRef, useState } from "react";
import { ApiFailure, api, ok, setCsrfToken, setWorkspace } from "../api.ts";
import { useMe } from "../session.tsx";

const ERRORS: Record<string, string> = {
  oidc_failed: "Signing in with that provider did not work. Try again or use your email.",
  oidc_no_account: "No OnHold account uses that address. Ask for an invitation first.",
};

/** Only paths on this site, so a link cannot send people elsewhere after signing in. */
export function safeRedirect(target: string | undefined): string {
  return target && target.startsWith("/") && !target.startsWith("//") ? target : "/";
}

function AuthCard({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex min-h-dvh items-center justify-center bg-muted/40 p-4">
      <Card className="w-full max-w-sm p-6">
        <div className="mb-6 flex items-center gap-2">
          <img src="/favicon.svg" alt="" className="size-7" />
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
      await qc.invalidateQueries({ queryKey: ["me"] });
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
        await qc.invalidateQueries({ queryKey: ["me"] });
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
      await qc.invalidateQueries({ queryKey: ["me"] });
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
      await qc.invalidateQueries({ queryKey: ["me"] });
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
    <AuthCard title={`Join ${inv.workspace_name}`}>
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
