import { useState, type FormEvent } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Lock, Plus, Server, Trash2 } from "lucide-react";
import { api, timeAgo, type S } from "../../../api/client";
import { Section, Empty, ErrorBox, Field, NewSecret, Q } from "../../../components/ui";
import { useToast } from "../../../shell/toast";

// Runners: a project's, a group's, the instance's (also loaded by Admin and groups).

const RUNNER_STATUS: Record<string, string> = { online: "ok", offline: "", never_contacted: "", paused: "warn" };

function RegisterHelp({ token }: { token: string }) {
  const cmd = `gitlab-runner register --non-interactive \\\n  --url ${window.location.origin} \\\n  --token ${token} \\\n  --executor docker --docker-image alpine:3.20`;
  return (
    <div className="stack" style={{ gap: 8 }}>
      <NewSecret token={token} />
      <span className="small">Register a stock GitLab Runner (17.x to 19.x) on the machine that runs jobs:</span>
      <pre className="markdown" style={{ margin: 0 }}>
        <code>{cmd}</code>
      </pre>
    </div>
  );
}

/** Runners at one level: `list` and `create` are API paths. */
export function RunnersPanel({ list, create, title = "Runners", note }: { list: string; create?: string; title?: string; note?: string }) {
  const qc = useQueryClient();
  const toast = useToast();
  const q = useQuery({ queryKey: ["runners", list], queryFn: () => api.get<S["RunnerDto"][]>(list) });
  const [form, setForm] = useState<{ description: string; tags: string; run_untagged: boolean; protected_only: boolean } | null>(null);
  const [token, setToken] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!form || !create) return;
    setError(null);
    try {
      const tags = form.tags.split(",").map((t) => t.trim()).filter(Boolean);
      const r = await api.post<S["NewRunnerToken"]>(create, { description: form.description, tags, run_untagged: form.run_untagged || tags.length === 0, protected_only: form.protected_only });
      setToken(r.token);
      setForm(null);
      qc.invalidateQueries({ queryKey: ["runners", list] });
    } catch (err) {
      setError(err);
    }
  };
  const update = async (r: S["RunnerDto"], patch: Partial<S["RunnerBody"]>) => {
    try {
      await api.put(`/runners/${r.id}`, {
        description: r.description,
        tags: r.tags,
        run_untagged: r.run_untagged,
        protected_only: r.protected_only,
        paused: r.paused,
        maximum_timeout_secs: r.maximum_timeout_secs,
        ...patch,
      });
      qc.invalidateQueries({ queryKey: ["runners", list] });
    } catch (e) {
      toast.error(e);
    }
  };
  const remove = async (r: S["RunnerDto"]) => {
    if (!confirm(`Delete runner #${r.id}? Machines using its token stop getting jobs.`)) return;
    try {
      await api.del(`/runners/${r.id}`);
      toast.ok("Runner deleted");
      qc.invalidateQueries({ queryKey: ["runners", list] });
    } catch (e) {
      toast.error(e);
    }
  };
  return (
    <Section
      title={title}
      description={note ?? "Machines that pick up and run the jobs."}
      actions={
        create && (
          <button className="btn small" onClick={() => setForm({ description: "", tags: "", run_untagged: true, protected_only: false })}>
            <Plus size={14} /> New runner
          </button>
        )
      }
    >
      <div className="stack">
        {token && <RegisterHelp token={token} />}
        {form && (
          <form className="card card-body stack" onSubmit={submit}>
            <ErrorBox error={error} />
            <Field label="Description">
              <input value={form.description} onChange={(e) => setForm({ ...form, description: e.target.value })} placeholder="e.g. docker on build-01" />
            </Field>
            <Field label="Tags" hint="Comma-separated; jobs with these tags can run here">
              <input value={form.tags} onChange={(e) => setForm({ ...form, tags: e.target.value })} placeholder="docker, linux" />
            </Field>
            <div className="row wrap" style={{ gap: 18 }}>
              <label className="check">
                <input type="checkbox" checked={form.run_untagged} onChange={(e) => setForm({ ...form, run_untagged: e.target.checked })} /> Run untagged jobs
              </label>
              <label className="check">
                <input type="checkbox" checked={form.protected_only} onChange={(e) => setForm({ ...form, protected_only: e.target.checked })} /> Protected branches only
              </label>
            </div>
            <div className="row">
              <button className="btn primary">Create runner</button>
              <button type="button" className="btn" onClick={() => setForm(null)}>
                Cancel
              </button>
            </div>
          </form>
        )}
        <Q q={q}>
          {(rs) =>
            rs.length === 0 ? (
              <Empty>No runners yet.</Empty>
            ) : (
              <div className="list">
                {rs.map((r) => (
                  <div className="item" key={r.id} style={{ alignItems: "flex-start" }}>
                    <Server size={16} className={`st-${RUNNER_STATUS[r.status] === "ok" ? "success" : r.status === "paused" ? "manual" : "skipped"}`} style={{ marginTop: 3 }} />
                    <div className="grow stack" style={{ gap: 3, minWidth: 0 }}>
                      <span>
                        <strong>#{r.id}</strong> {r.description || <span className="faint">no description</span>}{" "}
                        <span className={`badge ${RUNNER_STATUS[r.status] ?? ""}`}>{r.status.replace("_", " ")}</span> <span className="badge">{r.kind}</span>
                        {r.protected_only && (
                          <span className="badge">
                            <Lock size={10} /> protected
                          </span>
                        )}
                      </span>
                      <span className="small muted">
                        {r.tags.length ? `tags: ${r.tags.join(", ")}` : "no tags"}
                        {r.run_untagged ? " · runs untagged jobs" : ""} · token …{r.token_last4}
                        {r.contacted_at && ` · seen ${timeAgo(r.contacted_at)}`}
                      </span>
                      {r.managers.map((m) => (
                        <span key={m.id} className="small faint">
                          {m.system_id} · {m.version || "?"} {m.executor && `· ${m.executor}`} {m.platform && `· ${m.platform}/${m.architecture}`} · {timeAgo(m.contacted_at)}
                        </span>
                      ))}
                    </div>
                    {r.can_manage && (
                      <div className="row">
                        <button className="btn small" onClick={() => update(r, { paused: !r.paused })}>
                          {r.paused ? "Resume" : "Pause"}
                        </button>
                        <button className="icon-btn" aria-label={`Delete runner ${r.id}`} onClick={() => remove(r)}>
                          <Trash2 size={15} />
                        </button>
                      </div>
                    )}
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

// ---------------------------------------------------------------- project page
