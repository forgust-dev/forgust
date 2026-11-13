import { lazy, useEffect, useState, type ChangeEvent, type ComponentType } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, timeAgo, type S, bytes } from "../../api/client";
import { Badge, Section, ErrorBox, Field, Q, VisibilitySelect, ListHead, NotFound, Tile, QuotaBar, useFormRun } from "../../components/ui";
import { AvatarUpload } from "../../components/AvatarUpload";
import { Select } from "../../components/controls";
import { AccessTokens, DeployTokens } from "../../components/Tokens";
import { ApprovalRules, Webhooks } from "../../components/Webhooks";
import type { Ctx } from "./Project";
import { HostRules } from "../../components/HostRules";
import { useBranches } from "./Code";
import { fetchers } from "../../prefetch";
import { StatusBadge } from "./ci/Status";
import { settingsPage, type SettingsSlug } from "./settingsPages";
const CiSettings = lazy(() => import("./ci/Settings").then((m) => ({ default: m.CiSettings })));

/** Who may push or merge to a protected branch, or create a protected tag. */
const PROTECT_LEVELS = [
  { value: "no_one", label: "No one" },
  { value: "developer", label: "Developers + Maintainers" },
  { value: "maintainer", label: "Maintainers" },
];

/** Each settings page, by its slug in `settingsPages.ts` (a page missing here does not compile). */
const PAGES: Record<SettingsSlug, ComponentType<{ ctx: Ctx }>> = {
  "": GeneralSettings,
  repository: RepositorySettings,
  merge_requests: MergeRequestSettings,
  ci_cd: CiSettings,
  automation: AutomationSettings,
  integrations: IntegrationSettings,
};

/** A settings page of a project: `settings` (general) or `settings/<page>`. */
export function ProjectSettings({ ctx, page }: { ctx: Ctx; page: string }) {
  const found = settingsPage(page);
  if (!found) return <NotFound />;
  const Page = PAGES[found.slug];
  return <Page ctx={ctx} />;
}

/** Sections that had an anchor here before they moved to a page of their own. */
const MOVED: Record<string, SettingsSlug> = { "#pages": "ci_cd", "#security-scanning": "automation" };

function RepositorySettings({ ctx }: { ctx: Ctx }) {
  const p = ctx.p;
  return (
    <div className="stack settings-page">
      <ListHead title="Repository settings" sub="Protected branches and tags, push rules, locked files, and keys and tokens to clone the repository." />
      <Section title="Protected branches" description="Who can push and force-push to matching branches.">
        <ProtectedBranches ctx={ctx} />
      </Section>
      <Section title="Protected tags" description="Who can create matching tags. Nobody can move or delete them by pushing; Maintainers can delete them here.">
        <ProtectedTags ctx={ctx} />
      </Section>
      <Section title="Push rules" description="Checks every push (and every commit made in the browser) must pass.">
        <PushRules ctx={ctx} />
      </Section>
      <Section title="Locked files" description="Files locked with git lfs lock: on the default branch, only their owner can change them.">
        <LockedFiles ctx={ctx} />
      </Section>
      <Section title="Deploy keys" description="SSH keys that reach this repository only, read-only unless allowed to write.">
        <DeployKeys ctx={ctx} />
      </Section>
      <Section title="Deploy tokens" description="Credentials to clone the repository without a user account.">
        <DeployTokens base={`/projects/${p.id}`} />
      </Section>
    </div>
  );
}

function MergeRequestSettings({ ctx }: { ctx: Ctx }) {
  const p = ctx.p;
  return (
    <div className="stack settings-page">
      <ListHead title="Merge request settings" sub="What a merge request needs before it can be merged." />
      <Section title="Merge request approvals" description="How many approvals a merge request needs, and from whom, before it can be merged.">
        <ApprovalRules projectId={p.id} />
      </Section>
    </div>
  );
}

function AutomationSettings({ ctx }: { ctx: Ctx }) {
  return (
    <div className="stack settings-page">
      <ListHead title="Automation settings" sub="Built-in security scanning and dependency updates." />
      <Section
        id="security-scanning"
        title="Security scanning"
        description="Built-in scanners for code, secrets, dependencies, infrastructure files and container images, run on the instance's runners. Findings show in the Security tab."
      >
        <SecurityScanning ctx={ctx} />
      </Section>
      <Section
        title="Dependency updates"
        description="Renovate opens merge requests that update this project's dependencies, as a bot member of the project. No token or pipeline to set up."
      >
        <DependencyUpdates ctx={ctx} />
        <HostRules base={`/projects/${ctx.p.id}/renovate/host_rules`} empty="None. This instance's container registry needs none." />
      </Section>
    </div>
  );
}

function IntegrationSettings({ ctx }: { ctx: Ctx }) {
  const p = ctx.p;
  return (
    <div className="stack settings-page">
      <ListHead title="Integration settings" sub="Webhooks and access tokens for scripts and other services." />
      <Section title="Webhooks" description="Call a URL when something happens in this project.">
        <Webhooks base={`/projects/${p.id}`} />
      </Section>
      <Section title="Access tokens" description="Tokens for scripts and integrations, acting as a bot member of this project.">
        <AccessTokens base={`/projects/${p.id}`} />
      </Section>
    </div>
  );
}

function GeneralSettings({ ctx }: { ctx: Ctx }) {
  const p = ctx.p;
  const qc = useQueryClient();
  const nav = useNavigate();
  const [name, setName] = useState(p.name);
  const [description, setDescription] = useState(p.description);
  const [visibility, setVisibility] = useState(p.visibility);
  const [defaultBranch, setDefaultBranch] = useState(p.default_branch ?? "");
  const [path, setPath] = useState(p.path);
  const [target, setTarget] = useState("");
  const branches = useBranches(ctx);
  const { run, msg, error } = useFormRun(() => qc.invalidateQueries({ queryKey: ["project", p.id] }));
  // old links to a section that moved: follow it
  const { hash } = useLocation();
  useEffect(() => {
    if (Object.hasOwn(MOVED, hash)) nav(`${ctx.web}/-/settings/${MOVED[hash]}${hash}`, { replace: true });
  }, [hash]);

  return (
    <div className="stack settings-page">
      <ListHead title="General settings" sub="Name, picture and storage of this project." />
      <ErrorBox error={error} />
      {msg && <div className="alert ok">{msg}</div>}
      <Section title="Picture" description="The project's picture in lists, its page and the sidebar.">
        <AvatarUpload
          preview={<Tile name={p.name} src={p.avatar_url} big />}
          path={`/projects/${p.id}/avatar`}
          hasOne={!!p.avatar_url}
          onChanged={() => qc.invalidateQueries({ queryKey: ["project", p.id] })}
          what="in lists and on the project's page"
        />
      </Section>
      <Section title="General" description="Name, description, who can see the project, and the branch it opens on.">
        <form className="stack" onSubmit={run(() => api.put(`/projects/${p.id}`, { name, description, visibility, default_branch: defaultBranch || null }), "Saved.")}>
          <Field label="Name">
            <input value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label="Description">
            <input value={description} onChange={(e) => setDescription(e.target.value)} />
          </Field>
          <Field label="Visibility">
            <VisibilitySelect value={visibility} onChange={setVisibility} />
          </Field>
          <Field label="Default branch">
            <Select
              value={defaultBranch}
              onChange={setDefaultBranch}
              options={(branches.data ?? [{ name: defaultBranch } as S["BranchDto"]]).map((b) => ({ value: b.name, label: b.name }))}
            />
          </Field>
          <div>
            <button className="btn primary">Save</button>
          </div>
        </form>
      </Section>
      <Section id="storage" title="Storage" description="What this project keeps: its repository, uploaded files, CI artifacts and logs, Git LFS objects and container images; and its storage quotas, if any.">
        <StorageUse ctx={ctx} />
      </Section>
      {p.can_own && (
        <Section title="Advanced" description="Change the path, move the project to another group, or delete it." danger>
          <div className="stack">
            {p.redirects.length > 0 && (
              <p className="muted small" style={{ margin: 0 }}>
                Old paths still redirecting here: {p.redirects.join(", ")}
              </p>
            )}
            <form
              className="row wrap"
              onSubmit={run(async () => {
                const np = await api.post<S["ProjectDto"]>(`/projects/${p.id}/path`, { path });
                nav(`/${np.full_path}/-/settings`, { replace: true });
              }, "Path changed. Old URLs and remotes keep working.")}
            >
              <Field label="Change path">
                <input value={path} onChange={(e) => setPath(e.target.value)} />
              </Field>
              <button className="btn" style={{ alignSelf: "flex-end" }}>
                Rename
              </button>
            </form>
            <form
              className="row wrap"
              onSubmit={run(async () => {
                const ns = await fetchers.pathResolve(target);
                const np = await api.post<S["ProjectDto"]>(`/projects/${p.id}/transfer`, { namespace_id: ns.id });
                nav(`/${np.full_path}/-/settings`, { replace: true });
              }, "Project moved.")}
            >
              <Field label="Move to another group" hint="Target group or user path">
                <input placeholder="acme/platform" value={target} onChange={(e) => setTarget(e.target.value)} />
              </Field>
              <button className="btn" style={{ alignSelf: "flex-end" }}>
                Move
              </button>
            </form>
            <div className="row wrap" style={{ gap: 8, alignItems: "center" }}>
              <button
                className="btn"
                onClick={run(async () => {
                  if (!confirm(`Archive ${p.full_path}? It becomes read-only for everyone (code, issues, merge requests, CI, settings) until you unarchive it.`)) return;
                  await api.post(`/projects/${p.id}/archive`);
                  nav(`/${p.full_path}`);
                }, "Archived: the project is read-only.")}
              >
                Archive project
              </button>
              <span className="small muted">Read-only, kept and readable; unarchive it from its page.</span>
            </div>
            <div>
              <button
                className="btn danger"
                onClick={run(async () => {
                  if (!confirm(`Delete ${p.full_path} and its repository? This cannot be undone.`)) return;
                  await api.del(`/projects/${p.id}`);
                  nav("/");
                }, "Deleted.")}
              >
                Delete project
              </button>
            </div>
          </div>
        </Section>
      )}
    </div>
  );
}

function ProtectedBranches({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["protected", ctx.p.id];
  const list = useQuery({ queryKey: key, queryFn: () => api.get<S["ProtectedBranchDto"][]>(`/projects/${ctx.p.id}/protected_branches`) });
  const [form, setForm] = useState({
    name: "",
    push_access_level: "maintainer",
    merge_access_level: "maintainer",
    allow_force_push: false,
    allow_deploy_keys: false,
    code_owner_approval_required: false,
  });
  const [error, setError] = useState<unknown>(null);
  const add = useMutation({
    mutationFn: () => api.post(`/projects/${ctx.p.id}/protected_branches`, form),
    onSuccess: () => {
      setForm({ ...form, name: "" });
      qc.invalidateQueries({ queryKey: key });
    },
    onError: setError,
  });
  const del = useMutation({ mutationFn: (id: number) => api.del(`/projects/${ctx.p.id}/protected_branches/${id}`), onSuccess: () => qc.invalidateQueries({ queryKey: key }) });
  return (
    <div className="stack">
      <ErrorBox error={error} />
      <form
        className="stack"
        onSubmit={(e) => {
          e.preventDefault();
          add.mutate();
        }}
      >
        <div className="grid2">
          <Field label="Branch or pattern" hint="e.g. main or release/*">
            <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} />
          </Field>
          <Field label="Allowed to push">
            <Select value={form.push_access_level} onChange={(v) => setForm({ ...form, push_access_level: v })} options={PROTECT_LEVELS} />
          </Field>
        </div>
        <div className="row wrap" style={{ gap: 20 }}>
          <label className="check">
            <input type="checkbox" checked={form.allow_force_push} onChange={(e) => setForm({ ...form, allow_force_push: e.target.checked })} />
            Allow force push
          </label>
          <label className="check">
            <input type="checkbox" checked={form.allow_deploy_keys} onChange={(e) => setForm({ ...form, allow_deploy_keys: e.target.checked })} />
            Allow deploy keys to push
          </label>
          <label className="check">
            <input
              type="checkbox"
              checked={form.code_owner_approval_required}
              onChange={(e) => setForm({ ...form, code_owner_approval_required: e.target.checked })}
            />
            Require approval from code owners
          </label>
          <div className="spacer" />
          <button className="btn primary" disabled={!form.name}>
            Protect
          </button>
        </div>
      </form>
      <Q q={list}>
        {(rules) => (
          <div className="list">
            {rules.length === 0 && <div className="item muted">No protected branches.</div>}
            {rules.map((r) => (
              <div className="item" key={r.id}>
                <code className="grow">{r.name}</code>
                <Badge>push: {r.push_access_level.replace("_", " ")}</Badge>
                {r.allow_force_push && <Badge tone="warn">force push</Badge>}
                {r.allow_deploy_keys && <Badge>deploy keys</Badge>}
                {r.code_owner_approval_required && <Badge tone="accent">code owners approve</Badge>}
                <button className="btn small danger" onClick={() => del.mutate(r.id)}>
                  Unprotect
                </button>
              </div>
            ))}
          </div>
        )}
      </Q>
    </div>
  );
}

function PushRules({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["push-rule", ctx.p.id];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["PushRules"]>(`/projects/${ctx.p.id}/push_rule`) });
  return <Q q={q}>{(r) => <PushRulesForm ctx={ctx} initial={r} onSaved={() => qc.invalidateQueries({ queryKey: key })} />}</Q>;
}

function PushRulesForm({ ctx, initial, onSaved }: { ctx: Ctx; initial: S["PushRules"]; onSaved: () => void }) {
  const [r, setR] = useState(initial);
  const [error, setError] = useState<unknown>(null);
  const [saved, setSaved] = useState(false);
  const save = useMutation({
    mutationFn: () => api.put(`/projects/${ctx.p.id}/push_rule`, r),
    onSuccess: () => {
      setError(null);
      setSaved(true);
      onSaved();
    },
    onError: (e) => {
      setSaved(false);
      setError(e);
    },
  });
  const text = (k: "commit_message_regex" | "commit_message_negative_regex" | "author_email_regex" | "branch_name_regex") => ({
    value: r[k],
    onChange: (e: ChangeEvent<HTMLInputElement>) => setR({ ...r, [k]: e.target.value }),
    className: "mono",
  });
  return (
    <form
      className="stack"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <ErrorBox error={error} />
      <div className="grid2">
        <Field label="Commit messages must match" hint="A regular expression, e.g. ^(feat|fix|docs): ">
          <input {...text("commit_message_regex")} />
        </Field>
        <Field label="Commit messages must not match" hint="e.g. (?i)wip|fixup!">
          <input {...text("commit_message_negative_regex")} />
        </Field>
        <Field label="Author emails must match" hint="e.g. @example\.com$">
          <input {...text("author_email_regex")} />
        </Field>
        <Field label="New branch names must match" hint="e.g. ^(main|feature/.+|fix/.+)$">
          <input {...text("branch_name_regex")} />
        </Field>
        <Field label="Largest file, in MB" hint="0: no limit (Git LFS files do not count)">
          <input type="number" min={0} value={r.max_file_size_mb} onChange={(e) => setR({ ...r, max_file_size_mb: Math.max(0, Number(e.target.value) || 0) })} />
        </Field>
      </div>
      <div className="row wrap" style={{ gap: 20 }}>
        <label className="check">
          <input type="checkbox" checked={r.prevent_secrets} onChange={(e) => setR({ ...r, prevent_secrets: e.target.checked })} />
          Refuse files that look like secrets (private keys, credentials)
        </label>
        <label className="check">
          <input type="checkbox" checked={r.reject_unsigned_commits} onChange={(e) => setR({ ...r, reject_unsigned_commits: e.target.checked })} />
          Accept only verified signed commits
        </label>
        <div className="spacer" />
        {saved && <span className="small muted">Saved.</span>}
        <button className="btn primary" disabled={save.isPending}>
          Save push rules
        </button>
      </div>
    </form>
  );
}

const INTERVALS: [number, string][] = [
  [1, "Every hour"],
  [6, "Every 6 hours"],
  [12, "Every 12 hours"],
  [24, "Every day"],
  [168, "Every week"],
];

function DependencyUpdates({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["renovate", ctx.p.id];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["RenovateStatus"]>(`/projects/${ctx.p.id}/renovate`) });
  return <Q q={q}>{(s) => <DependencyUpdatesForm ctx={ctx} initial={s} onSaved={() => qc.invalidateQueries({ queryKey: key })} />}</Q>;
}

function DependencyUpdatesForm({ ctx, initial, onSaved }: { ctx: Ctx; initial: S["RenovateStatus"]; onSaved: () => void }) {
  const [enabled, setEnabled] = useState(initial.enabled);
  const [hours, setHours] = useState(initial.interval_hours);
  const [onboarding, setOnboarding] = useState(initial.onboarding);
  const [error, setError] = useState<unknown>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const done = (m: string) => {
    setError(null);
    setMsg(m);
    onSaved();
  };
  const save = useMutation({
    mutationFn: () => api.put(`/projects/${ctx.p.id}/renovate`, { enabled, interval_hours: hours, onboarding }),
    onSuccess: () => done(enabled ? "Dependency updates are on: the first run starts within a minute." : "Dependency updates are off."),
    onError: setError,
  });
  const runNow = useMutation({
    mutationFn: () => api.post<S["RenovateRunStarted"]>(`/projects/${ctx.p.id}/renovate/run`, {}),
    onSuccess: (r) => done(r.pipeline_iid != null ? `Started pipeline #${r.pipeline_iid}.` : "Nothing to run: the repository is empty."),
    onError: setError,
  });
  if (!initial.available) return <div className="muted small">This instance does not offer dependency updates.</div>;
  const last = initial.last_run;
  return (
    <form
      className="stack"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <ErrorBox error={error} />
      {msg && <div className="alert ok">{msg}</div>}
      <label className="check">
        <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} /> Open merge requests that update dependencies
      </label>
      <div className="grid2">
        <Field label="Runs">
          <select value={hours} onChange={(e) => setHours(Number(e.target.value))} disabled={!enabled}>
            {INTERVALS.map(([h, label]) => (
              <option key={h} value={h}>
                {label}
              </option>
            ))}
            {!INTERVALS.some(([h]) => h === hours) && <option value={hours}>Every {hours} hours</option>}
          </select>
        </Field>
        <Field label="Configuration">
          <select value={onboarding ? "onboarding" : "defaults"} onChange={(e) => setOnboarding(e.target.value === "onboarding")} disabled={!enabled}>
            <option value="onboarding">First, a merge request that adds renovate.json</option>
            <option value="defaults">Renovate's defaults, no config file needed</option>
          </select>
        </Field>
      </div>
      {initial.enabled && (
        <div className="list" aria-label="Dependency updates status">
          <div className="item">
            <span className="grow small">
              Runs as <strong>@{initial.bot_username}</strong> with <code>{initial.image}</code>
            </span>
          </div>
          <div className="item">
            <span className="grow small">
              {last ? (
                <>
                  Last run {timeAgo(last.created_at)}{" "}
                  <StatusBadge status={last.status} to={`${ctx.web}/-/pipelines/${last.pipeline_iid}`} />
                </>
              ) : (
                "No run yet"
              )}
              {initial.next_run_at && <span className="muted"> · next run {new Date(initial.next_run_at).toLocaleString()}</span>}
            </span>
            <button type="button" className="btn small" disabled={runNow.isPending} onClick={() => runNow.mutate()}>
              Run now
            </button>
          </div>
        </div>
      )}
      <div>
        <button className="btn primary" disabled={save.isPending}>
          Save
        </button>
      </div>
    </form>
  );
}

const SCANNER_LABEL: Record<string, string> = {
  sast: "Code (Semgrep, GitLab's rules)",
  secret_detection: "Secrets (gitleaks; the whole history the first time)",
  dependency_scanning: "Dependencies (Trivy, lockfiles)",
  iac: "Infrastructure files (Trivy: Dockerfiles, Terraform, Kubernetes...)",
  container_scanning: "Container images (Trivy, the newest tag of each image in this project's registry)",
};

function SecurityScanning({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["scanning", ctx.p.id];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["ScanningStatus"]>(`/projects/${ctx.p.id}/security/scanning`) });
  return <Q q={q}>{(s) => <SecurityScanningForm ctx={ctx} initial={s} onSaved={() => qc.invalidateQueries({ queryKey: key })} />}</Q>;
}

function SecurityScanningForm({ ctx, initial, onSaved }: { ctx: Ctx; initial: S["ScanningStatus"]; onSaved: () => void }) {
  const [enabled, setEnabled] = useState(initial.enabled);
  const [scanners, setScanners] = useState<string[]>(initial.scanners);
  const [hours, setHours] = useState(initial.interval_hours);
  const [error, setError] = useState<unknown>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const done = (m: string) => {
    setError(null);
    setMsg(m);
    onSaved();
  };
  const save = useMutation({
    mutationFn: () => api.put(`/projects/${ctx.p.id}/security/scanning`, { enabled, scanners, interval_hours: hours }),
    onSuccess: () => done(enabled ? "Security scanning is on: the first scan starts within a minute, as you." : "Security scanning is off; findings stay."),
    onError: setError,
  });
  const runNow = useMutation({
    mutationFn: () => api.post(`/projects/${ctx.p.id}/security/scanning/run`, {}),
    onSuccess: () => done("Scan started."),
    onError: setError,
  });
  if (!initial.instance_enabled) return <div className="muted small">This instance does not run security scanners.</div>;
  return (
    <form
      className="stack"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <ErrorBox error={error} />
      {msg && <div className="alert ok">{msg}</div>}
      <label className="check">
        <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} /> Scan the default branch and merge requests
      </label>
      <div className="stack" style={{ gap: 6 }}>
        {initial.available.map((sc) => (
          <label key={sc} className="check">
            <input
              type="checkbox"
              disabled={!enabled}
              checked={scanners.includes(sc)}
              onChange={(e) => setScanners(e.target.checked ? [...scanners, sc] : scanners.filter((x) => x !== sc))}
            />{" "}
            {SCANNER_LABEL[sc] ?? sc}
          </label>
        ))}
      </div>
      <Field label="Default branch scans">
        <select value={hours} onChange={(e) => setHours(Number(e.target.value))} disabled={!enabled}>
          {INTERVALS.map(([h, label]) => (
            <option key={h} value={h}>
              {label}
            </option>
          ))}
          {!INTERVALS.some(([h]) => h === hours) && <option value={hours}>Every {hours} hours</option>}
        </select>
      </Field>
      <p className="small muted" style={{ margin: 0 }}>
        Also after pushes to the default branch (at most every 10 minutes) and on each new commit of a merge request. Scans run without the project's CI/CD
        variables.
      </p>
      {initial.enabled && (
        <div className="list" aria-label="Security scanning status">
          <div className="item">
            <span className="grow small">
              {initial.run_as && (
                <>
                  Runs as <strong>@{initial.run_as}</strong> ·{" "}
                </>
              )}
              {initial.last_pipeline_iid != null ? (
                <>
                  Last scan <StatusBadge status={initial.last_status ?? "created"} to={`${ctx.web}/-/pipelines/${initial.last_pipeline_iid}`} />
                </>
              ) : (
                "No scan yet"
              )}
              {initial.next_run_at && <span className="muted"> · next {new Date(initial.next_run_at).toLocaleString()}</span>}
            </span>
            <button type="button" className="btn small" disabled={runNow.isPending} onClick={() => runNow.mutate()}>
              Run now
            </button>
          </div>
        </div>
      )}
      <div>
        <button className="btn primary" disabled={save.isPending}>
          Save
        </button>
      </div>
    </form>
  );
}

function LockedFiles({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["lfs-locks", ctx.p.id];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["LfsLock"][]>(`/projects/${ctx.p.id}/lfs_locks`) });
  const [error, setError] = useState<unknown>(null);
  const unlock = useMutation({
    mutationFn: (id: number) => api.del(`/projects/${ctx.p.id}/lfs_locks/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: key }),
    onError: setError,
  });
  return (
    <div className="stack">
      <ErrorBox error={error} />
      <Q q={q}>
        {(locks) => (
          <div className="list">
            {locks.length === 0 && <div className="item muted">No locked files. Lock one with git lfs lock &lt;path&gt;.</div>}
            {locks.map((l) => (
              <div className="item" key={l.id}>
                <code className="grow">{l.path}</code>
                <span className="small faint">
                  @{l.owner_username}, {timeAgo(l.created_at)}
                </span>
                <button className="btn small danger" onClick={() => unlock.mutate(l.id)}>
                  Unlock
                </button>
              </div>
            ))}
          </div>
        )}
      </Q>
    </div>
  );
}

function StorageUse({ ctx }: { ctx: Ctx }) {
  const q = useQuery({ queryKey: ["storage", ctx.p.id], queryFn: () => api.get<S["ProjectStorageUse"]>(`/projects/${ctx.p.id}/storage`) });
  // the storage banner links here: once the numbers are in, show them
  const loaded = !!q.data;
  useEffect(() => {
    if (loaded && window.location.hash === "#storage") document.getElementById("storage")?.scrollIntoView({ block: "start" });
  }, [loaded]);
  return (
    <Q q={q}>
      {(u) => (
        <div className="stack">
          {u.quota && <QuotaBar label="This project's quota" used={u.quota.used} quota={u.quota.quota} />}
          {u.namespace && <QuotaBar label={`Quota of ${u.namespace.name}, shared by all its projects`} used={u.namespace.used} quota={u.namespace.quota} />}
          <dl className="storage-use">
            <dt>Repository</dt>
            <dd>{bytes(u.repository_bytes)}</dd>
            <dt>Uploads</dt>
            <dd>
              {bytes(u.uploads_bytes)} <span className="muted">({u.uploads_count} files)</span>
            </dd>
            <dt>CI artifacts and logs</dt>
            <dd>{bytes(u.artifacts_bytes)}</dd>
            <dt>Git LFS</dt>
            <dd>{bytes(u.lfs_bytes)}</dd>
            <dt>Container images</dt>
            <dd>{bytes(u.registry_bytes)}</dd>
            <dt>Total</dt>
            <dd>
              <strong>{bytes(u.total_bytes)}</strong>
            </dd>
          </dl>
          {(u.quota || u.namespace) && (
            <p className="small muted" style={{ margin: 0 }}>
              Once a quota is full, new pushes, LFS files, CI artifacts, images and uploads are refused; reading, deleting and CI jobs carry on. Administrators set quotas.
            </p>
          )}
        </div>
      )}
    </Q>
  );
}

function ProtectedTags({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["protected-tags", ctx.p.id];
  const list = useQuery({ queryKey: key, queryFn: () => api.get<S["ProtectedTagDto"][]>(`/projects/${ctx.p.id}/protected_tags`) });
  const [form, setForm] = useState({ name: "", create_access_level: "maintainer" });
  const [error, setError] = useState<unknown>(null);
  const add = useMutation({
    mutationFn: () => api.post(`/projects/${ctx.p.id}/protected_tags`, form),
    onSuccess: () => {
      setForm({ ...form, name: "" });
      qc.invalidateQueries({ queryKey: key });
    },
    onError: setError,
  });
  const del = useMutation({ mutationFn: (id: number) => api.del(`/projects/${ctx.p.id}/protected_tags/${id}`), onSuccess: () => qc.invalidateQueries({ queryKey: key }) });
  return (
    <div className="stack">
      <ErrorBox error={error} />
      <form
        className="stack"
        onSubmit={(e) => {
          e.preventDefault();
          add.mutate();
        }}
      >
        <div className="grid2">
          <Field label="Tag or pattern" hint="e.g. v1.0 or v*">
            <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} />
          </Field>
          <Field label="Allowed to create">
            <Select value={form.create_access_level} onChange={(v) => setForm({ ...form, create_access_level: v })} options={PROTECT_LEVELS} />
          </Field>
        </div>
        <div className="row">
          <div className="spacer" />
          <button className="btn primary" disabled={!form.name}>
            Protect tags
          </button>
        </div>
      </form>
      <Q q={list}>
        {(rules) => (
          <div className="list" aria-label="Protected tags">
            {rules.length === 0 && <div className="item muted">No protected tags.</div>}
            {rules.map((r) => (
              <div className="item" key={r.id}>
                <code className="grow">{r.name}</code>
                <Badge>create: {r.create_access_level.replace("_", " ")}</Badge>
                <button className="btn small danger" onClick={() => del.mutate(r.id)} aria-label={`Unprotect ${r.name}`}>
                  Unprotect
                </button>
              </div>
            ))}
          </div>
        )}
      </Q>
    </div>
  );
}

function DeployKeys({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["deploy-keys", ctx.p.id];
  const list = useQuery({ queryKey: key, queryFn: () => api.get<S["DeployKey"][]>(`/projects/${ctx.p.id}/deploy_keys`) });
  const [form, setForm] = useState({ title: "", key: "", can_push: false });
  const [error, setError] = useState<unknown>(null);
  const add = useMutation({
    mutationFn: () => api.post(`/projects/${ctx.p.id}/deploy_keys`, form),
    onSuccess: () => {
      setForm({ title: "", key: "", can_push: false });
      qc.invalidateQueries({ queryKey: key });
    },
    onError: setError,
  });
  const del = useMutation({ mutationFn: (id: number) => api.del(`/projects/${ctx.p.id}/deploy_keys/${id}`), onSuccess: () => qc.invalidateQueries({ queryKey: key }) });
  return (
    <div className="stack">
      <ErrorBox error={error} />
      <form
        className="stack"
        onSubmit={(e) => {
          e.preventDefault();
          add.mutate();
        }}
      >
        <Field label="Title">
          <input value={form.title} onChange={(e) => setForm({ ...form, title: e.target.value })} />
        </Field>
        <Field label="Public key">
          <textarea placeholder="ssh-ed25519 AAAA…" value={form.key} onChange={(e) => setForm({ ...form, key: e.target.value })} />
        </Field>
        <div className="row">
          <label className="check grow">
            <input type="checkbox" checked={form.can_push} onChange={(e) => setForm({ ...form, can_push: e.target.checked })} />
            Grant write access
          </label>
          <button className="btn primary" disabled={!form.key}>
            Add key
          </button>
        </div>
      </form>
      <Q q={list}>
        {(keys) =>
          keys.length === 0 ? (
            <p className="muted">No deploy keys.</p>
          ) : (
            <div className="list">
              {keys.map((k) => (
                <div className="item" key={k.id}>
                  <div className="grow">
                    <strong>{k.title}</strong>
                    <div className="small faint mono">{k.fingerprint}</div>
                  </div>
                  <Badge>{k.can_push ? "read-write" : "read-only"}</Badge>
                  <button className="btn small danger" onClick={() => del.mutate(k.id)}>
                    Remove
                  </button>
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}
