import { useState } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronDown, ChevronRight, ShieldAlert, ShieldCheck } from "lucide-react";
import { api, qs, timeAgo, type S } from "../../api/client";
import { Empty, FilterTabs, ListHead, Q, SearchField, useDebounced } from "../../components/ui";
import { Select } from "../../components/controls";
import { useAct } from "../../shell/toast";
import type { Ctx } from "./Project";

/** What the report types are called. */
export const KIND: Record<string, string> = {
  sast: "Code",
  secret_detection: "Secret",
  dependency_scanning: "Dependency",
  container_scanning: "Container",
};

const SEVERITIES = ["critical", "high", "medium", "low", "info", "unknown"] as const;

const REASONS = [
  { value: "false_positive", label: "False positive" },
  { value: "acceptable_risk", label: "Acceptable risk" },
  { value: "used_in_tests", label: "Used in tests" },
  { value: "mitigating_control", label: "Mitigated elsewhere" },
  { value: "not_applicable", label: "Does not apply" },
];

export function SeverityBadge({ severity }: { severity: string }) {
  return <span className={`sev sev-${severity}`}>{severity}</span>;
}

type Finding = S["Finding"];

/** A finding in a list: a toggle (severity, title) and what it is about. */
function FindingRow({ ctx, f, open, onToggle, struck }: { ctx: Ctx; f: Finding; open: boolean; onToggle: () => void; struck?: boolean }) {
  return (
    <div className="list-group">
      <div className="item finding-row">
        <button type="button" className="finding-toggle" aria-expanded={open} onClick={onToggle}>
          {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          <SeverityBadge severity={f.severity} />
          <span className={`finding-title${struck ? " struck" : ""}`}>{f.title}</span>
        </button>
        <span className="row-meta small finding-meta">
          <span className="pill">{KIND[f.report_type] ?? f.report_type}</span>
          <Where ctx={ctx} f={f} />
          <span>{f.scanner}</span>
          {f.state === "dismissed" && <span className="faint">dismissed by {f.dismissed_by ?? "someone"}</span>}
          {f.state === "resolved" && f.resolved_at && <span className="ok-text">fixed {timeAgo(f.resolved_at)}</span>}
          <span className="faint">first seen {timeAgo(f.first_seen_at)}</span>
        </span>
      </div>
      {open && <FindingDetail ctx={ctx} id={f.id} />}
    </div>
  );
}

/** Where a finding is: file and lines (linked to the code), package, or image. */
function Where({ ctx, f }: { ctx: Ctx; f: Finding }) {
  const ref = ctx.p.default_branch ?? "main";
  if (f.report_type === "dependency_scanning" && f.package_name) {
    return (
      <span>
        <span className="mono">
          {f.package_name}@{f.package_version ?? "?"}
        </span>
        {f.fixed_version && <span className="ok-text"> → fixed in {f.fixed_version}</span>}
        {f.file && <span className="faint"> · {f.file}</span>}
      </span>
    );
  }
  if (f.report_type === "container_scanning") {
    return (
      <span className="mono">
        {f.image} · {f.package_name}@{f.package_version}
        {f.fixed_version && <span className="ok-text"> → {f.fixed_version}</span>}
      </span>
    );
  }
  if (!f.file) return null;
  const lines = f.start_line ? `#L${f.start_line}${f.end_line && f.end_line !== f.start_line ? `-${f.end_line}` : ""}` : "";
  return (
    <Link className="mono" to={`${ctx.web}/-/blob/${ref}/${f.file}${lines}`}>
      {f.file}
      {f.start_line ? `:${f.start_line}` : ""}
    </Link>
  );
}

export function SecurityPage({ ctx }: { ctx: Ctx }) {
  const [state, setState] = useState<"detected" | "dismissed" | "resolved" | "all">("detected");
  const [severity, setSeverity] = useState("");
  const [kind, setKind] = useState("");
  const [search, setSearch] = useState("");
  const [open, setOpen] = useState<number | null>(null);
  const term = useDebounced(search, 250);
  const q = useQuery({
    queryKey: ["security", ctx.p.id, state, severity, kind, term],
    queryFn: () => api.get<S["SecurityOverview"]>(`/projects/${ctx.p.id}/security/findings${qs({ state, severity, report_type: kind, search: term, per_page: 100 })}`),
  });
  return (
    <div className="stack">
      <ListHead
        title="Security"
        sub={
          q.data?.last_scan_pipeline_iid ? (
            <>
              Findings of the default branch's scans (the latest in <Link to={`${ctx.web}/-/pipelines/${q.data.last_scan_pipeline_iid}`}>pipeline #{q.data.last_scan_pipeline_iid}</Link>).
            </>
          ) : (
            "Findings of the default branch's security scans."
          )
        }
      />
      <Q q={q}>
        {(s) => (
          <>
            <div className="sev-summary" role="group" aria-label="By severity">
              {SEVERITIES.filter((sv) => sv !== "unknown" || s.counts.unknown > 0).map((sv) => (
                <button
                  key={sv}
                  type="button"
                  className={`sev-count${severity === sv ? " active" : ""}`}
                  aria-pressed={severity === sv}
                  onClick={() => setSeverity(severity === sv ? "" : sv)}
                >
                  <SeverityBadge severity={sv} /> <strong>{s.counts[sv]}</strong>
                </button>
              ))}
            </div>
            <div className="row wrap" style={{ gap: 8 }}>
              <FilterTabs
                label="State"
                value={state}
                onChange={setState}
                options={[
                  { value: "detected", label: "Open" },
                  { value: "dismissed", label: "Dismissed", count: s.counts.dismissed },
                  { value: "resolved", label: "Fixed", count: s.counts.resolved },
                  { value: "all", label: "All" },
                ]}
              />
            </div>
            <div className="row wrap" style={{ gap: 8 }}>
              <Select
                label="Kind"
                style={{ width: 170 }}
                value={kind}
                onChange={setKind}
                options={[{ value: "", label: "Every kind" }, ...Object.entries(KIND).map(([value, label]) => ({ value, label }))]}
              />
              <SearchField value={search} onChange={setSearch} placeholder="Filter by title, file, package or rule" />
            </div>
            {s.findings.length === 0 ? (
              <Empty>
                {s.last_scan_pipeline_iid == null ? (
                  <>
                    No security scan yet. Jobs that upload <code>artifacts:reports:sast</code>, <code>secret_detection</code>, <code>dependency_scanning</code> or{" "}
                    <code>container_scanning</code> (GitLab's format, or SARIF from CodeQL, Semgrep, Trivy and others) show their findings here.
                  </>
                ) : state === "detected" ? (
                  <>
                    <ShieldCheck size={16} className="ic-ok" /> Nothing open{severity || kind || term ? " matches" : ""}.
                  </>
                ) : (
                  "Nothing here."
                )}
              </Empty>
            ) : (
              <div className="list findings">
                {s.findings.map((f) => (
                  <FindingRow key={f.id} ctx={ctx} f={f} open={open === f.id} onToggle={() => setOpen(open === f.id ? null : f.id)} />
                ))}
              </div>
            )}
          </>
        )}
      </Q>
    </div>
  );
}

const ACTIONS: Record<string, string> = {
  detected: "Detected",
  dismissed: "Dismissed",
  reopened: "Reopened",
  resolved: "Fixed: no longer found",
  redetected: "Found again",
};

/** A finding: description, solution, identifiers, history; dismiss or reopen. */
export function FindingDetail({ ctx, id }: { ctx: Ctx; id: number }) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["finding", ctx.p.id, id], queryFn: () => api.get<S["FindingDetail"]>(`/projects/${ctx.p.id}/security/findings/${id}`) });
  const [reason, setReason] = useState("false_positive");
  const [comment, setComment] = useState("");
  const run = useAct(() => {
    qc.invalidateQueries({ queryKey: ["security", ctx.p.id] });
    qc.invalidateQueries({ queryKey: ["finding", ctx.p.id, id] });
    qc.invalidateQueries({ queryKey: ["mr-security", ctx.p.id] });
  });
  const act = (path: string, body: unknown, done: string) => run(() => api.post(`/projects/${ctx.p.id}/security/findings/${id}/${path}`, body), done);
  return (
    <div className="finding-detail">
      <Q q={q}>
        {(f) => (
          <div className="stack" style={{ gap: 12 }}>
            {f.description && f.description !== f.title && <p className="pre-wrap small" style={{ margin: 0 }}>{f.description}</p>}
            {f.solution && (
              <p className="small" style={{ margin: 0 }}>
                <strong>Solution:</strong> <span className="pre-wrap">{f.solution}</span>
              </p>
            )}
            <div className="row wrap small" style={{ gap: 6 }}>
              {/* a dependency's or an image's advisories say more than the scanner's own id */}
              {f.report_type !== "dependency_scanning" && f.report_type !== "container_scanning" && (
                <>
                  <span className="muted">Rule</span> <code>{f.rule_id}</code>
                </>
              )}
              {f.identifiers
                .filter((_, n) => n > 0 || f.report_type === "dependency_scanning" || f.report_type === "container_scanning")
                .filter((i) => !["gemnasium", "trivy"].includes(i.type))
                .map((i) =>
                  i.url && /^https?:\/\//.test(i.url) ? (
                    <a key={i.type + i.name} className="pill" href={i.url} target="_blank" rel="noreferrer noopener">
                      {i.name}
                    </a>
                  ) : (
                    <span key={i.type + i.name} className="pill">
                      {i.name}
                    </span>
                  ),
                )}
            </div>
            {f.fix_merge_requests.length > 0 && (
              <p className="small" style={{ margin: 0 }}>
                <strong>Fix:</strong>{" "}
                {f.fix_merge_requests.map((m, i) => (
                  <span key={m.iid}>
                    {i > 0 && ", "}
                    <Link to={`${ctx.web}/-/merge_requests/${m.iid}`}>
                      !{m.iid} {m.title}
                    </Link>
                    {m.state === "merged" && <span className="muted"> (merged: fixed once the default branch is scanned again)</span>}
                  </span>
                ))}
              </p>
            )}
            <ol className="finding-history small">
              {f.events.map((e, i) => (
                <li key={i}>
                  <strong>{ACTIONS[e.action] ?? e.action}</strong>
                  {e.username && <> by {e.username}</>}
                  {e.pipeline_iid != null && (
                    <>
                      {" "}
                      in <Link to={`${ctx.web}/-/pipelines/${e.pipeline_iid}`}>pipeline #{e.pipeline_iid}</Link>
                    </>
                  )}{" "}
                  <span className="faint">{timeAgo(e.created_at)}</span>
                  {e.note && <span className="muted">: {e.note}</span>}
                </li>
              ))}
            </ol>
            {!ctx.p.archived &&
              (f.state === "dismissed" ? (
                <div>
                  <button className="btn small" onClick={() => act("reopen", {}, "Reopened")}>
                    Reopen
                  </button>
                </div>
              ) : (
                <form
                  className="row wrap"
                  style={{ gap: 8, alignItems: "flex-end" }}
                  onSubmit={(e) => {
                    e.preventDefault();
                    act("dismiss", { reason, comment }, "Dismissed");
                  }}
                >
                  <Select label="Reason" value={reason} onChange={setReason} options={REASONS} />
                  <input className="grow" aria-label="Comment" placeholder="Why (optional)" value={comment} onChange={(e) => setComment(e.target.value)} />
                  <button className="btn small">Dismiss</button>
                </form>
              ))}
          </div>
        )}
      </Q>
    </div>
  );
}

/** A merge request's findings against the default branch: new and fixed ones. */
export function MrSecurity({ ctx, iid }: { ctx: Ctx; iid: number }) {
  const developer = ctx.p.can_push || ctx.p.can_admin || ["developer", "maintainer", "owner"].includes(ctx.p.access_level);
  const q = useQuery({
    queryKey: ["mr-security", ctx.p.id, iid],
    queryFn: () => api.get<S["MrSecurity"]>(`/projects/${ctx.p.id}/merge_requests/${iid}/security`),
    enabled: developer,
  });
  const [open, setOpen] = useState<number | null>(null);
  const s = q.data;
  if (!s?.head_pipeline_iid) return null;
  const high = s.new.filter((f) => (f.severity === "critical" || f.severity === "high") && f.state !== "dismissed").length;
  return (
    <section className="card test-report" aria-label="Security scanning">
      <header className="row wrap" style={{ gap: 8 }}>
        {s.new.length > 0 ? <ShieldAlert size={16} className={high > 0 ? "ic-danger" : undefined} /> : <ShieldCheck size={16} className="ic-ok" />}
        <strong>Security scanning</strong>
        <span className="muted small">
          {s.new.length} new, {s.fixed.length} fixed
        </span>
        <Link className="small" to={`${ctx.web}/-/pipelines/${s.head_pipeline_iid}`}>
          pipeline #{s.head_pipeline_iid}
        </Link>
      </header>
      <p className="small muted" style={{ margin: 0 }}>
        {s.base_pipeline_iid != null
          ? `Against the default branch (pipeline #${s.base_pipeline_iid}), for ${s.report_types.map((t) => KIND[t]?.toLowerCase() ?? t).join(", ")} scans.`
          : "No scan of the default branch to compare with: everything found is new."}
      </p>
      {(["new", "fixed"] as const).map(
        (k) =>
          s[k].length > 0 && (
            <div key={k}>
              <h3 className="small">{k === "new" ? "New" : "Fixed"}</h3>
              <div className="list findings">
                {s[k].map((f) => (
                  <FindingRow key={f.id} ctx={ctx} f={f} struck={k === "fixed"} open={open === f.id} onToggle={() => setOpen(open === f.id ? null : f.id)} />
                ))}
              </div>
            </div>
          ),
      )}
    </section>
  );
}
