import { useState, type FormEvent } from "react";
import { Route, Routes, useLocation, useSearchParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, enc, timeAgo, type S } from "../api/client";
import { useSession } from "../session";
import { Avatar, Badge, Card, CopyLabel, ErrorBox, Field, NewSecret, Q, Section, useCopy, useFormRun, StateBadge } from "../components/ui";
import { ScopePicker, useCrudList } from "../components/Tokens";
import { ResendLink } from "../components/ResendLink";
import { OtherEmails } from "../components/UserEmails";
import { AvatarUpload } from "../components/AvatarUpload";
import { NotificationDefaults } from "../components/Notifications";
import { cancelled, createCredential, webauthnSupported } from "../api/webauthn";
import { profilePage } from "./profilePages";
import { useTopOnSwitch } from "../shell/scroll";
import { Ban, CircleCheck, Link2, ShieldCheck, ShieldOff, Unlink } from "lucide-react";

export function Profile() {
  const loc = useLocation();
  useTopOnSwitch(loc.pathname);
  // the pages are in the sidebar while you are here (shell/Shell.tsx): here, which one this is
  return (
    <div className="page">
      <div className="admin-head">
        <p className="eyebrow">Your settings</p>
        <h1>{profilePage(loc.pathname)?.label ?? "Your settings"}</h1>
      </div>
      <Routes>
        <Route index element={<ProfileForm />} />
        <Route path="keys" element={<SshKeys />} />
        <Route path="gpg_keys" element={<GpgKeys />} />
        <Route path="tokens" element={<Tokens />} />
        <Route path="security" element={<Security />} />
        <Route path="notifications" element={<NotificationDefaults />} />
      </Routes>
    </div>
  );
}

function ProfileForm() {
  const { user, emailEnabled, setSession, refresh } = useSession();
  const [name, setName] = useState(user?.name ?? "");
  const [email, setEmail] = useState(user?.email ?? "");
  const [password, setPassword] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  // without email the address changes at once: nothing waits for a link
  const pending = emailEnabled ? user?.unconfirmed_email : undefined;
  const emailChanges = email.trim().toLowerCase() !== (user?.email ?? "").toLowerCase() && email.trim().toLowerCase() !== (pending ?? "").toLowerCase();
  const emailHint = !emailEnabled
    ? "This instance sends no email: a new address takes effect at once, with no confirmation link."
    : pending
      ? undefined
      : user?.email_verified === false
        ? "Not confirmed yet: emails wait until you open the link we sent."
        : "Changing it sends a confirmation link to the new address; the current one stays until then.";
  const [error, setError] = useState<unknown>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    try {
      // a link goes out for a new address only, not again for the one waiting
      const sent = emailChanges;
      await api.put("/user", { name, email, current_password: password });
      setPassword("");
      const s = await api.get<S["SessionInfo"]>("/session");
      setSession(s);
      setMsg(emailEnabled && sent && s.user.unconfirmed_email ? `Saved. We sent a confirmation link to ${s.user.unconfirmed_email}.` : "Saved.");
    } catch (err) {
      setError(err);
    }
  };
  return (
    <div className="settings-page">
      <Section title="Picture" description="Shown next to your name across Forgust.">
        <AvatarUpload
          preview={<Avatar name={user?.name || user?.username || "?"} src={user?.avatar_url} className="big" />}
          path="/user/avatar"
          hasOne={!!user?.avatar_url}
          onChanged={() => void refresh()}
          what="next to your name"
        />
      </Section>
      <Section title="Profile" description="Your name and email address, as other people see them.">
        <form className="stack" style={{ maxWidth: 520 }} onSubmit={submit}>
          <ErrorBox error={error} />
          {msg && <div className="alert ok">{msg}</div>}
          <Field label="Username">
            <input value={user?.username ?? ""} disabled />
          </Field>
          <Field label="Name">
            <input value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label="Email" hint={emailHint}>
            <input type="email" value={email} onChange={(e) => setEmail(e.target.value)} />
          </Field>
          {emailChanges && (
            <Field label="Current password" hint="Needed to change the address (single sign-on accounts: leave empty within 10 minutes of signing in)">
              <input type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} />
            </Field>
          )}
          {pending && (
            <div className="alert">
              <strong>{pending}</strong> is waiting for confirmation: open the link we sent there. Until then, {user?.email} stays your
              address. <ResendLink />
            </div>
          )}
          <div>
            <button className="btn primary">Save</button>
          </div>
        </form>
      </Section>
      <Section
        title="Other email addresses"
        description="Commits made with a confirmed one show as yours, and imported contributions under it become yours. Emails still go to your address above."
      >
        <OtherEmails />
      </Section>
    </div>
  );
}

/** OpenPGP keys: commits signed with them, by your confirmed address, show as verified. */
function GpgKeys() {
  const [key, setKey] = useState("");
  const { list, add, remove: del, error, setError } = useCrudList<S["GpgKey"]>(["gpg-keys"], "/user/gpg_keys", () => {
    setKey("");
    setError(null);
  });
  return (
    <div className="settings-page">
      <Section
        title="GPG keys"
        description="Commits you sign with these keys show as verified when they are committed with your confirmed email address (and the key lists it)."
      >
        <div className="stack">
          <Card title="Add a GPG key">
            <form
              className="stack"
              onSubmit={(e) => {
                e.preventDefault();
                add.mutate({ key });
              }}
            >
              <ErrorBox error={error} />
              <Field label="Public key" hint="The output of gpg --armor --export <your key id>">
                <textarea value={key} onChange={(e) => setKey(e.target.value)} placeholder="-----BEGIN PGP PUBLIC KEY BLOCK-----" rows={6} />
              </Field>
              <div>
                <button className="btn primary" disabled={!key.trim()}>
                  Add key
                </button>
              </div>
            </form>
          </Card>
          <Q q={list}>
            {(keys) =>
              keys.length === 0 ? (
                <p className="muted small" style={{ margin: 0 }}>No GPG keys yet.</p>
              ) : (
                <div className="list">
                  {keys.map((k) => (
                    <div className="item" key={k.id}>
                      <div className="grow">
                        <strong className="mono">{k.fingerprint.replace(/(.{4})/g, "$1 ").trim()}</strong>
                        <div className="small faint">{k.emails.length ? k.emails.join(", ") : "no email in the key"}</div>
                      </div>
                      <span className="small faint">added {timeAgo(k.created_at)}</span>
                      <button className="btn small danger" onClick={() => del.mutate(k.id)}>
                        Delete
                      </button>
                    </div>
                  ))}
                </div>
              )
            }
          </Q>
        </div>
      </Section>
    </div>
  );
}

/** The comment of an OpenSSH public key line (`ssh-ed25519 AAAA… you@laptop` → `you@laptop`), of the first one pasted. */
const keyComment = (line: string) => line.trim().split("\n")[0].trim().split(/\s+/).slice(2).join(" ");

const KEY_USAGE: Record<S["KeyUsage"], string> = {
  auth_and_signing: "Authentication and signing",
  auth: "Authentication",
  signing: "Signing",
};

/** Where ssh-keygen puts a public key of this type (`ssh-rsa AAAA…` → `~/.ssh/id_rsa.pub`). */
const KEY_FILES: Record<string, string> = {
  "ssh-ed25519": "id_ed25519",
  "ssh-rsa": "id_rsa",
  "ecdsa-sha2-nistp256": "id_ecdsa",
  "ecdsa-sha2-nistp384": "id_ecdsa",
  "ecdsa-sha2-nistp521": "id_ecdsa",
  "sk-ssh-ed25519@openssh.com": "id_ed25519_sk",
  "sk-ecdsa-sha2-nistp256@openssh.com": "id_ecdsa_sk",
};
const keyFile = (line: string) => {
  const type = line.trim().split(/\s+/)[0];
  // own keys only: a word such as "constructor" is not a key type
  return `~/.ssh/${Object.hasOwn(KEY_FILES, type) ? KEY_FILES[type] : "id_ed25519"}.pub`;
};

function SshKeys() {
  const [title, setTitle] = useState("");
  const [key, setKey] = useState("");
  const [usage, setUsage] = useState<S["KeyUsage"]>("auth_and_signing");
  const { list, add, remove: del, error, setError } = useCrudList<S["SshKey"]>(["ssh-keys"], "/user/keys", () => {
    setTitle("");
    setKey("");
    setUsage("auth_and_signing");
    setError(null);
  });
  return (
    <div className="settings-page">
      <Section
        title="SSH keys"
        description="Public keys that let you push and pull over SSH as yourself (authentication), and that verify the commits you sign with them (signing)."
      >
        <div className="stack">
          <Card title="Add an SSH key">
            <form
              className="stack"
              onSubmit={(e) => {
                e.preventDefault();
                add.mutate({ title, key, usage_type: usage });
              }}
            >
              <ErrorBox error={error} />
              <Field label="Key" hint="Contents of ~/.ssh/id_ed25519.pub (or another public key)">
                <textarea
                  value={key}
                  onChange={(e) => {
                    // the title follows the key's comment until you type your own
                    if (!title || title === keyComment(key)) setTitle(keyComment(e.target.value));
                    setKey(e.target.value);
                  }}
                  placeholder="ssh-ed25519 AAAA… you@laptop"
                />
              </Field>
              <Field label="Title" hint="Filled in from the key's comment; change it if you like">
                <input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="laptop" />
              </Field>
              <Field
                label="Use for"
                hint={
                  usage === "auth" ? (
                    "For signing in only: commits signed with this key show as unverified."
                  ) : (
                    <>
                      To sign commits with this key: <code>git config gpg.format ssh</code>, <code>git config user.signingkey {keyFile(key)}</code>, then{" "}
                      <code>git commit -S</code>. Signed commits show as verified when you commit with your confirmed email.
                    </>
                  )
                }
              >
                <select value={usage} onChange={(e) => setUsage(e.target.value as S["KeyUsage"])}>
                  {Object.entries(KEY_USAGE).map(([value, label]) => (
                    <option key={value} value={value}>
                      {label}
                    </option>
                  ))}
                </select>
              </Field>
              <div>
                <button className="btn primary" disabled={!key}>
                  Add key
                </button>
              </div>
            </form>
          </Card>
          <Q q={list}>
            {(keys) =>
              keys.length === 0 ? (
                <p className="muted small" style={{ margin: 0 }}>No SSH keys yet.</p>
              ) : (
                <div className="list">
                  {keys.map((k) => (
                    <div className="item" key={k.id}>
                      <div className="grow">
                        <strong>{k.title}</strong> <Badge>{KEY_USAGE[k.usage_type]}</Badge>
                        <div className="small faint mono">{k.fingerprint}</div>
                      </div>
                      {/* signing in is the use we see: verifying commits does not count */}
                      {k.usage_type !== "signing" && (
                        <span className="small faint">{k.last_used_at ? `last signed in ${timeAgo(k.last_used_at)}` : "never signed in"}</span>
                      )}
                      <button className="btn small danger" onClick={() => del.mutate(k.id)}>
                        Delete
                      </button>
                    </div>
                  ))}
                </div>
              )
            }
          </Q>
        </div>
      </Section>
    </div>
  );
}

function Tokens() {
  const { user: me } = useSession();
  const [name, setName] = useState("");
  const [scopes, setScopes] = useState<string[]>(["read_repository", "write_repository"]);
  const [expires, setExpires] = useState("");
  const [created, setCreated] = useState<S["NewToken"] | null>(null);
  const { list, add: create, remove: revoke, error } = useCrudList<S["AccessToken"], S["NewToken"]>(["pats"], "/user/tokens", (t) => {
    setCreated(t);
    setName("");
  });
  return (
    <div className="settings-page">
      <Section title="Personal access tokens" description="Tokens for scripts, the API and Git over HTTPS, acting as you within their scopes.">
        <div className="stack">
          <Card title="New personal access token">
            <form
              className="stack"
              onSubmit={(e) => {
                e.preventDefault();
                create.mutate({ name, scopes, expires_at: expires ? new Date(expires).toISOString() : null });
              }}
            >
              <ErrorBox error={error} />
              {created && <NewSecret token={created.token} />}
              <div className="grid2">
                <Field label="Name">
                  <input value={name} onChange={(e) => setName(e.target.value)} />
                </Field>
                <Field label="Expires">
                  <input type="date" value={expires} onChange={(e) => setExpires(e.target.value)} />
                </Field>
              </div>
              <ScopePicker
                scopes={scopes}
                onChange={setScopes}
                options={["api", "read_api", "read_user", "read_repository", "write_repository", "mcp", ...(me?.is_admin ? ["admin_mode"] : [])]}
              />
              <p className="muted small" style={{ margin: 0 }}>
                Use the token as your password for git over HTTPS once two-factor authentication is on. For an AI assistant, connect it to{" "}
                <code>{`${location.origin}/api/v4/mcp`}</code> with a <code>mcp</code> token (read-only) or an <code>api</code> one (it can change things too), or
                let it sign in through the browser.
              </p>
              <div>
                <button className="btn primary" disabled={!name}>
                  Create token
                </button>
              </div>
            </form>
          </Card>
          <Q q={list}>
            {(tokens) =>
              tokens.length === 0 ? (
                <p className="muted">No tokens.</p>
              ) : (
                <div className="list">
                  {tokens.map((t) => (
                    <div className="item" key={t.id}>
                      <div className="grow">
                        <strong>{t.name}</strong>
                        <div className="small muted">
                          {t.scopes.join(", ")}
                          {t.mcp_used_at && <> · used by an AI assistant (MCP) {timeAgo(t.mcp_used_at)}</>}
                        </div>
                      </div>
                      {t.expires_at && <span className="small faint">expires {new Date(t.expires_at).toLocaleDateString()}</span>}
                      {t.revoked ? (
                        <StateBadge tone="neutral" icon={Ban}>
                          Revoked
                        </StateBadge>
                      ) : (
                        <button className="btn small danger" onClick={() => revoke.mutate(t.id)}>
                          Revoke
                        </button>
                      )}
                    </div>
                  ))}
                </div>
              )
            }
          </Q>
        </div>
      </Section>
    </div>
  );
}

function Security() {
  const { user, setSession } = useSession();
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [setup, setSetup] = useState<S["TotpSetup"] | null>(null);
  const [code, setCode] = useState("");
  const [disablePw, setDisablePw] = useState("");
  // just made: shown this once
  const [codes, setCodes] = useState<string[] | null>(null);
  const [codesPw, setCodesPw] = useState("");
  const queryClient = useQueryClient();
  // the account changed: its session (2FA state) and its count of codes left
  const refresh = async () => {
    setSession(await api.get<S["SessionInfo"]>("/session"));
    await queryClient.invalidateQueries({ queryKey: ["recovery_codes"] });
  };
  const { run, msg, error } = useFormRun();
  return (
    <div className="settings-page">
      <ErrorBox error={error} />
      {msg && <div className="alert ok">{msg}</div>}
      <Section title="Password" description="Used to sign in with a username and password.">
        <form
          className="stack"
          onSubmit={run(async () => {
            await api.put("/user/password", { current, new: next });
            setCurrent("");
            setNext("");
          }, "Password changed.")}
        >
          <Field label="Current password">
            <input type="password" value={current} onChange={(e) => setCurrent(e.target.value)} />
          </Field>
          <Field label="New password" hint="At least 8 characters">
            <input type="password" value={next} onChange={(e) => setNext(e.target.value)} />
          </Field>
          <div>
            <button className="btn primary">Change password</button>
          </div>
        </form>
      </Section>
      <Section
        title={
          <>
            Two-factor authentication{" "}
            {user?.two_factor_enabled ? (
              <StateBadge tone="ok" icon={ShieldCheck}>
                Enabled
              </StateBadge>
            ) : (
              <StateBadge tone="neutral" icon={ShieldOff}>
                Off
              </StateBadge>
            )}
          </>
        }
        description="Asked for after your password: a code from an authenticator app, or a security key. Some organizations require it."
      >
        <div className="stack">
          {codes && <NewRecoveryCodes codes={codes} onDone={() => setCodes(null)} />}
          <Card
            title={
              <>
                Authenticator app{" "}
                {user?.totp_enabled && (
                  <StateBadge tone="ok" icon={CircleCheck}>
                    On
                  </StateBadge>
                )}
              </>
            }
          >
            {user?.totp_enabled ? (
              <form
                className="stack"
                onSubmit={run(async () => {
                  await api.post("/user/two_factor/disable", { password: disablePw });
                  setDisablePw("");
                  await refresh();
                }, "Authenticator app turned off.")}
              >
                <Field label="Confirm with your password">
                  <input type="password" autoComplete="current-password" value={disablePw} onChange={(e) => setDisablePw(e.target.value)} />
                </Field>
                <div>
                  <button className="btn danger">Turn off the app</button>
                </div>
              </form>
            ) : setup ? (
              <form
                className="stack"
                onSubmit={run(async () => {
                  const r = await api.post<S["TotpEnabled"]>("/user/two_factor/enable", { code, password: disablePw });
                  if (r.recovery_codes.length) setCodes(r.recovery_codes);
                  setSetup(null);
                  setDisablePw("");
                  await refresh();
                }, "Two-factor authentication enabled.")}
              >
                <p style={{ margin: 0 }}>Add this key to your authenticator app, then enter the 6-digit code it shows.</p>
                <code style={{ wordBreak: "break-all" }}>{setup.secret}</code>
                <a href={setup.otpauth_url} className="small">
                  Open in authenticator app
                </a>
                <Field label="Code">
                  <input inputMode="numeric" autoComplete="one-time-code" value={code} onChange={(e) => setCode(e.target.value)} />
                </Field>
                <div>
                  <button className="btn primary">Enable</button>
                </div>
              </form>
            ) : (
              <form
                className="stack"
                onSubmit={run(async () => setSetup(await api.post<S["TotpSetup"]>("/user/two_factor/setup", { password: disablePw })), "")}
              >
                <p className="muted" style={{ margin: 0 }}>
                  A time-based code from an app on your phone.
                </p>
                <Field label="Confirm with your password" hint="Accounts without a password (single sign-on): leave empty within 10 minutes of signing in">
                  <input type="password" autoComplete="current-password" value={disablePw} onChange={(e) => setDisablePw(e.target.value)} />
                </Field>
                <div>
                  <button className="btn primary">Set up 2FA</button>
                </div>
              </form>
            )}
          </Card>
          <WebauthnCard kind="security_key" onCodes={setCodes} onChange={refresh} />
          {user?.two_factor_enabled && (
            <Card title="Recovery codes">
              <form
                className="stack"
                onSubmit={run(async () => {
                  const r = await api.post<S["NewRecoveryCodes"]>("/user/two_factor/recovery_codes", { password: codesPw });
                  setCodes(r.recovery_codes);
                  setCodesPw("");
                  await queryClient.invalidateQueries({ queryKey: ["recovery_codes"] });
                }, "")}
              >
                <RecoveryCodesLeft />
                <Field label="Confirm with your password">
                  <input type="password" autoComplete="current-password" value={codesPw} onChange={(e) => setCodesPw(e.target.value)} />
                </Field>
                <div>
                  <button className="btn">New recovery codes</button>
                </div>
              </form>
            </Card>
          )}
        </div>
      </Section>
      <Section title="Passkeys" description="Sign in with your fingerprint, face or device PIN instead of your password. A passkey stays on your device or in your password manager.">
        <WebauthnCard kind="passkey" onCodes={setCodes} onChange={refresh} />
      </Section>
      <SignInProviders />
    </div>
  );
}

type CredentialKind = S["CredentialKind"];

/** What each kind is called, and how it is explained. */
const KINDS: Record<CredentialKind, { label: string; title: string; empty: string; hint: string }> = {
  security_key: {
    label: "security key",
    title: "Security keys",
    empty: "No security keys yet. Add one: you touch it after your password.",
    hint: "To tell your keys apart: “YubiKey 5C”",
  },
  passkey: { label: "passkey", title: "Your passkeys", empty: "No passkeys yet.", hint: "Where it is kept: “Phone”, “Laptop”" },
};

/** Security keys or passkeys: the list (rename, remove) and adding one. */
function WebauthnCard({ kind, onCodes, onChange }: { kind: CredentialKind; onCodes: (c: string[]) => void; onChange: () => Promise<void> }) {
  const list = useQuery({ queryKey: ["webauthn"], queryFn: () => api.get<S["WebauthnCredentials"]>("/user/webauthn") });
  const [name, setName] = useState("");
  const [pw, setPw] = useState("");
  const { run, msg, error } = useFormRun(() => list.refetch());
  const copy = KINDS[kind];
  const add = run(async () => {
    const start = await api.post<S["WebauthnRegistration"]>("/user/webauthn/register/start", { kind, password: pw });
    let credential;
    try {
      credential = await createCredential(start.options);
    } catch (e) {
      throw cancelled(e) ? new Error(`Nothing was added: the ${copy.label} was not confirmed.`) : e;
    }
    const r = await api.post<S["WebauthnRegistered"]>("/user/webauthn/register/finish", { ceremony: start.ceremony, name, credential });
    if (r.recovery_codes.length) onCodes(r.recovery_codes);
    setName("");
    setPw("");
    await onChange();
  }, `${copy.label[0].toUpperCase()}${copy.label.slice(1)} added.`);
  return (
    <Q q={list}>
      {(data) => {
        if (!data.available || !webauthnSupported()) {
          return (
            <Card title={copy.title}>
              <p className="muted" style={{ margin: 0 }}>
                {!data.available ? "Security keys and passkeys need Forgust served over https; ask your administrator." : "This browser cannot use security keys or passkeys."}
              </p>
            </Card>
          );
        }
        const items = data.credentials.filter((c) => c.kind === kind);
        return (
          <Card title={<>{copy.title} {items.length > 0 && <span className="section-count">{items.length}</span>}</>}>
            <div className="stack">
              <ErrorBox error={error} />
              {msg && <div className="alert ok">{msg}</div>}
              {items.length === 0 ? (
                <p className="muted" style={{ margin: 0 }}>
                  {copy.empty}
                </p>
              ) : (
                <div className="list">
                  {items.map((c) => (
                    <CredentialRow key={c.id} credential={c} run={run} onChange={onChange} />
                  ))}
                </div>
              )}
              <form className="stack" onSubmit={add}>
                <div className="grid2">
                  <Field label="Name" hint={copy.hint}>
                    <input value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
                  </Field>
                  <Field label="Confirm with your password">
                    <input type="password" autoComplete="current-password" value={pw} onChange={(e) => setPw(e.target.value)} />
                  </Field>
                </div>
                <div>
                  <button className="btn primary" disabled={!name.trim()}>
                    Add a {copy.label}
                  </button>
                </div>
              </form>
            </div>
          </Card>
        );
      }}
    </Q>
  );
}

/** One security key or passkey: rename it, or remove it (the password again). */
function CredentialRow({
  credential: c,
  run,
  onChange,
}: {
  credential: S["CredentialInfo"];
  run: ReturnType<typeof useFormRun>["run"];
  onChange: () => Promise<void>;
}) {
  // renaming (the new name) or removing (the password), or neither
  const [edit, setEdit] = useState<{ mode: "rename" | "remove"; value: string } | null>(null);
  const value = edit?.value ?? "";
  const set = (v: string) => edit && setEdit({ ...edit, value: v });
  return (
    <div className="item wrap">
      <div className="grow">
        {edit?.mode === "rename" ? <input aria-label="New name" autoFocus value={value} onChange={(e) => set(e.target.value)} /> : <strong>{c.name}</strong>}
        <div className="small muted">
          added {timeAgo(c.created_at)} · {c.last_used_at ? `last used ${timeAgo(c.last_used_at)}` : "never used"}
        </div>
      </div>
      {edit === null ? (
        <>
          <button className="btn small" onClick={() => setEdit({ mode: "rename", value: c.name })}>
            Rename
          </button>
          <button className="btn small danger" onClick={() => setEdit({ mode: "remove", value: "" })}>
            Remove…
          </button>
        </>
      ) : (
        <>
          {edit.mode === "remove" && (
            <input type="password" aria-label="Your password" placeholder="Your password" autoComplete="current-password" value={value} onChange={(e) => set(e.target.value)} />
          )}
          <button
            className={`btn small ${edit.mode === "remove" ? "danger" : "primary"}`}
            onClick={run(
              async () => {
                if (edit.mode === "rename") await api.put(`/user/webauthn/${c.id}`, { name: value });
                else {
                  await api.post(`/user/webauthn/${c.id}/remove`, { password: value });
                  await onChange();
                }
                setEdit(null);
              },
              edit.mode === "rename" ? "Renamed." : `${c.name} removed.`,
            )}
          >
            {edit.mode === "rename" ? "Save" : "Remove"}
          </button>
          <button className="btn small" onClick={() => setEdit(null)}>
            Cancel
          </button>
        </>
      )}
    </div>
  );
}

/** Codes just made: to save now, they are not shown again. */
function NewRecoveryCodes({ codes, onDone }: { codes: string[]; onDone: () => void }) {
  const [copied, copy] = useCopy();
  const text = codes.join("\n") + "\n";
  return (
    <div className="alert ok stack">
      <strong>Save your recovery codes</strong>
      <p style={{ margin: 0 }}>
        Each one signs you in once if you lose your authenticator. Keep them somewhere safe, apart from your password: they are not shown again.
      </p>
      <pre className="recovery-codes">{text}</pre>
      <div className="row">
        <button type="button" className="btn small" onClick={() => copy(text)}>
          <CopyLabel copied={copied} />
        </button>
        <a className="btn small" download="forgust-recovery-codes.txt" href={`data:text/plain;charset=utf-8,${encodeURIComponent(text)}`}>
          Download
        </a>
        <button type="button" className="btn small" onClick={onDone}>
          I saved them
        </button>
      </div>
    </div>
  );
}

function RecoveryCodesLeft() {
  const q = useQuery({ queryKey: ["recovery_codes"], queryFn: () => api.get<S["RecoveryCodesLeft"]>("/user/two_factor/recovery_codes") });
  if (!q.data) return null;
  const n = q.data.remaining;
  return (
    <p className={`small ${n < 3 ? "" : "muted"}`} style={{ margin: 0 }}>
      {n === 0 ? "No recovery codes left: make new ones." : `${n} recovery code${n === 1 ? "" : "s"} left.`}
      {n > 0 && n < 3 && " Make new ones soon."}
    </p>
  );
}

/** Single sign-on providers linked to the account: link one (through its sign-in page) or unlink it. */
function SignInProviders() {
  const [params] = useSearchParams();
  const providers = useQuery({ queryKey: ["identities"], queryFn: () => api.get<S["LinkedProvider"][]>("/user/identities") });
  const [password, setPassword] = useState("");
  const { run, msg, error } = useFormRun(() => providers.refetch());
  if (!providers.data?.length) return null;
  const linked = providers.data.find((p) => p.name === params.get("linked"));
  return (
    <Section id="sign-in-providers" title="Single sign-on" description="Sign in through a provider instead of your password. Linking takes you to the provider to sign in there.">
      <div className="stack">
        <ErrorBox error={error ?? params.get("link_error")} />
        {msg ? <div className="alert ok">{msg}</div> : linked?.linked_at && !error && <div className="alert ok">{linked.display_name} is linked: you can sign in with it.</div>}
        <Field label="Confirm with your password" hint="Needed to link or unlink (accounts without a password: leave empty within 10 minutes of signing in)">
          <input type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} />
        </Field>
        {providers.data.map((p) => (
          <div key={p.name} className="row wrap">
            <strong>{p.display_name}</strong>
            {p.linked_at ? (
              <StateBadge tone="ok" icon={Link2}>
                Linked {timeAgo(p.linked_at)}
              </StateBadge>
            ) : (
              <StateBadge tone="neutral" icon={Unlink}>
                Not linked
              </StateBadge>
            )}
            <span style={{ flex: 1 }} />
            {p.linked_at ? (
              <button
                className="btn danger"
                onClick={run(
                  async () => {
                    await api.post(`/user/identities/${enc(p.name)}/unlink`, { password });
                    setPassword("");
                  },
                  `${p.display_name} unlinked.`,
                  {
                    title: `Stop signing in with ${p.display_name}?`,
                    action: `Unlink ${p.display_name}`,
                    danger: true,
                  },
                )}
              >
                Unlink
              </button>
            ) : (
              <button
                className="btn"
                onClick={run(async () => {
                  const { url } = await api.post<S["LinkStart"]>(`/user/identities/${enc(p.name)}/link`, { password });
                  window.location.assign(url);
                }, "")}
              >
                Link {p.display_name}
              </button>
            )}
          </div>
        ))}
      </div>
    </Section>
  );
}
