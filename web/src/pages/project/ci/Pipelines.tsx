import { useState, type FormEvent } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarClock, GitBranch, Play, RotateCcw, Square, Tag, Timer, Plus, X } from "lucide-react";
import { api, qs, timeAgo, type S } from "../../../api/client";
import { Empty, ErrorBox, Field, FieldGroup, FilterTabs, ListHead, Q, SearchField } from "../../../components/ui";
import { Select } from "../../../components/controls";
import { useCiLive } from "../../../shell/live";
import { useAct } from "../../../shell/toast";
import type { Ctx } from "../Project";
import { PipelineTests } from "./Tests";
import { MiniPipeline, RETRYABLE, StatusBadge, StatusIcon, duration, isActive } from "./Status";
import { useBranches, useTags } from "../Code";

export const SOURCE: Record<string, string> = {
  push: "push",
  web: "run manually",
  api: "API",
  schedule: "scheduled",
  merge_request_event: "merge request",
  trigger: "trigger",
  parent_pipeline: "parent pipeline",
  pipeline: "upstream pipeline",
  renovate: "dependency updates",
};

/** Where a pipeline lives on this server (another project's for multi-project pipelines). */
export function pipelineHref(ctx: Ctx, l: S["PipelineLink"]) {
  return l.project_id === ctx.p.id ? `${ctx.web}/-/pipelines/${l.iid}` : `/${l.project_path}/-/pipelines/${l.iid}`;
}

/** Run a job that asks for confirmation first (`manual_confirmation`). */
export function confirmRun(j: S["JobDto"]): boolean {
  return !j.manual_confirmation || window.confirm(j.manual_confirmation);
}

export function PipelineRow({ ctx, p }: { ctx: Ctx; p: S["PipelineSummary"] }) {
  const to = `${ctx.web}/-/pipelines/${p.iid}`;
  return (
    <div className="item pipeline-row">
      <div className="pipeline-row-status">
        <StatusBadge status={p.status} to={to} />
        {p.duration != null && (
          <span className="row-meta">
            <Timer size={12} /> {duration(p.duration)}
          </span>
        )}
      </div>
      <div className="grow" style={{ minWidth: 0 }}>
        <Link to={to} className="row-title">
          <span className="ellipsis">{p.commit_title || p.sha.slice(0, 8)}</span>
        </Link>
        <div className="row-meta">
          <span>#{p.iid}</span>
          <Link to={`${ctx.web}/-/tree/${p.ref}`} className="ref-chip small">
            {p.tag ? <Tag size={11} /> : <GitBranch size={11} />}
            {p.ref}
          </Link>
          <Link to={`${ctx.web}/-/commit/${p.sha}`} className="sha">
            {p.sha.slice(0, 8)}
          </Link>
          <span>{SOURCE[p.source] ?? p.source}</span>
          {p.username && <span>{p.username}</span>}
          <span>{timeAgo(p.created_at)}</span>
        </div>
        {p.yaml_errors && <div className="small" style={{ color: "var(--danger)" }}>{p.yaml_errors}</div>}
      </div>
      <MiniPipeline stages={p.stages} to={to} />
    </div>
  );
}

export function PipelineList({ ctx }: { ctx: Ctx }) {
  useCiLive(ctx.p.id);
  const [status, setStatus] = useState("");
  const [gitRef, setRef] = useState("");
  const q = useQuery({
    queryKey: ["pipelines", ctx.p.id, status, gitRef],
    queryFn: () => api.get<S["PipelineSummary"][]>(`/projects/${ctx.p.id}/pipelines${qs({ status, ref: gitRef, per_page: 50 })}`),
  });
  return (
    <div className="stack list-page">
      <ListHead title="Pipelines">
        <Link className="btn" to={`${ctx.web}/-/pipeline_schedules`}>
          <CalendarClock size={15} /> Schedules
        </Link>
        {ctx.p.can_push && (
          <Link className="btn primary" to={`${ctx.web}/-/pipelines/new`}>
            <Play size={15} /> Run pipeline
          </Link>
        )}
      </ListHead>
      <div className="list-toolbar">
        <FilterTabs
          label="Status"
          value={status}
          onChange={setStatus}
          options={[
            { value: "", label: "All" },
            { value: "running", label: "Running" },
            { value: "pending", label: "Pending" },
            { value: "success", label: "Passed" },
            { value: "failed", label: "Failed" },
            { value: "canceled", label: "Canceled" },
            { value: "manual", label: "Manual" },
          ]}
        />
        <SearchField value={gitRef} onChange={(v) => setRef(v.trim())} placeholder="Filter by branch or tag" />
      </div>
      <Q q={q}>
        {(list) =>
          list.length === 0 ? (
            <Empty>
              {status || gitRef ? (
                "No pipelines match."
              ) : (
                <>
                  No pipelines yet. Add a <code>.gitlab-ci.yml</code> to the repository and push, then register a runner in{" "}
                  <Link to={`${ctx.web}/-/settings/ci_cd`}>CI/CD settings</Link>.
                </>
              )}
            </Empty>
          ) : (
            <div className="list rows">
              {list.map((p) => (
                <PipelineRow key={p.id} ctx={ctx} p={p} />
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}

/** Rows of key/value inputs (pipeline and schedule variables). */
export function VariableRows({ rows, setRows }: { rows: { key: string; value: string }[]; setRows: (r: { key: string; value: string }[]) => void }) {
  return (
    <div className="stack" style={{ gap: 6 }}>
      {rows.map((r, i) => (
        <div className="row" key={i}>
          <input placeholder="KEY" value={r.key} aria-label="Variable name" onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, key: e.target.value } : x)))} style={{ fontFamily: "var(--mono)", maxWidth: 220 }} />
          <input className="grow" placeholder="value" value={r.value} aria-label="Variable value" onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, value: e.target.value } : x)))} />
          <button type="button" className="icon-btn" aria-label="Remove variable" onClick={() => setRows(rows.filter((_, j) => j !== i))}>
            <X size={15} />
          </button>
        </div>
      ))}
      <div>
        <button type="button" className="btn small" onClick={() => setRows([...rows, { key: "", value: "" }])}>
          <Plus size={14} /> Add variable
        </button>
      </div>
    </div>
  );
}

export function RunPipeline({ ctx }: { ctx: Ctx }) {
  const nav = useNavigate();
  const [params] = useSearchParams();
  const branches = useBranches(ctx);
  const tags = useTags(ctx);
  const [gitRef, setRef] = useState(params.get("ref") ?? ctx.p.default_branch ?? "main");
  const [rows, setRows] = useState<{ key: string; value: string }[]>([]);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const p = await api.post<S["PipelineDetail"]>(`/projects/${ctx.p.id}/pipelines`, { ref: gitRef, variables: rows.filter((r) => r.key) });
      nav(`${ctx.web}/-/pipelines/${p.iid}`);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className="stack" style={{ maxWidth: 720 }} onSubmit={submit}>
      <h1 style={{ margin: 0 }}>Run pipeline</h1>
      <ErrorBox error={error} />
      <Field label="Run for branch or tag">
        <Select
          value={gitRef}
          onChange={setRef}
          options={[
            ...(branches.data ?? []).map((b) => ({ value: b.name, label: b.name, group: "Branches" })),
            ...(tags.data ?? []).filter((t) => !(branches.data ?? []).some((b) => b.name === t.name)).map((t) => ({ value: t.name, label: t.name, group: "Tags" })),
          ]}
        />
      </Field>
      <FieldGroup label="Variables" hint="Passed to every job; they override project variables with the same name">
        <VariableRows rows={rows} setRows={setRows} />
      </FieldGroup>
      <div className="row">
        <button className="btn primary" disabled={busy}>
          <Play size={15} /> Run pipeline
        </button>
        <Link className="btn" to={`${ctx.web}/-/pipelines`}>
          Cancel
        </Link>
      </div>
    </form>
  );
}

/** The job the Jobs tab opens on: the first failure, else what runs, else the first job. */
export function pickJob(jobs: S["JobDto"][]): S["JobDto"] | undefined {
  const live = jobs.filter((j) => !j.retried);
  return (
    live.find((j) => j.status === "failed" && !j.allow_failure) ??
    live.find((j) => j.status === "running") ??
    live.find((j) => isActive(j.status)) ??
    live[0]
  );
}

/** Jobs `j` waits for: its `needs`, else every job of the closest earlier stage. */
function upstreamOf(j: S["JobDto"], jobs: S["JobDto"][]): Set<string> {
  if (j.needs) return new Set(j.needs);
  const before = jobs.filter((o) => o.stage_idx < j.stage_idx);
  const last = Math.max(-1, ...before.map((o) => o.stage_idx));
  return new Set(before.filter((o) => o.stage_idx === last).map((o) => o.name));
}

const pipelineKey = (pid: number, iid: number) => ["pipeline", pid, iid];

export function usePipeline(ctx: Ctx, iid: number | undefined) {
  useCiLive(ctx.p.id);
  return useQuery({
    queryKey: pipelineKey(ctx.p.id, iid ?? 0),
    queryFn: () => api.get<S["PipelineDetail"]>(`/projects/${ctx.p.id}/pipelines/${iid}`),
    enabled: iid != null,
  });
}

/** Title, actions, facts and the Graph / Jobs tabs, shared by both pipeline views. */
export function PipelineHeader({ ctx, p, tab }: { ctx: Ctx; p: S["PipelineDetail"]; tab: "graph" | "jobs" }) {
  const qc = useQueryClient();
  const run = useAct(() => {
    qc.invalidateQueries({ queryKey: ["pipeline", ctx.p.id] });
    qc.invalidateQueries({ queryKey: ["job", ctx.p.id] });
  });
  const act = (what: "cancel" | "retry") => run(() => api.post(`/projects/${ctx.p.id}/pipelines/${p.iid}/${what}`), what === "cancel" ? "Pipeline canceled" : "Retrying failed jobs");
  const live = p.jobs.filter((j) => !j.retried);
  const failedJobs = live.filter((j) => j.status === "failed" || j.status === "canceled").length;
  const first = pickJob(p.jobs);
  return (
    <div className="stack" style={{ gap: 14 }}>
      <div className="row wrap" style={{ gap: 12 }}>
        <StatusBadge status={p.status} />
        <h1 className="pipeline-title grow">
          {p.commit_title || `Pipeline #${p.iid}`} <span className="faint mono">#{p.iid}</span>
        </h1>
        {p.can_run && isActive(p.status) && (
          <button className="btn" onClick={() => act("cancel")}>
            <Square size={14} /> Cancel
          </button>
        )}
        {p.can_run && failedJobs > 0 && !isActive(p.status) && (
          <button className="btn" onClick={() => act("retry")}>
            <RotateCcw size={14} /> Retry failed jobs
          </button>
        )}
      </div>
      <dl className="facts-bar">
        <div>
          <dt>Trigger</dt>
          <dd>
            {SOURCE[p.source] ?? p.source}
            {p.username && <span className="muted"> by {p.username}</span>}
          </dd>
        </div>
        <div>
          <dt>Ref</dt>
          <dd className="mono">
            {p.tag ? <Tag size={12} /> : <GitBranch size={12} />} <Link to={`${ctx.web}/-/tree/${p.ref}`}>{p.ref}</Link>{" "}
            <Link to={`${ctx.web}/-/commit/${p.sha}`} className="muted">
              @{p.sha.slice(0, 8)}
            </Link>
            {p.protected && <span className="badge">protected</span>}
          </dd>
        </div>
        <div>
          <dt>Created</dt>
          <dd>{timeAgo(p.created_at)}</dd>
        </div>
        {p.upstream && (
          <div>
            <dt>Upstream</dt>
            <dd>
              <Link to={pipelineHref(ctx, p.upstream)}>
                {p.upstream.project_id === ctx.p.id ? "" : `${p.upstream.project_path} `}#{p.upstream.iid}
              </Link>
            </dd>
          </div>
        )}
        {p.coverage != null && (
          <div>
            <dt>Coverage</dt>
            <dd className="mono">{p.coverage.toFixed(2)}%</dd>
          </div>
        )}
        <div>
          <dt>Duration</dt>
          <dd className="mono">
            <Timer size={12} /> {p.duration != null ? duration(p.duration) : "—"}
          </dd>
        </div>
        <div className="grow">
          <dt>Stages</dt>
          <dd>
            <StageDots p={p} />
          </dd>
        </div>
      </dl>
      {p.yaml_errors && (
        <div className="alert error">
          <strong>This pipeline could not run.</strong> {p.yaml_errors}
        </div>
      )}
      <nav className="tabs" style={{ marginBottom: 4 }}>
        <Link to={`${ctx.web}/-/pipelines/${p.iid}`} className={tab === "graph" ? "active" : ""}>
          Graph
        </Link>
        {first ? (
          <Link to={`${ctx.web}/-/jobs/${first.id}`} className={tab === "jobs" ? "active" : ""}>
            Jobs <span className="tab-count">{live.length}</span>
          </Link>
        ) : (
          <span className="tab-disabled">Jobs</span>
        )}
      </nav>
    </div>
  );
}

/** One dot per job, grouped by stage. */
function StageDots({ p }: { p: S["PipelineDetail"] }) {
  const live = p.jobs.filter((j) => !j.retried);
  return (
    <span className="stage-dots">
      {p.stages.map((stage, i) => (
        <span key={stage} className="row" style={{ gap: 4 }}>
          {i > 0 && <span className="stage-dots-sep" />}
          <span className="small">{stage}</span>
          {live
            .filter((j) => j.stage === stage)
            .map((j) => (
              <span key={j.id} className={`dot st-${j.status === "failed" && j.allow_failure ? "warn" : j.status}`} title={`${j.name}: ${j.status}`} />
            ))}
        </span>
      ))}
    </span>
  );
}

export function JobCard({ ctx, j, canRun, rel, onHover }: { ctx: Ctx; j: S["JobDto"]; canRun: boolean; rel?: "focus" | "up" | "down" | "dim"; onHover?: (name: string | null) => void }) {
  const qc = useQueryClient();
  const run = useAct(() => qc.invalidateQueries({ queryKey: ["pipeline", ctx.p.id] }));
  const act = async (what: "retry" | "play" | "cancel") => {
    if (what === "play" && !confirmRun(j)) return;
    await run(() => api.post(`/projects/${ctx.p.id}/jobs/${j.id}/${what}`), what === "retry" ? `Retrying ${j.name}` : what === "play" ? `Started ${j.name}` : `Canceling ${j.name}`);
  };
  const action =
    !canRun || j.retried ? null : j.status === "manual" || j.status === "scheduled" ? (
      <button className="icon-btn small-icon" title={`Run ${j.name}`} aria-label={`Run ${j.name}`} onClick={() => act("play")}>
        <Play size={14} />
      </button>
    ) : isActive(j.status) ? (
      <button className="icon-btn small-icon" title={`Cancel ${j.name}`} aria-label={`Cancel ${j.name}`} onClick={() => act("cancel")}>
        <Square size={13} />
      </button>
    ) : RETRYABLE.includes(j.status) ? (
      <button className="icon-btn small-icon" title={`Retry ${j.name}`} aria-label={`Retry ${j.name}`} onClick={() => act("retry")}>
        <RotateCcw size={14} />
      </button>
    ) : null;
  return (
    <div
      className={`job-card ${j.retried ? "retried" : ""} ${rel ?? ""}`}
      onMouseEnter={() => onHover?.(j.name)}
      onMouseLeave={() => onHover?.(null)}
      onFocus={() => onHover?.(j.name)}
      onBlur={() => onHover?.(null)}
    >
      <StatusIcon status={j.status} allowFailure={j.allow_failure} />
      {/* the name gets the whole first line; what else there is goes below */}
      <div className="job-main">
        <Link to={`${ctx.web}/-/jobs/${j.id}`} className="job-name" title={j.name}>
          {j.name}
        </Link>
        {(j.needs || j.downstream || (j.environment && j.environment.action === "start") || (j.duration != null && j.status !== "running")) && (
          <div className="job-meta">
            {j.needs && <span className="needs-mark" title={`needs: ${j.needs.join(", ") || "nothing"}`}>needs {j.needs.length}</span>}
            {j.downstream && (
              <Link className="needs-mark" to={pipelineHref(ctx, j.downstream)} title={`Downstream pipeline in ${j.downstream.project_path}`}>
                #{j.downstream.iid}
              </Link>
            )}
            {j.environment && j.environment.action === "start" && (
              <span className="needs-mark env" title={`Deploys to ${j.environment.name}`}>
                {j.environment.name}
              </span>
            )}
            {j.duration != null && j.status !== "running" && <span className="faint mono">{duration(j.duration)}</span>}
          </div>
        )}
      </div>
      {action}
    </div>
  );
}

function stageSummary(jobs: S["JobDto"][]) {
  const n = (f: (j: S["JobDto"]) => boolean) => jobs.filter(f).length;
  const parts: string[] = [];
  const ok = n((j) => j.status === "success");
  const failed = n((j) => j.status === "failed" && !j.allow_failure);
  const allowed = n((j) => j.status === "failed" && j.allow_failure);
  const manual = n((j) => j.status === "manual");
  const running = n((j) => j.status === "running" || j.status === "canceling");
  const waiting = n((j) => isActive(j.status) && j.status !== "running" && j.status !== "canceling");
  if (ok) parts.push(`${ok} passed`);
  if (failed) parts.push(`${failed} failed`);
  if (allowed) parts.push(`${allowed} allowed to fail`);
  if (running) parts.push(`${running} running`);
  if (waiting) parts.push(`${waiting} waiting`);
  if (manual) parts.push(`${manual} manual`);
  return parts.join(" · ");
}

export function PipelinePage({ ctx, iid }: { ctx: Ctx; iid: number }) {
  const [showRetried, setShowRetried] = useState(false);
  const [hover, setHover] = useState<string | null>(null);
  const q = usePipeline(ctx, iid);
  return (
    <Q q={q}>
      {(p) => {
        const live = p.jobs.filter((j) => !j.retried);
        const retried = p.jobs.filter((j) => j.retried);
        // dependencies of the hovered job, both ways
        const focus = hover ? live.find((j) => j.name === hover) : undefined;
        const up = focus ? upstreamOf(focus, live) : new Set<string>();
        const rel = (j: S["JobDto"]): "focus" | "up" | "down" | "dim" | undefined => {
          if (!focus) return undefined;
          if (j.name === focus.name) return "focus";
          if (up.has(j.name)) return "up";
          if (upstreamOf(j, live).has(focus.name)) return "down";
          return "dim";
        };
        return (
          <div className="stack" style={{ gap: 18 }}>
            <PipelineHeader ctx={ctx} p={p} tab="graph" />
            <PipelineTests ctx={ctx} iid={iid} />
            {live.length > 0 && (
              <div className="pipeline-graph" role="list" aria-label="Stages">
                {p.stages.map((stage, i) => {
                  const jobs = live.filter((j) => j.stage === stage);
                  return (
                    <div className="stage-wrap" key={stage} role="listitem">
                      {i > 0 && <span className="stage-link" aria-hidden="true" />}
                      <div className="stage-col">
                        <div className="stage-name">
                          {stage} <span className="stage-sum">{stageSummary(jobs)}</span>
                        </div>
                        {jobs.map((j) => (
                          <JobCard key={j.id} ctx={ctx} j={j} canRun={p.can_run} rel={rel(j)} onHover={setHover} />
                        ))}
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
            {live.length > 0 && (
              <div className="graph-legend small muted">
                <span>
                  Hover a job to see what it waits for <span className="legend up" /> and what waits for it <span className="legend down" />
                </span>
              </div>
            )}
            {retried.length > 0 && (
              <div>
                <button className="btn small ghost" onClick={() => setShowRetried(!showRetried)}>
                  {showRetried ? "Hide" : "Show"} {retried.length} retried job{retried.length === 1 ? "" : "s"}
                </button>
                {showRetried && (
                  <div className="list" style={{ marginTop: 8 }}>
                    {retried.map((j) => (
                      <div className="item" key={j.id}>
                        <JobCard ctx={ctx} j={j} canRun={false} />
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>
        );
      }}
    </Q>
  );
}
