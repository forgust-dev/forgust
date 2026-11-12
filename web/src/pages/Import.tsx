import { useMemo, useState, type FormEvent } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, ArrowRight, CheckCircle2, CircleDashed, CircleX, Download, ExternalLink, Loader2, Lock, Plus, RotateCcw, Square } from "lucide-react";
import { api, bytes, timeAgo, type S } from "../api/client";
import { Empty, ErrorBox, Field, FieldGroup, ListHead, Q, SearchField } from "../components/ui";
import { Segmented, Select } from "../components/controls";
import { useSession } from "../session";
import { useAct } from "../shell/toast";
import { useGroups } from "../prefetch";

type Source = "gitlab" | "github";

const SOURCE_NAME: Record<string, string> = { gitlab: "GitLab", github: "GitHub" };
const ACTIVE = ["planning", "running"];

const STATUS_TEXT: Record<string, string> = {
  planning: "Dry run in progress",
  planned: "Dry run done: review, then start",
  running: "Importing",
  finished: "Finished",
  failed: "Failed",
  canceled: "Canceled",
};

const OPTION_LABELS: [keyof S["Options"], string, string][] = [
  ["merge_requests", "Merge requests", "with their comments and review threads"],
  ["issues", "Issues", "with their comments"],
  ["labels", "Labels", ""],
  ["lfs", "Git LFS objects", "copied through the source's LFS API"],
  ["variables", "CI/CD variables", "Owners of the target group only"],
  ["protected_branches", "Protected branches", "Owners of the target group only"],
  ["members", "Members", "matched by email; Owners of the target group only"],
];

/** A project's address (`https://gitlab.com/group/project/-/tree/main`,
 * `github.com/owner/repo`): the forge it is on, or null for a forge's own address. */
const projectAddress = (raw: string): { origin: string; host: string } | null => {
  const text = raw.trim();
  if (!text) return null;
  try {
    const u = new URL(/^[a-z]+:\/\//i.test(text) ? text : `https://${text}`);
    const path = u.pathname.split("/-/")[0];
    const segs = path.split("/").filter(Boolean);
    return segs.length >= 2 ? { origin: u.origin, host: u.host.replace(/^www\./, "") } : null;
  } catch {
    return null;
  }
};

const hostOf = (u: string) => {
  try {
    return new URL(u).host;
  } catch {
    return u;
  }
};

export function ImportPage() {
  const nav = useNavigate();
  const { user } = useSession();
  const [source, setSource] = useState<Source>("gitlab");
  const [url, setUrl] = useState("https://gitlab.com");
  const [token, setToken] = useState("");
  const [scope, setScope] = useState("");
  const [connected, setConnected] = useState<S["Connected"] | null>(null);
  // connected through one project's URL: nothing to pick
  const [single, setSingle] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const project = projectAddress(url);
  const connect = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      if (project) {
        // one project by its URL: straight to where it goes
        const found = await api.post<S["SourceProject"]>("/imports/lookup", { source, url: project.origin, token, path: url.trim() });
        setUrl(project.origin);
        setScope("");
        setSingle(true);
        setConnected({ username: "", name: "", projects: [found] });
        return;
      }
      setSingle(false);
      setConnected(await api.post<S["Connected"]>("/imports/connect", { source, url, token, scope }));
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="page stack list-page">
      <ListHead title="Import projects" sub="Bring repositories, merge requests, issues, labels and CI/CD variables over from another forge." />
      {!connected ? (
        <form className="import-card stack" onSubmit={connect}>
          <ErrorBox error={error} />
          <FieldGroup label="From">
            <Segmented
              label="Source"
              value={source}
              onChange={(v) => {
                setSource(v);
                setUrl(v === "github" ? "https://github.com" : "https://gitlab.com");
              }}
              options={[
                { value: "gitlab", label: "GitLab" },
                { value: "github", label: "GitHub" },
              ]}
            />
          </FieldGroup>
          <div className={project ? "stack" : "grid2"}>
            <Field
              label="Address"
              hint={`${source === "github" ? "github.com or GitHub Enterprise Server" : "gitlab.com or your own GitLab"}, or the URL of one project to import just that one`}
            >
              <input
                value={url}
                onChange={(e) => {
                  const v = e.target.value;
                  setUrl(v);
                  // a pasted github.com / gitlab.com link says which source it is
                  const host = projectAddress(v)?.host;
                  if (host === "github.com") setSource("github");
                  if (host === "gitlab.com") setSource("gitlab");
                }}
                placeholder={source === "github" ? "https://github.com" : "https://gitlab.example.com"}
              />
            </Field>
            {!project && (
              <Field label={source === "github" ? "Only under this organization or user (optional)" : "Only under this group (optional)"} hint={source === "github" ? "e.g. my-org" : "e.g. my-team or my-team/backend"}>
                <input value={scope} onChange={(e) => setScope(e.target.value)} placeholder="Every project you are a member of" />
              </Field>
            )}
          </div>
          <Field
            label="Access token"
            hint={`Optional for public projects. ${source === "github" ? "A personal access token that can read the repositories, their issues and pull requests (classic: repo); without one, GitHub allows 60 requests an hour and the import waits for its limit (a few hundred issues and pull requests take about an hour)." : "A personal access token with read_api and read_repository."} Kept encrypted, and erased when the import ends.`}
          >
            <input type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value)} />
          </Field>
          <p className="small muted" style={{ margin: 0 }}>
            <Lock size={12} />{" "}
            {source === "github"
              ? "People are matched to accounts here by their public email on GitHub; the others keep their name as text."
              : "An administrator token lets Forgust read people's emails and match them to accounts here; otherwise their names are kept as text."}
          </p>
          <div className="form-actions">
            <button className="btn primary" disabled={busy || !url.trim()}>
              {busy ? <Loader2 size={15} className="spin" /> : null} {project ? "Continue" : "Connect"}
            </button>
          </div>
        </form>
      ) : (
        <PickProjects
          source={source}
          url={url}
          token={token}
          scope={scope}
          connected={connected}
          single={single}
          back={() => setConnected(null)}
          started={(id) => nav(`/import/${id}`)}
          personal={user?.namespace_id ? { id: user.namespace_id, label: `${user.username} (personal)` } : null}
        />
      )}
      <ImportHistory />
    </div>
  );
}

function PickProjects({
  source,
  url,
  token,
  scope,
  connected,
  single,
  back,
  started,
  personal,
}: {
  source: Source;
  url: string;
  token: string;
  scope: string;
  connected: S["Connected"];
  single: boolean;
  back: () => void;
  started: (id: number) => void;
  personal: { id: number; label: string } | null;
}) {
  const groups = useGroups();
  const targets = [...(groups.data ?? []).map((g) => ({ id: g.id, label: g.full_path })), ...(personal ? [personal] : [])];
  const [target, setTarget] = useState("");
  const targetId = target || (targets[0] ? String(targets[0].id) : "");
  const prefix = scope.trim().replace(/^\/+|\/+$/g, "");
  const relative = (p: S["SourceProject"]) => (prefix && p.full_path.toLowerCase().startsWith(prefix.toLowerCase() + "/") ? p.full_path.slice(prefix.length + 1) : p.path);
  const [projects, setProjects] = useState<S["SourceProject"][]>(connected.projects);
  const [picked, setPicked] = useState<Set<string>>(() => new Set(connected.projects.filter((p) => !p.archived).map((p) => p.id)));
  const [adding, setAdding] = useState("");
  const [addError, setAddError] = useState<unknown>(null);
  const add = async (e: FormEvent) => {
    e.preventDefault();
    setAddError(null);
    try {
      const p = await api.post<S["SourceProject"]>("/imports/lookup", { source, url, token, path: adding });
      if (!projects.some((x) => x.id === p.id)) setProjects([...projects, p]);
      setPicked(new Set([...picked, p.id]));
      setAdding("");
    } catch (err) {
      setAddError(err);
    }
  };
  const [paths, setPaths] = useState<Record<string, string>>({});
  const [filter, setFilter] = useState("");
  const [options, setOptions] = useState<S["Options"]>({ issues: true, merge_requests: true, labels: true, lfs: true, variables: true, protected_branches: true, members: true });
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const list = projects.filter((p) => p.full_path.toLowerCase().includes(filter.toLowerCase()));
  const all = list.length > 0 && list.every((p) => picked.has(p.id));
  const toggle = (id: string) => {
    const next = new Set(picked);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setPicked(next);
  };
  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const chosen = projects.filter((p) => picked.has(p.id));
      const d = await api.post<S["ImportDetail"]>("/imports", {
        source,
        url,
        token,
        target_namespace_id: Number(targetId),
        options,
        projects: chosen.map((p) => ({ source_id: p.id, source_path: p.full_path, target_path: paths[p.id] ?? relative(p) })),
      });
      started(d.id);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  const targetLabel = targets.find((t) => String(t.id) === targetId)?.label ?? "";
  return (
    <div className="stack">
      <div className="import-connected">
        <CheckCircle2 size={16} className="ic-ok" />
        <span>
          Connected to <strong>{hostOf(url)}</strong>{" "}
          {connected.username ? (
            <>
              as <strong>@{connected.username}</strong>
            </>
          ) : token ? (
            "with your token"
          ) : (
            "without a token: public projects only"
          )}
          {scope && (
            <>
              {" "}
              · under <code>{scope}</code>
            </>
          )}
        </span>
        <span className="grow" />
        <button className="link-btn small" onClick={back}>
          Change
        </button>
      </div>
      <ErrorBox error={error} />
      <section className="settings-section">
        <div className="settings-intro">
          <h2>Into</h2>
          <p>The group the projects land in. Their subgroups are created under it.</p>
        </div>
        <div className="settings-body">
          <Select label="Target group" value={targetId} onChange={setTarget} options={targets.map((t) => ({ value: String(t.id), label: t.label }))} style={{ maxWidth: 360 }} />
        </div>
      </section>
      <section className="settings-section">
        <div className="settings-intro">
          <h2>{single ? "Project" : "Projects"}</h2>
          <p>
            {single
              ? "The project at the address you gave, and its path here."
              : `${picked.size} of ${projects.length} selected. Archived projects are left out unless you pick them. Any public project can be added by its path or URL.`}
          </p>
        </div>
        <div className="settings-body stack">
          <form className="inline-form" onSubmit={add} style={single ? { display: "none" } : undefined}>
            <input
              className="grow"
              style={{ minWidth: 220 }}
              value={adding}
              onChange={(e) => setAdding(e.target.value)}
              placeholder={source === "github" ? "owner/repo or https://github.com/owner/repo" : "group/project or its URL"}
              aria-label="Add a project by path or URL"
            />
            <button className="btn" disabled={!adding.trim()}>
              <Plus size={14} /> Add
            </button>
          </form>
          <ErrorBox error={addError} />
          {projects.length === 0 ? (
            <Empty>
              {connected.username ? `The token sees no project${scope ? ` under ${scope}` : ""}.` : "Add public projects by their path or URL, or connect with a group to list."}
            </Empty>
          ) : (
            <>
              <div className="row wrap" style={single ? { display: "none" } : undefined}>
                <SearchField value={filter} onChange={setFilter} placeholder="Filter projects" />
                <span className="grow" />
                <button
                  className="btn small"
                  onClick={() => {
                    const next = new Set(picked);
                    for (const p of list) all ? next.delete(p.id) : next.add(p.id);
                    setPicked(next);
                  }}
                >
                  {all ? "Select none" : "Select all"}
                </button>
              </div>
              <div className="list rows import-pick">
                {list.map((p) => (
                  <label className="item" key={p.id}>
                    {!single && <input type="checkbox" checked={picked.has(p.id)} onChange={() => toggle(p.id)} />}
                    <div className="grow" style={{ minWidth: 0 }}>
                      <div className="row-title">
                        <span className="ellipsis">{p.full_path}</span>
                        {p.archived && <span className="pill">archived</span>}
                        {p.empty && <span className="pill">empty</span>}
                      </div>
                      {p.description && <div className="row-desc ellipsis">{p.description}</div>}
                    </div>
                    {picked.has(p.id) && (
                      <span className="import-target" onClick={(e) => e.preventDefault()}>
                        <ArrowRight size={13} className="faint" />
                        <span className="faint mono small">{targetLabel}/</span>
                        <input
                          className="mono"
                          aria-label={`Path for ${p.full_path}`}
                          value={paths[p.id] ?? relative(p)}
                          onChange={(e) => setPaths({ ...paths, [p.id]: e.target.value })}
                        />
                      </span>
                    )}
                  </label>
                ))}
              </div>
            </>
          )}
        </div>
      </section>
      <section className="settings-section">
        <div className="settings-intro">
          <h2>Bring along</h2>
          <p>The repository always comes: branches, tags, and the commits of every merge request.</p>
        </div>
        <div className="settings-body import-options">
          {OPTION_LABELS.map(([k, label, hint]) => (
            <label className="check" key={k}>
              <input type="checkbox" checked={options[k]} onChange={(e) => setOptions({ ...options, [k]: e.target.checked })} /> {label}
              {hint && <span className="faint small"> · {hint}</span>}
            </label>
          ))}
        </div>
      </section>
      <div className="form-actions">
        <button className="btn" onClick={back}>
          Back
        </button>
        <button className="btn primary" disabled={busy || picked.size === 0 || !targetId} onClick={submit}>
          {busy ? <Loader2 size={15} className="spin" /> : null} Plan the import
        </button>
      </div>
      <p className="small muted" style={{ margin: 0, textAlign: "right" }}>
        Nothing is created yet: a dry run lists what will come over and what will not.
      </p>
    </div>
  );
}

function ImportHistory() {
  const q = useQuery({ queryKey: ["imports"], queryFn: () => api.get<S["Import"][]>("/imports") });
  if (!q.data?.length) return null;
  return (
    <section className="stack" style={{ gap: 10, marginTop: 12 }}>
      <h2 className="section-title">Previous imports</h2>
      <div className="list rows">
        {q.data.map((i) => (
          <Link className="item import-row" key={i.id} to={`/import/${i.id}`}>
            <StatusIcon status={i.status} />
            <div className="grow" style={{ minWidth: 0 }}>
              <div className="row-title">
                {SOURCE_NAME[i.source]} · {hostOf(i.base_url)}
                <ArrowRight size={13} className="faint" />
                {i.target_path}
              </div>
              <div className="row-meta">
                <span>#{i.id}</span>
                <span>{STATUS_TEXT[i.status] ?? i.status}</span>
              </div>
            </div>
            <span className="row-time">{timeAgo(i.created_at)}</span>
          </Link>
        ))}
      </div>
    </section>
  );
}

function StatusIcon({ status, size = 16 }: { status: string; size?: number }) {
  if (status === "finished" || status === "planned") return <CheckCircle2 size={size} className="ic-ok" aria-label={status} />;
  if (status === "failed") return <CircleX size={size} className="ic-danger" aria-label={status} />;
  if (status === "canceled" || status === "skipped") return <Square size={size} className="faint" aria-label={status} />;
  if (status === "pending") return <CircleDashed size={size} className="faint" aria-label={status} />;
  return <Loader2 size={size} className="spin ic-accent" aria-label={status} />;
}

const STAGES: Record<string, string> = {
  project: "Creating the project",
  repository: "Fetching the repository",
  lfs: "Git LFS objects",
  labels: "Labels",
  issues: "Issues",
  merge_requests: "Merge requests",
  variables: "CI/CD variables",
  protected_branches: "Protected branches",
  members: "Members",
  cleanup: "Finishing",
};

export function ImportDetailPage() {
  const { id } = useParams();
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["import", id],
    queryFn: () => api.get<S["ImportDetail"]>(`/imports/${id}`),
    refetchInterval: (query) => (query.state.data && ACTIVE.includes(query.state.data.status) ? 2000 : false),
  });
  const [skip, setSkip] = useState<Set<number>>(new Set());
  const [paths, setPaths] = useState<Record<string, string>>({});
  const [token, setToken] = useState("");
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["import", id] });
    qc.invalidateQueries({ queryKey: ["imports"] });
  };
  const run = useAct(refresh);
  const act = (path: string, body: object, done: string) => run(() => api.post(`/imports/${id}/${path}`, body), done);
  return (
    <div className="page stack list-page">
      <Q q={q}>
        {(d) => {
          const planned = d.status === "planned";
          const failed = d.projects.filter((p) => p.status === "failed").length;
          const unmatched = d.users.filter((u) => !u.user_id).length;
          return (
            <>
              <ListHead
                title={`Import from ${SOURCE_NAME[d.source] ?? d.source}`}
                sub={
                  <span className="row wrap" style={{ gap: 6 }}>
                    {hostOf(d.base_url)} <ArrowRight size={13} /> <Link to={`/${d.target_path}`}>{d.target_path}</Link> · started {timeAgo(d.created_at)}
                  </span>
                }
              >
                {(planned || ACTIVE.includes(d.status)) && (
                  <button className="btn" onClick={() => confirm("Stop this import? Projects not imported yet are skipped.") && act("cancel", {}, "Import canceled")}>
                    Cancel
                  </button>
                )}
                {planned && (
                  <button
                    className="btn primary"
                    disabled={d.projects.every((p) => skip.has(p.id))}
                    onClick={() => act("start", { skip: [...skip], paths }, "Import started")}
                  >
                    <Download size={15} /> Start import
                  </button>
                )}
              </ListHead>
              <div className={`import-status ${d.status}`}>
                <StatusIcon status={d.status} />
                <strong>{STATUS_TEXT[d.status] ?? d.status}</strong>
                <ImportCounts d={d} />
              </div>
              {failed > 0 && !ACTIVE.includes(d.status) && (
                <form
                  className="import-retry"
                  onSubmit={(e) => {
                    e.preventDefault();
                    act("retry", { token: token || undefined, paths }, "Retrying the failed projects");
                    setToken("");
                  }}
                >
                  <AlertTriangle size={16} className="ic-warn" />
                  <span className="grow">
                    {failed} project{failed === 1 ? "" : "s"} failed. Retrying skips what was already imported.
                  </span>
                  {!d.has_token && (
                    <input type="password" placeholder="Token again (empty: public only)" aria-label="Token" value={token} onChange={(e) => setToken(e.target.value)} />
                  )}
                  <button className="btn small">
                    <RotateCcw size={13} /> Retry
                  </button>
                </form>
              )}
              <div className="list rows import-projects">
                {d.projects.map((p) => (
                  <ImportRow
                    key={p.id}
                    p={p}
                    target={d.target_path}
                    planned={planned}
                    editable={planned || (p.status === "failed" && !p.project_id && !ACTIVE.includes(d.status))}
                    skipped={skip.has(p.id)}
                    toggle={() => {
                      const next = new Set(skip);
                      next.has(p.id) ? next.delete(p.id) : next.add(p.id);
                      setSkip(next);
                    }}
                    path={paths[p.id] ?? p.target_path}
                    setPath={(v) => setPaths({ ...paths, [p.id]: v })}
                  />
                ))}
              </div>
              {d.users.length > 0 && (
                <section className="stack" style={{ gap: 10 }}>
                  <h2 className="section-title">
                    People · {d.users.length - unmatched} matched, {unmatched} kept as names
                  </h2>
                  <p className="small muted" style={{ margin: 0 }}>
                    Authors are matched to accounts here by email. The others keep their name from {SOURCE_NAME[d.source]}, marked “imported”.
                  </p>
                  <div className="import-users">
                    {d.users.map((u) => (
                      <span key={u.source_user_id} className={`import-user ${u.user_id ? "matched" : ""}`} title={u.email ?? "no email shared"}>
                        @{u.username}
                        {u.local_username && (
                          <>
                            <ArrowRight size={11} /> @{u.local_username}
                          </>
                        )}
                      </span>
                    ))}
                  </div>
                </section>
              )}
            </>
          );
        }}
      </Q>
    </div>
  );
}

function ImportCounts({ d }: { d: S["ImportDetail"] }) {
  const counts = useMemo(() => {
    const m = new Map<string, number>();
    for (const p of d.projects) m.set(p.status, (m.get(p.status) ?? 0) + 1);
    return m;
  }, [d.projects]);
  const parts = ["finished", "running", "pending", "planned", "failed", "skipped", "canceled"].filter((s) => counts.get(s));
  return (
    <span className="muted small">
      {d.projects.length} project{d.projects.length === 1 ? "" : "s"}
      {parts.map((s) => ` · ${counts.get(s)} ${s}`)}
    </span>
  );
}

type Counts = { done: number; total: number };

function ImportRow({
  p,
  target,
  planned,
  editable,
  skipped,
  toggle,
  path,
  setPath,
}: {
  p: S["ImportProject"];
  target: string;
  planned: boolean;
  editable: boolean;
  skipped: boolean;
  toggle: () => void;
  path: string;
  setPath: (v: string) => void;
}) {
  const r = p.report as unknown as S["Report"];
  const progress = p.progress as unknown as Record<string, Counts>;
  const [open, setOpen] = useState(false);
  const warnings = r.warnings ?? [];
  const facts: [string, number | null | undefined][] = [
    ["merge requests", r.merge_requests],
    ["issues", r.issues],
    ["labels", r.labels],
    ["variables", r.variables],
    ["members", r.members],
  ];
  return (
    <div className={`item import-project ${skipped ? "skipped" : ""}`}>
      {planned ? <input type="checkbox" checked={!skipped} onChange={toggle} aria-label={`Import ${p.source_path}`} /> : <StatusIcon status={p.status} />}
      <div className="grow stack" style={{ gap: 6, minWidth: 0 }}>
        <div className="row-title wrap">
          <span className="ellipsis">{p.source_path}</span>
          <ArrowRight size={13} className="faint" />
          {editable ? (
            <span className="import-target">
              <span className="faint mono small">{target}/</span>
              <input className="mono" aria-label={`Path for ${p.source_path}`} value={path} onChange={(e) => setPath(e.target.value)} disabled={skipped} />
            </span>
          ) : p.project_id && p.status === "finished" ? (
            <Link to={`/${target}/${p.target_path}`} className="mono">
              {target}/{p.target_path}
            </Link>
          ) : (
            <span className="mono faint">
              {target}/{p.target_path}
            </span>
          )}
        </div>
        <div className="row-meta">
          {facts
            .filter(([, n]) => n != null)
            .map(([k, n]) => (
              <span key={k}>
                {n} {k}
              </span>
            ))}
          {r.repository_bytes != null && <span>{bytes(r.repository_bytes)} repository</span>}
          {r.lfs_bytes != null && <span>{bytes(r.lfs_bytes)} LFS</span>}
        </div>
        {p.status === "running" && (
          <div className="import-progress">
            <span>{STAGES[p.stage ?? ""] ?? p.stage ?? "Starting"}</span>
            {Object.entries(progress).map(([k, c]) => (
              <span key={k} className="import-bar" title={`${k}: ${c.done} of ${c.total}`}>
                <span style={{ width: `${c.total ? (100 * c.done) / c.total : 100}%` }} />
              </span>
            ))}
          </div>
        )}
        {p.error && <div className={`small ${p.status === "failed" ? "error-text" : "muted"}`}>{p.error}</div>}
        {warnings.length > 0 && (
          <div className="import-warnings">
            <button className="link-btn small" onClick={() => setOpen(!open)} aria-expanded={open}>
              <AlertTriangle size={13} className="ic-warn" /> {warnings.length} thing{warnings.length === 1 ? "" : "s"} will not come over as is
            </button>
            {open && (
              <ul>
                {warnings.map((w, i) => (
                  <li key={i}>{w}</li>
                ))}
              </ul>
            )}
          </div>
        )}
      </div>
      {p.project_id && p.status === "finished" && (
        <Link className="icon-btn" to={`/${target}/${p.target_path}`} aria-label="Open the project" title="Open the project">
          <ExternalLink size={15} />
        </Link>
      )}
    </div>
  );
}
