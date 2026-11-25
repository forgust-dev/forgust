import { lazy, useEffect, useState, type ChangeEvent, type ComponentType, type FormEvent } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, timeAgo, type S, bytes } from "../../api/client";
import { Badge, Section, ErrorBox, Field, FieldGroup, Q, VisibilitySelect, ListHead, NotFound, Tile, QuotaBar, useFormRun } from "../../components/ui";
import { AvatarUpload } from "../../components/AvatarUpload";
import { Segmented, Select, Switch } from "../../components/controls";
import { AccessTokens, DeployTokens } from "../../components/Tokens";
import { ApprovalRules, Webhooks } from "../../components/Webhooks";
import type { Ctx } from "./Project";
import { HostRules } from "../../components/HostRules";
import { useBranches } from "./Code";
import { fetchers, useGroups } from "../../prefetch";
import { useSession } from "../../session";
import { StatusBadge } from "./ci/Status";
import { useConfirm, usePrompt } from "../../shell/dialog";
import { settingsPage, type SettingsSlug } from "./settingsPages";
import { useTopOnSwitch } from "../../shell/scroll";
import { AddActions, EmptyRow, ListSection, SaveRow, SettingSwitch, useSaved, whoCan } from "../../components/settings";
import { saveCiSettings, useCiSettings } from "./ciSettings";
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
  useTopOnSwitch(page);
  useHashSection();
  const found = settingsPage(page);
  if (!found) return <NotFound />;
  const Page = PAGES[found.slug];
  return <Page ctx={ctx} />;
}

/** Sections that had an anchor here before they moved to a page of their own. */
const MOVED: Record<string, SettingsSlug> = { "#pages": "ci_cd", "#security-scanning": "automation", "#smart-view": "automation" };

/**
 * Brings the section the address names (`#id`) into view once it is there: the page may show it
 * later (a page loaded on demand, its data). It keeps it in place while the sections above it
 * load and grow, until the page settles or someone scrolls. Also when the anchor changes on the
 * page: the Ctrl+K palette leads to any section.
 */
function useHashSection() {
  const { hash } = useLocation();
  useEffect(() => {
    const id = decodeURIComponent(hash.slice(1));
    if (!id) return;
    const main = document.querySelector("main") ?? document.body;
    let el = document.getElementById(id);
    const place = () => el?.scrollIntoView({ block: "start" });
    const grows = new ResizeObserver(place);
    const appears = new MutationObserver(() => {
      if (el || !(el = document.getElementById(id))) return;
      appears.disconnect();
      place();
      grows.observe(main);
    });
    if (el) {
      place();
      grows.observe(main);
    } else appears.observe(main, { childList: true, subtree: true });
    const stop = () => {
      grows.disconnect();
      appears.disconnect();
    };
    const settled = window.setTimeout(stop, 3000);
    const away = ["wheel", "touchstart", "keydown"] as const;
    for (const ev of away) window.addEventListener(ev, stop, { once: true, passive: true });
    return () => {
      stop();
      window.clearTimeout(settled);
      for (const ev of away) window.removeEventListener(ev, stop);
    };
  }, [hash]);
}

function RepositorySettings({ ctx }: { ctx: Ctx }) {
  const p = ctx.p;
  return (
    <div className="stack settings-page">
      <ListHead title="Repository settings" sub="Protected branches and tags, push rules, locked files, and keys and tokens to clone the repository." />
      <ProtectedBranches ctx={ctx} />
      <ProtectedTags ctx={ctx} />
      <Section id="push-rules" title="Push rules" description="Checks every push (and every commit made in the browser) must pass.">
        <PushRules ctx={ctx} />
      </Section>
      <Section id="locked-files" title="Locked files" description="Files locked with git lfs lock: on the default branch, only their owner can change them.">
        <LockedFiles ctx={ctx} />
      </Section>
      <DeployKeys ctx={ctx} />
      <DeployTokens base={`/projects/${p.id}`} title="Deploy tokens" description="Credentials to clone the repository without a user account." />
      <Mirrors ctx={ctx} />
    </div>
  );
}

function MergeRequestSettings({ ctx }: { ctx: Ctx }) {
  const p = ctx.p;
  return (
    <div className="stack settings-page">
      <ListHead title="Merge request settings" sub="What a merge request needs before it can be merged." />
      <Section id="merge-checks" title="Merge checks" description="What a merge request needs, besides approvals, before it can be merged.">
        <MergeChecks ctx={ctx} />
      </Section>
      <ApprovalRules projectId={p.id} />
    </div>
  );
}

/** The pipeline, merge trains and security findings a merge request must get past (CI settings, saved in part). */
function MergeChecks({ ctx }: { ctx: Ctx }) {
  const q = useCiSettings(ctx.p.id);
  return <Q q={q}>{(s) => <MergeChecksForm ctx={ctx} initial={s} />}</Q>;
}

function MergeChecksForm({ ctx, initial }: { ctx: Ctx; initial: S["ProjectCiSettings"] }) {
  const qc = useQueryClient();
  const form = useSaved({
    only_merge_if_pipeline_succeeds: initial.only_merge_if_pipeline_succeeds,
    merge_trains_enabled: initial.merge_trains_enabled,
    block_new_security_findings: initial.block_new_security_findings,
  });
  const s = form.value;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const save = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await saveCiSettings(qc, ctx.p.id, s);
      form.done();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className="stack" onSubmit={save}>
      <ErrorBox error={error} />
      <label className="check">
        <input
          type="checkbox"
          checked={s.only_merge_if_pipeline_succeeds}
          // merge trains test each merge request's pipeline: without this rule, they go too
          onChange={(e) => form.set({ ...s, only_merge_if_pipeline_succeeds: e.target.checked, merge_trains_enabled: e.target.checked && s.merge_trains_enabled })}
        />{" "}
        Merge requests can only be merged when the pipeline succeeds
      </label>
      <label className="check">
        <input type="checkbox" checked={s.merge_trains_enabled} disabled={!s.only_merge_if_pipeline_succeeds} onChange={(e) => form.set({ ...s, merge_trains_enabled: e.target.checked })} />{" "}
        <span>
          Merge through merge trains: each merge request is tested on top of the ones queued ahead of it, then they merge in order
          {!s.only_merge_if_pipeline_succeeds && <span className="muted"> (needs the rule above)</span>}
        </span>
      </label>
      <label className="check">
        <input type="checkbox" checked={s.block_new_security_findings} onChange={(e) => form.set({ ...s, block_new_security_findings: e.target.checked })} />
        <span>Merge requests whose pipeline finds new critical or high security findings can only be merged once those are fixed or dismissed</span>
      </label>
      <SaveRow dirty={form.dirty} busy={busy} saved={form.saved} />
    </form>
  );
}

function AutomationSettings({ ctx }: { ctx: Ctx }) {
  return (
    <div className="stack settings-page">
      <ListHead title="Automation settings" sub="Built-in security scanning, dependency updates and AI review." />
      <Section
        id="security-scanning"
        title="Security scanning"
        description="Built-in scanners for code, secrets, dependencies, infrastructure files and container images, run on the instance's runners. Findings show in the Security tab."
      >
        <SecurityScanning ctx={ctx} />
      </Section>
      <Section
        id="dependency-updates"
        title="Dependency updates"
        description="Renovate opens merge requests that update this project's dependencies, as a bot member of the project. No token or pipeline to set up."
      >
        <DependencyUpdates ctx={ctx} />
      </Section>
      <Section
        id="ai-review"
        title="AI review"
        description="People who can push ask a model for a review of a merge request; its comments are posted by a bot of the project, which never approves."
      >
        <AiReview ctx={ctx} />
      </Section>
      <Section
        id="smart-view"
        title="Smart View"
        description="The parts of this code, what each needs and who to ask, mapped by a job on the instance's runners at each new commit of the default branch and of open merge requests."
      >
        <SmartViewSetting ctx={ctx} />
      </Section>
    </div>
  );
}

function SmartViewSetting({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["smart-view-settings", ctx.p.id];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["SmartViewSettings"]>(`/projects/${ctx.p.id}/smart_view`) });
  // the Smart View's page links here when it is off
  const done = () => {
    qc.invalidateQueries({ queryKey: key });
    qc.invalidateQueries({ queryKey: ["map", ctx.p.id] });
  };
  return <Q q={q}>{(s) => <SmartViewSettingForm ctx={ctx} initial={s} onSaved={done} />}</Q>;
}

function SmartViewSettingForm({ ctx, initial, onSaved }: { ctx: Ctx; initial: S["SmartViewSettings"]; onSaved: () => void }) {
  if (!initial.available && !initial.enabled) return <div className="muted small">This instance does not offer the Smart View.</div>;
  return (
    <div className="stack">
      <SettingSwitch
        checked={initial.enabled}
        onSave={async (on) => {
          await api.put(`/projects/${ctx.p.id}/smart_view`, { enabled: on });
          onSaved();
        }}
        confirmOff={{ title: "Turn the Smart View off?", body: "Its maps of this project are removed; turning it on again maps the default branch anew.", action: "Turn it off" }}
      >
        Map this project for the Smart View
      </SettingSwitch>
      <p className="small muted" style={{ margin: 0 }}>
        The job runs as a bot of the project that can only read it, and nothing leaves the instance. Only the latest map of the default branch and of each open
        merge request is kept. An <code>assay.toml</code> at the top of the repository corrects what is found.
      </p>
    </div>
  );
}

function AiReview({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["ai-review", ctx.p.id];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["AiReviewSettings"]>(`/projects/${ctx.p.id}/ai_review`) });
  return <Q q={q}>{(s) => <AiReviewForm ctx={ctx} initial={s} onSaved={() => qc.invalidateQueries({ queryKey: key })} />}</Q>;
}

function AiReviewForm({ ctx, initial, onSaved }: { ctx: Ctx; initial: S["AiReviewSettings"]; onSaved: () => void }) {
  if (!initial.available && !initial.enabled) return <div className="muted small">This instance does not offer AI review.</div>;
  return (
    <div className="stack">
      <SettingSwitch
        checked={initial.enabled}
        onSave={async (on) => {
          await api.put(`/projects/${ctx.p.id}/ai_review`, { enabled: on });
          onSaved();
        }}
      >
        Offer AI review on merge requests
      </SettingSwitch>
      <p className="small muted" style={{ margin: 0 }}>
        A review sends the merge request&apos;s changes, title and description to{" "}
        {initial.endpoint ? <strong>{initial.endpoint}</strong> : "the instance's model endpoint"}
        {initial.model && (
          <>
            {" "}
            (model <code>{initial.model}</code>)
          </>
        )}
        . A <code>.forgust/review.md</code> on the target branch adds the project&apos;s own guidelines.
      </p>
    </div>
  );
}

function IntegrationSettings({ ctx }: { ctx: Ctx }) {
  const p = ctx.p;
  return (
    <div className="stack settings-page">
      <ListHead title="Integration settings" sub="Webhooks and access tokens for scripts and other services." />
      <Webhooks base={`/projects/${p.id}`} title="Webhooks" description="Call a URL when something happens in this project." />
      <AccessTokens base={`/projects/${p.id}`} title="Access tokens" description="Tokens for scripts and integrations, acting as a bot member of this project." />
    </div>
  );
}

function GeneralSettings({ ctx }: { ctx: Ctx }) {
  const p = ctx.p;
  const qc = useQueryClient();
  const nav = useNavigate();
  const general = useSaved({ name: p.name, description: p.description, visibility: p.visibility, default_branch: p.default_branch ?? "" });
  const g = general.value;
  const [savingGeneral, setSavingGeneral] = useState(false);
  const [generalError, setGeneralError] = useState<unknown>(null);
  const saveGeneral = async (e: FormEvent) => {
    e.preventDefault();
    setSavingGeneral(true);
    setGeneralError(null);
    try {
      await api.put(`/projects/${p.id}`, { name: g.name, description: g.description, visibility: g.visibility, default_branch: g.default_branch || null });
      general.done();
      qc.invalidateQueries({ queryKey: ["project", p.id] });
    } catch (err) {
      setGeneralError(err);
    } finally {
      setSavingGeneral(false);
    }
  };
  const [path, setPath] = useState(p.path);
  const [target, setTarget] = useState("");
  // a subgroup, or a place the list does not hold: by its path, as before the list
  const [typed, setTyped] = useState("");
  const { user } = useSession();
  const groups = useGroups(p.can_own);
  const places = [
    ...(user?.namespace_id ? [{ value: String(user.namespace_id), label: `${user.username} (personal)` }] : []),
    ...(groups.data ?? []).map((g) => ({ value: String(g.id), label: g.full_path })),
  ]
    .filter((o) => o.value !== String(p.namespace_id))
    .concat([{ value: "path", label: "Another group, by its path…" }]);
  const prompt = usePrompt();
  const remove = async () => {
    const typed = await prompt({
      title: `Delete ${p.full_path}?`,
      body: "Its repository, issues, merge requests, pipelines and everything else go with it. This cannot be undone.",
      action: "Delete project",
      danger: true,
      input: { label: `Type ${p.full_path} to confirm`, match: p.full_path, mono: true },
    });
    if (typed === null) return;
    await run(async () => {
      await api.del(`/projects/${p.id}`);
      nav("/");
    }, "Deleted.")();
  };
  const branches = useBranches(ctx);
  const { run, msg, error } = useFormRun(() => qc.invalidateQueries({ queryKey: ["project", p.id] }));
  // old links to a section that moved: follow it
  const { hash } = useLocation();
  useEffect(() => {
    if (Object.hasOwn(MOVED, hash)) nav(`${ctx.web}/-/settings/${MOVED[hash]}${hash}`, { replace: true });
  }, [hash]);

  // archived, the settings are read-only: its Owner (the only one who sees this
  // page then) can still rename, move, delete or unarchive it
  const archived = p.archived;
  return (
    <div className="stack settings-page">
      <ListHead
        title="General settings"
        sub={archived ? "This project is archived: unarchive it to change its settings or see its storage. It can still be renamed, moved or deleted." : "Name, picture and storage of this project."}
      />
      <ErrorBox error={error} />
      {msg && <div className="alert ok">{msg}</div>}
      {!archived && (
        <>
          <Section id="picture" title="Picture" description="The project's picture in lists, its page and the sidebar.">
            <AvatarUpload
              preview={<Tile name={p.name} src={p.avatar_url} big />}
              path={`/projects/${p.id}/avatar`}
              hasOne={!!p.avatar_url}
              onChanged={() => qc.invalidateQueries({ queryKey: ["project", p.id] })}
              what="in lists and on the project's page"
            />
          </Section>
          <Section id="general" title="General" description="Name, description, who can see the project, and the branch it opens on.">
            <form className="stack" onSubmit={saveGeneral}>
              <ErrorBox error={generalError} />
              <Field label="Name">
                <input value={g.name} onChange={(e) => general.set({ ...g, name: e.target.value })} />
              </Field>
              <Field label="Description">
                <input value={g.description} onChange={(e) => general.set({ ...g, description: e.target.value })} />
              </Field>
              <Field label="Visibility">
                <VisibilitySelect value={g.visibility} onChange={(v) => general.set({ ...g, visibility: v })} />
              </Field>
              <Field label="Default branch">
                <Select
                  value={g.default_branch}
                  onChange={(v) => general.set({ ...g, default_branch: v })}
                  options={(branches.data ?? [{ name: g.default_branch } as S["BranchDto"]]).map((b) => ({ value: b.name, label: b.name }))}
                />
              </Field>
              <SaveRow dirty={general.dirty} busy={savingGeneral} saved={general.saved} />
            </form>
          </Section>
        </>
      )}
      {/* its figures are for Maintainers, and an archived project has none */}
      {!archived && (
        <Section id="storage" title="Storage" description="What this project keeps: its repository, uploaded files, CI artifacts and logs, Git LFS objects and container images; and its storage quotas, if any.">
          <StorageUse ctx={ctx} />
        </Section>
      )}
      {p.can_own && (
        <Section id="path" title="Path and place" description="Where the project lives: its address, and the group or namespace it belongs to.">
          <div className="stack">
            <form
              className="stack"
              onSubmit={run(async () => {
                const np = await api.post<S["ProjectDto"]>(`/projects/${p.id}/path`, { path });
                nav(`/${np.full_path}/-/settings`, { replace: true });
              }, "Path changed. Old URLs and remotes keep working.")}
            >
              <Field
                label="Change path"
                hint={
                  p.redirects.length > 0
                    ? `Old addresses and clone remotes keep working: they lead here. Leading here already: ${p.redirects.join(", ")}`
                    : "Old addresses and clone remotes keep working: they lead here."
                }
              >
                <input value={path} onChange={(e) => setPath(e.target.value)} />
              </Field>
              <div>
                <button className="btn" disabled={!path.trim() || path === p.path}>
                  Rename
                </button>
              </div>
            </form>
            <form
              className="stack"
              onSubmit={run(async () => {
                const id = target === "path" ? (await fetchers.pathResolve(typed.trim())).id : Number(target);
                const np = await api.post<S["ProjectDto"]>(`/projects/${p.id}/transfer`, { namespace_id: id });
                nav(`/${np.full_path}/-/settings`, { replace: true });
              }, "Project moved.")}
            >
              <Field label="Move to another group" hint="An organization, a subgroup, or your personal namespace">
                <Select value={target} onChange={setTarget} placeholder="Choose where" options={places} />
              </Field>
              {target === "path" && (
                <Field label="Group path" hint="Its path, as in its address: acme/platform">
                  <input className="mono" placeholder="acme/platform" value={typed} onChange={(e) => setTyped(e.target.value)} />
                </Field>
              )}
              <div>
                <button className="btn" disabled={!target || (target === "path" && !typed.trim())}>
                  Move
                </button>
              </div>
            </form>
          </div>
        </Section>
      )}
      {p.can_own && (
        <Section id="danger-zone" title="Danger zone" description="What touches the whole project at once." danger>
          <div className="danger-rows">
            {archived ? (
              <div className="danger-row">
                <div className="grow">
                  <strong>Unarchive this project</strong>
                  <p className="small muted">Writable again for its members, as before it was archived.</p>
                </div>
                <button
                  className="btn"
                  onClick={run(async () => {
                    await api.post(`/projects/${p.id}/unarchive`);
                    // every page's permissions change with it (settings, pushing, the banner)
                    await qc.invalidateQueries();
                  }, "Unarchived: the project can change again.")}
                >
                  Unarchive project
                </button>
              </div>
            ) : (
              <div className="danger-row">
                <div className="grow">
                  <strong>Archive this project</strong>
                  <p className="small muted">Read-only for everyone (code, issues, merge requests, CI, settings), kept and readable until it is unarchived.</p>
                </div>
                <button
                  className="btn"
                  onClick={run(
                    async () => {
                      await api.post(`/projects/${p.id}/archive`);
                      nav(`/${p.full_path}`);
                    },
                    "Archived: the project is read-only.",
                    {
                      title: `Archive ${p.full_path}?`,
                      body: "It becomes read-only for everyone (code, issues, merge requests, CI, settings) until you unarchive it.",
                      action: "Archive project",
                    },
                  )}
                >
                  Archive project
                </button>
              </div>
            )}
            <div className="danger-row">
              <div className="grow">
                <strong>Delete this project</strong>
                <p className="small muted">Its repository, issues, merge requests, pipelines and everything else go with it, for good.</p>
              </div>
              <button className="btn danger" onClick={remove}>
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
  const blank = {
    name: "",
    push_access_level: "maintainer",
    merge_access_level: "maintainer",
    allow_force_push: false,
    allow_deploy_keys: false,
    code_owner_approval_required: false,
  };
  const [form, setForm] = useState(blank);
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const close = () => {
    setAdding(false);
    setForm(blank);
    setError(null);
  };
  const add = useMutation({
    mutationFn: () => api.post(`/projects/${ctx.p.id}/protected_branches`, form),
    onSuccess: () => {
      close();
      qc.invalidateQueries({ queryKey: key });
    },
    onError: setError,
  });
  const del = useMutation({ mutationFn: (id: number) => api.del(`/projects/${ctx.p.id}/protected_branches/${id}`), onSuccess: () => qc.invalidateQueries({ queryKey: key }) });
  return (
    <ListSection
      id="protected-branches"
      title="Protected branches"
      description="Who can push and force-push to matching branches."
      add="Protect a branch"
      adding={adding}
      onAdd={() => setAdding(true)}
      form={
      <form
        className="stack"
        aria-label="Protect a branch"
        onSubmit={(e) => {
          e.preventDefault();
          add.mutate();
        }}
      >
        <ErrorBox error={error} />
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
        </div>
        <AddActions label="Protect" disabled={!form.name} busy={add.isPending} onCancel={close} />
      </form>
      }
    >
      <Q q={list}>
        {(rules) =>
          rules.length === 0 ? (
            <EmptyRow>No protected branches.</EmptyRow>
          ) : (
            <div className="list" aria-label="Protected branches">
              {rules.map((r) => (
                <div className="item" key={r.id}>
                  <div className="grow" style={{ minWidth: 0 }}>
                    <code>{r.name}</code>
                    <div className="small muted">
                      {[
                        r.merge_access_level === r.push_access_level
                          ? `${whoCan(r.push_access_level)} can push and merge`
                          : `${whoCan(r.push_access_level)} can push, ${whoCan(r.merge_access_level)} can merge`,
                        r.allow_force_push ? "force push allowed" : "no force push",
                        r.allow_deploy_keys && "deploy keys can push",
                        r.code_owner_approval_required && "code owners must approve",
                      ]
                        .filter(Boolean)
                        .join(" · ")}
                    </div>
                  </div>
                  <button className="btn small danger" onClick={() => del.mutate(r.id)} aria-label={`Unprotect ${r.name}`}>
                    Unprotect
                  </button>
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </ListSection>
  );
}

function PushRules({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["push-rule", ctx.p.id];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["PushRules"]>(`/projects/${ctx.p.id}/push_rule`) });
  return <Q q={q}>{(r) => <PushRulesForm ctx={ctx} initial={r} onSaved={() => qc.invalidateQueries({ queryKey: key })} />}</Q>;
}

function PushRulesForm({ ctx, initial, onSaved }: { ctx: Ctx; initial: S["PushRules"]; onSaved: () => void }) {
  const rules = useSaved(initial);
  const r = rules.value;
  const setR = rules.set;
  const [error, setError] = useState<unknown>(null);
  const save = useMutation({
    mutationFn: () => api.put(`/projects/${ctx.p.id}/push_rule`, r),
    onSuccess: () => {
      setError(null);
      rules.done();
      onSaved();
    },
    onError: setError,
  });
  const text = (k: "commit_message_regex" | "commit_message_negative_regex" | "author_email_regex" | "branch_name_regex") => ({
    value: r[k],
    onChange: (e: ChangeEvent<HTMLInputElement>) => setR({ ...r, [k]: e.target.value }),
    className: "mono",
  });
  // the patterns are folded unless one is set
  const patterns = !!(initial.commit_message_regex || initial.commit_message_negative_regex || initial.author_email_regex || initial.branch_name_regex);
  return (
    <form
      className="stack"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <ErrorBox error={error} />
      <label className="check">
        <input type="checkbox" checked={r.prevent_secrets} onChange={(e) => setR({ ...r, prevent_secrets: e.target.checked })} />
        Refuse files that look like secrets (private keys, credentials)
      </label>
      <label className="check">
        <input type="checkbox" checked={r.reject_unsigned_commits} onChange={(e) => setR({ ...r, reject_unsigned_commits: e.target.checked })} />
        Accept only verified signed commits
      </label>
      <div style={{ maxWidth: 260 }}>
        <Field label="Largest file, in MB" hint="0: no limit (Git LFS files do not count)">
          <input type="number" min={0} value={r.max_file_size_mb} onChange={(e) => setR({ ...r, max_file_size_mb: Math.max(0, Number(e.target.value) || 0) })} />
        </Field>
      </div>
      <details className="more-rules" open={patterns}>
        <summary>Advanced rules: patterns for commit messages, author emails and branch names</summary>
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
        </div>
      </details>
      <SaveRow dirty={rules.dirty} busy={save.isPending} saved={rules.saved} label="Save push rules" />
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
/** The intervals, and the one a project already has if it is none of them. */
const intervals = (hours: number) => [
  ...INTERVALS.map(([h, label]) => ({ value: String(h), label })),
  ...(INTERVALS.some(([h]) => h === hours) ? [] : [{ value: String(hours), label: `Every ${hours} hours` }]),
];

function DependencyUpdates({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["renovate", ctx.p.id];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["RenovateStatus"]>(`/projects/${ctx.p.id}/renovate`) });
  return <Q q={q}>{(s) => <DependencyUpdatesForm ctx={ctx} initial={s} onSaved={() => qc.invalidateQueries({ queryKey: key })} />}</Q>;
}

function DependencyUpdatesForm({ ctx, initial, onSaved }: { ctx: Ctx; initial: S["RenovateStatus"]; onSaved: () => void }) {
  const form = useSaved({ enabled: initial.enabled, interval_hours: initial.interval_hours, onboarding: initial.onboarding });
  const { enabled, interval_hours: hours, onboarding } = form.value;
  const setEnabled = (v: boolean) => form.set({ ...form.value, enabled: v });
  const setHours = (v: number) => form.set({ ...form.value, interval_hours: v });
  const setOnboarding = (v: boolean) => form.set({ ...form.value, onboarding: v });
  const [error, setError] = useState<unknown>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const done = (m: string) => {
    setError(null);
    setMsg(m);
    onSaved();
  };
  const save = useMutation({
    mutationFn: () => api.put(`/projects/${ctx.p.id}/renovate`, { enabled, interval_hours: hours, onboarding }),
    onSuccess: () => {
      form.done();
      done(enabled ? "Dependency updates are on: the first run starts within a minute." : "Dependency updates are off.");
    },
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
    <>
    <form
      className="stack"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <ErrorBox error={error} />
      {msg && <div className="alert ok">{msg}</div>}
      <Switch checked={enabled} onChange={setEnabled}>
        Open merge requests that update dependencies
      </Switch>
      {/* its options, once it is on (kept and sent as they are while it is off) */}
      {enabled && (
        <div className="grid2">
          <Field label="Runs">
            <Select value={String(hours)} onChange={(v) => setHours(Number(v))} options={intervals(hours)} />
          </Field>
          <Field label="Configuration">
            <Select
              value={onboarding ? "onboarding" : "defaults"}
              onChange={(v) => setOnboarding(v === "onboarding")}
              options={[
                { value: "onboarding", label: "First, a merge request that adds renovate.json" },
                { value: "defaults", label: "Renovate's defaults, no config file needed" },
              ]}
            />
          </Field>
        </div>
      )}
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
      <SaveRow dirty={form.dirty} busy={save.isPending} saved={form.saved} />
    </form>
    {enabled && <HostRules base={`/projects/${ctx.p.id}/renovate/host_rules`} empty="None. This instance's container registry needs none." />}
    </>
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
  const form = useSaved({ enabled: initial.enabled, scanners: initial.scanners, interval_hours: initial.interval_hours, sbom_on_tags: initial.sbom_on_tags });
  const { enabled, scanners, interval_hours: hours, sbom_on_tags: sbom } = form.value;
  const setEnabled = (v: boolean) => form.set({ ...form.value, enabled: v });
  const setScanners = (v: string[]) => form.set({ ...form.value, scanners: v });
  const setHours = (v: number) => form.set({ ...form.value, interval_hours: v });
  const setSbom = (v: boolean) => form.set({ ...form.value, sbom_on_tags: v });
  const [error, setError] = useState<unknown>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const done = (m: string) => {
    setError(null);
    setMsg(m);
    onSaved();
  };
  const save = useMutation({
    mutationFn: () => api.put(`/projects/${ctx.p.id}/security/scanning`, { enabled, scanners, interval_hours: hours, sbom_on_tags: sbom }),
    onSuccess: () => {
      form.done();
      done(enabled ? "Security scanning is on: the first scan starts within a minute, as you." : "Security scanning is off; findings stay.");
    },
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
      <Switch checked={enabled} onChange={setEnabled}>
        Scan the default branch and merge requests
      </Switch>
      {/* its scanners and schedule, once it is on (kept and sent as they are while it is off) */}
      {enabled && (
        <>
          <FieldGroup label="Scanners" hint="The ones this instance runs.">
            <div className="stack" style={{ gap: 6 }}>
              {initial.available.map((sc) => (
                <label key={sc} className="check">
                  <input type="checkbox" checked={scanners.includes(sc)} onChange={(e) => setScanners(e.target.checked ? [...scanners, sc] : scanners.filter((x) => x !== sc))} />{" "}
                  {SCANNER_LABEL[sc] ?? sc}
                </label>
              ))}
            </div>
          </FieldGroup>
          <Field label="Default branch scans" hint="Also after pushes to the default branch (at most every 10 minutes) and on each new commit of a merge request. Scans run without the project's CI/CD variables.">
            <Select value={String(hours)} onChange={(v) => setHours(Number(v))} options={intervals(hours)} />
          </Field>
          <label className="check">
            <input type="checkbox" checked={sbom} onChange={(e) => setSbom(e.target.checked)} /> Make an SBOM (CycloneDX and SPDX) of each new tag, attached to its release
            (for those who can read the code)
          </label>
        </>
      )}
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
      <SaveRow dirty={form.dirty} busy={save.isPending} saved={form.saved} />
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
              Once a quota is full, new pushes, commits, merges, LFS files, CI artifacts, images and uploads are refused; reading, deleting and CI jobs carry on. Administrators set quotas.
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
  const blank = { name: "", create_access_level: "maintainer" };
  const [form, setForm] = useState(blank);
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const close = () => {
    setAdding(false);
    setForm(blank);
    setError(null);
  };
  const add = useMutation({
    mutationFn: () => api.post(`/projects/${ctx.p.id}/protected_tags`, form),
    onSuccess: () => {
      close();
      qc.invalidateQueries({ queryKey: key });
    },
    onError: setError,
  });
  const del = useMutation({ mutationFn: (id: number) => api.del(`/projects/${ctx.p.id}/protected_tags/${id}`), onSuccess: () => qc.invalidateQueries({ queryKey: key }) });
  return (
    <ListSection
      id="protected-tags"
      title="Protected tags"
      description="Who can create matching tags. Nobody can move or delete them by pushing; Maintainers can delete them here."
      add="Protect a tag"
      adding={adding}
      onAdd={() => setAdding(true)}
      form={
      <form
        className="stack"
        aria-label="Protect a tag"
        onSubmit={(e) => {
          e.preventDefault();
          add.mutate();
        }}
      >
        <ErrorBox error={error} />
        <div className="grid2">
          <Field label="Tag or pattern" hint="e.g. v1.0 or v*">
            <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} />
          </Field>
          <Field label="Allowed to create">
            <Select value={form.create_access_level} onChange={(v) => setForm({ ...form, create_access_level: v })} options={PROTECT_LEVELS} />
          </Field>
        </div>
        <AddActions label="Protect tags" disabled={!form.name} busy={add.isPending} onCancel={close} />
      </form>
      }
    >
      <Q q={list}>
        {(rules) => (
          <div className="list" aria-label="Protected tags">
            {rules.length === 0 && <div className="item muted">No protected tags.</div>}
            {rules.map((r) => (
              <div className="item" key={r.id}>
                <div className="grow" style={{ minWidth: 0 }}>
                  <code>{r.name}</code>
                  <div className="small muted">{whoCan(r.create_access_level)} can create matching tags</div>
                </div>
                <button className="btn small danger" onClick={() => del.mutate(r.id)} aria-label={`Unprotect ${r.name}`}>
                  Unprotect
                </button>
              </div>
            ))}
          </div>
        )}
      </Q>
    </ListSection>
  );
}

function DeployKeys({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const key = ["deploy-keys", ctx.p.id];
  const list = useQuery({ queryKey: key, queryFn: () => api.get<S["DeployKey"][]>(`/projects/${ctx.p.id}/deploy_keys`) });
  const blank = { title: "", key: "", can_push: false };
  const [form, setForm] = useState(blank);
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const close = () => {
    setAdding(false);
    setForm(blank);
    setError(null);
  };
  const add = useMutation({
    mutationFn: () => api.post(`/projects/${ctx.p.id}/deploy_keys`, form),
    onSuccess: () => {
      close();
      qc.invalidateQueries({ queryKey: key });
    },
    onError: setError,
  });
  const del = useMutation({ mutationFn: (id: number) => api.del(`/projects/${ctx.p.id}/deploy_keys/${id}`), onSuccess: () => qc.invalidateQueries({ queryKey: key }) });
  return (
    <ListSection
      id="deploy-keys"
      title="Deploy keys"
      description="SSH keys that reach this repository only, read-only unless allowed to write."
      add="Add a deploy key"
      adding={adding}
      onAdd={() => setAdding(true)}
      form={
      <form
        className="stack"
        aria-label="New deploy key"
        onSubmit={(e) => {
          e.preventDefault();
          add.mutate();
        }}
      >
        <ErrorBox error={error} />
        <Field label="Title">
          <input value={form.title} onChange={(e) => setForm({ ...form, title: e.target.value })} />
        </Field>
        <Field label="Public key">
          <textarea placeholder="ssh-ed25519 AAAA…" value={form.key} onChange={(e) => setForm({ ...form, key: e.target.value })} />
        </Field>
        <label className="check">
          <input type="checkbox" checked={form.can_push} onChange={(e) => setForm({ ...form, can_push: e.target.checked })} />
          Grant write access
        </label>
        <AddActions label="Add key" disabled={!form.key} busy={add.isPending} onCancel={close} />
      </form>
      }
    >
      <Q q={list}>
        {(keys) =>
          keys.length === 0 ? (
            <EmptyRow>No deploy keys.</EmptyRow>
          ) : (
            <div className="list">
              {keys.map((k) => (
                <div className="item" key={k.id}>
                  <div className="grow" style={{ minWidth: 0 }}>
                    <strong>{k.title}</strong>
                    <div className="small muted">
                      {k.can_push ? "Can read and push" : "Can read"} · added {timeAgo(k.created_at)} · <span className="mono" style={{ overflowWrap: "anywhere" }}>
                        {k.fingerprint}
                      </span>
                    </div>
                  </div>
                  <button className="btn small danger" onClick={() => del.mutate(k.id)}>
                    Remove
                  </button>
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </ListSection>
  );
}

const MIRROR_INTERVALS = [
  { value: "15", label: "Every 15 minutes" },
  { value: "60", label: "Every hour" },
  { value: "360", label: "Every 6 hours" },
  { value: "1440", label: "Every day" },
];

type MirrorForm = { direction: "pull" | "push"; url: string; username: string; token: string; only_protected: boolean; trigger_pipelines: boolean; interval_minutes: string };
const NEW_MIRROR: MirrorForm = { direction: "push", url: "", username: "", token: "", only_protected: false, trigger_pipelines: false, interval_minutes: "60" };

function Mirrors({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const confirm = useConfirm();
  const key = ["mirrors", ctx.p.id];
  const base = `/projects/${ctx.p.id}/mirrors`;
  // updates run in the background: after starting one, look again for a while
  const [pollUntil, setPollUntil] = useState(0);
  const list = useQuery({
    queryKey: key,
    queryFn: () => api.get<S["MirrorDto"][]>(base),
    refetchInterval: (q) => (Date.now() < pollUntil || q.state.data?.some((m) => m.enabled && !m.last_success_at && !m.last_error) ? 2000 : false),
  });
  const [form, setForm] = useState<MirrorForm>(NEW_MIRROR);
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const close = () => {
    setAdding(false);
    setForm(NEW_MIRROR);
    setError(null);
  };
  const done = (list: S["MirrorDto"][]) => {
    setPollUntil(Date.now() + 30_000);
    qc.setQueryData(key, list);
    qc.invalidateQueries({ queryKey: ["project", ctx.p.id] });
  };
  const add = useMutation({
    mutationFn: () => api.post<S["MirrorDto"][]>(base, { ...form, interval_minutes: Number(form.interval_minutes) }),
    onSuccess: (l) => {
      close();
      done(l);
    },
    onError: setError,
  });
  // each button passes its own request
  const act = useMutation({ mutationFn: (call: () => Promise<S["MirrorDto"][]>) => call(), onSuccess: done, onError: setError });
  const pulling = list.data?.some((m) => m.direction === "pull");
  const submit = async () => {
    const pull = form.direction === "pull";
    const ok = await confirm({
      title: pull ? "Make this repository a pull mirror?" : "Push this repository there?",
      body: pull
        ? "Its branches and tags become the source's: branches and tags that exist only here are deleted, and nobody can push, commit or merge here until the mirror is removed."
        : form.only_protected
          ? "Protected branches and every tag are pushed there after each change, overwriting what is there."
          : "Every branch and tag is pushed there after each change, overwriting what is there; branches and tags deleted here are deleted there.",
      action: pull ? "Pull mirror" : "Push mirror",
      danger: pull ? !ctx.p.empty_repo : !form.only_protected,
    });
    if (ok) add.mutate();
  };
  return (
    <ListSection
      id="mirroring"
      title="Mirroring"
      description="Keep this repository in step with another one, on any forge: pull its branches and tags from there (this repository is then read-only), or push them there after every change. Only the repository is mirrored: not issues, merge requests or LFS files."
      add="Add a mirror"
      adding={adding}
      onAdd={() => setAdding(true)}
      form={
      <form
        className="stack"
        aria-label="New mirror"
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <ErrorBox error={error} />
        <Segmented
          label="Direction"
          value={form.direction}
          onChange={(direction) => setForm({ ...form, direction })}
          options={[
            { value: "push", label: "Push to" },
            { value: "pull", label: "Pull from", disabled: pulling, hint: pulling ? "This repository is already a pull mirror" : undefined },
          ]}
        />
        <Field label="Repository address" hint="Its http(s) clone address, without a name or password in it.">
          <input value={form.url} onChange={(e) => setForm({ ...form, url: e.target.value })} placeholder="https://github.com/owner/repository.git" />
        </Field>
        <div className="grid2">
          <Field label="Username" hint="Optional: GitLab, GitHub, Gitea and Forgejo take any name with a token.">
            <input value={form.username} autoComplete="off" onChange={(e) => setForm({ ...form, username: e.target.value })} />
          </Field>
          <Field label="Token or password" hint={form.direction === "push" ? "With write access to the repository. Kept encrypted." : "Optional for a public repository. Kept encrypted."}>
            <input type="password" autoComplete="off" value={form.token} onChange={(e) => setForm({ ...form, token: e.target.value })} />
          </Field>
        </div>
        {form.direction === "pull" ? (
          <div className="row wrap">
            <Select label="How often" value={form.interval_minutes} onChange={(v) => setForm({ ...form, interval_minutes: v })} options={MIRROR_INTERVALS} style={{ maxWidth: 220 }} />
            <label className="check grow">
              <input type="checkbox" checked={form.trigger_pipelines} onChange={(e) => setForm({ ...form, trigger_pipelines: e.target.checked })} />
              Run pipelines for what arrives (as you)
            </label>
          </div>
        ) : (
          <label className="check">
            <input type="checkbox" checked={form.only_protected} onChange={(e) => setForm({ ...form, only_protected: e.target.checked })} />
            Protected branches only <span className="faint small">· and nothing deleted there</span>
          </label>
        )}
        <AddActions label="Add mirror" disabled={!form.url.trim()} busy={add.isPending} onCancel={close} />
      </form>
      }
    >
      {!adding && <ErrorBox error={error} />}
      <Q q={list}>
        {(mirrors) =>
          mirrors.length === 0 ? (
            <EmptyRow>No mirrors.</EmptyRow>
          ) : (
            <div className="list">
              {mirrors.map((m) => (
                <div className="item mirror-row" key={m.id}>
                  <Badge>{m.direction === "pull" ? "Pull" : "Push"}</Badge>
                  <div className="grow" style={{ minWidth: 0 }}>
                    <div className="mono small ellipsis">{m.url}</div>
                    <div className="small">
                      {!m.enabled ? (
                        <span className="faint">Paused</span>
                      ) : m.last_error ? (
                        <span className="error-text" title={m.last_error}>
                          Failed {m.last_update_at ? timeAgo(m.last_update_at) : ""}: {m.last_error}
                        </span>
                      ) : m.last_success_at ? (
                        <span className="faint">Updated {timeAgo(m.last_success_at)}</span>
                      ) : (
                        <span className="faint">Waiting for its first update…</span>
                      )}
                      <span className="faint">
                        {m.direction === "pull" ? ` · ${MIRROR_INTERVALS.find((i) => i.value === String(m.interval_minutes))?.label.toLowerCase() ?? `every ${m.interval_minutes} minutes`}` : ""}
                        {m.only_protected ? " · protected branches" : ""}
                        {m.trigger_pipelines ? " · runs pipelines" : ""}
                        {m.has_token ? "" : " · no token"}
                      </span>
                    </div>
                  </div>
                  <div className="mirror-actions">
                    <button className="btn small" disabled={!m.enabled} onClick={() => act.mutate(() => api.post(`${base}/${m.id}/update`, {}))}>
                      Update now
                    </button>
                    <button className="btn small" onClick={() => act.mutate(() => api.put(`${base}/${m.id}`, { enabled: !m.enabled }))}>
                      {m.enabled ? "Pause" : "Resume"}
                    </button>
                    <button
                      className="btn small danger"
                      onClick={async () => {
                        const ok = await confirm({
                          title: "Remove this mirror?",
                          body: m.direction === "pull" ? "The repository stops following its source and can be pushed to again (pausing keeps it read-only)." : "Changes here stop going there.",
                          action: "Remove",
                          danger: true,
                        });
                        if (ok) act.mutate(() => api.del(`${base}/${m.id}`));
                      }}
                    >
                      Remove
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </ListSection>
  );
}
