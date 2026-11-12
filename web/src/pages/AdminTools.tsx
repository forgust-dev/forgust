import { useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, RefreshCw, XCircle } from "lucide-react";
import { api, bytes, qs, timeAgo, type S } from "../api/client";
import { Card, ErrorBox, Field, Q, SearchField, useDebounced } from "../components/ui";
import { Segmented } from "../components/controls";
import { useAct, useToast } from "../shell/toast";

// Admin > Overview, Storage, Jobs and Health: read-only views.

function Stat({ label, value, hint, to }: { label: string; value: React.ReactNode; hint?: React.ReactNode; to?: string }) {
  return (
    <div className="stat">
      <span className="stat-label">{label}</span>
      <span className="stat-value">{to ? <Link to={to}>{value}</Link> : value}</span>
      {hint && (
        <span className="small muted" style={{ overflowWrap: "anywhere" }}>
          {hint}
        </span>
      )}
    </div>
  );
}

export function AdminOverview() {
  const q = useQuery({ queryKey: ["admin-overview"], queryFn: () => api.get<S["Overview"]>("/admin/overview"), refetchInterval: 30_000 });
  return (
    <Q q={q}>
      {(o) => {
        const c = o.counts;
        const ci = o.ci;
        return (
          <div className="stack" style={{ gap: 20 }}>
            <div className="stat-grid">
              <Stat label="Users" value={c.users_active} hint={`${c.users_admins} admin${c.users_admins === 1 ? "" : "s"} · ${c.users_blocked} blocked · ${c.users_unconfirmed} unconfirmed`} to="/admin/users" />
              <Stat label="Bots" value={c.bots} hint="project, group and Renovate bots" />
              <Stat label="Groups" value={c.groups} />
              <Stat label="Projects" value={c.projects} to="/admin/storage" />
              <Stat label="Merge requests" value={c.merge_requests_open} hint={`open · ${c.merge_requests_merged} merged`} />
              <Stat label="Issues" value={c.issues_open} hint={`open · ${c.issues_closed} closed`} />
              <Stat label="Dependency updates" value={o.renovate_projects} hint={`projects · ${o.renovate_runs} run${o.renovate_runs === 1 ? "" : "s"} going on`} to="/admin/dependency-updates" />
            </div>

            <Card title="CI, last 30 days">
              <div className="stack">
                <div className="stat-grid">
                  <Stat label="Jobs" value={ci.jobs_last_30_days} hint="last 30 days" />
                  <Stat label="Waiting for a runner" value={ci.jobs_queued} hint={`${ci.jobs_running} running now`} />
                  <Stat label="Runners online" value={`${ci.runners_online} / ${ci.runners}`} hint={`${ci.runners_paused} paused · seen in the last hour`} to="/admin/runners" />
                </div>
                <PipelinesChart days={ci.pipelines_per_day} />
                {ci.busiest_projects.length > 0 && (
                  <div>
                    <h3 className="small">Most pipelines</h3>
                    <div className="list">
                      {ci.busiest_projects.map((b) => (
                        <div className="item small" key={b.project_id}>
                          <Link className="grow" to={`/${b.full_path}/-/pipelines`}>
                            {b.full_path}
                          </Link>
                          <span className="mono">{b.pipelines}</span>
                        </div>
                      ))}
                    </div>
                  </div>
                )}
              </div>
            </Card>

            <div className="stat-grid">
              <Stat label="Version" value={<span className="mono">{o.version}</span>} />
              <Stat label="Blob store" value={<span className="mono">{o.blob_backend}</span>} to="/admin/storage" />
              <Stat label="Cached responses" value={o.cache_entries} />
              <Stat label="Live git cat-file processes" value={o.git_processes} />
            </div>
            <p className="small muted" style={{ margin: 0 }}>
              More in the <a href="/metrics">Prometheus metrics</a> and <Link to="/admin/health">Health</Link>.
            </p>
          </div>
        );
      }}
    </Q>
  );
}

/** Pipelines per day, stacked by outcome (status colors: they mean the state). */
function PipelinesChart({ days }: { days: S["PipelineDay"][] }) {
  const max = Math.max(1, ...days.map((d) => d.success + d.failed + d.other));
  const total = days.reduce((n, d) => n + d.success + d.failed + d.other, 0);
  const fmt = (d: string) => new Date(`${d}T00:00:00`).toLocaleDateString(undefined, { month: "short", day: "numeric" });
  return (
    <figure className="pipe-chart" aria-label={`Pipelines per day, ${total} in 30 days`}>
      <div className="pipe-chart-head">
        <strong className="small">Pipelines per day</strong>
        <span className="small muted">{total} in 30 days</span>
        <span className="grow" />
        <span className="pipe-legend small">
          <i className="ok" /> passed <i className="bad" /> failed <i className="other" /> other
        </span>
      </div>
      <div className="pipe-bars" role="list">
        {days.map((d) => {
          const n = d.success + d.failed + d.other;
          return (
            <div
              key={d.day}
              className="pipe-bar"
              role="listitem"
              title={`${fmt(d.day)}: ${n} pipeline${n === 1 ? "" : "s"}\n${d.success} passed · ${d.failed} failed · ${d.other} other`}
            >
              {/* bottom up: passed on the baseline, then failed, then the rest */}
              {d.success > 0 && <span className="seg ok" style={{ height: `${(d.success / max) * 100}%` }} />}
              {d.failed > 0 && <span className="seg bad" style={{ height: `${(d.failed / max) * 100}%` }} />}
              {d.other > 0 && <span className="seg other" style={{ height: `${(d.other / max) * 100}%` }} />}
            </div>
          );
        })}
      </div>
      <div className="pipe-axis small faint">
        <span>{fmt(days[0].day)}</span>
        <span>{fmt(days[days.length - 1].day)}</span>
      </div>
    </figure>
  );
}

const KINDS: { key: keyof S["StorageKinds"]; label: string }[] = [
  { key: "repositories", label: "Git repositories" },
  { key: "lfs", label: "Git LFS" },
  { key: "artifacts", label: "CI artifacts" },
  { key: "job_logs_archived", label: "Job logs (archived)" },
  { key: "job_logs_live", label: "Job logs (in the database)" },
  { key: "registry", label: "Container registry" },
  { key: "uploads", label: "Uploads" },
];

/** What each family of unreferenced objects is (`blob_scan.rs`). */
const ORPHANS: Record<string, string> = {
  lfs: "LFS objects without a record",
  lfs_unused: "LFS objects no project uses (kept)",
  registry: "Image layers and manifests without a record",
  registry_uploads: "Image uploads no longer in progress",
  ci: "CI artifacts and job logs without a job",
  uploads: "Attachments without a record",
  avatars: "Avatars of deleted users, projects and groups",
  tmp: "Interrupted writes",
  unrecognized: "Keys Forgust does not write (not counted)",
};

export function AdminStorage() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["admin-storage"], queryFn: () => api.get<S["StorageOverview"]>("/admin/storage") });
  const scanning = q.data?.scan && !q.data.scan.finished_at;
  // follow a scan while it runs
  useQuery({
    queryKey: ["admin-storage", "poll"],
    queryFn: async () => {
      await qc.invalidateQueries({ queryKey: ["admin-storage"], exact: true });
      return 1;
    },
    enabled: !!scanning,
    refetchInterval: 3000,
  });
  const scan = useMutation({ mutationFn: () => api.post("/admin/storage/scan", {}), onSuccess: () => qc.invalidateQueries({ queryKey: ["admin-storage"] }) });
  return (
    <Q q={q}>
      {(s) => (
        <div className="stack" style={{ gap: 20 }}>
          <div className="stat-grid">
            <Stat label="Total" value={bytes(s.total)} hint={`${s.blob_backend} · ${s.blob_location}`} />
            {KINDS.map((k) => (
              <Stat key={k.key} label={k.label} value={bytes(s.kinds[k.key])} />
            ))}
          </div>
          <p className="small muted" style={{ margin: 0 }}>
            Repositories are measured after each push and once a day
            {s.repositories_unmeasured > 0 && <> ({s.repositories_unmeasured} not measured in the last day: measuring them now)</>}. LFS objects and image layers count once here, and for each project that
            uses them below.
          </p>

          <Card
            title="Blob store contents"
            actions={
              <button className="btn small" disabled={!!scanning || scan.isPending} onClick={() => scan.mutate()}>
                <RefreshCw size={13} /> {scanning ? "Scanning…" : s.scan ? "Scan again" : "Scan"}
              </button>
            }
          >
            <ErrorBox error={scan.error} />
            {!s.scan ? (
              <p className="small muted" style={{ margin: 0 }}>
                What the bucket holds, by top-level prefix: a scan lists every object (in the background; on a large bucket it takes minutes).
              </p>
            ) : (
              <div className="stack">
                <p className="small muted" style={{ margin: 0 }}>
                  {scanning ? `Started ${timeAgo(s.scan.started_at)}…` : `Scanned ${timeAgo(s.scan.finished_at!)}${s.scan.complete ? "" : " (stopped at its limits: partial)"}.`}
                  {s.scan.error && <span className="error-text"> {s.scan.error}</span>}
                </p>
                {s.scan.prefixes.length > 0 && (
                  <table className="table small">
                    <thead>
                      <tr>
                        <th>Prefix</th>
                        <th className="num">Objects</th>
                        <th className="num">Size</th>
                      </tr>
                    </thead>
                    <tbody>
                      {s.scan.prefixes.map((p) => (
                        <tr key={p.prefix}>
                          <td className="mono">{p.prefix || "(root)"}/</td>
                          <td className="num">{p.objects.toLocaleString()}</td>
                          <td className="num">{bytes(p.bytes)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                )}
                {!scanning && !s.scan.error && <Orphans scan={s.scan} />}
              </div>
            )}
          </Card>

          <Card title="Largest projects">
            <div className="table-scroll">
            <table className="table small">
              <thead>
                <tr>
                  <th>Project</th>
                  <th className="num">Repository</th>
                  <th className="num">LFS</th>
                  <th className="num">Artifacts</th>
                  <th className="num">Registry</th>
                  <th className="num">Uploads</th>
                  <th className="num">Total</th>
                  <th className="num">Quota</th>
                </tr>
              </thead>
              <tbody>
                {s.projects.map((p) => (
                  <tr key={p.project_id}>
                    <td>
                      <Link to={`/${p.full_path}/-/settings`}>{p.full_path}</Link>
                    </td>
                    <td className="num">{bytes(p.repository)}</td>
                    <td className="num">{bytes(p.lfs)}</td>
                    <td className="num">{bytes(p.artifacts)}</td>
                    <td className="num">{bytes(p.registry)}</td>
                    <td className="num">{bytes(p.uploads)}</td>
                    <td className="num strong">{bytes(p.total)}</td>
                    <td className="num">
                      <QuotaCell label={`Quota of ${p.full_path}`} value={p.quota ?? null} used={p.total} path={`/admin/projects/${p.project_id}/quota`} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            </div>
          </Card>

          <Card title="Storage quotas">
            <DefaultQuota value={s.default_quota ?? null} />
          </Card>

          <Card title="Largest groups and users">
            <table className="table small">
              <thead>
                <tr>
                  <th>Namespace</th>
                  <th className="num">Projects</th>
                  <th className="num">Total</th>
                  <th className="num">Quota</th>
                </tr>
              </thead>
              <tbody>
                {s.namespaces.map((n) => (
                  <tr key={n.namespace_id}>
                    <td>
                      <Link to={`/${n.full_path}`}>{n.full_path}</Link> <span className="faint">{n.kind === "group" ? "group" : "user"}</span>
                    </td>
                    <td className="num">{n.projects}</td>
                    <td className="num strong">{bytes(n.total)}</td>
                    <td className="num">
                      <QuotaCell
                        label={`Quota of ${n.full_path}`}
                        value={n.own_quota ?? null}
                        effective={n.quota ?? null}
                        used={n.total}
                        path={`/admin/namespaces/${n.namespace_id}/quota`}
                        withDefault
                      />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </Card>
        </div>
      )}
    </Q>
  );
}

export function AdminJobs() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["admin-jobs"], queryFn: () => api.get<S["JobsOverview"]>("/admin/jobs"), refetchInterval: 10_000 });
  const tasks = useQuery({ queryKey: ["admin-tasks"], queryFn: () => api.get<S["Runnable"][]>("/admin/tasks") });
  const act = useAct(() => qc.invalidateQueries({ queryKey: ["admin-jobs"] }), { always: true });
  return (
    <Q q={q}>
      {(j) => (
        <div className="stack" style={{ gap: 20 }}>
          <Card title="Background jobs by task">
            {j.queues.length === 0 ? (
              <p className="small muted" style={{ margin: 0 }}>
                Nothing queued.
              </p>
            ) : (
              <table className="table small">
                <thead>
                  <tr>
                    <th>Task</th>
                    <th className="num">Queued</th>
                    <th className="num">Running</th>
                    <th className="num">Retrying</th>
                    <th className="num">Failed</th>
                  </tr>
                </thead>
                <tbody>
                  {j.queues.map((t) => (
                    <tr key={t.task}>
                      <td className="mono">{t.task}</td>
                      <td className="num">{t.queued}</td>
                      <td className="num">{t.running}</td>
                      <td className="num">{t.retrying}</td>
                      <td className={`num ${t.failed ? "error-text strong" : ""}`}>{t.failed}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
            <p className="small muted" style={{ margin: "8px 0 0" }}>
              Scheduled tasks (every minute: CI housekeeping; every 5 minutes: cleanup; hourly: repository maintenance) come back once done.
            </p>
          </Card>
          <Card title="Run now">
            <Q q={tasks}>
              {(list) => (
                <div className="list">
                  {list.map((t) => (
                    <div className="item small" key={t.task}>
                      <code>{t.task}</code>
                      <span className="grow muted">{t.description}</span>
                      <button className="btn small" onClick={() => act(() => api.post(`/admin/tasks/${t.task}`), `${t.task} queued`)}>
                        Run now
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </Q>
          </Card>
          <Card title="Failed jobs">
            {j.failed.length === 0 ? (
              <p className="small muted" style={{ margin: 0 }}>
                None: every job ran or will try again.
              </p>
            ) : (
              <div className="list">
                {j.failed.map((f) => (
                  <div className="item small" key={f.id}>
                    <XCircle size={14} className="ic-danger" />
                    <code>{f.task}</code>
                    <span className="grow ellipsis" title={f.error}>
                      {f.error || "no error recorded"}
                    </span>
                    <span className="faint nowrap">
                      {f.attempts} attempt{f.attempts === 1 ? "" : "s"} · {timeAgo(f.updated_at)}
                    </span>
                    <button className="btn small" onClick={() => act(() => api.post(`/admin/jobs/${f.id}/retry`), `${f.task} queued again`)}>
                      Retry
                    </button>
                    <button
                      className="btn small danger"
                      onClick={() => window.confirm(`Drop this ${f.task} job? Whatever it was doing will not happen.`) && act(() => api.del(`/admin/jobs/${f.id}`), "Job dropped")}
                    >
                      Drop
                    </button>
                  </div>
                ))}
              </div>
            )}
          </Card>
        </div>
      )}
    </Q>
  );
}

export function AdminHealth() {
  const q = useQuery({ queryKey: ["admin-health"], queryFn: () => api.get<S["Health"]>("/admin/health") });
  return (
    <Q q={q}>
      {(h) => (
        <div className="stack">
          <div className="list">
            {h.checks.map((c) => (
              <div className="item" key={c.name}>
                {c.status === "ok" ? <CheckCircle2 size={16} className="ic-ok" aria-label="OK" /> : c.status === "warn" ? <AlertTriangle size={16} className="ic-warn" aria-label="Warning" /> : <XCircle size={16} className="ic-danger" aria-label="Failing" />}
                <strong style={{ width: 140 }}>{c.name}</strong>
                <span className="grow small">{c.detail}</span>
                {c.millis != null && <span className="small faint mono">{c.millis} ms</span>}
              </div>
            ))}
          </div>
          <div>
            <button className="btn small" onClick={() => q.refetch()} disabled={q.isFetching}>
              <RefreshCw size={13} /> Check again
            </button>
          </div>
          <TestEmail />
        </div>
      )}
    </Q>
  );
}


/** Send a test email now: the answer says whether the server accepted it. */
function TestEmail() {
  const toast = useToast();
  const [to, setTo] = useState("");
  const send = useMutation({
    mutationFn: () => api.post<{ sent: string }>("/admin/health/email", { to: to || null }),
    onSuccess: (r) => toast.ok(`Sent to ${r.sent}: check the inbox`),
  });
  return (
    <Card title="Test email">
      <form
        className="row wrap"
        style={{ gap: 8 }}
        onSubmit={(e) => {
          e.preventDefault();
          send.mutate();
        }}
      >
        <input type="email" placeholder="Your own address when empty" value={to} onChange={(e) => setTo(e.target.value)} aria-label="Send to" style={{ maxWidth: 320 }} />
        <button className="btn" disabled={send.isPending}>
          Send a test email
        </button>
      </form>
      <ErrorBox error={send.error} />
    </Card>
  );
}

/** The banner every page shows (Admin > Settings). */
export function BannerForm() {
  const qc = useQueryClient();
  const toast = useToast();
  const q = useQuery({ queryKey: ["admin-banner"], queryFn: () => api.get<S["Banner"]>("/admin/banner") });
  return <Q q={q}>{(b) => <BannerEdit initial={b} onSaved={() => (qc.invalidateQueries({ queryKey: ["banner"] }), toast.ok("Banner saved"))} />}</Q>;
}

function BannerEdit({ initial, onSaved }: { initial: S["Banner"]; onSaved: () => void }) {
  const [text, setText] = useState(initial.text);
  const [tone, setTone] = useState(initial.tone);
  const local = (d?: string | null) => (d ? new Date(new Date(d).getTime() - new Date().getTimezoneOffset() * 60000).toISOString().slice(0, 16) : "");
  const [until, setUntil] = useState(local(initial.until));
  const save = useMutation({
    mutationFn: (b: S["Banner"]) => api.put("/admin/banner", b),
    onSuccess: onSaved,
  });
  return (
    <form
      className="stack"
      style={{ maxWidth: 640 }}
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate({ text, tone, until: until ? new Date(until).toISOString() : null });
      }}
    >
      <ErrorBox error={save.error} />
      <Field label="Message" hint="One line of plain text, on every page (signed out too). Empty: no banner.">
        <input value={text} maxLength={500} onChange={(e) => setText(e.target.value)} placeholder="e.g. Maintenance on Saturday 10:00–11:00 UTC" />
      </Field>
      <div className="row wrap" style={{ gap: 12 }}>
        <Field label="Tone">
          <select value={tone} onChange={(e) => setTone(e.target.value)}>
            <option value="info">Information</option>
            <option value="warn">Warning</option>
          </select>
        </Field>
        <Field label="Until" hint="Empty: until you remove it.">
          <input type="datetime-local" value={until} onChange={(e) => setUntil(e.target.value)} />
        </Field>
      </div>
      <div className="row" style={{ gap: 8 }}>
        <button className="btn primary" disabled={save.isPending}>
          Save
        </button>
        {initial.text && (
          <button type="button" className="btn" onClick={() => (setText(""), save.mutate({ text: "", tone, until: null }))}>
            Remove the banner
          </button>
        )}
      </div>
    </form>
  );
}

const VIS: Record<number, string> = { 0: "private", 10: "internal", 20: "public" };

/** Every project or group: what they keep, how busy they are; links to act on them. */
export function AdminProjects() {
  const toast = useToast();
  const [kind, setKind] = useState<"projects" | "groups">("projects");
  const [sort, setSort] = useState<"activity" | "size" | "name">("activity");
  const [search, setSearch] = useState("");
  const term = useDebounced(search, 250);
  const projects = useQuery({
    queryKey: ["admin-projects", term, sort],
    queryFn: () => api.get<S["AdminProject"][]>(`/admin/projects${qs({ search: term, sort, per_page: 100 })}`),
    enabled: kind === "projects",
  });
  const groups = useQuery({
    queryKey: ["admin-groups", term],
    queryFn: () => api.get<S["AdminGroup"][]>(`/admin/groups${qs({ search: term, per_page: 100 })}`),
    enabled: kind === "groups",
  });
  const repack = async (p: S["AdminProject"]) => {
    try {
      await api.post(`/admin/projects/${p.id}/maintenance`);
      toast.ok(`${p.full_path}: repack queued`);
    } catch (e) {
      toast.error(e);
    }
  };
  return (
    <div className="stack">
      <div className="list-toolbar">
        <Segmented
          label="Show"
          value={kind}
          onChange={setKind}
          options={[
            { value: "projects", label: "Projects" },
            { value: "groups", label: "Groups" },
          ]}
        />
        {kind === "projects" && (
          <Segmented
            label="Sort by"
            value={sort}
            onChange={setSort}
            options={[
              { value: "activity", label: "Recent activity" },
              { value: "size", label: "Size" },
              { value: "name", label: "Name" },
            ]}
          />
        )}
        <SearchField value={search} onChange={setSearch} placeholder="Path or name" />
      </div>
      {kind === "projects" ? (
        <Q q={projects}>
          {(list) => (
            <table className="table small">
              <thead>
                <tr>
                  <th>Project</th>
                  <th>Visibility</th>
                  <th className="num">Size</th>
                  <th className="num hide-narrow">Open MRs</th>
                  <th className="num hide-narrow">Pipelines (30 d)</th>
                  <th className="hide-narrow">Last activity</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {list.map((p) => (
                  <tr key={p.id}>
                    <td>
                      <Link to={`/${p.full_path}`}>{p.full_path}</Link>
                    </td>
                    <td className="muted">{VIS[p.visibility] ?? p.visibility}</td>
                    <td className="num" title={`repository ${bytes(p.repository)}`}>
                      {bytes(p.total)}
                    </td>
                    <td className="num hide-narrow">{p.open_merge_requests}</td>
                    <td className="num hide-narrow">{p.pipelines_30_days}</td>
                    <td className="muted nowrap hide-narrow" title={new Date(p.last_activity_at).toLocaleString()}>
                      {timeAgo(p.last_activity_at)}
                    </td>
                    <td className="nowrap" style={{ textAlign: "right" }}>
                      <button className="btn small" onClick={() => repack(p)} title="Full git maintenance now (queued after its pushes)">
                        Repack
                      </button>{" "}
                      <Link className="btn small" to={`/${p.full_path}/-/settings`} title="Transfer, rename or delete it in its settings">
                        Settings
                      </Link>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Q>
      ) : (
        <Q q={groups}>
          {(list) => (
            <table className="table small">
              <thead>
                <tr>
                  <th>Group</th>
                  <th>Visibility</th>
                  <th className="num">Projects</th>
                  <th className="num">Subgroups</th>
                  <th className="num">Members</th>
                  <th className="hide-narrow">Created</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {list.map((g) => (
                  <tr key={g.id}>
                    <td>
                      <Link to={`/${g.full_path}`}>{g.full_path}</Link>
                    </td>
                    <td className="muted">{VIS[g.visibility] ?? g.visibility}</td>
                    <td className="num">{g.projects}</td>
                    <td className="num">{g.subgroups}</td>
                    <td className="num">{g.members}</td>
                    <td className="muted nowrap hide-narrow">{timeAgo(g.created_at)}</td>
                    <td style={{ textAlign: "right" }}>
                      <Link className="btn small" to={`/${g.full_path}/-/settings`}>
                        Settings
                      </Link>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Q>
      )}
    </div>
  );
}

/** Backups of the database and the repositories, made in the background. */
export function AdminBackups() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["admin-backups"], queryFn: () => api.get<S["Backups"]>("/admin/backups"), refetchInterval: (r) => (r.state.data?.running ? 3000 : false) });
  const act = useAct(() => qc.invalidateQueries({ queryKey: ["admin-backups"] }), { always: true });
  return (
    <Q q={q}>
      {(b) => (
        <div className="stack">
          <div className="banner" role="note">
            <AlertTriangle size={16} />
            <span className="grow small">
              A backup holds the <strong>database and every repository</strong>, not the blob store (LFS, artifacts, registry, uploads): back that up with its own tools (S3
              versioning or replication, or a copy of the blobs directory). Restore with <code>forgust restore --input &lt;dir&gt;</code>. Copy backups off this server.
            </span>
          </div>
          <div className="row" style={{ gap: 8 }}>
            <button className="btn primary" disabled={b.running} onClick={() => act(() => api.post("/admin/backups"), "Backup started")}>
              {b.running ? "Backing up…" : "Back up now"}
            </button>
            <span className="small muted">
              Into <code>{b.location}</code> (on the server)
            </span>
          </div>
          {b.backups.length === 0 ? (
            <p className="muted small">No backup made from here yet.</p>
          ) : (
            <div className="list">
              {b.backups.map((x) => (
                <div className="item small" key={x.name}>
                  {x.complete ? <CheckCircle2 size={14} className="ic-ok" aria-label="Complete" /> : <AlertTriangle size={14} className="ic-warn" aria-label="Incomplete" />}
                  <code className="grow">{x.name}</code>
                  <span className="muted">{x.complete ? `${x.repositories} repositories` : b.running ? "in progress" : "incomplete (failed?)"}</span>
                  <span className="mono">{bytes(x.bytes)}</span>
                  {x.created_at && <span className="faint nowrap">{timeAgo(x.created_at)}</span>}
                  <button className="btn small danger" onClick={() => window.confirm(`Delete ${x.name}?`) && act(() => api.del(`/admin/backups/${x.name}`), "Backup deleted")}>
                    Delete
                  </button>
                </div>
              ))}
            </div>
          )}
          <p className="small muted" style={{ margin: 0 }}>
            A failed backup shows its error in <Link to="/admin/jobs">Jobs</Link>.
          </p>
        </div>
      )}
    </Q>
  );
}

/** The scan's dry run: objects nothing references, by family. */
function Orphans({ scan }: { scan: S["BlobScan"] }) {
  const counted = scan.orphans.filter((o) => o.family !== "unrecognized");
  const total = counted.reduce((n, o) => n + o.bytes, 0);
  return (
    <div className="stack" style={{ gap: 8 }}>
      <h4 style={{ margin: 0 }}>Not referenced</h4>
      <p className="small muted" style={{ margin: 0 }}>
        {counted.length === 0
          ? "Every object older than a day belongs to something."
          : `${bytes(total)} in objects older than a day that nothing refers to${scan.complete ? "" : " (in the part scanned)"}. A report only: nothing is deleted.`}
      </p>
      {scan.orphans.length > 0 && (
        <table className="table small">
          <thead>
            <tr>
              <th>What</th>
              <th>For example</th>
              <th className="num">Objects</th>
              <th className="num">Size</th>
            </tr>
          </thead>
          <tbody>
            {scan.orphans.map((o) => (
              <tr key={o.family} className={o.family === "unrecognized" ? "muted" : undefined}>
                <td>{ORPHANS[o.family] ?? o.family}</td>
                <td className="mono small" style={{ wordBreak: "break-all" }}>
                  {o.sample[0]}
                  {o.objects > 1 && <span className="muted"> …</span>}
                </td>
                <td className="num">{o.objects.toLocaleString()}</td>
                <td className="num">{bytes(o.bytes)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

const GB = 1024 ** 3;

/** Bytes typed as GB ("1.5"), or null for an empty field. */
function parseGb(v: string): number | null {
  const n = Number(v.replace(",", "."));
  return v.trim() === "" || !isFinite(n) || n < 0 ? null : Math.round(n * GB);
}

/** The instance's default quota for top-level groups and personal namespaces. */
function DefaultQuota({ value }: { value: number | null }) {
  const qc = useQueryClient();
  const toast = useToast();
  const [v, setV] = useState(value ? String(+(value / GB).toFixed(2)) : "");
  const save = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api.put("/admin/storage/quota", { bytes: parseGb(v) });
      toast.ok("Saved");
      qc.invalidateQueries({ queryKey: ["admin-storage"] });
    } catch (err) {
      toast.error(err);
    }
  };
  return (
    <form className="stack" onSubmit={save}>
      <p className="small muted" style={{ margin: 0 }}>
        Each top-level group and personal namespace gets this quota for all its projects, unless it has its own (below). A project can also have its own. Once a quota is full, new
        pushes, LFS files, CI artifacts, images and uploads are refused; Owners get an email at 80 % and 100 %.
      </p>
      <div className="row" style={{ gap: 8, alignItems: "flex-end" }}>
        <Field label="Default quota (GB)">
          <input inputMode="decimal" value={v} onChange={(e) => setV(e.target.value)} placeholder="No quota" style={{ maxWidth: 160 }} />
        </Field>
        <button className="btn">Save</button>
      </div>
    </form>
  );
}

/** A quota in a table, edited in place. `value`: its own (null: none, or the default with `withDefault`). */
function QuotaCell({
  label,
  value,
  effective,
  used,
  path,
  withDefault,
}: {
  label: string;
  value: number | null;
  effective?: number | null;
  used: number;
  path: string;
  withDefault?: boolean;
}) {
  const qc = useQueryClient();
  const toast = useToast();
  const [editing, setEditing] = useState(false);
  const [v, setV] = useState("");
  const shown = withDefault ? (effective ?? null) : value;
  const set = async (bytes: number | null) => {
    try {
      await api.put(path, { bytes });
      toast.ok("Saved");
      setEditing(false);
      qc.invalidateQueries({ queryKey: ["admin-storage"] });
    } catch (err) {
      toast.error(err);
    }
  };
  if (editing) {
    return (
      <form
        className="row"
        style={{ gap: 4, justifyContent: "flex-end" }}
        onSubmit={(e) => {
          e.preventDefault();
          set(parseGb(v) ?? 0);
        }}
      >
        <input aria-label={`${label} (GB)`} inputMode="decimal" autoFocus value={v} onChange={(e) => setV(e.target.value)} placeholder="GB" style={{ width: 70 }} />
        <button className="btn small">Save</button>
        {withDefault && (
          <button type="button" className="btn small" onClick={() => set(null)}>
            Default
          </button>
        )}
        <button type="button" className="btn small ghost" onClick={() => setEditing(false)}>
          Cancel
        </button>
      </form>
    );
  }
  const pct = shown ? Math.round((used / shown) * 100) : 0;
  return (
    <button
      className="btn small ghost"
      title={withDefault && value === null ? "The instance's default" : undefined}
      aria-label={`Edit ${label}`}
      onClick={() => {
        setV(value ? String(+(value / GB).toFixed(2)) : "");
        setEditing(true);
      }}
    >
      {shown ? (
        <span className={pct >= 100 ? "error-text" : pct >= 80 ? "warn-text" : undefined}>
          {bytes(shown)} · {pct} %{withDefault && value === null ? " (default)" : ""}
        </span>
      ) : (
        <span className="faint">{withDefault && value === 0 ? "none" : "—"}</span>
      )}
    </button>
  );
}
