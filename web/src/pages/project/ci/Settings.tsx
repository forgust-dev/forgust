import { useState, type FormEvent } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Eye, EyeOff, KeyRound, Pencil, Plus, Trash2 } from "lucide-react";
import { Link } from "react-router-dom";
import { api, timeAgo, type S } from "../../../api/client";
import { Section, ErrorBox, Field, Q, ListHead } from "../../../components/ui";
import { RunnersPanel } from "./Runners";
import { EmptyRow, ListSection, SaveRow, SettingSwitch, useSaved } from "../../../components/settings";
import { FormActions, FormDialog, useJustAdded } from "../../../components/FormDialog";
import { saveCiSettings, useCiSettings } from "../ciSettings";
import { Select } from "../../../components/controls";
import { useToast } from "../../../shell/toast";
import { useConfirm } from "../../../shell/dialog";
import type { Ctx } from "../Project";

// ---------------------------------------------------------------- variables

type VarForm = { key: string; value: string; variable_type: string; protected: boolean; masked: boolean; raw: boolean; description: string };
const EMPTY: VarForm = { key: "", value: "", variable_type: "env_var", protected: false, masked: false, raw: false, description: "" };

/** CI/CD variables of a project (`/projects/:id`) or group (`/groups/:id`). */
export function VariablesPanel({ base }: { base: string }) {
  const qc = useQueryClient();
  const toast = useToast();
  const confirm = useConfirm();
  const q = useQuery({ queryKey: ["variables", base], queryFn: () => api.get<S["VariableDto"][]>(`${base}/variables`) });
  const [form, setForm] = useState<VarForm | null>(null);
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [shown, setShown] = useState<Record<string, boolean>>({});
  const fresh = useJustAdded();
  const close = () => {
    setForm(null);
    setError(null);
  };
  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (!form) return;
    setError(null);
    try {
      // masked values are write-only: an empty value keeps the stored one
      const body = { ...form, value: editing && form.masked && !form.value ? undefined : form.value };
      await api.post(`${base}/variables`, body);
      toast.ok(`Saved ${form.key}`);
      fresh.mark(form.key);
      close();
      qc.invalidateQueries({ queryKey: ["variables", base] });
    } catch (err) {
      setError(err);
    }
  };
  const remove = async (key: string) => {
    if (!(await confirm({ title: `Delete the variable ${key}?`, body: "Jobs that start from now on no longer get it.", action: "Delete variable", danger: true }))) return;
    try {
      await api.del(`${base}/variables/${encodeURIComponent(key)}`);
      toast.ok(`Deleted ${key}`);
      qc.invalidateQueries({ queryKey: ["variables", base] });
    } catch (e) {
      toast.error(e);
    }
  };
  return (
    <Section
      id="variables"
      title="Variables"
      description="Values given to jobs as environment variables."
      actions={
        <button
          className="btn small"
          onClick={() => {
            setEditing(false);
            setForm({ ...EMPTY });
          }}
        >
          <Plus size={14} /> Add a variable
        </button>
      }
    >
      <div className="stack">
        <p className="muted small" style={{ margin: 0 }}>
          Values are encrypted at rest. <strong>Protected</strong> variables only reach pipelines of protected branches;{" "}
          <strong>masked</strong> ones are hidden in job logs and cannot be read back here.
        </p>
        <FormDialog open={form != null} onClose={close} title={editing ? "Edit the variable" : "Add a variable"}>
        {form && (
          <form className="stack" onSubmit={save}>
            <ErrorBox error={error} />
            <div className="grid2">
              <Field label="Key">
                <input value={form.key} disabled={editing} onChange={(e) => setForm({ ...form, key: e.target.value.toUpperCase().replace(/[^A-Z0-9_]/g, "_") })} style={{ fontFamily: "var(--mono)" }} required />
              </Field>
              <Field label="Type">
                <Select
                  value={form.variable_type}
                  onChange={(v) => setForm({ ...form, variable_type: v })}
                  options={[
                    { value: "env_var", label: "Variable" },
                    { value: "file", label: "File", hint: "The job gets a path to a file with the value" },
                  ]}
                />
              </Field>
            </div>
            <Field label="Value" hint={editing && form.masked ? "Leave empty to keep the current value" : undefined}>
              <textarea rows={3} value={form.value} onChange={(e) => setForm({ ...form, value: e.target.value })} style={{ fontFamily: "var(--mono)", fontSize: 13 }} />
            </Field>
            <Field label="Description">
              <input value={form.description} onChange={(e) => setForm({ ...form, description: e.target.value })} />
            </Field>
            <div className="row wrap" style={{ gap: 18 }}>
              <label className="check">
                <input type="checkbox" checked={form.protected} onChange={(e) => setForm({ ...form, protected: e.target.checked })} /> Protected
              </label>
              <label className="check">
                <input type="checkbox" checked={form.masked} onChange={(e) => setForm({ ...form, masked: e.target.checked })} /> Masked (8+ characters, one line, no spaces)
              </label>
              <label className="check">
                <input type="checkbox" checked={form.raw} onChange={(e) => setForm({ ...form, raw: e.target.checked })} />
                <span>
                  Do not expand <code>$VARS</code>
                </span>
              </label>
            </div>
            <FormActions label={editing ? "Save" : "Add variable"} onCancel={close} />
          </form>
        )}
        </FormDialog>
        <Q q={q}>
          {(list) =>
            list.length === 0 ? (
              <EmptyRow>No variables yet.</EmptyRow>
            ) : (
              <div className="list">
                {list.map((v) => (
                  <div className={"item" + fresh.fresh(v.key)} key={v.key}>
                    <KeyRound size={15} className="faint" />
                    <div className="grow" style={{ minWidth: 0 }}>
                      <code>{v.key}</code>{" "}
                      {v.protected && <span className="badge">protected</span>} {v.masked && <span className="badge">masked</span>}{" "}
                      {v.variable_type === "file" && <span className="badge">file</span>} {v.raw && <span className="badge">raw</span>}
                      {v.description && <div className="small muted ellipsis">{v.description}</div>}
                    </div>
                    <span className="mono small ellipsis" style={{ maxWidth: 260 }}>
                      {v.masked ? "••••••••" : shown[v.key] ? v.value : "••••"}
                    </span>
                    {!v.masked && (
                      <button className="icon-btn" aria-label={shown[v.key] ? `Hide ${v.key}` : `Reveal ${v.key}`} onClick={() => setShown({ ...shown, [v.key]: !shown[v.key] })}>
                        {shown[v.key] ? <EyeOff size={15} /> : <Eye size={15} />}
                      </button>
                    )}
                    <button
                      className="icon-btn"
                      aria-label={`Edit ${v.key}`}
                      onClick={() => {
                        setEditing(true);
                        setForm({ key: v.key, value: v.value ?? "", variable_type: v.variable_type, protected: v.protected, masked: v.masked, raw: v.raw, description: v.description });
                      }}
                    >
                      <Pencil size={15} />
                    </button>
                    <button className="icon-btn" aria-label={`Delete ${v.key}`} onClick={() => remove(v.key)}>
                      <Trash2 size={15} />
                    </button>
                  </div>
                ))}
              </div>
            )
          }
        </Q>
      </div>
    </Section>
  );
}

// ---------------------------------------------------------------- runners

/** Where the configuration lives, how long a job may run, how deep it clones, older pipelines. */
function Pipelines({ ctx }: { ctx: Ctx }) {
  const q = useCiSettings(ctx.p.id);
  return (
    <Section id="pipelines" title="Pipelines" description="Where the pipeline configuration lives, how long a job may run, how deep it clones, and what happens to older pipelines.">
      <Q q={q}>{(s) => <PipelinesForm ctx={ctx} initial={s} />}</Q>
    </Section>
  );
}

function PipelinesForm({ ctx, initial }: { ctx: Ctx; initial: S["ProjectCiSettings"] }) {
  const qc = useQueryClient();
  const form = useSaved({
    ci_config_path: initial.ci_config_path,
    default_timeout_secs: initial.default_timeout_secs,
    default_git_depth: initial.default_git_depth,
    auto_cancel_redundant: initial.auto_cancel_redundant,
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
      <div className="grid2">
        <Field label="CI/CD configuration file">
          <input value={s.ci_config_path} onChange={(e) => form.set({ ...s, ci_config_path: e.target.value })} />
        </Field>
        <Field label="Job timeout (minutes)">
          <input type="number" min={10} value={Math.round(s.default_timeout_secs / 60)} onChange={(e) => form.set({ ...s, default_timeout_secs: Number(e.target.value) * 60 })} />
        </Field>
        <Field label="Git shallow clone depth" hint="0 fetches everything">
          <input type="number" min={0} max={1000} value={s.default_git_depth} onChange={(e) => form.set({ ...s, default_git_depth: Number(e.target.value) })} />
        </Field>
      </div>
      <label className="check">
        <input type="checkbox" checked={s.auto_cancel_redundant} onChange={(e) => form.set({ ...s, auto_cancel_redundant: e.target.checked })} />
        <span>
          Cancel older pipelines of a branch when a pipeline for a newer commit starts (jobs marked <code>interruptible</code>, and jobs not started yet)
        </span>
      </label>
      <SaveRow dirty={form.dirty} busy={busy} saved={form.saved} />
    </form>
  );
}

/** The instance's runners for this project: one switch, beside the runners it adds. */
function SharedRunners({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const q = useCiSettings(ctx.p.id);
  if (!q.data) return null;
  return (
    <SettingSwitch checked={q.data.instance_runners_enabled} onSave={(on) => saveCiSettings(qc, ctx.p.id, { instance_runners_enabled: on })}>
      Use the instance&apos;s shared runners
    </SettingSwitch>
  );
}

function JobTokenAllowlist({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const toast = useToast();
  const q = useQuery({ queryKey: ["allowlist", ctx.p.id], queryFn: () => api.get<S["AllowedProject"][]>(`/projects/${ctx.p.id}/job_token_allowlist`) });
  const [path, setPath] = useState("");
  const [adding, setAdding] = useState(false);
  const fresh = useJustAdded();
  const close = () => {
    setAdding(false);
    setPath("");
  };
  const add = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api.post(`/projects/${ctx.p.id}/job_token_allowlist`, { project: path });
      fresh.mark(path.trim());
      close();
      qc.invalidateQueries({ queryKey: ["allowlist", ctx.p.id] });
    } catch (err) {
      toast.error(err);
    }
  };
  const remove = async (id: number) => {
    try {
      await api.del(`/projects/${ctx.p.id}/job_token_allowlist/${id}`);
      qc.invalidateQueries({ queryKey: ["allowlist", ctx.p.id] });
    } catch (err) {
      toast.error(err);
    }
  };
  const ci = useCiSettings(ctx.p.id);
  return (
    <ListSection
      id="job-token-access"
      title="Job token access"
      description="Other projects whose CI job tokens may reach this one."
      add="Add a project"
      adding={adding}
      onClose={close}
      onAdd={() => setAdding(true)}
      form={
        <form className="stack" aria-label="Allow a project" onSubmit={add}>
          <Field label="Project path">
            <input placeholder="group/project" value={path} onChange={(e) => setPath(e.target.value)} />
          </Field>
          <FormActions label="Add project" disabled={!path.trim()} onCancel={close} />
        </form>
      }
    >
        {ci.data && (
          <SettingSwitch checked={ci.data.job_token_scope_enabled} onSave={(on) => saveCiSettings(qc, ctx.p.id, { job_token_scope_enabled: on })}>
            Only the projects listed here can use their job tokens to access this project
          </SettingSwitch>
        )}
        <p className="muted small" style={{ margin: 0 }}>
          Jobs of these projects can read this one with <code>CI_JOB_TOKEN</code> (clone, artifacts), within what their pipeline's user can see.
        </p>
        <Q q={q}>
          {(list) =>
            list.length === 0 ? (
              <EmptyRow>No other project: only this project's own jobs can use its job token.</EmptyRow>
            ) : (
              <div className="list">
                {list.map((a) => (
                  <div className={"item" + fresh.fresh(a.full_path)} key={a.project_id}>
                    <span className="grow">{a.full_path}</span>
                    <button className="icon-btn" aria-label={`Remove ${a.full_path}`} onClick={() => remove(a.project_id)}>
                      <Trash2 size={15} />
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

export function CiSettings({ ctx }: { ctx: Ctx }) {
  return (
    <div className="stack settings-page">
      <ListHead title="CI/CD settings" sub="Pipelines, runners, variables, environments and Pages of this project." />
      <Pipelines ctx={ctx} />
      <RunnersPanel
        list={`/projects/${ctx.p.id}/runners`}
        create={`/projects/${ctx.p.id}/runners`}
        note="Project runners, plus the runners of its groups and the instance's shared runners. A new runner here serves this project only."
      >
        <SharedRunners ctx={ctx} />
      </RunnersPanel>
      <VariablesPanel base={`/projects/${ctx.p.id}`} />
      <ProtectedEnvironments ctx={ctx} />
      <Section id="pages" title="Pages" description="A static site built by CI: a job named pages (or with pages: true) publishes the public/ folder of its artifacts.">
        <PagesSite ctx={ctx} />
      </Section>
      <JobTokenAllowlist ctx={ctx} />
    </div>
  );
}

/** Who may deploy to matching environments (jobs of others fail). */
function ProtectedEnvironments({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const toast = useToast();
  const key = ["protected-envs", ctx.p.id];
  const q = useQuery({ queryKey: key, queryFn: () => api.get<S["ProtectedEnvironment"][]>(`/projects/${ctx.p.id}/protected_environments`) });
  const [name, setName] = useState("");
  const [level, setLevel] = useState("maintainer");
  const [adding, setAdding] = useState(false);
  const fresh = useJustAdded();
  const [error, setError] = useState<unknown>(null);
  const close = () => {
    setAdding(false);
    setName("");
    setError(null);
  };
  const add = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api.post(`/projects/${ctx.p.id}/protected_environments`, { name, deploy_access_level: level });
      fresh.mark(name.trim());
      close();
      qc.invalidateQueries({ queryKey: key });
    } catch (err) {
      setError(err);
    }
  };
  const del = async (id: number) => {
    try {
      await api.del(`/projects/${ctx.p.id}/protected_environments/${id}`);
      qc.invalidateQueries({ queryKey: key });
    } catch (err) {
      toast.error(err);
    }
  };
  const levelName = (l: number) => (l >= 50 ? "Owners" : l >= 40 ? "Maintainers" : "Developers and Maintainers");
  return (
    <ListSection
      id="protected-environments"
      title="Protected environments"
      description="Only these roles may deploy to (or stop) matching environments; others' jobs fail."
      add="Protect an environment"
      adding={adding}
      onClose={close}
      onAdd={() => setAdding(true)}
      form={
      <form className="stack" aria-label="Protect an environment" onSubmit={add}>
        <ErrorBox error={error} />
        <div className="grid2">
        <Field label="Environment or pattern">
          <input value={name} onChange={(e) => setName(e.target.value)} placeholder="production or review/*" />
        </Field>
        <Field label="Allowed to deploy">
          <Select
            value={level}
            onChange={setLevel}
            options={[
              { value: "developer", label: "Developers + Maintainers" },
              { value: "maintainer", label: "Maintainers" },
              { value: "owner", label: "Owners" },
            ]}
          />
        </Field>
        </div>
        <FormActions label="Protect" disabled={!name.trim()} onCancel={close} />
      </form>
      }
    >
      <Q q={q}>
        {(list) => (
          <div className="list">
            {list.length === 0 && <div className="item muted">No protected environments.</div>}
            {list.map((pe) => (
              <div className={"item" + fresh.fresh(pe.name)} key={pe.id}>
                <div className="grow" style={{ minWidth: 0 }}>
                  <code>{pe.name}</code>
                  <div className="small muted">Only {levelName(pe.deploy_access_level)} can deploy to it or stop it</div>
                </div>
                <button className="btn small danger" onClick={() => del(pe.id)}>
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

/** The project's static site, published by its Pages job. */
function PagesSite({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const toast = useToast();
  const confirm = useConfirm();
  const q = useQuery({ queryKey: ["pages", ctx.p.id], queryFn: () => api.get<S["PagesInfo"]>(`/projects/${ctx.p.id}/pages`) });
  return (
    <Q q={q}>
      {(s) =>
        !s.enabled ? (
          <p className="small muted" style={{ margin: 0 }}>
            This instance does not serve sites: an administrator sets <code>pages.domain</code> (a wildcard DNS name) to turn them on.
          </p>
        ) : (
          <div className="stack">
            {s.deployment ? (
              <>
                <p style={{ margin: 0 }}>
                  {s.public ? (
                    <a href={s.url ?? "#"} target="_blank" rel="noreferrer">
                      {s.url}
                    </a>
                  ) : (
                    <span className="mono">{s.url}</span>
                  )}
                </p>
                <p className="small muted" style={{ margin: 0 }}>
                  Published from <code>{s.deployment.publish}/</code> of <Link to={`${ctx.web}/-/jobs/${s.deployment.build_id}`}>job #{s.deployment.build_id}</Link> (pipeline #{s.deployment.pipeline_iid}),{" "}
                  {timeAgo(s.deployment.created_at)}. Its artifacts are kept while it is the site.
                  {!s.public && " Only public projects' sites are served: this one is not reachable."}
                </p>
                <div>
                  <button
                    className="btn small danger"
                    onClick={async () => {
                      if (!(await confirm({ title: "Take the site down?", body: "The next Pages job publishes it again.", action: "Take the site down", danger: true }))) return;
                      try {
                        await api.del(`/projects/${ctx.p.id}/pages`);
                        toast.ok("Site removed");
                        qc.invalidateQueries({ queryKey: ["pages", ctx.p.id] });
                      } catch (e) {
                        toast.error(e);
                      }
                    }}
                  >
                    Remove the site
                  </button>
                </div>
              </>
            ) : (
              <p className="small muted" style={{ margin: 0 }}>
                Not published yet. Once a Pages job passes, the site is at <span className="mono">{s.url}</span>
                {!s.public && " (public projects only)"}.
              </p>
            )}
          </div>
        )
      }
    </Q>
  );
}
