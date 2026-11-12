import { useEffect, useState, type FormEvent } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Eye, EyeOff, KeyRound, Pencil, Plus, Trash2 } from "lucide-react";
import { api, type S } from "../../../api/client";
import { Section, Empty, ErrorBox, Field, Q, ListHead } from "../../../components/ui";
import { RunnersPanel } from "./Runners";
import { Select } from "../../../components/controls";
import { useToast } from "../../../shell/toast";
import type { Ctx } from "../Project";

// ---------------------------------------------------------------- variables

type VarForm = { key: string; value: string; variable_type: string; protected: boolean; masked: boolean; raw: boolean; description: string };
const EMPTY: VarForm = { key: "", value: "", variable_type: "env_var", protected: false, masked: false, raw: false, description: "" };

/** CI/CD variables of a project (`/projects/:id`) or group (`/groups/:id`). */
export function VariablesPanel({ base }: { base: string }) {
  const qc = useQueryClient();
  const toast = useToast();
  const q = useQuery({ queryKey: ["variables", base], queryFn: () => api.get<S["VariableDto"][]>(`${base}/variables`) });
  const [form, setForm] = useState<VarForm | null>(null);
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [shown, setShown] = useState<Record<string, boolean>>({});
  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (!form) return;
    setError(null);
    try {
      // masked values are write-only: an empty value keeps the stored one
      const body = { ...form, value: editing && form.masked && !form.value ? undefined : form.value };
      await api.post(`${base}/variables`, body);
      toast.ok(`Saved ${form.key}`);
      setForm(null);
      qc.invalidateQueries({ queryKey: ["variables", base] });
    } catch (err) {
      setError(err);
    }
  };
  const remove = async (key: string) => {
    if (!confirm(`Delete the variable ${key}?`)) return;
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
          <Plus size={14} /> Add variable
        </button>
      }
    >
      <div className="stack">
        <p className="muted small" style={{ margin: 0 }}>
          Values are encrypted at rest. <strong>Protected</strong> variables only reach pipelines of protected branches;{" "}
          <strong>masked</strong> ones are hidden in job logs and cannot be read back here.
        </p>
        {form && (
          <form className="card card-body stack" onSubmit={save}>
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
            <div className="row">
              <button className="btn primary">Save</button>
              <button type="button" className="btn" onClick={() => setForm(null)}>
                Cancel
              </button>
            </div>
          </form>
        )}
        <Q q={q}>
          {(list) =>
            list.length === 0 ? (
              <Empty>No variables yet.</Empty>
            ) : (
              <div className="list">
                {list.map((v) => (
                  <div className="item" key={v.key}>
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

function General({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const toast = useToast();
  const q = useQuery({ queryKey: ["ci-settings", ctx.p.id], queryFn: () => api.get<S["ProjectCiSettings"]>(`/projects/${ctx.p.id}/ci/settings`) });
  const [s, setS] = useState<S["ProjectCiSettings"] | null>(null);
  useEffect(() => {
    if (q.data) setS(q.data);
  }, [q.data]);
  const [error, setError] = useState<unknown>(null);
  if (!s) return <Q q={q}>{() => null}</Q>;
  const save = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      await api.put(`/projects/${ctx.p.id}/ci/settings`, s);
      toast.ok("CI/CD settings saved");
      qc.invalidateQueries({ queryKey: ["ci-settings", ctx.p.id] });
    } catch (err) {
      setError(err);
    }
  };
  return (
    <Section title="General pipelines" description="Where the pipeline configuration lives, job timeouts and clone depth, and when merge requests may merge.">
      <form className="stack" onSubmit={save}>
        <ErrorBox error={error} />
        <div className="grid2">
          <Field label="CI/CD configuration file">
            <input value={s.ci_config_path} onChange={(e) => setS({ ...s, ci_config_path: e.target.value })} />
          </Field>
          <Field label="Job timeout (minutes)">
            <input type="number" min={10} value={Math.round(s.default_timeout_secs / 60)} onChange={(e) => setS({ ...s, default_timeout_secs: Number(e.target.value) * 60 })} />
          </Field>
          <Field label="Git shallow clone depth" hint="0 fetches everything">
            <input type="number" min={0} max={1000} value={s.default_git_depth} onChange={(e) => setS({ ...s, default_git_depth: Number(e.target.value) })} />
          </Field>
        </div>
        <label className="check">
          <input type="checkbox" checked={s.only_merge_if_pipeline_succeeds} onChange={(e) => setS({ ...s, only_merge_if_pipeline_succeeds: e.target.checked, merge_trains_enabled: e.target.checked && s.merge_trains_enabled })} /> Merge requests can
          only be merged when the pipeline succeeds
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={s.merge_trains_enabled}
            disabled={!s.only_merge_if_pipeline_succeeds}
            onChange={(e) => setS({ ...s, merge_trains_enabled: e.target.checked })}
          />{" "}
          Merge through merge trains: each merge request is tested on top of the ones queued ahead of it, then they merge in order
        </label>
        <label className="check">
          <input type="checkbox" checked={s.block_new_security_findings} onChange={(e) => setS({ ...s, block_new_security_findings: e.target.checked })} />
          <span>Merge requests whose pipeline finds new critical or high security findings can only be merged once those are fixed or dismissed</span>
        </label>
        <label className="check">
          <input type="checkbox" checked={s.auto_cancel_redundant} onChange={(e) => setS({ ...s, auto_cancel_redundant: e.target.checked })} />
          <span>
            Cancel older pipelines of a branch when a pipeline for a newer commit starts (jobs marked <code>interruptible</code>, and jobs not started yet)
          </span>
        </label>
        <label className="check">
          <input type="checkbox" checked={s.instance_runners_enabled} onChange={(e) => setS({ ...s, instance_runners_enabled: e.target.checked })} /> Use the instance's shared
          runners
        </label>
        <label className="check">
          <input type="checkbox" checked={s.job_token_scope_enabled} onChange={(e) => setS({ ...s, job_token_scope_enabled: e.target.checked })} /> Only projects on the job
          token allowlist below can use their job tokens to access this project
        </label>
        <div>
          <button className="btn primary">Save</button>
        </div>
      </form>
    </Section>
  );
}

function JobTokenAllowlist({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const toast = useToast();
  const q = useQuery({ queryKey: ["allowlist", ctx.p.id], queryFn: () => api.get<S["AllowedProject"][]>(`/projects/${ctx.p.id}/job_token_allowlist`) });
  const [path, setPath] = useState("");
  const add = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api.post(`/projects/${ctx.p.id}/job_token_allowlist`, { project: path });
      setPath("");
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
  return (
    <Section title="Job token access" description="Other projects whose CI job tokens may reach this one.">
      <div className="stack">
        <p className="muted small" style={{ margin: 0 }}>
          Jobs of these projects can read this one with <code>CI_JOB_TOKEN</code> (clone, artifacts), within what their pipeline's user can see.
        </p>
        <form className="row" onSubmit={add}>
          <input className="grow" placeholder="group/project" value={path} onChange={(e) => setPath(e.target.value)} aria-label="Project path" />
          <button className="btn" disabled={!path.trim()}>
            Add project
          </button>
        </form>
        <Q q={q}>
          {(list) =>
            list.length === 0 ? (
              <span className="faint small">Only this project's own jobs.</span>
            ) : (
              <div className="list">
                {list.map((a) => (
                  <div className="item" key={a.project_id}>
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
      </div>
    </Section>
  );
}

export function CiSettings({ ctx }: { ctx: Ctx }) {
  return (
    <div className="stack settings-page">
      <ListHead title="CI/CD settings" sub="Pipelines, runners and variables of this project." />
      <General ctx={ctx} />
      <RunnersPanel
        list={`/projects/${ctx.p.id}/runners`}
        create={`/projects/${ctx.p.id}/runners`}
        note="Project runners, plus the runners of its groups and the instance's shared runners. A new runner here serves this project only."
      />
      <VariablesPanel base={`/projects/${ctx.p.id}`} />
      <ProtectedEnvironments ctx={ctx} />
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
  const [error, setError] = useState<unknown>(null);
  const add = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api.post(`/projects/${ctx.p.id}/protected_environments`, { name, deploy_access_level: level });
      setName("");
      setError(null);
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
  const levelName = (l: number) => (l >= 50 ? "Owners" : l >= 40 ? "Maintainers" : "Developers");
  return (
    <Section title="Protected environments" description="Only these roles may deploy to (or stop) matching environments; others' jobs fail.">
      <form className="row wrap" style={{ gap: 10 }} onSubmit={add}>
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
        <div style={{ alignSelf: "end" }}>
          <button className="btn primary" disabled={!name.trim()}>
            Protect
          </button>
        </div>
      </form>
      <ErrorBox error={error} />
      <Q q={q}>
        {(list) => (
          <div className="list" style={{ marginTop: 10 }}>
            {list.length === 0 && <div className="item muted">No protected environments.</div>}
            {list.map((pe) => (
              <div className="item" key={pe.id}>
                <code className="grow">{pe.name}</code>
                <span className="small muted">{levelName(pe.deploy_access_level)}</span>
                <button className="btn small danger" onClick={() => del(pe.id)}>
                  Unprotect
                </button>
              </div>
            ))}
          </div>
        )}
      </Q>
    </Section>
  );
}
