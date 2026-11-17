import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronDown, ChevronRight, ShieldAlert, ShieldCheck } from "lucide-react";
import { api, listAnd, plural, qs, refLink, timeAgo, timeUntil, type S } from "../../api/client";
import { Empty, FilterTabs, ListHead, Q, SearchField, useDebounced } from "../../components/ui";
import { DecideCard } from "../../components/decide";
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
  return <span className={`sev sev-${severity}`}>{severity[0].toUpperCase() + severity.slice(1)}</span>;
}

type Finding = S["Finding"];

/** Where a finding is, in a few words: a file and line, a package, or an image. */
function whereText(f: Finding) {
  if (f.report_type === "dependency_scanning" && f.package_name) return `${f.package_name}@${f.package_version ?? "?"}`;
  if (f.report_type === "container_scanning") return f.image ?? f.package_name ?? "";
  return f.file ? `${f.file}${f.start_line ? `:${f.start_line}` : ""}` : "";
}

/**
 * The page's emphasized card: the critical and high findings still open on the
 * default branch, to fix or to dismiss. None when there are none.
 */
function Triage({ ctx, counts, open, onOpen }: { ctx: Ctx; counts: S["SecurityCounts"]; open?: Finding[]; onOpen: (f: Finding) => void }) {
  const urgent = counts.critical + counts.high;
  // the open findings: those the list shows when it is not filtered, else their own request
  const q = useQuery({
    queryKey: ["security", ctx.p.id, "detected", "triage"],
    queryFn: () => api.get<S["SecurityOverview"]>(`/projects/${ctx.p.id}/security/findings${qs({ state: "detected", per_page: 100 })}`),
    enabled: urgent > 0 && !open,
  });
  if (urgent === 0) return null;
  const rank = (f: Finding) => (f.severity === "critical" ? 0 : 1);
  const top = (open ?? q.data?.findings ?? [])
    .filter((f) => f.severity === "critical" || f.severity === "high")
    .sort((a, b) => rank(a) - rank(b))
    .slice(0, 4);
  const parts = [counts.critical > 0 && `${counts.critical} critical`, counts.high > 0 && `${counts.high} high`].filter(Boolean).join(" and ");
  return (
    <DecideCard
      label="To triage"
      late={counts.critical > 0}
      eyebrow={
        <>
          <ShieldAlert size={14} aria-hidden /> To triage
        </>
      }
      title={`${parts} finding${urgent === 1 ? "" : "s"}`}
      sub="Open on the default branch: fix them, or dismiss them with a reason."
      actions={
        top[0] && (
          <button type="button" className="btn run" onClick={() => onOpen(top[0])}>
            Look at the first
          </button>
        )
      }
      rows={top.map((f) => ({
        key: String(f.id),
        tone: f.severity === "critical" ? "bad" : "warn",
        text: (
          <button type="button" className="decide-link" onClick={() => onOpen(f)} title={`${f.severity}: ${f.title}`}>
            {f.title}
          </button>
        ),
        aside: whereText(f),
      }))}
      foot={top.length > 0 && urgent > top.length ? `And ${urgent - top.length} more below.` : undefined}
    />
  );
}

/** What each built-in scanner looks at, in a few words. */
const SCANS: Record<string, string> = {
  sast: "its code",
  secret_detection: "secrets",
  dependency_scanning: "dependencies",
  iac: "infrastructure files",
  container_scanning: "container images",
};

/**
 * No scan yet: how findings come here. The built-in scanners first (a click away
 * for maintainers, or already on), then the reports of the project's own jobs.
 */
function NoScanYet({ ctx }: { ctx: Ctx }) {
  // the settings page's request
  const q = useQuery({ queryKey: ["scanning", ctx.p.id], queryFn: () => api.get<S["ScanningStatus"]>(`/projects/${ctx.p.id}/security/scanning`), retry: false });
  const s = q.data;
  const what = listAnd((s?.available ?? []).map((k) => SCANS[k] ?? k));
  const hours = s?.interval_hours;
  const every = hours === 24 ? "every day" : hours ? `every ${plural(hours, "hour")}` : null;
  return (
    <Empty>
      <div className="no-scan">
        <ShieldAlert size={26} className="faint" aria-hidden />
        <h2>No security scan yet</h2>
        {s?.enabled ? (
          <p>
            Security scanning is on{s.next_run_at ? `: the first scan runs ${timeUntil(s.next_run_at)}` : ""}. Its findings show up here.
          </p>
        ) : s?.instance_enabled && what ? (
          <>
            <p>
              Forgust can scan this project itself{every ? `, ${every}` : ""}: {what}.
              {!ctx.p.can_admin && " A maintainer turns it on in the project's settings."}
            </p>
            {ctx.p.can_admin && (
              <Link className="btn primary" to={`${ctx.web}/-/settings/automation#security-scanning`}>
                <ShieldCheck size={15} /> Turn on security scanning
              </Link>
            )}
          </>
        ) : null}
        <p className="small">
          Jobs can also upload their reports: <code>artifacts:reports:sast</code>, <code>secret_detection</code>, <code>dependency_scanning</code> or{" "}
          <code>container_scanning</code> (GitLab's format, or SARIF from CodeQL, Semgrep, Trivy and others).
        </p>
      </div>
    </Empty>
  );
}

/** A finding in a list: a toggle (severity, title) and what it is about. */
function FindingRow({ ctx, f, open, onToggle, struck }: { ctx: Ctx; f: Finding; open: boolean; onToggle: () => void; struck?: boolean }) {
  return (
    <div className="list-group" id={`finding-${f.id}`}>
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
    <Link className="mono" to={`${refLink(ctx.web, "blob", ref, f.file)}${lines}`}>
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
  // a finding picked in the card: shown open in the list, once it is there
  const [focus, setFocus] = useState<number | null>(null);
  const term = useDebounced(search, 250);
  const q = useQuery({
    queryKey: ["security", ctx.p.id, state, severity, kind, term],
    queryFn: () => api.get<S["SecurityOverview"]>(`/projects/${ctx.p.id}/security/findings${qs({ state, severity, report_type: kind, search: term, per_page: 100 })}`),
  });
  useEffect(() => {
    if (focus == null) return;
    const el = document.getElementById(`finding-${focus}`);
    if (!el) return;
    el.scrollIntoView({ block: "start", behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
    setFocus(null);
  }, [focus, q.data]);
  const openFinding = (f: Finding) => {
    setState("detected");
    setSeverity("");
    setKind("");
    setSearch("");
    setOpen(f.id);
    setFocus(f.id);
  };
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
        {(s) =>
          // never scanned: nothing to count or filter, only how findings come
          s.last_scan_pipeline_iid == null && s.findings.length === 0 && state === "detected" && !severity && !kind && !term ? (
            <NoScanYet ctx={ctx} />
          ) : (
            <>
              <Triage ctx={ctx} counts={s.counts} open={state === "detected" && !severity && !kind && !term ? s.findings : undefined} onOpen={openFinding} />
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
                  {state === "detected" ? (
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
          )
        }
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
