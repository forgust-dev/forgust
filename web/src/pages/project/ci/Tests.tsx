import { useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { CheckCircle2, ChevronDown, ChevronRight, CircleSlash, FlaskConical, XCircle } from "lucide-react";
import { api, type S } from "../../../api/client";
import type { Ctx } from "../Project";
import { Segmented } from "../../../components/controls";

type Case = { suite?: string; classname?: string; name?: string; status?: string; message?: string; output?: string; time?: number };

function line(t: { total: number; failed: number; errored: number; skipped: number; success: number }) {
  const parts = [`${t.total} tests`, `${t.failed + t.errored} failed`, `${t.success} passed`];
  if (t.skipped) parts.push(`${t.skipped} skipped`);
  return parts.join(" · ");
}

/** One failing case, its message and output as text (never HTML). */
function Failure({ c, job }: { c: Case; job?: string }) {
  const [open, setOpen] = useState(false);
  return (
    <li className="test-failure">
      <button type="button" className="link-btn" aria-expanded={open} onClick={() => setOpen(!open)}>
        {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        <XCircle size={14} className="ic-danger" />
        <span className="mono-ish">
          {c.classname ? `${c.classname} › ` : ""}
          {c.name}
        </span>
        {job && <span className="faint small"> in {job}</span>}
      </button>
      {open && (
        <pre className="test-output">
          {c.message ? `${c.message}\n\n` : ""}
          {c.output || "(no output)"}
        </pre>
      )}
    </li>
  );
}

/** Every test of a pipeline, by job and suite, filterable; a failing one opens
 *  on its message and output (from the report's failures). */
export function TestCaseList({ ctx, iid }: { ctx: Ctx; iid: number }) {
  const [open, setOpen] = useState(false);
  const [show, setShow] = useState<"all" | "failed" | "skipped">("all");
  const [search, setSearch] = useState("");
  const cases = useQuery({
    queryKey: ["test-cases", ctx.p.id, iid],
    queryFn: () => api.get<S["JobTestCases"][]>(`/projects/${ctx.p.id}/pipelines/${iid}/test_cases`),
    enabled: open,
  });
  const report = useQuery({ queryKey: ["tests", ctx.p.id, iid], queryFn: () => api.get<S["PipelineTests"]>(`/projects/${ctx.p.id}/pipelines/${iid}/test_report`), enabled: open });
  const failureOf = (job: number, c: S["TestCaseBrief"]) =>
    ((report.data?.jobs.find((j) => j.build_id === job)?.failures as unknown as Case[]) ?? []).find((f) => f.classname === c.classname && f.name === c.name);
  const q = search.trim().toLowerCase();
  const keep = (c: S["TestCaseBrief"]) =>
    (show === "all" || (show === "failed" ? c.status === "failed" || c.status === "error" : c.status === "skipped")) &&
    (!q || `${c.classname} ${c.name}`.toLowerCase().includes(q));
  return (
    <div className="test-cases">
      <button type="button" className="link-btn small" aria-expanded={open} onClick={() => setOpen(!open)}>
        {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />} {open ? "Hide the tests" : "Show all tests"}
      </button>
      {open && (
        <div className="stack" style={{ gap: 8, marginTop: 8 }}>
          <div className="row wrap" style={{ gap: 8 }}>
            <Segmented
              label="Show"
              value={show}
              onChange={setShow}
              options={[
                { value: "all", label: "All" },
                { value: "failed", label: "Failed" },
                { value: "skipped", label: "Skipped" },
              ]}
            />
            <input className="small-input" placeholder="Filter by name" aria-label="Filter tests by name" value={search} onChange={(e) => setSearch(e.target.value)} />
          </div>
          {cases.isLoading && <p className="muted small">Loading…</p>}
          {(cases.data ?? []).map((j) => {
            const rows = j.cases.filter(keep);
            if (rows.length === 0) return null;
            const suites = [...new Set(rows.map((c) => c.suite))];
            return (
              <div key={j.build_id} className="test-cases-job">
                <div className="small strong">
                  <Link to={`${ctx.web}/-/jobs/${j.build_id}`}>{j.job_name}</Link> <span className="muted">· {rows.length} shown</span>
                </div>
                {suites.map((s) => (
                  <div key={s}>
                    {s && <div className="test-suite small muted">{s}</div>}
                    <ul className="test-case-list">
                      {rows
                        .filter((c) => c.suite === s)
                        .map((c, i) => (
                          <CaseRowView key={i} c={c} failure={c.status === "failed" || c.status === "error" ? failureOf(j.build_id, c) : undefined} />
                        ))}
                    </ul>
                  </div>
                ))}
              </div>
            );
          })}
          {cases.data && cases.data.every((j) => j.cases.filter(keep).length === 0) && <p className="muted small">No test matches.</p>}
        </div>
      )}
    </div>
  );
}

function CaseRowView({ c, failure }: { c: S["TestCaseBrief"]; failure?: Case }) {
  const [open, setOpen] = useState(false);
  const failed = c.status === "failed" || c.status === "error";
  const icon = failed ? <XCircle size={14} className="ic-danger" /> : c.status === "skipped" ? <CircleSlash size={14} className="faint" /> : <CheckCircle2 size={14} className="ic-ok" />;
  const label = (
    <>
      {icon}
      <span className="grow mono-ish ellipsis">
        {c.classname ? <span className="faint">{c.classname} › </span> : null}
        {c.name}
      </span>
      <span className="mono small faint nowrap">{c.time < 1 ? `${Math.round(c.time * 1000)} ms` : `${c.time.toFixed(2)} s`}</span>
    </>
  );
  return (
    <li>
      {failed ? (
        <button type="button" className="test-case-row" aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
          {label}
        </button>
      ) : (
        <div className="test-case-row">
          <span style={{ width: 13 }} />
          {label}
        </div>
      )}
      {open && (
        <pre className="test-output">
          {failure ? `${failure.message ? `${failure.message}\n\n` : ""}${failure.output || "(no output)"}` : "(the report kept no details for this failure)"}
        </pre>
      )}
    </li>
  );
}

/** A pipeline's test results (jobs with a JUnit report). */
export function PipelineTests({ ctx, iid }: { ctx: Ctx; iid: number }) {
  const q = useQuery({ queryKey: ["tests", ctx.p.id, iid], queryFn: () => api.get<S["PipelineTests"]>(`/projects/${ctx.p.id}/pipelines/${iid}/test_report`) });
  const t = q.data;
  if (!t || t.jobs.length === 0) return null;
  return (
    <section className="card test-report" aria-label="Tests">
      <header className="row" style={{ gap: 8 }}>
        <FlaskConical size={16} />
        <strong>Tests</strong>
        <span className="muted small">{line(t)}</span>
      </header>
      {t.jobs.map((j) => {
        const failures = (j.failures as unknown as Case[]) ?? [];
        return (
          <div key={j.build_id} className="test-job">
            <div className="row small" style={{ gap: 8 }}>
              {j.failed + j.errored > 0 ? <XCircle size={14} className="ic-danger" /> : <CheckCircle2 size={14} className="ic-ok" />}
              <Link to={`${ctx.web}/-/jobs/${j.build_id}`}>{j.job_name}</Link>
              <span className="muted">
                {j.total} tests · {j.failed + j.errored} failed · {j.duration.toFixed(1)} s
              </span>
              {j.error && <span className="error-text">Report not readable: {j.error}</span>}
            </div>
            {failures.length > 0 && (
              <ul className="test-failures">
                {failures.map((c, i) => (
                  <Failure key={i} c={c} />
                ))}
              </ul>
            )}
          </div>
        );
      })}
      <TestCaseList ctx={ctx} iid={iid} />
    </section>
  );
}

/** A merge request's tests, compared with the target branch. */
export function MrTestSummary({ ctx, iid }: { ctx: Ctx; iid: number }) {
  const q = useQuery({ queryKey: ["mr-tests", ctx.p.id, iid], queryFn: () => api.get<S["MrTests"]>(`/projects/${ctx.p.id}/merge_requests/${iid}/test_summary`) });
  const s = q.data;
  if (!s?.head) return null;
  const failing = s.new_failures.length + s.existing_failures.length;
  return (
    <section className="card test-report" aria-label="Test summary">
      <header className="row wrap" style={{ gap: 8 }}>
        <FlaskConical size={16} />
        <strong>Test summary</strong>
        <span className="muted small">{line(s.head)}</span>
        {s.head_pipeline_iid && (
          <Link className="small" to={`${ctx.web}/-/pipelines/${s.head_pipeline_iid}`}>
            pipeline #{s.head_pipeline_iid}
          </Link>
        )}
      </header>
      {s.base ? (
        <p className="small muted" style={{ margin: 0 }}>
          Against the target branch (pipeline #{s.base_pipeline_iid}): {s.new_failures.length} new failure{s.new_failures.length === 1 ? "" : "s"},{" "}
          {s.existing_failures.length} already failing, {s.resolved.length} fixed.
        </p>
      ) : (
        failing > 0 && <p className="small muted" style={{ margin: 0 }}>No test results on the target branch to compare with.</p>
      )}
      {s.new_failures.length > 0 && (
        <div>
          <h3 className="small">New failures</h3>
          <ul className="test-failures">
            {s.new_failures.map((f, i) => (
              <Failure key={i} c={f.case as unknown as Case} job={f.job} />
            ))}
          </ul>
        </div>
      )}
      {s.existing_failures.length > 0 && (
        <div>
          <h3 className="small">Already failing</h3>
          <ul className="test-failures">
            {s.existing_failures.map((f, i) => (
              <Failure key={i} c={f.case as unknown as Case} job={f.job} />
            ))}
          </ul>
        </div>
      )}
      {s.resolved.length > 0 && (
        <div>
          <h3 className="small">Fixed</h3>
          <ul className="test-failures">
            {s.resolved.map((k) => (
              <li key={k} className="small">
                <CheckCircle2 size={14} className="ic-ok" /> <span className="mono-ish">{k.replace("::", " › ")}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
      {s.head_pipeline_iid != null && <TestCaseList ctx={ctx} iid={s.head_pipeline_iid} />}
    </section>
  );
}
