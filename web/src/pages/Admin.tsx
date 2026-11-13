import { HostRules } from "../components/HostRules";
import { useState, type FormEvent } from "react";
import { Route, Routes } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, ApiError, qs, timeAgo, type S } from "../api/client";
import { useSession } from "../session";
import { Avatar, Badge, Card, ErrorBox, Field, FilterTabs, NotFound, Q, SearchField, Tabs, UserLink, useDebounced } from "../components/ui";
import { Menu, type MenuItem } from "../components/controls";
import { MoreHorizontal } from "lucide-react";
import { useAct, useToast } from "../shell/toast";
import { RunnersPanel } from "./project/ci/Runners";
import { AdminBackups, AdminHealth, AdminJobs, AdminOverview, AdminProjects, AdminStorage, BannerForm } from "./AdminTools";

export function Admin() {
  const { user } = useSession();
  if (!user?.is_admin) return <NotFound />;
  return (
    <div className="page">
      <h1>Administration</h1>
      <Tabs
        items={[
          { to: "/admin", label: "Overview", end: true },
          { to: "/admin/users", label: "Users" },
          { to: "/admin/projects", label: "Projects" },
          { to: "/admin/storage", label: "Storage" },
          { to: "/admin/runners", label: "Runners" },
          { to: "/admin/jobs", label: "Jobs" },
          { to: "/admin/health", label: "Health" },
          { to: "/admin/backups", label: "Backups" },
          { to: "/admin/settings", label: "Settings" },
          { to: "/admin/dependency-updates", label: "Dependency updates" },
          { to: "/admin/security-scanning", label: "Security scanning" },
          { to: "/admin/audit", label: "Audit log" },
        ]}
      />
      <Routes>
        <Route index element={<AdminOverview />} />
        <Route path="users" element={<Users />} />
        <Route path="storage" element={<AdminStorage />} />
        <Route path="projects" element={<AdminProjects />} />
        <Route path="backups" element={<AdminBackups />} />
        <Route path="jobs" element={<AdminJobs />} />
        <Route path="health" element={<AdminHealth />} />
        <Route
          path="runners"
          element={<RunnersPanel title="Instance runners" list="/admin/runners" create="/admin/runners" note="Shared runners: every project can use them unless it turns them off in its CI/CD settings." />}
        />
        <Route path="settings" element={<Settings />} />
        <Route path="dependency-updates" element={<DependencyUpdatesAdmin />} />
        <Route path="security-scanning" element={<SecurityScanningAdmin />} />
        <Route path="audit" element={<AuditLog />} />
      </Routes>
    </div>
  );
}

function Users() {
  const qc = useQueryClient();
  const [search, setSearch] = useState("");
  const term = useDebounced(search, 250);
  const [filter, setFilter] = useState<UserFilter>("all");
  const list = useQuery({
    queryKey: ["admin-users", term, filter],
    queryFn: () => api.get<S["AdminUser"][]>(`/admin/users${qs({ search: term, filter, per_page: 100 })}`),
  });
  const toast = useToast();
  // what they wrote stays (without an author); personal projects are asked about
  const deleteUser = async (u: S["AdminUser"]) => {
    const typed = window.prompt(
      `Delete ${u.username}'s account?\n\nTheir merge requests, issues, comments and pipelines stay, without an author. Their memberships, keys, tokens, approvals and to-dos go. This cannot be undone.\n\nType their username to confirm:`,
    );
    if (typed !== u.username) return;
    const path = `/admin/users/${u.id}`;
    try {
      await api.del(path);
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 409 && e.message.includes("personal projects"))) return toast.error(e);
      if (!window.confirm(`${e.message}.\n\nDelete those projects (repositories, issues, merge requests, CI) with the account?`)) return;
      try {
        await api.del(`${path}?delete_projects=true`);
      } catch (e2) {
        return toast.error(e2);
      }
    }
    toast.ok(`${u.username} deleted`);
    qc.invalidateQueries({ queryKey: ["admin-users"] });
  };
  const run = useAct(() => qc.invalidateQueries({ queryKey: ["admin-users"] }));
  const act = async (f: () => Promise<unknown>, done: string, confirmText?: string) => {
    if (confirmText && !window.confirm(confirmText)) return;
    await run(f, done);
  };
  const [form, setForm] = useState({ username: "", email: "", password: "", is_admin: false });
  const [error, setError] = useState<unknown>(null);
  const create = useMutation({
    mutationFn: () => api.post("/admin/users", form),
    onSuccess: () => {
      setForm({ username: "", email: "", password: "", is_admin: false });
      qc.invalidateQueries({ queryKey: ["admin-users"] });
    },
    onError: setError,
  });
  return (
    <div className="stack">
      <ErrorBox error={error} />
      <Card title="New user">
        <form
          className="stack"
          onSubmit={(e: FormEvent) => {
            e.preventDefault();
            create.mutate();
          }}
        >
          <div className="grid2">
            <Field label="Username">
              <input value={form.username} onChange={(e) => setForm({ ...form, username: e.target.value })} />
            </Field>
            <Field label="Email">
              <input type="email" value={form.email} onChange={(e) => setForm({ ...form, email: e.target.value })} />
            </Field>
            <Field label="Initial password" hint="Leave empty for SSO-only accounts">
              <input type="password" value={form.password} onChange={(e) => setForm({ ...form, password: e.target.value })} />
            </Field>
            <label className="check" style={{ alignSelf: "flex-end", paddingBottom: 8 }}>
              <input type="checkbox" checked={form.is_admin} onChange={(e) => setForm({ ...form, is_admin: e.target.checked })} />
              Administrator
            </label>
          </div>
          <div>
            <button className="btn primary" disabled={!form.username || !form.email}>
              Create user
            </button>
          </div>
        </form>
      </Card>
      <div className="list-toolbar">
        <FilterTabs label="Show" value={filter} onChange={setFilter} options={USER_FILTERS} />
        <SearchField value={search} onChange={setSearch} placeholder="Name, username or email" />
      </div>
      <Q q={list}>
        {(users) =>
          users.length === 0 ? (
            <p className="muted small">Nobody matches.</p>
          ) : (
            <div className="list">
              {users.map((u) => {
                const path = `/admin/users/${u.id}`;
                const human = u.user_type === "human";
                const items: MenuItem[] = human
                  ? [
                      { label: u.is_admin ? "Remove admin rights" : "Make administrator", onSelect: () => act(() => api.put(path, { is_admin: !u.is_admin }), u.is_admin ? "Admin rights removed" : "Now an administrator") },
                      { label: u.is_external ? "Make internal" : "Make external", disabled: u.is_admin && !u.is_external, onSelect: () => act(() => api.put(path, { is_external: !u.is_external }), u.is_external ? "Now internal" : "Now external") },
                      "separator",
                      { label: "Sign out everywhere", disabled: u.sessions === 0, onSelect: () => act(() => api.post(`${path}/sign_out`), "Signed out everywhere", `End ${u.username}'s ${u.sessions} session(s)?`) },
                      { label: "Revoke access tokens", disabled: u.tokens === 0, onSelect: () => act(() => api.post(`${path}/revoke_tokens`), "Tokens revoked", `Revoke ${u.username}'s ${u.tokens} personal access token(s)? Tools using them stop working.`) },
                      { label: "Reset two-factor authentication", disabled: !u.two_factor_enabled, onSelect: () => act(() => api.post(`${path}/reset_two_factor`), "Two-factor authentication reset", `Turn off ${u.username}'s two-factor authentication? Do it only once you know it is them (a lost authenticator).`) },
                      {
                        label: "Impersonate",
                        disabled: u.is_admin || u.state === "blocked",
                        onSelect: () =>
                          window.confirm(`Sign in as ${u.username} for up to an hour? Your own session ends; everything you do is audited as yours.`) &&
                          act(async () => {
                            await api.post(`${path}/impersonate`);
                            window.location.assign("/");
                          }, `Signed in as ${u.username}`),
                      },
                      { label: "Resend the confirmation email", disabled: u.email_verified, onSelect: () => act(() => api.post(`${path}/resend_confirmation`), "Confirmation email sent") },
                      "separator",
                      { label: "Delete account…", danger: true, onSelect: () => deleteUser(u) },
                      {
                        label: u.state === "blocked" ? "Unblock" : "Block",
                        danger: u.state !== "blocked",
                        onSelect: () => act(() => api.put(path, { state: u.state === "blocked" ? "active" : "blocked" }), u.state === "blocked" ? "Unblocked" : "Blocked", u.state === "blocked" ? undefined : `Block ${u.username}? They are signed out and cannot sign in, push or use tokens.`),
                      },
                    ]
                  : [];
                return (
                  <div className="item admin-user" key={u.id}>
                    <Avatar name={u.name || u.username} username={u.username} />
                    <div className="grow" style={{ minWidth: 0 }}>
                      <div className="row wrap" style={{ gap: 6 }}>
                        <UserLink username={u.username}>{u.name || u.username}</UserLink>
                        <span className="muted small">@{u.username}</span>
                        {u.is_admin && <Badge tone="accent">admin</Badge>}
                        {u.is_external && <Badge>external</Badge>}
                        {u.state === "blocked" && <Badge tone="warn">blocked</Badge>}
                        {!human && <Badge>{u.user_type.replace("_", " ")}</Badge>}
                        {human && u.two_factor_enabled && <Badge tone="ok">2FA</Badge>}
                      </div>
                      <div className="small muted ellipsis">
                        {u.email} {human && !u.email_verified && <span className="warn-text">· not confirmed</span>}
                      </div>
                    </div>
                    <span className="small faint nowrap hide-narrow" title={u.last_sign_in_at ? new Date(u.last_sign_in_at).toLocaleString() : undefined}>
                      {u.last_sign_in_at ? `signed in ${timeAgo(u.last_sign_in_at)}` : human ? "never signed in" : `created ${timeAgo(u.created_at)}`}
                    </span>
                    <span className="small faint nowrap hide-narrow">
                      {u.sessions} session{u.sessions === 1 ? "" : "s"} · {u.tokens} token{u.tokens === 1 ? "" : "s"}
                    </span>
                    {items.length > 0 && (
                      <Menu
                        trigger={
                          <button className="icon-btn bordered" aria-label={`Actions for ${u.username}`} title="Actions">
                            <MoreHorizontal size={15} />
                          </button>
                        }
                        items={items}
                      />
                    )}
                  </div>
                );
              })}
            </div>
          )
        }
      </Q>
    </div>
  );
}

type UserFilter = "all" | "admins" | "blocked" | "external" | "unconfirmed" | "two_factor_off" | "bots";
const USER_FILTERS: { value: UserFilter; label: string }[] = [
  { value: "all", label: "People" },
  { value: "admins", label: "Admins" },
  { value: "blocked", label: "Blocked" },
  { value: "external", label: "External" },
  { value: "unconfirmed", label: "Unconfirmed" },
  { value: "two_factor_off", label: "Without 2FA" },
  { value: "bots", label: "Bots" },
];

function Settings() {
  const q = useQuery({ queryKey: ["admin-settings"], queryFn: () => api.get<S["InstanceSettings"]>("/admin/settings") });
  return (
    <div className="stack" style={{ gap: 20 }}>
      <Q q={q}>{(s) => <SettingsForm initial={s} />}</Q>
      <Card title="Banner">
        <BannerForm />
      </Card>
    </div>
  );
}

function SettingsForm({ initial }: { initial: S["InstanceSettings"] }) {
  const qc = useQueryClient();
  const [s, setS] = useState(initial);
  const [msg, setMsg] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  return (
    <form
      className="stack"
      style={{ maxWidth: 520 }}
      onSubmit={async (e) => {
        e.preventDefault();
        try {
          await api.put("/admin/settings", s);
          qc.invalidateQueries({ queryKey: ["admin-update"] });
          setMsg("Saved.");
        } catch (err) {
          setError(err);
        }
      }}
    >
      <ErrorBox error={error} />
      {msg && <div className="alert ok">{msg}</div>}
      <label className="check">
        <input type="checkbox" checked={s.signup_enabled} onChange={(e) => setS({ ...s, signup_enabled: e.target.checked })} />
        Allow anyone to sign up
      </label>
      <label className="check">
        <input type="checkbox" checked={s.allow_personal_projects} onChange={(e) => setS({ ...s, allow_personal_projects: e.target.checked })} />
        Allow personal projects
      </label>
      <label className="check">
        <input type="checkbox" checked={s.mcp_enabled} onChange={(e) => setS({ ...s, mcp_enabled: e.target.checked })} />
        <span>
          MCP server <span className="muted small">— AI assistants connect to <code>/api/v4/mcp</code> with a token or by signing in</span>
        </span>
      </label>
      <label className="check">
        <input type="checkbox" checked={s.update_check_enabled ?? true} onChange={(e) => setS({ ...s, update_check_enabled: e.target.checked })} />
        <span>Check for new releases</span>
      </label>
      <Field label="Default branch name for new projects">
        <input value={s.default_branch_name} onChange={(e) => setS({ ...s, default_branch_name: e.target.value })} />
      </Field>
      <div>
        <button className="btn primary">Save</button>
      </div>
    </form>
  );
}

function AuditLog() {
  const q = useQuery({ queryKey: ["admin-audit"], queryFn: () => api.get<S["AuditEvent"][]>("/admin/audit") });
  return (
    <Q q={q}>
      {(events) => (
        <div className="list">
          {events.map((e) => (
            <div className="item small" key={e.id}>
              <code>{e.action}</code>
              <span className="muted">
                {e.target_type} {e.target_id}
              </span>
              <span className="grow muted ellipsis">{JSON.stringify(e.details)}</span>
              <span>{e.principal_name ?? e.principal_kind}</span>
              <span className="faint">{timeAgo(e.created_at)}</span>
            </div>
          ))}
        </div>
      )}
    </Q>
  );
}

function DependencyUpdatesAdmin() {
  const q = useQuery({ queryKey: ["admin-renovate"], queryFn: () => api.get<S["AdminRenovateStatus"]>("/admin/renovate") });
  return <Q q={q}>{(s) => <DependencyUpdatesForm initial={s} />}</Q>;
}

function DependencyUpdatesForm({ initial }: { initial: S["AdminRenovateStatus"] }) {
  const qc = useQueryClient();
  const [s, setS] = useState(initial);
  const [env, setEnv] = useState(
    Object.entries(initial.extra_env)
      .map(([k, v]) => `${k}=${v}`)
      .join("\n"),
  );
  const [token, setToken] = useState("");
  const [pullPassword, setPullPassword] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const save = async (e: FormEvent, clearToken = false, clearPull = false) => {
    e.preventDefault();
    setError(null);
    const extra_env: Record<string, string> = {};
    for (const line of env.split("\n").map((l) => l.trim()).filter(Boolean)) {
      const i = line.indexOf("=");
      if (i > 0) extra_env[line.slice(0, i).trim()] = line.slice(i + 1);
    }
    try {
      const r = await api.put<S["AdminRenovateStatus"]>("/admin/renovate", {
        ...s,
        extra_env,
        onboarding_config: s.onboarding_config || null,
        github_com_token: clearToken ? "" : token || undefined,
        image_pull_password: clearPull ? "" : pullPassword || undefined,
      });
      setS(r);
      setToken("");
      setPullPassword("");
      setMsg("Saved.");
      qc.invalidateQueries({ queryKey: ["admin-renovate"] });
    } catch (err) {
      setError(err);
    }
  };
  return (
    <>
    <form className="stack" style={{ maxWidth: 640 }} onSubmit={(e) => save(e)}>
      <p className="muted small">
        Projects turn on dependency updates in their settings; Forgust runs Renovate on the instance runners as a bot of each project. {initial.enabled_projects}{" "}
        project(s) use it, {initial.active_runs} run(s) going on. These settings apply over the config file's <code>[renovate]</code>.
      </p>
      <ErrorBox error={error} />
      {msg && <div className="alert ok">{msg}</div>}
      <label className="check">
        <input type="checkbox" checked={s.enabled} onChange={(e) => setS({ ...s, enabled: e.target.checked })} /> Offer dependency updates to projects
      </label>
      <div className="grid2">
        <Field label="Renovate image" hint="The version CI tests against is the default">
          <input className="mono" value={s.image} onChange={(e) => setS({ ...s, image: e.target.value })} />
        </Field>
        <Field label="Runner tags" hint="Comma-separated; instance runners only">
          <input value={s.tags.join(", ")} onChange={(e) => setS({ ...s, tags: e.target.value.split(",").map((t) => t.trim()) })} />
        </Field>
        <Field label="Runs at once">
          <input type="number" min={1} max={100} value={s.max_concurrent} onChange={(e) => setS({ ...s, max_concurrent: Number(e.target.value) })} />
        </Field>
        <Field label="Timeout (minutes)">
          <input type="number" min={5} max={1440} value={Math.round(s.timeout_secs / 60)} onChange={(e) => setS({ ...s, timeout_secs: Number(e.target.value) * 60 })} />
        </Field>
      </div>
      <div className="grid2">
        <Field label="Image pull username" hint="When the image is in a private registry or a mirror">
          <input value={s.image_pull_username} autoComplete="off" onChange={(e) => setS({ ...s, image_pull_username: e.target.value })} />
        </Field>
        <Field label="Image pull password" hint={initial.image_pull_password_set ? "Set. Type a new one to replace it." : "Given to the runner (DOCKER_AUTH_CONFIG), masked"}>
          <input type="password" autoComplete="new-password" value={pullPassword} onChange={(e) => setPullPassword(e.target.value)} />
        </Field>
      </div>
      <Field label="Onboarding config" hint='JSON the onboarding merge request proposes, e.g. {"extends": ["config:recommended"]}'>
        <textarea className="mono" rows={3} value={s.onboarding_config ?? ""} onChange={(e) => setS({ ...s, onboarding_config: e.target.value })} />
      </Field>
      <Field label="More variables" hint="One NAME=value per line, given to every run and shown in its job: no secrets">
        <textarea className="mono" rows={3} value={env} onChange={(e) => setEnv(e.target.value)} />
      </Field>
      <Field label="github.com token" hint={initial.github_com_token_set ? "Set. Type a new one to replace it." : "For changelogs. Use a token with no scopes: every run has it."}>
        <input type="password" autoComplete="new-password" value={token} onChange={(e) => setToken(e.target.value)} />
      </Field>
      <div className="row" style={{ gap: 8 }}>
        <button className="btn primary">Save</button>
        {initial.github_com_token_set && (
          <button type="button" className="btn" onClick={(e) => save(e, true)}>
            Remove the github.com token
          </button>
        )}
        {initial.image_pull_password_set && (
          <button type="button" className="btn" onClick={(e) => save(e, false, true)}>
            Remove the image pull credentials
          </button>
        )}
      </div>
    </form>
    <div style={{ maxWidth: 640 }}>
      <HostRules
        base="/admin/renovate/host_rules"
        title="Private registries for every project"
        empty="None. Projects can add their own (theirs win for the same host); this instance's container registry needs none."
      />
    </div>
    </>
  );
}

/** The built-in scanners' images, runner tags and limits. */
function SecurityScanningAdmin() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["admin-scanning"], queryFn: () => api.get<S["AdminScanning"]>("/admin/security/scanning") });
  return <Q q={q}>{(a) => <SecurityScanningAdminForm initial={a} onSaved={() => qc.invalidateQueries({ queryKey: ["admin-scanning"] })} />}</Q>;
}

function SecurityScanningAdminForm({ initial, onSaved }: { initial: S["AdminScanning"]; onSaved: () => void }) {
  const [s, setS] = useState(initial.settings);
  const [tags, setTags] = useState(initial.settings.tags.join(", "));
  const toast = useToast();
  const save = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api.put("/admin/security/scanning", { ...s, tags: tags.split(",").map((t) => t.trim()).filter(Boolean) });
      toast.ok("Saved");
      onSaved();
    } catch (err) {
      toast.error(err);
    }
  };
  return (
    <form className="stack" onSubmit={save} style={{ maxWidth: 720 }}>
      <p className="muted" style={{ margin: 0 }}>
        {initial.projects} project{initial.projects === 1 ? "" : "s"} scanned · {initial.running} scan{initial.running === 1 ? "" : "s"} running. Scans run on instance runners only,
        without projects' CI/CD variables.
      </p>
      <label className="check">
        <input type="checkbox" checked={s.enabled} onChange={(e) => setS({ ...s, enabled: e.target.checked })} /> Projects may turn on security scanning
      </label>
      <Field label="Code scanner image" hint="GitLab's Semgrep-based analyzer (MIT)">
        <input value={s.sast_image} onChange={(e) => setS({ ...s, sast_image: e.target.value })} />
      </Field>
      <Field label="Secrets scanner image" hint="GitLab's secrets analyzer (MIT, gitleaks inside)">
        <input value={s.secrets_image} onChange={(e) => setS({ ...s, secrets_image: e.target.value })} />
      </Field>
      <Field label="Trivy image" hint="Dependencies, containers and infrastructure files (Apache-2.0)">
        <input value={s.trivy_image} onChange={(e) => setS({ ...s, trivy_image: e.target.value })} />
      </Field>
      <div className="grid2">
        <Field label="Runner tags" hint="Comma-separated; empty: runners that take untagged jobs">
          <input value={tags} onChange={(e) => setTags(e.target.value)} />
        </Field>
        <Field label="Scans at once">
          <input type="number" min={1} max={50} value={s.max_concurrent} onChange={(e) => setS({ ...s, max_concurrent: Number(e.target.value) })} />
        </Field>
      </div>
      <Field label="Timeout (minutes)">
        <input type="number" min={1} max={1440} value={Math.round(s.timeout_secs / 60)} onChange={(e) => setS({ ...s, timeout_secs: Number(e.target.value) * 60 })} />
      </Field>
      <div>
        <button className="btn primary">Save</button>
      </div>
      <Card title="Projects with open critical or high findings">
        {initial.exposed.length === 0 ? (
          <p className="small muted" style={{ margin: 0 }}>
            None.
          </p>
        ) : (
          <table className="table small">
            <thead>
              <tr>
                <th>Project</th>
                <th className="num">Critical</th>
                <th className="num">High</th>
              </tr>
            </thead>
            <tbody>
              {initial.exposed.map((x) => (
                <tr key={x.project_id}>
                  <td>
                    <a href={`/${x.full_path}/-/security`}>{x.full_path}</a>
                  </td>
                  <td className="num">{x.critical}</td>
                  <td className="num">{x.high}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Card>
    </form>
  );
}

