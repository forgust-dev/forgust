import { useLayoutEffect, useState, type FormEvent } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarClock, Clock3, CornerDownRight, GitBranch, Play, RotateCcw, Server, ServerOff, Square, Tag, Timer, Plus, X } from "lucide-react";
import { api, qs, refLink, timeAgo, type S } from "../../../api/client";
import { Empty, ErrorBox, Field, FieldGroup, FilterTabs, ListHead, Q, SearchField } from "../../../components/ui";
import { Select } from "../../../components/controls";
import { useCiLive } from "../../../shell/live";
import { useAct } from "../../../shell/toast";
import { useConfirm } from "../../../shell/dialog";
import type { Ctx } from "../Project";
import { PipelineTests } from "./Tests";
import { MiniPipeline, RETRYABLE, StatusBadge, StatusIcon, duration, isActive, statusLabel } from "./Status";
import { DecideCard } from "../../../components/decide";
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

/** Before running a job: those that ask for a confirmation (`manual_confirmation`)
 *  ask it in the app's dialog. True to go on. */
export function useConfirmRun() {
  const confirm = useConfirm();
  return (j: S["JobDto"]) => (j.manual_confirmation ? confirm({ title: `Run ${j.name}?`, body: j.manual_confirmation, action: `Run ${j.name}` }) : Promise.resolve(true));
}

/** A pipeline in the list; the child pipelines it started (`trigger: include`) go
 *  under it, rather than in rows of their own with the same title. */
export function PipelineRow({ ctx, p, kids = [] }: { ctx: Ctx; p: S["PipelineSummary"]; kids?: S["PipelineSummary"][] }) {
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
          <Link to={refLink(ctx.web, "tree", p.ref)} className="ref-chip small">
            {p.tag ? <Tag size={11} /> : <GitBranch size={11} />}
            {p.ref}
          </Link>
          <Link to={`${ctx.web}/-/commit/${p.sha}`} className="sha">
            {p.sha.slice(0, 8)}
          </Link>
          {/* a child whose parent is not listed here (another page, a filter) says whose it is */}
          {p.parent_iid != null ? <Link to={`${ctx.web}/-/pipelines/${p.parent_iid}`}>child of #{p.parent_iid}</Link> : <span>{SOURCE[p.source] ?? p.source}</span>}
          {p.username && <span>by {p.username}</span>}
          <span>{timeAgo(p.created_at)}</span>
        </div>
        {p.yaml_errors && <div className="small" style={{ color: "var(--danger)" }}>{p.yaml_errors}</div>}
        {kids.length > 0 && (
          <ul className="pipe-kids" aria-label="Child pipelines">
            {kids.map((k) => (
              <li key={k.id}>
                <CornerDownRight size={13} aria-hidden className="faint" />
                <span className={`st-${k.status}`}>
                  <StatusIcon status={k.status} size={14} />
                </span>
                <Link to={`${ctx.web}/-/pipelines/${k.iid}`}>Child pipeline #{k.iid}</Link>
                <span className="faint">
                  {statusLabel(k.status).toLowerCase()}
                  {k.duration != null && ` · ${duration(k.duration)}`}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
      <MiniPipeline stages={p.stages} to={to} />
    </div>
  );
}

/** Pipeline states that wait for runners to take their jobs. */
const WAITING = ["created", "pending", "running", "preparing", "waiting_for_resource"];

/**
 * The list's emphasized card, for maintainers: pipelines wait and no runner the
 * project may use is online, so nothing will run (a "running" one there only
 * waits). None when a runner is online, or nothing waits.
 */
function NoRunner({ ctx, list }: { ctx: Ctx; list: S["PipelineSummary"][] }) {
  // the CI/CD settings' request: the project's runners, its groups' and the instance's
  const runners = useQuery({
    queryKey: ["runners", `/projects/${ctx.p.id}/runners`],
    queryFn: () => api.get<S["RunnerDto"][]>(`/projects/${ctx.p.id}/runners`),
    enabled: ctx.p.can_admin,
    retry: false,
  });
  const waiting = list.filter((p) => WAITING.includes(p.status)).length;
  const rs = runners.data;
  if (!rs || waiting === 0 || rs.some((r) => r.status === "online")) return null;
  return (
    <DecideCard
      label="No runner online"
      late
      eyebrow={
        <>
          <ServerOff size={14} aria-hidden /> Nothing runs
        </>
      }
      title={`${waiting} pipeline${waiting === 1 ? " waits" : "s wait"} for a runner`}
      sub={
        rs.length === 0
          ? "This project has no runner: neither its own, nor its groups', nor the instance's. Jobs stay pending until one takes them."
          : `${rs.length === 1 ? "Its runner is" : `Its ${rs.length} runners are`} offline or paused: jobs stay pending until one is back.`
      }
      actions={
        <Link className="btn run" to={`${ctx.web}/-/settings/ci_cd`}>
          <Server size={14} /> Set up a runner
        </Link>
      }
      rows={rs.slice(0, 4).map((r) => ({
        key: String(r.id),
        tone: "warn" as const,
        icon: <Server size={15} className="row-ic" />,
        text: r.description || `Runner #${r.id}`,
        aside: r.status === "paused" ? "paused" : r.contacted_at ? `seen ${timeAgo(r.contacted_at)}` : "never connected",
      }))}
    />
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
            (() => {
              // child pipelines go under their parent when it is listed too
              const listed = new Set(list.map((p) => p.iid));
              const kids = new Map<number, S["PipelineSummary"][]>();
              for (const p of list) if (p.parent_iid != null && listed.has(p.parent_iid)) kids.set(p.parent_iid, [...(kids.get(p.parent_iid) ?? []), p]);
              const rows = list.filter((p) => !(p.parent_iid != null && listed.has(p.parent_iid)));
              return (
                <>
                  {/* counts every pipeline, children too: a parent that triggered
                      one (without strategy: depend) is done while its child waits */}
                  <NoRunner ctx={ctx} list={list} />
                  <div className="list rows">
                    {rows.map((p) => (
                      <PipelineRow key={p.id} ctx={ctx} p={p} kids={kids.get(p.iid)} />
                    ))}
                  </div>
                </>
              );
            })()
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

/** The room between two stages, where the lines turn (styles.css: .stage-link). */
const STAGE_GAP = 48;

type Edge = { from: string; to: string; d: string };

/** The lines from each job to the jobs that wait for it, drawn behind the cards:
 *  quiet, until a hovered job lights its own and dims the rest. */
function GraphEdges({ graph, jobs, focus }: { graph: HTMLDivElement | null; jobs: S["JobDto"][]; focus?: string }) {
  const [geo, setGeo] = useState<{ w: number; h: number; edges: Edge[] }>();
  // the lines move only when the jobs or what they wait for do, not with a status
  const shape = jobs.map((j) => `${j.name}:${j.stage_idx}:${j.needs?.join(",") ?? "-"}`).join("|");
  // the graph comes as state, not a ref: a child's effects run before its parent's ref is set
  useLayoutEffect(() => {
    const el = graph;
    if (!el) return;
    const measure = () => {
      // stacked on a phone: the cards follow each other, a line would only cross them
      if (getComputedStyle(el).flexDirection === "column") return setGeo(undefined);
      const box = el.getBoundingClientRect();
      const ox = el.scrollLeft - box.left - el.clientLeft;
      const oy = el.scrollTop - box.top - el.clientTop;
      const at = new Map<string, DOMRect>();
      el.querySelectorAll<HTMLElement>("[data-job]").forEach((c) => at.set(c.dataset.job!, c.getBoundingClientRect()));
      const edges: Edge[] = [];
      for (const j of jobs) {
        const b = at.get(j.name);
        if (!b) continue;
        for (const u of upstreamOf(j, jobs)) {
          const a = at.get(u);
          if (!a) continue;
          const y1 = a.top + a.height / 2 + oy;
          const y2 = b.top + b.height / 2 + oy;
          let d: string;
          if (b.left > a.right) {
            // out of the earlier card, turn in the gap after its stage, then straight on
            const x1 = a.right + ox;
            const x2 = b.left + ox;
            const g = Math.min(STAGE_GAP, x2 - x1);
            d = `M${x1} ${y1}C${x1 + g * 0.6} ${y1} ${x1 + g * 0.4} ${y2} ${x1 + g} ${y2}H${x2}`;
          } else {
            // a need inside one stage: a loop along the column's left side
            const x = Math.min(a.left, b.left) + ox;
            d = `M${a.left + ox} ${y1}C${x - 18} ${y1} ${x - 18} ${y2} ${b.left + ox} ${y2}`;
          }
          edges.push({ from: u, to: j.name, d });
        }
      }
      setGeo({ w: el.scrollWidth, h: el.scrollHeight, edges });
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    el.querySelectorAll(".stage-col").forEach((c) => ro.observe(c));
    return () => ro.disconnect();
  }, [graph, shape]);
  if (!geo) return null;
  const rel = (e: Edge) => (!focus ? "" : e.to === focus ? "up" : e.from === focus ? "down" : "dim");
  // the lit lines last, over the others
  const edges = focus ? [...geo.edges].sort((a, b) => Number(rel(a) !== "dim") - Number(rel(b) !== "dim")) : geo.edges;
  return (
    <svg className="graph-edges" width={geo.w} height={geo.h} aria-hidden="true">
      {edges.map((e) => (
        <path key={`${e.from}>${e.to}`} d={e.d} className={rel(e)} />
      ))}
    </svg>
  );
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
      <div className="row wrap pipeline-head" style={{ gap: 12 }}>
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
      <div className="pipe-summary">
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
              {p.tag ? <Tag size={12} /> : <GitBranch size={12} />} <Link to={refLink(ctx.web, "tree", p.ref)}>{p.ref}</Link>{" "}
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
        <ManualCard ctx={ctx} p={p} />
      </div>
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

/** A manual job the pipeline waits on: the next step is someone's to take, so it
 *  gets the page's emphasized card (the first such job; the graph shows the rest). */
function ManualCard({ ctx, p }: { ctx: Ctx; p: S["PipelineDetail"] }) {
  const qc = useQueryClient();
  const confirmRun = useConfirmRun();
  const run = useAct(() => qc.invalidateQueries({ queryKey: ["pipeline", ctx.p.id] }));
  const waiting = p.jobs.filter((j) => !j.retried && j.status === "manual");
  if (!p.can_run || waiting.length === 0) return null;
  const j = waiting[0];
  const env = j.environment && j.environment.action === "start" ? j.environment.name : null;
  const play = async () => {
    if (!(await confirmRun(j))) return;
    await run(() => api.post(`/projects/${ctx.p.id}/jobs/${j.id}/play`), `Started ${j.name}`);
  };
  // as tall as the facts beside it: what waits and its button on one line, the why in its tip
  return (
    <section className="manual-card" aria-label="Waiting for you">
      <div className="manual-text">
        <span className="eyebrow">
          <Clock3 size={14} aria-hidden /> Waiting for you
          {waiting.length > 1 && ` · ${waiting.length} manual jobs`}
        </span>
        <h2>{env ? `Deploy to ${env}` : `Run ${j.name}`}</h2>
      </div>
      <button
        className="btn run"
        onClick={play}
        aria-label={`Start ${j.name}`}
        title={
          waiting.length === 1
            ? `${j.name} is a manual job: it runs when someone starts it.`
            : `${j.name} and ${waiting.length - 1} more manual job${waiting.length === 2 ? "" : "s"} run when someone starts them.`
        }
      >
        <Play size={14} /> Start
      </button>
    </section>
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
  const confirmRun = useConfirmRun();
  const run = useAct(() => qc.invalidateQueries({ queryKey: ["pipeline", ctx.p.id] }));
  const act = async (what: "retry" | "play" | "cancel") => {
    if (what === "play" && !(await confirmRun(j))) return;
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
      className={`job-card ${j.retried ? "retried" : ""} ${j.status === "manual" ? "manual" : ""} ${rel ?? ""}`}
      data-job={j.retried ? undefined : j.name}
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
  const [graph, setGraph] = useState<HTMLDivElement | null>(null);
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
              <div className="pipeline-graph" role="list" aria-label="Stages" ref={setGraph}>
                <GraphEdges graph={graph} jobs={live} focus={focus?.name} />
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
