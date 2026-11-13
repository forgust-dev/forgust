import { useState, type FormEvent } from "react";
import { Route, Routes } from "react-router-dom";
import { api, timeAgo, type S } from "../api/client";
import { useSession } from "../session";
import { Avatar, Badge, Card, ErrorBox, Field, ListHead, NewSecret, Q, Tabs, Section, useFormRun } from "../components/ui";
import { ScopePicker, useCrudList } from "../components/Tokens";
import { ResendLink } from "../components/ResendLink";
import { AvatarUpload } from "../components/AvatarUpload";
import { NotificationDefaults } from "../components/Notifications";

export function Profile() {
  return (
    <div className="page">
      <ListHead title="Your settings" />
      <Tabs
        items={[
          { to: "/profile", label: "Profile", end: true },
          { to: "/profile/keys", label: "SSH keys" },
          { to: "/profile/gpg_keys", label: "GPG keys" },
          { to: "/profile/tokens", label: "Access tokens" },
          { to: "/profile/security", label: "Password & 2FA" },
          { to: "/profile/notifications", label: "Notifications" },
        ]}
      />
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
  const { user, setSession, refresh } = useSession();
  const [name, setName] = useState(user?.name ?? "");
  const [email, setEmail] = useState(user?.email ?? "");
  const [password, setPassword] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const emailChanges = email.trim().toLowerCase() !== (user?.email ?? "").toLowerCase() && email.trim().toLowerCase() !== (user?.unconfirmed_email ?? "").toLowerCase();
  const [error, setError] = useState<unknown>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api.put("/user", { name, email, current_password: password });
      setPassword("");
      const s = await api.get<S["SessionInfo"]>("/session");
      setSession(s);
      setMsg(s.user.unconfirmed_email ? `Saved. We sent a confirmation link to ${s.user.unconfirmed_email}.` : "Saved.");
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
          <Field
            label="Email"
            hint={
              user?.unconfirmed_email
                ? undefined
                : user?.email_verified === false
                  ? "Not confirmed yet: emails wait until you open the link we sent."
                  : "Changing it sends a confirmation link to the new address; the current one stays until then."
            }
          >
            <input type="email" value={email} onChange={(e) => setEmail(e.target.value)} />
          </Field>
          {emailChanges && (
            <Field label="Current password" hint="Needed to change the address (single sign-on accounts: leave empty within 10 minutes of signing in)">
              <input type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} />
            </Field>
          )}
          {user?.unconfirmed_email && (
            <div className="alert">
              <strong>{user.unconfirmed_email}</strong> is waiting for confirmation: open the link we sent there. Until then, {user.email} stays your
              address. <ResendLink />
            </div>
          )}
          <div>
            <button className="btn primary">Save</button>
          </div>
        </form>
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

/** The comment of an OpenSSH public key line (`ssh-ed25519 AAAA… you@laptop` → `you@laptop`). */
const keyComment = (line: string) => line.trim().split(/\s+/).slice(2).join(" ");

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
const keyFile = (line: string) => `~/.ssh/${KEY_FILES[line.trim().split(/\s+/)[0]] ?? "id_ed25519"}.pub`;

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
                  <>
                    To sign commits with this key: <code>git config gpg.format ssh</code>, <code>git config user.signingkey {keyFile(key)}</code>, then{" "}
                    <code>git commit -S</code>. Signed commits show as verified when you commit with your confirmed email.
                  </>
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
                        <strong>{k.title}</strong> <span className="badge">{KEY_USAGE[k.usage_type]}</span>
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
                        <Badge>revoked</Badge>
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
  const refresh = async () => setSession(await api.get<S["SessionInfo"]>("/session"));
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
      <Section title={<>Two-factor authentication {user?.two_factor_enabled ? <Badge tone="ok">enabled</Badge> : <Badge>off</Badge>}</>} description="A code from an authenticator app, asked for after your password.">
        {user?.two_factor_enabled ? (
          <form
            className="stack"
            onSubmit={run(async () => {
              await api.post("/user/two_factor/disable", { password: disablePw });
              await refresh();
            }, "Two-factor authentication disabled.")}
          >
            <Field label="Confirm with your password">
              <input type="password" value={disablePw} onChange={(e) => setDisablePw(e.target.value)} />
            </Field>
            <div>
              <button className="btn danger">Disable 2FA</button>
            </div>
          </form>
        ) : setup ? (
          <form
            className="stack"
            onSubmit={run(async () => {
              await api.post("/user/two_factor/enable", { code, password: disablePw });
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
            <Field label="Confirm with your password" hint="Accounts without a password (single sign-on): leave empty within 10 minutes of signing in">
              <input type="password" autoComplete="current-password" value={disablePw} onChange={(e) => setDisablePw(e.target.value)} />
            </Field>
            <div>
              <button className="btn primary">Enable</button>
            </div>
          </form>
        ) : (
          <div className="stack">
            <p className="muted" style={{ margin: 0 }}>
              Protect your account with a time-based code. Some organizations require it.
            </p>
            <div>
              <button className="btn primary" onClick={run(async () => setSetup(await api.post<S["TotpSetup"]>("/user/two_factor/setup")), "")}>
                Set up 2FA
              </button>
            </div>
          </div>
        )}
      </Section>
    </div>
  );
}
