import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { keepPreviousData, queryOptions, useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { useWindowVirtualizer } from "@tanstack/react-virtual";
import { ArrowDownToLine, ChevronDown, ChevronRight, Download, FileText, Play, RotateCcw, Square, FolderOpen } from "lucide-react";
import { api, bytes, timeAgo, timeUntil, type S } from "../../../api/client";
import { Q, SPINNER_DELAY } from "../../../components/ui";
import { useProjectEvents } from "../../../shell/live";
import { useAct } from "../../../shell/toast";
import { usePref } from "../../../shell/theme";
import type { Ctx } from "../Project";
import { LogParser, sectionDuration, type LogLine, type Span } from "./log";
import { RETRYABLE, StatusBadge, StatusIcon, duration, isActive } from "./Status";
import { PipelineHeader, pipelineHref, pipelineQuery, useConfirmRun, usePipeline } from "./Pipelines";

const jobQuery = (pid: number, jobId: number) => queryOptions({ queryKey: ["job", pid, jobId], queryFn: () => api.get<S["JobDetail"]>(`/projects/${pid}/jobs/${jobId}`) });

/** The start of a job's log, read before its page shows; kept a minute for coming back to it. */
const logStartQuery = (pid: number, jobId: number) =>
  queryOptions({ queryKey: ["job-log", pid, jobId], queryFn: () => api.get<S["TraceChunk"]>(`/projects/${pid}/jobs/${jobId}/trace?offset=0`), staleTime: Infinity, gcTime: 60_000 });

/**
 * The page being left (a pipeline, another job) stays on screen until this job, the start of its
 * log and its pipeline are here, and the job's page comes whole, at once: it used to go blank for
 * a frame, then say "This job has no log." until its log came. A navigation is a transition, which
 * waits for what suspends. It waits as long as a loader waits before it shows, no longer: past
 * that, the page comes with its own loader.
 */
function useJobReady(pid: number, jobId: number) {
  const qc = useQueryClient();
  // once per visit (each navigation has its own key): what it waits for has its own cache
  const visit = useLocation().key;
  useSuspenseQuery({
    queryKey: ["job-ready", pid, jobId, visit],
    queryFn: async () => {
      const ready = (async () => {
        // the job and its pipeline as they are now (a job that ran on since it was last seen),
        // and its log's start: kept when it is the whole log, read again when it may have grown
        const kept = qc.getQueryData(logStartQuery(pid, jobId).queryKey);
        const start = kept?.complete && !kept.more ? kept : qc.fetchQuery({ ...logStartQuery(pid, jobId), staleTime: 0 }).catch(() => null);
        const [job] = await Promise.all([qc.fetchQuery(jobQuery(pid, jobId)), start]);
        await qc.fetchQuery(pipelineQuery(pid, job.pipeline_iid));
      })().catch(() => {
        // the page says what went wrong, as it did without waiting
      });
      await Promise.race([ready, new Promise((done) => window.setTimeout(done, SPINNER_DELAY))]);
      return true;
    },
    staleTime: Infinity,
    gcTime: 0,
  });
}

/**
 * The job log: its start, which its page waited for (logStartQuery), then what follows, fetched
 * while the job runs. Another job keeps this one's log on screen until the other's start is here,
 * so the pane never goes blank in between; and a log says it is empty only once it knows.
 */
function useTrace(pid: number, jobId: number, status: string | undefined) {
  // the same query its page waited for: no second request when the start comes after the page
  const startQ = useQuery(logStartQuery(pid, jobId));
  const start = startQ.data;
  const begin = (s: S["TraceChunk"]) => ({ job: jobId, text: s.content, complete: s.complete, loaded: true });
  const [log, setLog] = useState(() => (start ? begin(start) : { job: jobId, text: "", complete: false, loaded: false }));
  // another job whose start is here: shown from this very render, not the frame after
  const shown = log.job !== jobId && start ? begin(start) : log;
  const job = useRef(jobId);
  // the job whose log is read on from its start (until its start is here, nothing to read on from)
  const reading = useRef<number | null>(null);
  const offset = useRef(0);
  const busy = useRef(false);
  const want = useRef(false);

  const pull = async () => {
    if (reading.current !== job.current) return;
    if (busy.current) {
      want.current = true;
      return;
    }
    busy.current = true;
    try {
      for (;;) {
        want.current = false;
        const id = job.current;
        // another job was opened, its start not here yet: reading goes on from it once it is
        if (reading.current !== id) break;
        const c = await api.get<S["TraceChunk"]>(`/projects/${pid}/jobs/${id}/trace?offset=${offset.current}`);
        // another job was opened meanwhile: start over, for it
        if (job.current !== id) continue;
        if (c.offset !== offset.current) break;
        // sizes are in bytes; keep our own byte offset from the server's answer
        if (c.content) offset.current = c.size;
        setLog((l) => (l.job === id ? { job: id, text: l.text + (c.content ?? ""), complete: c.complete, loaded: true } : { job: id, text: c.content ?? "", complete: c.complete, loaded: true }));
        if (!want.current && !c.more) break;
      }
    } catch {
      // the next event or poll retries; meanwhile no other job's log stands in for this one
      setLog((l) => (l.job === job.current ? { ...l, loaded: true } : { job: job.current, text: "", complete: false, loaded: true }));
    } finally {
      busy.current = false;
    }
  };

  useEffect(() => {
    job.current = jobId;
    if (reading.current === jobId) return;
    if (start) {
      // its start is here: what follows it
      reading.current = jobId;
      offset.current = start.content ? start.size : 0;
      setLog((l) => (l.job === jobId && l.loaded ? l : begin(start)));
      if (start.more || !start.complete) pull();
    } else if (startQ.isError) {
      // no start to be had: the whole log, from its first byte
      reading.current = jobId;
      offset.current = 0;
      pull();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pid, jobId, start, startQ.isError]);
  useProjectEvents(pid, (ev) => {
    if (ev.job_id !== jobId) return;
    // new bytes, or the job ended (its last bytes may have come with that)
    if ((ev.type === "job_trace" && (ev.trace_size ?? 0) > offset.current) || (ev.type === "job" && ev.status && !isActive(ev.status))) pull();
  });
  // fallback poll while running (events are hints)
  useEffect(() => {
    if (shown.complete || (status && !isActive(status))) return;
    const t = window.setInterval(pull, 5000);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [shown.complete, status, jobId]);
  return shown;
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

export function LogView({ text, live, loading = false }: { text: string; live: boolean; loading?: boolean }) {
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
            {loading ? "Loading the log…" : live ? "Waiting for the runner…" : "This job has no log."}
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

export function JobPage({ ctx, jobId }: { ctx: Ctx; jobId: number }) {
  useJobReady(ctx.p.id, jobId);
  const qc = useQueryClient();
  const confirmRun = useConfirmRun();
  const nav = useNavigate();
  // a job that came after the wait: the last one stays on screen until it arrives
  const q = useQuery({ ...jobQuery(ctx.p.id, jobId), placeholderData: keepPreviousData });
  // status changes: through usePipeline's live updates (the "job" queries too)
  const pipeline = usePipeline(ctx, q.data?.pipeline_iid);
  const trace = useTrace(ctx.p.id, jobId, q.data?.status);
  // a job opened from further down: its pane starts at its top, as a page of its own would
  const pane = useRef<HTMLElement>(null);
  const shown = useRef(trace.job);
  useEffect(() => {
    if (shown.current === trace.job) return;
    shown.current = trace.job;
    const top = pane.current?.getBoundingClientRect().top;
    const bar = parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--topbar-h")) || 56;
    if (top != null && top < bar) window.scrollTo({ top: window.scrollY + top - bar - 16 });
  }, [trace.job]);
  const run = useAct(() => {
    qc.invalidateQueries({ queryKey: ["job", ctx.p.id] });
    qc.invalidateQueries({ queryKey: ["pipeline", ctx.p.id] });
  });
  // the job on screen, which is the one left while the next one comes (keepPreviousData)
  const act = async (j: S["JobDto"], what: "retry" | "play" | "cancel" | "unschedule") => {
    if (what === "play" && !(await confirmRun(j))) return;
    let retried: number | null = null;
    const done = what === "retry" ? "Retrying" : what === "play" ? "Started" : what === "unschedule" ? "Unscheduled: it waits for someone to run it" : "Canceling";
    if (await run(async () => (retried = (await api.post<S["JobDto"]>(`/projects/${ctx.p.id}/jobs/${j.id}/${what}`)).id), done) && what === "retry" && retried != null) {
      nav(`${ctx.web}/-/jobs/${retried}`);
    }
  };
  return (
    <Q q={q}>
      {(j) => {
        const live = isActive(j.status);
        // the log under the job's own header: another job's, while this one's start comes, says it loads
        const log = trace.job === j.id ? trace : { job: j.id, text: "", complete: false, loaded: false };
        return (
          <div className="stack" style={{ gap: 18 }}>
            {pipeline.data && <PipelineHeader ctx={ctx} p={pipeline.data} tab="jobs" />}
            <div className="jobs-split">
              {pipeline.data && <JobList ctx={ctx} p={pipeline.data} current={jobId} />}
              <section ref={pane} className="job-pane" aria-label={`Job ${j.name}`}>
                <div className="job-pane-head">
                  <StatusIcon status={j.status} allowFailure={j.allow_failure} size={18} />
                  <h2 className="grow">{j.name}</h2>
                  {j.can_run && (j.status === "manual" || j.status === "scheduled") && (
                    <button className="btn primary small" onClick={() => act(j, "play")}>
                      <Play size={14} /> {j.status === "scheduled" ? "Run now" : "Run"}
                    </button>
                  )}
                  {j.can_run && j.status === "scheduled" && (
                    <button className="btn small" onClick={() => act(j, "unschedule")}>
                      Unschedule
                    </button>
                  )}
                  {j.can_run && live && (
                    <button className="btn small" onClick={() => act(j, "cancel")}>
                      <Square size={13} /> Cancel
                    </button>
                  )}
                  {j.can_run && !j.retried && RETRYABLE.includes(j.status) && (
                    <button className="btn small" onClick={() => act(j, "retry")}>
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
                <dl className="facts-bar job-facts">
                  <div>
                    <dt>Status</dt>
                    <dd>
                      <StatusBadge status={j.status} />
                    </dd>
                  </div>
                  <div>
                    <dt>Stage</dt>
                    <dd>{j.stage}</dd>
                  </div>
                  {j.runner_description && (
                    <div>
                      <dt>Runner</dt>
                      <dd>{j.runner_description}</dd>
                    </div>
                  )}
                  {j.image && (
                    <div>
                      <dt>Image</dt>
                      <dd className="mono">{j.image}</dd>
                    </div>
                  )}
                  {j.duration != null && (
                    <div>
                      <dt>Duration</dt>
                      <dd>{duration(j.duration)}</dd>
                    </div>
                  )}
                  {j.queued_duration != null && (
                    <div>
                      <dt>Queued</dt>
                      <dd>{duration(j.queued_duration)}</dd>
                    </div>
                  )}
                  {j.finished_at && (
                    <div>
                      <dt>Finished</dt>
                      <dd title={new Date(j.finished_at).toLocaleString()}>{timeAgo(j.finished_at)}</dd>
                    </div>
                  )}
                  {j.attempt > 1 && (
                    <div>
                      <dt>Attempt</dt>
                      <dd>{j.attempt}</dd>
                    </div>
                  )}
                  {j.coverage != null && (
                    <div>
                      <dt>Coverage</dt>
                      <dd>{j.coverage.toFixed(2)}%</dd>
                    </div>
                  )}
                  {j.environment && (
                    <div>
                      <dt>{j.environment.action === "stop" ? "Stops" : "Deploys to"}</dt>
                      <dd>
                        <Link to={`${ctx.web}/-/environments`}>{j.environment.name}</Link>
                      </dd>
                    </div>
                  )}
                </dl>
                {j.retried && j.attempts[0] !== j.id && (
                  <p className="small" style={{ margin: 0 }}>
                    <Link to={`${ctx.web}/-/jobs/${j.attempts[0]}`}>A newer attempt exists</Link>
                  </p>
                )}
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
                <LogView key={log.job} text={log.text} live={live && !log.complete} loading={!log.loaded} />
              </section>
            </div>
          </div>
        );
      }}
    </Q>
  );
}
