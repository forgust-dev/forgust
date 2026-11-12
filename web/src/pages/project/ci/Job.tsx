import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useWindowVirtualizer } from "@tanstack/react-virtual";
import { ArrowDownToLine, ChevronDown, ChevronRight, Download, FileText, Play, RotateCcw, Square, FolderOpen } from "lucide-react";
import { api, bytes, timeAgo, type S } from "../../../api/client";
import { Q } from "../../../components/ui";
import { useProjectEvents } from "../../../shell/live";
import { useAct } from "../../../shell/toast";
import { usePref } from "../../../shell/theme";
import type { Ctx } from "../Project";
import { LogParser, sectionDuration, type LogLine, type Span } from "./log";
import { RETRYABLE, StatusBadge, StatusIcon, duration, isActive } from "./Status";
import { PipelineHeader, confirmRun, pipelineHref, usePipeline } from "./Pipelines";

/** The job log, fetched incrementally while the job runs. */
function useTrace(pid: number, jobId: number, status: string | undefined) {
  const [text, setText] = useState("");
  const [complete, setComplete] = useState(false);
  const offset = useRef(0);
  const busy = useRef(false);
  const want = useRef(false);

  const pull = async () => {
    if (busy.current) {
      want.current = true;
      return;
    }
    busy.current = true;
    try {
      for (;;) {
        want.current = false;
        const c = await api.get<S["TraceChunk"]>(`/projects/${pid}/jobs/${jobId}/trace?offset=${offset.current}`);
        if (c.offset !== offset.current) break;
        if (c.content) {
          // sizes are in bytes; keep our own byte offset from the server's answer
          offset.current = c.size;
          setText((t) => t + c.content);
        }
        setComplete(c.complete);
        if (!want.current && !c.more) break;
      }
    } catch {
      /* the next event or poll retries */
    } finally {
      busy.current = false;
    }
  };

  useEffect(() => {
    offset.current = 0;
    setText("");
    setComplete(false);
    pull();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pid, jobId]);
  useProjectEvents(pid, (ev) => {
    if (ev.job_id !== jobId) return;
    // new bytes, or the job ended (its last bytes may have come with that)
    if ((ev.type === "job_trace" && (ev.trace_size ?? 0) > offset.current) || (ev.type === "job" && ev.status && !isActive(ev.status))) pull();
  });
  // fallback poll while running (events are hints)
  useEffect(() => {
    if (complete || (status && !isActive(status))) return;
    const t = window.setInterval(pull, 5000);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [complete, status, jobId]);
  return { text, complete };
}

function SpanView({ s }: { s: Span }) {
  const st = s.style;
  if (!st.fg && !st.bg && !st.bold && !st.italic && !st.underline && !st.faint) return <>{s.text}</>;
  return (
    <span
      style={{
        color: st.fg,
        background: st.bg,
        fontWeight: st.bold ? 700 : undefined,
        fontStyle: st.italic ? "italic" : undefined,
        textDecoration: st.underline ? "underline" : undefined,
        opacity: st.faint ? 0.7 : undefined,
      }}
    >
      {s.text}
    </span>
  );
}

export function LogView({ text, live }: { text: string; live: boolean }) {
  // the text only grows: parse just what is new
  const parser = useRef<{ p: LogParser; fed: number }>({ p: new LogParser(), fed: 0 });
  const parsed = useMemo(() => {
    if (text.length < parser.current.fed) parser.current = { p: new LogParser(), fed: 0 };
    parser.current.p.feed(text.slice(parser.current.fed));
    parser.current.fed = text.length;
    return parser.current.p.snapshot();
  }, [text]);
  const [open, setOpen] = useState<Record<string, boolean>>({});
  const [showTime, setShowTime] = usePref("log.timestamps", false);
  const [follow, setFollow] = useState(true);
  const ref = useRef<HTMLDivElement>(null);

  const isOpen = (name: string) => {
    const h = parsed.sections.get(name);
    return open[name] ?? !h?.header?.collapsed;
  };
  // hidden when any enclosing section is collapsed
  const visible = useMemo(() => {
    const hidden = (l: LogLine) => {
      let s = l.section;
      while (s) {
        if (!isOpen(s)) return true;
        s = parsed.sections.get(s)?.section;
      }
      return false;
    };
    return parsed.lines.filter((l) => !hidden(l));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [parsed, open]);

  const virtual = visible.length > 1500;
  const v = useWindowVirtualizer({ count: visible.length, estimateSize: () => 20, overscan: 50, scrollMargin: ref.current?.offsetTop ?? 0 });

  // follow the end while the job runs, until the reader scrolls up
  useEffect(() => {
    if (!live || !follow) return;
    if (virtual) v.scrollToIndex(visible.length - 1, { align: "end" });
    else window.scrollTo({ top: document.body.scrollHeight });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible.length, live, follow]);
  useEffect(() => {
    if (!live) return;
    const onScroll = () => {
      const atEnd = window.innerHeight + window.scrollY >= document.body.scrollHeight - 80;
      setFollow(atEnd);
    };
    window.addEventListener("wheel", onScroll, { passive: true });
    window.addEventListener("keydown", onScroll);
    return () => {
      window.removeEventListener("wheel", onScroll);
      window.removeEventListener("keydown", onScroll);
    };
  }, [live]);

  const row = (l: LogLine, style?: React.CSSProperties) => {
    const h = l.header;
    const dur = sectionDuration(h);
    return (
      <div key={l.n} className={`log-line ${l.stderr ? "stderr" : ""} ${h ? "log-header" : ""}`} style={style} data-n={l.n}>
        <span className="log-n">{l.n}</span>
        {showTime && <span className="log-time">{l.time ? l.time.slice(11, 19) : ""}</span>}
        {h ? (
          <button className="log-toggle" onClick={() => setOpen({ ...open, [h.name]: !isOpen(h.name) })} aria-expanded={isOpen(h.name)}>
            {isOpen(h.name) ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
          </button>
        ) : (
          <span className="log-indent" />
        )}
        <span className="log-text">
          {l.spans.map((s, i) => (
            <SpanView key={i} s={s} />
          ))}
          {!l.spans.length && " "}
        </span>
        {dur != null && <span className="log-dur">{duration(dur)}</span>}
      </div>
    );
  };

  return (
    <div className="log">
      <div className="log-bar">
        <label className="check small">
          <input type="checkbox" checked={showTime} onChange={(e) => setShowTime(e.target.checked)} /> Timestamps
        </label>
        <span className="grow" />
        {live && !follow && (
          <button className="btn small" onClick={() => setFollow(true)}>
            <ArrowDownToLine size={14} /> Follow
          </button>
        )}
      </div>
      <div className="log-body" ref={ref}>
        {visible.length === 0 ? (
          <div className="log-line faint">
            <span className="log-n" />
            {live ? "Waiting for the runner…" : "This job has no log."}
          </div>
        ) : virtual ? (
          <div style={{ height: v.getTotalSize(), position: "relative" }}>
            {v.getVirtualItems().map((vi) => row(visible[vi.index], { position: "absolute", top: 0, left: 0, right: 0, transform: `translateY(${vi.start - v.options.scrollMargin}px)` }))}
          </div>
        ) : (
          visible.map((l) => row(l))
        )}
      </div>
    </div>
  );
}

/** The jobs of a pipeline, by stage, beside the open job's log. */
function JobList({ ctx, p, current }: { ctx: Ctx; p: S["PipelineDetail"]; current: number }) {
  const live = p.jobs.filter((j) => !j.retried);
  return (
    <nav className="job-list" aria-label="Jobs">
      {p.stages.map((stage) => {
        const jobs = live.filter((j) => j.stage === stage);
        if (!jobs.length) return null;
        const secs = jobs.reduce((t, j) => t + (j.duration ?? 0), 0);
        return (
          <div key={stage} className="job-list-stage">
            <div className="job-list-head">
              <span>{stage}</span>
              {secs > 0 && <span className="mono">{duration(secs)}</span>}
            </div>
            {jobs.map((j) => (
              <Link key={j.id} to={`${ctx.web}/-/jobs/${j.id}`} className={`job-list-item ${j.id === current ? "active" : ""}`} aria-current={j.id === current ? "page" : undefined}>
                <StatusIcon status={j.status} allowFailure={j.allow_failure} size={14} />
                <span className="grow ellipsis">{j.name}</span>
                <span className="mono faint small">{j.status === "manual" ? "manual" : j.duration != null ? duration(j.duration) : ""}</span>
              </Link>
            ))}
          </div>
        );
      })}
    </nav>
  );
}

/** "in 12 minutes", for a moment ahead. */
function timeUntil(ts: string): string {
  const s = Math.max(0, (Date.parse(ts) - Date.now()) / 1000);
  if (s < 60) return "any moment now";
  const [n, unit] = s >= 86400 ? [Math.round(s / 86400), "day"] : s >= 3600 ? [Math.round(s / 3600), "hour"] : [Math.round(s / 60), "minute"];
  return `in ${n} ${unit}${n === 1 ? "" : "s"}`;
}

export function JobPage({ ctx, jobId }: { ctx: Ctx; jobId: number }) {
  const qc = useQueryClient();
  const nav = useNavigate();
  const q = useQuery({ queryKey: ["job", ctx.p.id, jobId], queryFn: () => api.get<S["JobDetail"]>(`/projects/${ctx.p.id}/jobs/${jobId}`) });
  // status changes: through usePipeline's live updates (the "job" queries too)
  const pipeline = usePipeline(ctx, q.data?.pipeline_iid);
  const trace = useTrace(ctx.p.id, jobId, q.data?.status);
  const run = useAct(() => {
    qc.invalidateQueries({ queryKey: ["job", ctx.p.id] });
    qc.invalidateQueries({ queryKey: ["pipeline", ctx.p.id] });
  });
  const act = async (what: "retry" | "play" | "cancel" | "unschedule") => {
    if (what === "play" && q.data && !confirmRun(q.data)) return;
    let retried: number | null = null;
    const done = what === "retry" ? "Retrying" : what === "play" ? "Started" : what === "unschedule" ? "Unscheduled: it waits for someone to run it" : "Canceling";
    if (await run(async () => (retried = (await api.post<S["JobDto"]>(`/projects/${ctx.p.id}/jobs/${jobId}/${what}`)).id), done) && what === "retry" && retried != null) {
      nav(`${ctx.web}/-/jobs/${retried}`);
    }
  };
  return (
    <Q q={q}>
      {(j) => {
        const live = isActive(j.status);
        return (
          <div className="stack" style={{ gap: 18 }}>
            {pipeline.data && <PipelineHeader ctx={ctx} p={pipeline.data} tab="jobs" />}
            <div className="jobs-split">
              {pipeline.data && <JobList ctx={ctx} p={pipeline.data} current={j.id} />}
              <section className="job-pane" aria-label={`Job ${j.name}`}>
                <div className="job-pane-head">
                  <StatusIcon status={j.status} allowFailure={j.allow_failure} size={18} />
                  <h2 className="grow">{j.name}</h2>
                  {j.can_run && (j.status === "manual" || j.status === "scheduled") && (
                    <button className="btn primary small" onClick={() => act("play")}>
                      <Play size={14} /> {j.status === "scheduled" ? "Run now" : "Run"}
                    </button>
                  )}
                  {j.can_run && j.status === "scheduled" && (
                    <button className="btn small" onClick={() => act("unschedule")}>
                      Unschedule
                    </button>
                  )}
                  {j.can_run && live && (
                    <button className="btn small" onClick={() => act("cancel")}>
                      <Square size={13} /> Cancel
                    </button>
                  )}
                  {j.can_run && !j.retried && RETRYABLE.includes(j.status) && (
                    <button className="btn small" onClick={() => act("retry")}>
                      <RotateCcw size={13} /> Retry
                    </button>
                  )}
                  {j.artifacts.some((a) => a.file_type === "archive") && (
                    <>
                      <Link className="btn small" to={`${ctx.web}/-/jobs/${j.id}/artifacts`}>
                        <FolderOpen size={13} /> Browse
                      </Link>
                      <a className="btn small" href={`/api/v1/projects/${ctx.p.id}/jobs/${j.id}/artifacts`}>
                        <Download size={13} /> Artifacts
                      </a>
                    </>
                  )}
                  <a className="btn small" href={`/api/v1/projects/${ctx.p.id}/jobs/${j.id}/raw`} target="_blank" rel="noreferrer">
                    <FileText size={13} /> Raw
                  </a>
                </div>
                <div className="job-pane-meta small muted">
                  <StatusBadge status={j.status} />
                  <span>stage {j.stage}</span>
                  {j.runner_description && <span>runner {j.runner_description}</span>}
                  {j.image && <span className="mono">{j.image}</span>}
                  {j.duration != null && <span>ran {duration(j.duration)}</span>}
                  {j.queued_duration != null && <span>queued {duration(j.queued_duration)}</span>}
                  {j.finished_at && <span>finished {timeAgo(j.finished_at)}</span>}
                  {j.attempt > 1 && <span>attempt {j.attempt}</span>}
                  {j.coverage != null && <span>coverage {j.coverage.toFixed(2)}%</span>}
                  {j.environment && (
                    <span>
                      {j.environment.action === "stop" ? "stops" : "deploys to"}{" "}
                      <Link to={`${ctx.web}/-/environments`}>{j.environment.name}</Link>
                    </span>
                  )}
                  {j.retried && j.attempts[0] !== j.id && <Link to={`${ctx.web}/-/jobs/${j.attempts[0]}`}>a newer attempt exists</Link>}
                </div>
                {j.failure_reason && j.status === "failed" && (
                  <div className={`alert ${j.allow_failure ? "" : "error"}`}>
                    {j.allow_failure ? "Failed (allowed to fail)" : "Failed"}: {j.failure_reason.replace(/_/g, " ")}
                    {j.exit_code != null && ` (exit code ${j.exit_code})`}
                  </div>
                )}
                {j.status === "scheduled" && j.scheduled_at && (
                  <div className="alert">This is a delayed job: it starts {new Date(j.scheduled_at).toLocaleString()} ({timeUntil(j.scheduled_at)}).</div>
                )}
                {j.status === "waiting_for_resource" && <div className="alert">Waiting for another job of its resource group to finish.</div>}
                {j.bridge && j.downstream && (
                  <div className="alert">
                    This job started <Link to={pipelineHref(ctx, j.downstream)}>pipeline #{j.downstream.iid}</Link>
                    {j.downstream.project_id === ctx.p.id ? "" : ` in ${j.downstream.project_path}`} ({j.downstream.status}).
                  </div>
                )}
                {j.status === "pending" && !j.bridge && (
                  <div className="alert">
                    Waiting for a runner. Make sure a runner with tags {j.tags.length ? j.tags.map((t) => `"${t}"`).join(", ") : "(none)"} is online in{" "}
                    <Link to={`${ctx.web}/-/settings/ci_cd`}>CI/CD settings</Link>.
                  </div>
                )}
                {j.artifacts.length > 0 && (
                  <details className="artifacts small">
                    <summary>
                      {j.artifacts.length} artifact{j.artifacts.length === 1 ? "" : "s"}
                    </summary>
                    <div className="stack" style={{ gap: 4, marginTop: 6 }}>
                      {j.artifacts.map((a) => (
                        <span key={a.id}>
                          {a.file_type} · {a.filename} · {bytes(a.size)}
                          {a.expire_at && <span className="faint"> · kept until {new Date(a.expire_at).toLocaleDateString()}</span>}
                        </span>
                      ))}
                    </div>
                  </details>
                )}
                <LogView key={jobId} text={trace.text} live={live && !trace.complete} />
              </section>
            </div>
          </div>
        );
      }}
    </Q>
  );
}
