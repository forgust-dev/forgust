import { useEffect, useRef, useState, type FormEvent } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { api, type S } from "../api/client";
import { useSession } from "../session";
import { ErrorBox, Field } from "../components/ui";

/** `?next=` only ever leads to a path on this site (no `//host`, no `/\host`). */
export function safeNext(next: string | null): string {
  if (!next || !next.startsWith("/") || next.startsWith("//") || next.includes("\\") || /[\u0000-\u001f]/.test(next)) return "/";
  return next;
}

export function Login() {
  const { setSession } = useSession();
  const nav = useNavigate();
  const [params] = useSearchParams();
  const next = safeNext(params.get("next"));
  const [login, setLogin] = useState("");
  const [password, setPassword] = useState("");
  // single sign-on of an account with 2FA comes back here for the code
  const [challenge, setChallenge] = useState<string | null>(() => {
    const m = /^#totp=([A-Za-z0-9_-]+)$/.exec(window.location.hash);
    if (m) history.replaceState(null, "", window.location.pathname + window.location.search);
    return m ? m[1] : null;
  });
  const [code, setCode] = useState("");
  const [error, setError] = useState<unknown>(params.get("error"));
  const [busy, setBusy] = useState(false);
  const providers = useQuery({ queryKey: ["providers"], queryFn: () => api.get<S["Provider"][]>("/auth/providers") });
  const setup = useQuery({ queryKey: ["setup"], queryFn: () => api.get<S["SetupStatus"]>("/setup") });

  const finish = (r: S["LoginResponse"]) => {
    if (r.two_factor_required) {
      setChallenge(r.challenge ?? null);
      return;
    }
    if (r.session) {
      setSession(r.session);
      nav(next, { replace: true });
    }
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      if (challenge) finish(await api.post<S["LoginResponse"]>("/auth/login/totp", { challenge, code }));
      else finish(await api.post<S["LoginResponse"]>("/auth/login", { login, password }));
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  if (setup.data?.needs_setup) {
    return (
      <div className="page narrow">
        <h1>Welcome to Forgust</h1>
        <p className="muted">
          No account exists yet. Open the setup link printed in the server log (<code>/setup?token=…</code>) to create the first
          administrator, or run <code>forgust admin create-user --admin</code>.
        </p>
      </div>
    );
  }

  return (
    <div className="page narrow auth-page">
      <img src="/favicon.svg" width={36} height={36} alt="" className="auth-logo" />
      <h1>Sign in to Forgust</h1>
      <form className="stack" onSubmit={submit}>
        <ErrorBox error={error} />
        {challenge ? (
          <Field label="Authentication code" hint="From your authenticator app">
            <input autoFocus inputMode="numeric" autoComplete="one-time-code" value={code} onChange={(e) => setCode(e.target.value)} />
          </Field>
        ) : (
          <>
            <Field label="Username or email">
              <input autoFocus autoComplete="username" value={login} onChange={(e) => setLogin(e.target.value)} />
            </Field>
            <Field label="Password">
              <input type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} />
            </Field>
            <Link className="small" to={`/forgot-password${login ? `?login=${encodeURIComponent(login)}` : ""}`}>
              Forgot your password?
            </Link>
          </>
        )}
        <button className="btn primary" disabled={busy}>
          {challenge ? "Verify" : "Sign in"}
        </button>
        {!challenge &&
          providers.data?.map((p) => (
            <a key={p.name} className="btn" href={`/api/v1/auth/oidc/${p.name}/start`}>
              Sign in with {p.display_name}
            </a>
          ))}
      </form>
    </div>
  );
}

export function Setup() {
  const { setSession } = useSession();
  const nav = useNavigate();
  const [params] = useSearchParams();
  const [form, setForm] = useState({ token: params.get("token") ?? "", username: "", email: "", password: "" });
  const [error, setError] = useState<unknown>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    try {
      const r = await api.post<S["LoginResponse"]>("/setup", form);
      if (r.session) setSession(r.session);
      nav("/", { replace: true });
    } catch (err) {
      setError(err);
    }
  };
  const set = (k: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [k]: e.target.value });
  return (
    <div className="page narrow auth-page">
      <img src="/favicon.svg" width={36} height={36} alt="" className="auth-logo" />
      <h1>Create the first administrator</h1>
      <form className="stack" onSubmit={submit}>
        <ErrorBox error={error} />
        <Field label="Setup token" hint="Printed in the server log on first start">
          <input value={form.token} onChange={set("token")} />
        </Field>
        <Field label="Username">
          <input value={form.username} onChange={set("username")} />
        </Field>
        <Field label="Email">
          <input type="email" value={form.email} onChange={set("email")} />
        </Field>
        <Field label="Password" hint="At least 8 characters">
          <input type="password" value={form.password} onChange={set("password")} />
        </Field>
        <button className="btn primary">Create administrator</button>
      </form>
    </div>
  );
}

/** Ask for a reset link (the answer is the same whether the account exists). */
export function ForgotPassword() {
  const [params] = useSearchParams();
  const [login, setLogin] = useState(params.get("login") ?? "");
  const [sent, setSent] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  return (
    <div className="page narrow auth-page">
      <img src="/favicon.svg" width={36} height={36} alt="" className="auth-logo" />
      <h1>Reset your password</h1>
      {sent ? (
        <div className="stack">
          <p>If an account matches “{login}”, we sent a link to its email address. It works once, within an hour.</p>
          <Link to="/login">Back to sign in</Link>
        </div>
      ) : (
        <form
          className="stack"
          onSubmit={async (e) => {
            e.preventDefault();
            setBusy(true);
            setError(null);
            try {
              await api.post("/auth/password/forgot", { login });
              setSent(true);
            } catch (err) {
              setError(err);
            } finally {
              setBusy(false);
            }
          }}
        >
          <ErrorBox error={error} />
          <p className="muted">Enter your username or email address. We send a link to choose a new password.</p>
          <Field label="Username or email">
            <input autoFocus autoComplete="username" value={login} onChange={(e) => setLogin(e.target.value)} />
          </Field>
          <button className="btn primary" disabled={busy || !login.trim()}>
            Send the link
          </button>
          <Link className="small" to="/login">
            Back to sign in
          </Link>
        </form>
      )}
    </div>
  );
}

/** The emailed reset link: choose a new password. */
export function ResetPassword() {
  const [params] = useSearchParams();
  const token = params.get("token") ?? "";
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [done, setDone] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const mismatch = again !== "" && again !== password;
  return (
    <div className="page narrow auth-page">
      <img src="/favicon.svg" width={36} height={36} alt="" className="auth-logo" />
      <h1>Choose a new password</h1>
      {done ? (
        <div className="stack">
          <p>Your password is changed, and every session of your account was signed out.</p>
          <Link className="btn primary" to="/login">
            Sign in
          </Link>
        </div>
      ) : (
        <form
          className="stack"
          onSubmit={async (e) => {
            e.preventDefault();
            setBusy(true);
            setError(null);
            try {
              await api.post("/auth/password/reset", { token, password });
              setDone(true);
            } catch (err) {
              setError(err);
            } finally {
              setBusy(false);
            }
          }}
        >
          <ErrorBox error={error} />
          {!token && <p className="muted">This page needs the link from your email.</p>}
          <Field label="New password" hint="At least 8 characters">
            <input type="password" autoFocus autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} />
          </Field>
          <Field label="Repeat it">
            <input type="password" autoComplete="new-password" value={again} onChange={(e) => setAgain(e.target.value)} aria-invalid={mismatch} />
          </Field>
          {mismatch && <span className="small danger-text">The two passwords differ.</span>}
          <button className="btn primary" disabled={busy || !token || password.length < 8 || password !== again}>
            Set the password
          </button>
        </form>
      )}
    </div>
  );
}

/** The emailed confirmation link: confirmed on arrival by the signed-in
 *  account it belongs to (signed out, the page asks to sign in first). */
export function VerifyEmail() {
  const [params] = useSearchParams();
  const token = params.get("token") ?? "";
  const { user, loading, refresh } = useSession();
  const [state, setState] = useState<{ ok?: string; error?: unknown }>({});
  const ran = useRef(false);
  useEffect(() => {
    if (ran.current || !token || loading || !user) return;
    ran.current = true;
    api
      .post<{ email: string }>("/auth/email/confirm", { token })
      .then((r) => {
        setState({ ok: r.email });
        void refresh(); // the banner goes, the profile shows the new address
      })
      .catch((error) => setState({ error }));
  }, [token, loading, user]);
  const back = `/login?next=${encodeURIComponent(`/verify-email?token=${token}`)}`;
  return (
    <div className="page narrow auth-page">
      <img src="/favicon.svg" width={36} height={36} alt="" className="auth-logo" />
      <h1>Confirm your email</h1>
      {!token ? (
        <p className="muted">This page needs the link from your email.</p>
      ) : !loading && !user ? (
        <div className="stack">
          <p>Sign in to the account this address belongs to, and it is confirmed. If you did not create an account here, ignore the email.</p>
          <Link className="btn primary" to={back}>
            Sign in to confirm
          </Link>
        </div>
      ) : state.ok ? (
        <div className="stack">
          <p>
            <strong>{state.ok}</strong> is confirmed.
          </p>
          <Link className="btn primary" to={user ? "/" : "/login"}>
            {user ? "Continue" : "Sign in"}
          </Link>
        </div>
      ) : state.error ? (
        <div className="stack">
          <ErrorBox error={state.error} />
          <Link to={user ? "/profile" : "/login"}>{user ? "Send a new link from your profile" : "Sign in"}</Link>
        </div>
      ) : (
        <p className="muted">Confirming…</p>
      )}
    </div>
  );
}
