import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { api, ApiError, type S } from "../api/client";
import { cancelled, getCredential, webauthnSupported } from "../api/webauthn";
import { useProviders, useSetup } from "../prefetch";
import { useSession } from "../session";
import { ErrorBox, Field, PasswordInput } from "../components/ui";
import { Logo } from "../components/Logo";
import { ThemeButton } from "../shell/Shell";

/** `?next=` only ever leads to a path on this site (no `//host`, no `/\host`). */
export function safeNext(next: string | null): string {
  if (!next || !next.startsWith("/") || next.startsWith("//") || next.includes("\\") || /[\u0000-\u001f]/.test(next)) return "/";
  return next;
}


/** The frame of the pages that let someone in: the mark, a title and the form
 *  on a card, over paper that glows like a forge. Signed in (a confirmation
 *  link), it is the plain card. */
function AuthLayout({ title, lede, children }: { title: string; lede?: ReactNode; children: ReactNode }) {
  const { user } = useSession();
  if (user) {
    return (
      <div className="page narrow auth-page">
        <Logo size={36} className="auth-logo" />
        <h1>{title}</h1>
        {children}
      </div>
    );
  }
  return (
    <div className="auth">
      <div className="auth-glow" aria-hidden="true" />
      <div className="auth-top">
        <ThemeButton />
      </div>
      <div className="auth-center">
        <div className="auth-head">
          <Link to="/" className="auth-tile" aria-label="Forgust">
            <Logo size={44} />
          </Link>
          <h1>{title}</h1>
          {lede && <p className="auth-lede">{lede}</p>}
        </div>
        <div className="auth-card">{children}</div>
        <p className="auth-foot">{window.location.host} · Forgust</p>
      </div>
    </div>
  );
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
  // what can finish it: the app, a security key, a recovery code
  const [factors, setFactors] = useState<S["SecondFactors"] | null>(null);
  const [code, setCode] = useState("");
  // a lost authenticator: one of the recovery codes instead
  const [recovery, setRecovery] = useState(false);
  const [error, setError] = useState<unknown>(params.get("error"));
  const [busy, setBusy] = useState(false);
  const providers = useProviders();
  const setup = useSetup();
  const passkeys = !!setup.data?.webauthn && webauthnSupported();

  // back to the password: another account, or a challenge that expired
  const restart = (why: string | null = null) => {
    setChallenge(null);
    setFactors(null);
    setRecovery(false);
    setCode("");
    setError(why);
  };
  // too slow, or used up: the code is not the problem, the password is asked again
  const expired = (err: unknown) => err instanceof ApiError && err.code === "challenge_expired";
  const EXPIRED = "Your sign-in expired. Enter your password again.";

  useEffect(() => {
    if (!challenge) return;
    api.post<S["SecondFactors"]>("/auth/login/second_factor", { challenge }).then(setFactors, (err) => (expired(err) ? restart(EXPIRED) : setError(err)));
  }, [challenge]);

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

  // `rejected`: what to say when the server answers its generic 401
  const attempt = async (f: () => Promise<S["LoginResponse"]>, rejected?: string) => {
    setBusy(true);
    setError(null);
    try {
      finish(await f());
    } catch (err) {
      if (cancelled(err)) return;
      if (expired(err)) restart(EXPIRED);
      else if (rejected && err instanceof ApiError && err.status === 401) setError(rejected);
      else setError(err);
    } finally {
      setBusy(false);
    }
  };

  const submit = (e: FormEvent) => {
    e.preventDefault();
    attempt(() =>
      challenge
        ? api.post<S["LoginResponse"]>(recovery ? "/auth/login/recovery" : "/auth/login/totp", { challenge, code })
        : api.post<S["LoginResponse"]>("/auth/login", { login, password }),
      !challenge
        ? "Wrong username or password."
        : recovery
          ? "That recovery code is not valid, or it has been used."
          : "That code is not valid. Try the current one.",
    );
  };

  const securityKey = () =>
    attempt(async () => {
      // each try asks a fresh question (it replaces the last one)
      const { options } = await api.post<S["WebauthnChallenge"]>("/auth/login/webauthn/start", { challenge });
      const credential = await getCredential(options);
      return api.post<S["LoginResponse"]>("/auth/login/webauthn", { challenge, credential });
    });

  const passkey = () =>
    attempt(async () => {
      const start = await api.post<S["PasskeyStart"]>("/auth/passkey/start");
      const credential = await getCredential(start.options);
      return api.post<S["LoginResponse"]>("/auth/passkey/finish", { ceremony: start.ceremony, credential });
    });

  if (setup.data?.needs_setup) {
    return (
      <AuthLayout title="Welcome to Forgust" lede="No account exists yet.">
        <p className="muted">
          Open the setup link printed in the server log (<code>/setup?token=…</code>) to create the first administrator, or run{" "}
          <code>forgust admin create-user --admin</code>.
        </p>
      </AuthLayout>
    );
  }

  const withKey = !!factors?.security_key && webauthnSupported();
  // the code field: the app's, or a recovery code; not when a key is all there is
  const codeField = recovery || !factors || factors.totp;

  return (
    <AuthLayout
      title={challenge ? "Two-factor authentication" : "Sign in to Forgust"}
      lede={
        !challenge
          ? undefined
          : recovery
            ? "Enter one of your recovery codes."
            : withKey && factors?.totp
              ? "Use your security key, or enter the code from your authenticator app."
              : withKey
                ? "Use your security key."
                : "Enter the code from your authenticator app."
      }
    >
      <form className="stack auth-stack" onSubmit={submit}>
        <ErrorBox error={error} />
        {challenge ? (
          <>
            {withKey && !recovery && (
              <button type="button" className={`btn auth-submit${factors?.totp ? "" : " primary"}`} aria-busy={busy} disabled={busy} onClick={securityKey}>
                Use your security key
              </button>
            )}
            {codeField && (
              <Field
                label={recovery ? "Recovery code" : "Authentication code"}
                hint={recovery ? "One of the codes you saved when you set up two-factor authentication; it works once" : "From your authenticator app"}
              >
                <input
                  key={String(recovery)}
                  autoFocus
                  className={recovery ? undefined : "auth-code"}
                  inputMode={recovery ? undefined : "numeric"}
                  autoComplete={recovery ? "off" : "one-time-code"}
                  spellCheck={false}
                  value={code}
                  onChange={(e) => setCode(e.target.value)}
                />
              </Field>
            )}
            {(recovery || factors?.recovery_code) && (
              <button
                type="button"
                className="auth-back"
                onClick={() => {
                  setRecovery(!recovery);
                  setCode("");
                  setError(null);
                }}
              >
                {recovery ? "Back" : `Lost your ${withKey && !factors?.totp ? "security key" : "authenticator"}? Use a recovery code`}
              </button>
            )}
          </>
        ) : (
          <>
            <Field label="Username or email">
              <input autoFocus autoComplete="username" value={login} onChange={(e) => setLogin(e.target.value)} />
            </Field>
            <div className="auth-pw">
              <Field label="Password">
                <PasswordInput autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} />
              </Field>
              <Link className="auth-forgot" to={`/forgot-password${login ? `?login=${encodeURIComponent(login)}` : ""}`}>
                Forgot password?
              </Link>
            </div>
          </>
        )}
        {(!challenge || codeField) && (
          <button className="btn primary auth-submit" aria-busy={busy} disabled={busy}>
            {challenge ? "Verify" : "Sign in"}
          </button>
        )}
        {challenge && (
          <button type="button" className="auth-back" onClick={() => restart()}>
            Use another account
          </button>
        )}
        {!challenge && (passkeys || !!providers.data?.length) && (
          <>
            <div className="auth-or">
              <span>or</span>
            </div>
            {passkeys && (
              <button type="button" className="btn auth-submit" disabled={busy} onClick={passkey}>
                Sign in with a passkey
              </button>
            )}
            {providers.data?.map((p) => (
              <a key={p.name} className="btn auth-submit" href={`/api/v1/auth/oidc/${p.name}/start`}>
                Continue with {p.display_name}
              </a>
            ))}
          </>
        )}
      </form>
    </AuthLayout>
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
    <AuthLayout title="Create the first administrator" lede="Set up the account that runs this instance.">
      <form className="stack auth-stack" onSubmit={submit}>
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
          <PasswordInput autoComplete="new-password" value={form.password} onChange={set("password")} />
        </Field>
        <button className="btn primary auth-submit">Create administrator</button>
      </form>
    </AuthLayout>
  );
}

/** Ask for a reset link (the answer is the same whether the account exists). */
export function ForgotPassword() {
  const [params] = useSearchParams();
  const [login, setLogin] = useState(params.get("login") ?? "");
  const [sent, setSent] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const setup = useSetup();
  return (
    <AuthLayout title="Reset your password">
      {setup.data?.email_enabled === false ? (
        <div className="stack">
          <p>This instance sends no email, so it cannot send a reset link. Ask an administrator to set a new password for you.</p>
          <Link to="/login">Back to sign in</Link>
        </div>
      ) : sent ? (
        <div className="stack">
          <p>If an account matches “{login}”, we sent a link to its email address. It works once, within an hour.</p>
          <Link to="/login">Back to sign in</Link>
        </div>
      ) : (
        <form
          className="stack auth-stack"
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
          <button className="btn primary auth-submit" aria-busy={busy} disabled={busy || !login.trim() || !setup.data}>
            Send the link
          </button>
          <Link className="small" to="/login">
            Back to sign in
          </Link>
        </form>
      )}
    </AuthLayout>
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
    <AuthLayout title="Choose a new password">
      {done ? (
        <div className="stack">
          <p>Your password is changed, and every session of your account was signed out.</p>
          <Link className="btn primary" to="/login">
            Sign in
          </Link>
        </div>
      ) : (
        <form
          className="stack auth-stack"
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
            <PasswordInput autoFocus autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} />
          </Field>
          <Field label="Repeat it">
            <PasswordInput autoComplete="new-password" value={again} onChange={(e) => setAgain(e.target.value)} aria-invalid={mismatch} />
          </Field>
          {mismatch && <span className="small danger-text">The two passwords differ.</span>}
          <button className="btn primary auth-submit" aria-busy={busy} disabled={busy || !token || password.length < 8 || password !== again}>
            Set the password
          </button>
        </form>
      )}
    </AuthLayout>
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
    <AuthLayout title="Confirm your email">
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
    </AuthLayout>
  );
}
