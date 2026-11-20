import { HostRules } from "../components/HostRules";
import { AuditEvents } from "../components/AuditEvents";
import { useState, type FormEvent } from "react";
import { Route, Routes, useLocation } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, ApiError, bytes, enc, qs, timeAgo, type S } from "../api/client";
import { useProviders } from "../prefetch";
import { useSession } from "../session";
import { Avatar, Badge, Card, ErrorBox, Field, FilterTabs, NotFound, Q, SearchField, UserLink, useDebounced, useFormRun } from "../components/ui";
import { adminPage } from "./adminPages";
import { Combobox, Menu, Select, type MenuItem } from "../components/controls";
import { MoreHorizontal } from "lucide-react";
import { useAct, useToast } from "../shell/toast";
import { useConfirm, usePrompt, type Ask } from "../shell/dialog";
import { RunnersPanel } from "./project/ci/Runners";
import { AdminBackups, AdminHealth, AdminJobs, AdminOverview, AdminProjects, AdminStorage, BannerForm } from "./AdminTools";

export function Admin() {
  const { user } = useSession();
  const loc = useLocation();
  if (!user?.is_admin) return <NotFound />;
  // the area's pages are in the sidebar (shell/Shell.tsx): here, which one this is
  return (
    <div className="page">
      <div className="admin-head">
        <p className="eyebrow">Admin area</p>
        <h1>{adminPage(loc.pathname)?.label ?? "Administration"}</h1>
      </div>
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
        <Route path="ai-review" element={<AiReviewAdmin />} />
        <Route path="audit" element={<AdminAuditLog />} />
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
  const confirm = useConfirm();
  const prompt = usePrompt();
  // what they wrote stays (without an author); personal projects are asked about
  const deleteUser = async (u: S["AdminUser"]) => {
    const typed = await prompt({
      title: `Delete ${u.username}'s account?`,
      body: (
        <>
          <p>Their merge requests, issues, comments and pipelines stay, without an author. Their memberships, keys, tokens, approvals and to-dos go.</p>
          <p>This cannot be undone.</p>
        </>
      ),
      action: "Delete account",
      danger: true,
      input: { label: "Type their username to confirm", match: u.username, mono: true },
    });
    if (typed !== u.username) return;
    const path = `/admin/users/${u.id}`;
    try {
      await api.del(path);
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 409 && e.message.includes("personal projects"))) return toast.error(e);
      const also = await confirm({
        title: "Delete their personal projects too?",
        body: `${e.message}. Those projects (repositories, issues, merge requests, CI) are deleted with the account.`,
        action: "Delete projects and account",
        danger: true,
      });
      if (!also) return;
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
  const act = async (f: () => Promise<unknown>, done: string, ask?: Omit<Ask, "input">) => {
    if (ask && !(await confirm(ask))) return;
    await run(f, done);
  };
  const providers = useProviders();
  const { emailEnabled } = useSession();
  // for someone who cannot do it from their own settings: by who they are at the provider
  const linkProvider = async (u: S["AdminUser"], pr: S["Provider"]) => {
    const subject = await prompt({
      title: `Link ${u.username} to ${pr.display_name}`,
      body: "They can then sign in with it. Give who they are at the provider: the sub claim of its ID tokens.",
      action: "Link",
      input: { label: "Their identifier (sub)", mono: true },
    });
    if (!subject) return;
    await run(() => api.post(`/admin/users/${u.id}/identities`, { provider: pr.name, subject }), `${pr.display_name} linked`);
  };
  // confirmed at once: the administrator vouches for it (no link, so it works without email too)
  const changeEmail = async (u: S["AdminUser"]) => {
    const email = await prompt({
      title: `A new email address for ${u.username}`,
      body: "It is theirs at once, confirmed: enter it only if you know it is their address.",
      action: "Change address",
      input: { label: "Email address", type: "email", value: u.email },
    });
    if (!email || email === u.email) return;
    await run(() => api.put(`/admin/users/${u.id}`, { email }), "Email address changed");
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
                      {
                        label: "Sign out everywhere",
                        disabled: u.sessions === 0,
                        onSelect: () =>
                          act(() => api.post(`${path}/sign_out`), "Signed out everywhere", {
                            title: `End ${u.username}'s ${u.sessions === 1 ? "session" : `${u.sessions} sessions`}?`,
                            body: "They are signed out on every device and sign in again to go on.",
                            action: "Sign out everywhere",
                            danger: true,
                          }),
                      },
                      {
                        label: "Revoke access tokens",
                        disabled: u.tokens === 0,
                        onSelect: () =>
                          act(() => api.post(`${path}/revoke_tokens`), "Tokens revoked", {
                            title: `Revoke ${u.username}'s ${u.tokens === 1 ? "personal access token" : `${u.tokens} personal access tokens`}?`,
                            body: "Tools using them stop working.",
                            action: "Revoke tokens",
                            danger: true,
                          }),
                      },
                      {
                        label: "Reset two-factor authentication",
                        disabled: !u.two_factor_enabled,
                        onSelect: () =>
                          act(() => api.post(`${path}/reset_two_factor`), "Two-factor authentication reset", {
                            title: `Turn off ${u.username}'s two-factor authentication?`,
                            body: "Do it only once you know it is them (a lost authenticator).",
                            action: "Turn it off",
                            danger: true,
                          }),
                      },
                      {
                        label: "Impersonate",
                        disabled: u.is_admin || u.state === "blocked",
                        onSelect: () =>
                          act(
                            async () => {
                              await api.post(`${path}/impersonate`);
                              window.location.assign("/");
                            },
                            `Signed in as ${u.username}`,
                            {
                              title: `Sign in as ${u.username}?`,
                              body: "For up to an hour. Your own session ends; everything you do is audited as yours.",
                              action: `Sign in as ${u.username}`,
                            },
                          ),
                      },
                      { label: "Change the email address…", onSelect: () => changeEmail(u) },
                      {
                        label: "Confirm the email address",
                        disabled: u.email_verified,
                        onSelect: () =>
                          act(() => api.post(`${path}/confirm_email`, { email: u.email }), "Email address confirmed", {
                            title: `Confirm ${u.email} without a link?`,
                            body: `Do it only if you know it is ${u.username}'s: a confirmed address is trusted for verified commits and code owners.`,
                            action: "Confirm address",
                          }),
                      },
                      { label: "Resend the confirmation email", disabled: u.email_verified || !emailEnabled, onSelect: () => act(() => api.post(`${path}/resend_confirmation`), "Confirmation email sent") },
                      ...(providers.data ?? []).map(
                        (pr): MenuItem =>
                          u.providers.includes(pr.name)
                            ? {
                                label: `Unlink ${pr.display_name}`,
                                onSelect: () =>
                                  act(() => api.del(`${path}/identities/${enc(pr.name)}`), `${pr.display_name} unlinked`, {
                                    title: `Stop ${u.username} signing in with ${pr.display_name}?`,
                                    action: `Unlink ${pr.display_name}`,
                                    danger: true,
                                  }),
                              }
                            : { label: `Link ${pr.display_name}…`, onSelect: () => linkProvider(u, pr) },
                      ),
                      "separator",
                      { label: "Delete account…", danger: true, onSelect: () => deleteUser(u) },
                      {
                        label: u.state === "blocked" ? "Unblock" : "Block",
                        danger: u.state !== "blocked",
                        onSelect: () =>
                          act(
                            () => api.put(path, { state: u.state === "blocked" ? "active" : "blocked" }),
                            u.state === "blocked" ? "Unblocked" : "Blocked",
                            u.state === "blocked"
                              ? undefined
                              : { title: `Block ${u.username}?`, body: "They are signed out and cannot sign in, push or use tokens.", action: "Block", danger: true },
                          ),
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
                        {u.providers.map((name) => (
                          <Badge key={name}>{providers.data?.find((pr) => pr.name === name)?.display_name ?? name}</Badge>
                        ))}
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
          const saved = await api.put<S["InstanceSettings"]>("/admin/settings", s);
          // opened again, the form starts from what was saved, not from before
          qc.setQueryData(["admin-settings"], saved);
          qc.invalidateQueries({ queryKey: ["admin-update"] });
          // turned on, the check runs in the background: ask again once it has answered
          if (saved.update_check_enabled && !initial.update_check_enabled) {
            setTimeout(() => qc.invalidateQueries({ queryKey: ["admin-update"] }), 5000);
          }
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
        <input type="checkbox" checked={s.update_check_enabled} onChange={(e) => setS({ ...s, update_check_enabled: e.target.checked })} />
        <span>
          Check for new releases <span className="muted small">— the server asks GitHub for the latest one every 6 hours, and tells administrators</span>
        </span>
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

function AdminAuditLog() {
  // the instance endpoint returns 200 events a call
  return <AuditEvents path="/admin/audit" perPage={200} target />;
}

/** `NAME=value` lines, one a variable. */
const envLines = (m: Record<string, string>) =>
  Object.entries(m)
    .map(([k, v]) => `${k}=${v}`)
    .join("\n");

const parseEnvLines = (text: string) => {
  const out: Record<string, string> = {};
  for (const line of text.split("\n").map((l) => l.trim()).filter(Boolean)) {
    const i = line.indexOf("=");
    if (i > 0) out[line.slice(0, i).trim()] = line.slice(i + 1);
  }
  return out;
};

function DependencyUpdatesAdmin() {
  const q = useQuery({ queryKey: ["admin-renovate"], queryFn: () => api.get<S["AdminRenovateStatus"]>("/admin/renovate") });
  return <Q q={q}>{(s) => <DependencyUpdatesForm initial={s} />}</Q>;
}

function DependencyUpdatesForm({ initial }: { initial: S["AdminRenovateStatus"] }) {
  const qc = useQueryClient();
  const [s, setS] = useState(initial);
  const [env, setEnv] = useState(envLines(initial.extra_env));
  const [token, setToken] = useState("");
  const [pullPassword, setPullPassword] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const save = async (e: FormEvent, clearToken = false, clearPull = false) => {
    e.preventDefault();
    setError(null);
    const extra_env = parseEnvLines(env);
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

function AiReviewAdmin() {
  const q = useQuery({ queryKey: ["admin-ai-review"], queryFn: () => api.get<S["AdminAiReviewStatus"]>("/admin/ai_review") });
  return <Q q={q}>{(s) => <AiReviewForm initial={s} />}</Q>;
}

/** The APIs the released reviewer image talks to (its `AI_REVIEW_PROVIDER`). */
const AI_PROVIDERS = [{ value: "openai", label: "OpenAI-compatible (OpenAI, Ollama, vLLM, llama.cpp, LiteLLM)" }];

/** Where AI reviews send the code, which model they ask, and their limits. */
function AiReviewForm({ initial }: { initial: S["AdminAiReviewStatus"] }) {
  const qc = useQueryClient();
  const [s, setS] = useState(initial);
  const [key, setKey] = useState("");
  const [options, setOptions] = useState(envLines(initial.options ?? {}));
  const secretNames = initial.secret_option_names ?? [];
  const [secrets, setSecrets] = useState("");
  const { run, msg, error } = useFormRun(() => qc.invalidateQueries({ queryKey: ["admin-ai-review"] }));
  const save = (clearKey = false, clearSecrets = false) =>
    run(async () => {
      const r = await api.put<S["AdminAiReviewStatus"]>("/admin/ai_review", {
        ...s,
        options: parseEnvLines(options),
        ca_pem: s.ca_pem || null,
        api_key: clearKey ? "" : key || undefined,
        // all of them: what is typed replaces what is stored
        secret_options: clearSecrets ? {} : secrets.trim() ? parseEnvLines(secrets) : undefined,
      });
      setS(r);
      setKey("");
      setSecrets("");
    }, "Saved.");
  // listing the endpoint's models tests the endpoint and the key before saving
  const probe = useMutation({
    mutationFn: () => api.post<S["AdminAiReviewModels"]>("/admin/ai_review/models", { base_url: s.base_url, api_key: key || undefined, ca_pem: s.ca_pem || null }),
    onSuccess: (r) => {
      if (!s.model && r.models.length === 1) setS({ ...s, model: r.models[0] });
    },
  });
  const models = probe.data?.models;
  let host = "";
  try {
    host = s.base_url ? new URL(s.base_url).host : "";
  } catch {
    // not a URL yet
  }
  return (
    <form className="stack" style={{ maxWidth: 640 }} onSubmit={save()}>
      <p className="muted small">
        Projects turn on AI review in their settings; then people who can push ask for a review on a merge request. Forgust runs the review on the instance runners
        and posts the model&apos;s comments as a bot of the project; the bot never approves. {initial.enabled_projects} project(s) use it, {initial.active_runs}{" "}
        review(s) going on. These settings apply over the config file&apos;s <code>[ai_review]</code>.
      </p>
      <div className="alert warn">
        The changes of every reviewed merge request are sent to {host ? <strong>{host}</strong> : "the endpoint below"}. Use an endpoint you trust with this code.
      </div>
      <ErrorBox error={error} />
      {msg && <div className="alert ok">{msg}</div>}
      <label className="check">
        <input type="checkbox" checked={s.enabled} onChange={(e) => setS({ ...s, enabled: e.target.checked })} /> Offer AI review to projects
      </label>
      <Field label="Provider">
        <Select
          value={s.provider}
          onChange={(provider) => setS({ ...s, provider })}
          // one a reviewer image of your own knows, set before or in the config file: kept
          options={AI_PROVIDERS.some((p) => p.value === s.provider) ? AI_PROVIDERS : [...AI_PROVIDERS, { value: s.provider, label: s.provider }]}
          label="Provider"
        />
      </Field>
      <Field label="Endpoint" hint="The API's base, reached from the runners: https://api.openai.com/v1, http://vllm:8000/v1">
        <input className="mono" value={s.base_url} placeholder="https://api.openai.com/v1" onChange={(e) => setS({ ...s, base_url: e.target.value })} />
      </Field>
      <Field label="API key" hint={initial.api_key_set ? "Set. Type a new one to replace it." : "Sent as a bearer token, masked in the job; none for a local model"}>
        <input type="password" autoComplete="new-password" value={key} onChange={(e) => setKey(e.target.value)} />
      </Field>
      <Field label="Model">
        {models?.length ? (
          <Combobox value={s.model} onChange={(model) => setS({ ...s, model })} options={models.map((m) => ({ value: m, label: m }))} label="Model" />
        ) : (
          <input className="mono" value={s.model} placeholder="qwen2.5-coder:32b" onChange={(e) => setS({ ...s, model: e.target.value })} />
        )}
      </Field>
      <div className="row" style={{ gap: 8, alignItems: "center" }}>
        <button type="button" className="btn" disabled={!s.base_url.trim() || probe.isPending} onClick={() => probe.mutate()}>
          {probe.isPending ? "Asking the endpoint…" : "Fetch models"}
        </button>
        <span className="muted small">Asks the endpoint for its models with the key above (or the saved one): a test of both. Asked from the server, not the runners.</span>
      </div>
      <ErrorBox error={probe.error} />
      {models && (
        <div className={`alert ${models.length && (!s.model || models.includes(s.model)) ? "ok" : "warn"}`}>
          {!models.length
            ? "The endpoint answered, with no models."
            : !s.model || models.includes(s.model)
              ? `The endpoint answered: ${models.length} model(s).`
              : `The endpoint answered, but does not list ${s.model}.`}
        </div>
      )}
      <div className="grid2">
        <Field label="Comments per review">
          <input type="number" min={1} max={50} value={s.max_comments} onChange={(e) => setS({ ...s, max_comments: Number(e.target.value) })} />
        </Field>
        <Field label="Reviews at once">
          <input type="number" min={1} max={50} value={s.max_concurrent} onChange={(e) => setS({ ...s, max_concurrent: Number(e.target.value) })} />
        </Field>
        <Field label="Timeout (minutes)">
          <input type="number" min={1} max={120} value={Math.round(s.timeout_secs / 60)} onChange={(e) => setS({ ...s, timeout_secs: Number(e.target.value) * 60 })} />
        </Field>
      </div>
      <Field label="Reviewer image" hint="Empty: the one released with this server, which follows each upgrade. Or an image of your own that runs forgust-review with the same contract">
        <input className="mono" value={s.image} placeholder={initial.default_image} onChange={(e) => setS({ ...s, image: e.target.value })} />
      </Field>
      <Field label="Runner tags" hint="Comma-separated; instance runners only">
        <input value={s.tags.join(", ")} onChange={(e) => setS({ ...s, tags: e.target.value.split(",").map((t) => t.trim()) })} />
      </Field>
      <Field
        label="Reviewer options"
        hint={
          <>
            One NAME=value per line, given to the job as AI_REVIEW_NAME and shown in its variables: no secrets. For example MAX_DIFF_BYTES (changes per model
            request: fit the model&apos;s context) or TEMPERATURE; the{" "}
            <a href="https://github.com/forgust-dev/forgust/tree/main/crates/forgust-reviewer#variables" target="_blank" rel="noreferrer">
              reviewer&apos;s README
            </a>{" "}
            lists them.
          </>
        }
      >
        <textarea className="mono" rows={3} value={options} placeholder="MAX_DIFF_BYTES=200000" onChange={(e) => setOptions(e.target.value)} />
      </Field>
      <Field
        label="Secret options"
        hint={
          secretNames.length
            ? `Set: ${secretNames.join(", ")}. Typing replaces all of them.`
            : "Like the options, masked in the job: a second key, cloud credentials"
        }
      >
        <textarea className="mono" rows={2} value={secrets} autoComplete="off" placeholder="NAME=value" onChange={(e) => setSecrets(e.target.value)} />
      </Field>
      <Field label="Certificate authority" hint="PEM, when the endpoint's certificate comes from an in-house CA">
        <textarea className="mono" rows={3} value={s.ca_pem ?? ""} placeholder="-----BEGIN CERTIFICATE-----" onChange={(e) => setS({ ...s, ca_pem: e.target.value })} />
      </Field>
      <div className="row" style={{ gap: 8 }}>
        <button className="btn primary">Save</button>
        {initial.api_key_set && (
          <button type="button" className="btn" onClick={save(true)}>
            Remove the API key
          </button>
        )}
        {secretNames.length > 0 && (
          <button type="button" className="btn" onClick={save(false, true)}>
            Remove the secret options
          </button>
        )}
      </div>
    </form>
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
      <label className="check">
        <input type="checkbox" checked={s.mirror_vulnerability_dbs} onChange={(e) => setS({ ...s, mirror_vulnerability_dbs: e.target.checked })} /> Keep Trivy's
        databases on this server, and scan with them
      </label>
      <p className="small muted" style={{ margin: 0 }}>
        Copied from the sources below every 6 hours (Admin &gt; Jobs runs it now), or imported from files with{" "}
        <code>forgust admin import-vulnerability-db</code> where the server cannot reach them. Scans then need no access to ghcr.io.
      </p>
      <Field label="Vulnerability database source">
        <input value={s.trivy_db_source} disabled={!s.mirror_vulnerability_dbs} onChange={(e) => setS({ ...s, trivy_db_source: e.target.value })} />
      </Field>
      <Field label="Java index source">
        <input value={s.trivy_java_db_source} disabled={!s.mirror_vulnerability_dbs} onChange={(e) => setS({ ...s, trivy_java_db_source: e.target.value })} />
      </Field>
      <Field label="Infrastructure checks source">
        <input value={s.trivy_checks_source} disabled={!s.mirror_vulnerability_dbs} onChange={(e) => setS({ ...s, trivy_checks_source: e.target.value })} />
      </Field>
      <div>
        <button className="btn primary">Save</button>
      </div>
      <Card title="Databases on this server">
        {initial.mirrors.length === 0 ? (
          <p className="small muted" style={{ margin: 0 }}>
            None yet.
          </p>
        ) : (
          <table className="table small">
            <thead>
              <tr>
                <th>Database</th>
                <th>From</th>
                <th className="num">Size</th>
                <th>Stored</th>
              </tr>
            </thead>
            <tbody>
              {initial.mirrors.map((m) => (
                <tr key={m.name}>
                  <td>
                    <code title={m.digest}>
                      {m.name}:{m.tag}
                    </code>
                  </td>
                  <td className="ellipsis">{m.origin === "file" ? "a file" : m.origin}</td>
                  <td className="num">{bytes(m.size)}</td>
                  <td>{timeAgo(m.stored_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Card>
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

