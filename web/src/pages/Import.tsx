import { useMemo, useState, type FormEvent } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, ArrowRight, CheckCircle2, CircleDashed, CircleX, Download, ExternalLink, Loader2, Lock, Plus, RotateCcw, Square, Archive } from "lucide-react";
import { api, bytes, timeAgo, type S } from "../api/client";
import { Empty, ErrorBox, Field, FieldGroup, ListHead, Q, SearchField, Section, StateBadge } from "../components/ui";
import { Choices, Segmented, Select } from "../components/controls";
import { useSession } from "../session";
import { useAct } from "../shell/toast";
import { useConfirm } from "../shell/dialog";
import { useGroups } from "../prefetch";

type Kind = S["ImportSource"];
type Mode = "one" | "many";

const ACTIVE = ["planning", "running"];

/** The forges to import from (names, public instances, minimum versions), from the server. */
const useSources = () => useQuery({ queryKey: ["import-sources"], queryFn: () => api.get<S["SourceInfo"][]>("/imports/sources"), staleTime: Infinity });

/** A source's name, while the list loads too. */
function useSourceName() {
  const sources = useSources();
  return (k: string) => sources.data?.find((s) => s.id === k)?.name ?? k;
}

const STATUS_TEXT: Record<string, string> = {
  planning: "Dry run in progress",
  planned: "Dry run done: review, then start",
  running: "Importing",
  finished: "Finished",
  failed: "Failed",
  canceled: "Canceled",
};

const OPTION_LABELS: [Exclude<keyof S["Options"], "mirror">, string, string][] = [
  ["merge_requests", "Merge requests", "with their comments and review threads"],
  ["issues", "Issues", "with their comments"],
  ["labels", "Labels", ""],
  ["lfs", "Git LFS objects", "copied through the source's LFS API"],
  ["variables", "CI/CD variables", "Owners of the target group only"],
  ["protected_branches", "Protected branches", "Owners of the target group only"],
  ["members", "Members", "matched by email; Owners of the target group only"],
];

const ALL_OPTIONS: S["Options"] = { issues: true, merge_requests: true, labels: true, lfs: true, variables: true, protected_branches: true, members: true, mirror: false };

/** Where an http(s) address points (`https://www.github.com/a/b` → github.com). */
const parseUrl = (raw: string): { origin: string; host: string } | null => {
  const text = raw.trim();
  if (!/^https?:\/\//i.test(text)) return null;
  try {
    const u = new URL(text);
    return { origin: u.origin, host: u.host.replace(/^www\./, "") };
  } catch {
    return null;
  }
};

const hostOf = (u: string) => parseUrl(u)?.host ?? u;

/** Where the source and its token are, as the form holds them. */
type Conn = { source: Kind; url: string; token: string };

export function ImportPage() {
  const nav = useNavigate();
  const sources = useSources();
  const [source, setSource] = useState<Kind>("gitlab");
  const [url, setUrl] = useState("https://gitlab.com");
  const [token, setToken] = useState("");
  const [mode, setMode] = useState<Mode>("one");
  const [path, setPath] = useState("");
  // what the first step found: one project, or the source to list projects from
  const [found, setFound] = useState<S["SourceProject"] | null>(null);
  const [connected, setConnected] = useState<S["Connected"] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const info = sources.data?.find((s) => s.id === source);
  const conn: Conn = { source, url, token };

  const pickSource = (k: Kind) => {
    setSource(k);
    const d = sources.data?.find((s) => s.id === k)?.default_url;
    if (d) setUrl(d);
  };
  // a pasted project URL on another forge moves the address there
  const pastePath = (v: string) => {
    setPath(v);
    const at = parseUrl(v);
    if (!at || at.host === hostOf(url)) return;
    setUrl(at.origin);
    // a token typed for the other address is not sent to this one
    setToken("");
    // a public instance says which forge it runs
    const known = sources.data?.find((s) => hostOf(s.default_url) === at.host);
    if (known) setSource(known.id);
  };

  const next = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      if (mode === "one") setFound(await api.post<S["SourceProject"]>("/imports/lookup", { ...conn, path }));
      else setConnected(await api.post<S["Connected"]>("/imports/connect", conn));
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  const back = () => {
    setFound(null);
    setConnected(null);
  };
  const started = (id: number) => nav(`/import/${id}`);

  return (
    <div className="page stack list-page import-page">
      <ListHead title="Import projects" sub="Bring a repository over from another forge, with its merge requests, issues, labels and CI/CD variables." />
      {found ? (
        <ImportOne conn={conn} project={found} back={back} started={started} />
      ) : connected ? (
        <ImportMany conn={conn} connected={connected} back={back} started={started} />
      ) : (
        <form className="import-card stack" onSubmit={next}>
          <ErrorBox error={error} />
          <FieldGroup label="From">
            <Choices
              label="Source"
              value={source}
              onChange={pickSource}
              options={(sources.data ?? []).map((s) => ({
                value: s.id,
                label: s.name,
                hint: s.id === "github" ? `${s.minimum_version}+ (Enterprise)` : `${s.minimum_version}+`,
              }))}
            />
          </FieldGroup>
          <Field label="Address" hint={`${hostOf(info?.default_url ?? "")} or your own ${info?.name ?? "server"}`}>
            <input value={url} onChange={(e) => setUrl(e.target.value)} placeholder={info?.default_url} />
          </Field>
          <Field
            label="Access token"
            hint={`Optional for public projects. Needs ${info?.token_scopes ?? "read access"}.${source === "github" ? " Without one, GitHub allows 60 requests an hour and the import waits for its limit." : ""} Kept encrypted, and erased when the import ends (unless the repository is kept in sync).`}
          >
            <input type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value)} />
          </Field>
          <FieldGroup label="Import">
            <Segmented
              label="What to import"
              value={mode}
              onChange={setMode}
              options={[
                { value: "one", label: "One project" },
                { value: "many", label: "Several projects" },
              ]}
            />
          </FieldGroup>
          {mode === "one" && (
            <Field label="Project" hint="Its path, or its URL (a URL on another forge fills in the address).">
              <input value={path} onChange={(e) => pastePath(e.target.value)} placeholder={source === "gitlab" ? "group/project" : "owner/repository"} />
            </Field>
          )}
          <p className="small muted" style={{ margin: 0 }}>
            <Lock size={12} />{" "}
            {source === "gitlab"
              ? "An administrator token lets Forgust read people's emails and match them to accounts here; otherwise their names are kept as text."
              : "People are matched to accounts here by the email they show; the others keep their name as text."}
          </p>
          <div className="form-actions">
            <button className="btn primary" aria-busy={busy} disabled={busy || !url.trim() || (mode === "one" && !path.trim())}>
              {busy ? <Loader2 size={15} className="spin" /> : null} {mode === "one" ? "Find the project" : "Connect"}
            </button>
          </div>
        </form>
      )}
      <ImportHistory />
    </div>
  );
}

/** Where projects land: the user's namespace and their groups (the first
 * until one is picked), as select options. */
function useTarget(personalFirst: boolean) {
  const { user } = useSession();
  const groups = useGroups();
  const [picked, setTarget] = useState("");
  const personal = user?.namespace_id ? [{ value: String(user.namespace_id), label: `${user.username} (personal)` }] : [];
  const list = (groups.data ?? []).map((g) => ({ value: String(g.id), label: g.full_path }));
  const options = personalFirst ? [...personal, ...list] : [...list, ...personal];
  const targetId = picked || (options[0]?.value ?? "");
  return { options, targetId, setTarget, label: options.find((o) => o.value === targetId)?.label ?? "" };
}

function ConnectedBar({ conn, who, version, back }: { conn: Conn; who?: string; version?: string | null; back: () => void }) {
  const sourceName = useSourceName();
  return (
    <div className="import-connected">
      <CheckCircle2 size={16} className="ic-ok" />
      <span>
        {sourceName(conn.source)} at <strong>{hostOf(conn.url)}</strong>{" "}
        {who ? (
          <>
            as <strong>@{who}</strong>
          </>
        ) : conn.token ? (
          "with your token"
        ) : (
          "without a token: public projects only"
        )}
        {version && <span className="muted"> · {version}</span>}
      </span>
      <span className="grow" />
      <button className="link-btn small" onClick={back}>
        Change
      </button>
    </div>
  );
}

function BringAlong({ options, setOptions }: { options: S["Options"]; setOptions: (o: S["Options"]) => void }) {
  return (
    <Section title="Bring along" description="The repository always comes: branches, tags, and the commits of every merge request.">
      <div className="import-options">
        {OPTION_LABELS.map(([k, label, hint]) => (
          <label className="check" key={k}>
            <input type="checkbox" checked={options[k]} onChange={(e) => setOptions({ ...options, [k]: e.target.checked })} /> {label}
            {hint && <span className="faint small"> · {hint}</span>}
          </label>
        ))}
      </div>
      <label className="check">
        <input type="checkbox" checked={options.mirror ?? false} onChange={(e) => setOptions({ ...options, mirror: e.target.checked })} /> Keep the repository in sync
        <span className="faint small">
          {" "}
          · a pull mirror: its branches and tags keep following the source (read-only here), with the token kept for it. Issues and merge requests come over once.
        </span>
      </label>
    </Section>
  );
}

/** Start the import (its dry run), with what was chosen. */
function usePlan(conn: Conn, started: (id: number) => void) {
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const plan = async (target: string, options: S["Options"], projects: S["ImportProjectBody"][]) => {
    setBusy(true);
    setError(null);
    try {
      const d = await api.post<S["ImportDetail"]>("/imports", { ...conn, target_namespace_id: Number(target), options, projects });
      started(d.id);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  return { plan, error, busy };
}

function PlanButton({ busy, disabled, back, onClick }: { busy: boolean; disabled: boolean; back: () => void; onClick: () => void }) {
  return (
    <>
      <div className="form-actions">
        <button className="btn" onClick={back}>
          Back
        </button>
        <button className="btn primary" aria-busy={busy} disabled={busy || disabled} onClick={onClick}>
          {busy ? <Loader2 size={15} className="spin" /> : null} Plan the import
        </button>
      </div>
      <p className="small muted" style={{ margin: 0, textAlign: "right" }}>
        Nothing is created yet: a dry run lists what will come over and what will not.
      </p>
    </>
  );
}

function ProjectLine({ p }: { p: S["SourceProject"] }) {
  return (
    <div className="grow" style={{ minWidth: 0 }}>
      <div className="row-title">
        <span className="ellipsis">{p.full_path}</span>
        <span className="pill">{p.visibility}</span>
        {p.archived && (
          <StateBadge tone="neutral" icon={Archive}>
            Archived
          </StateBadge>
        )}
        {p.empty && <span className="pill">empty</span>}
      </div>
      {p.description && <div className="row-desc ellipsis">{p.description}</div>}
    </div>
  );
}

function ImportOne({ conn, project, back, started }: { conn: Conn; project: S["SourceProject"]; back: () => void; started: (id: number) => void }) {
  const { options: targets, targetId, setTarget } = useTarget(true);
  const [path, setPath] = useState(project.path);
  const [options, setOptions] = useState<S["Options"]>(ALL_OPTIONS);
  const { plan, error, busy } = usePlan(conn, started);
  return (
    <div className="stack">
      <ConnectedBar conn={conn} back={back} />
      <ErrorBox error={error} />
      <Section title="Project" description="The project at the address you gave.">
        <div className="list rows">
          <a className="item" href={project.web_url} target="_blank" rel="noreferrer">
            <ProjectLine p={project} />
            <ExternalLink size={14} className="faint" />
          </a>
        </div>
      </Section>
      <Section title="Into" description="Where it lands here, and its path. A path like sub/project creates the subgroup.">
        <div className="row wrap">
          <Select label="Namespace" value={targetId} onChange={setTarget} options={targets} style={{ maxWidth: 320 }} />
          <span className="faint">/</span>
          <input className="mono" aria-label="Path" value={path} onChange={(e) => setPath(e.target.value)} style={{ maxWidth: 260 }} />
        </div>
      </Section>
      <BringAlong options={options} setOptions={setOptions} />
      <PlanButton
        busy={busy}
        back={back}
        disabled={!targetId || !path.trim()}
        onClick={() => plan(targetId, options, [{ source_id: project.id, source_path: project.full_path, target_path: path.trim() }])}
      />
    </div>
  );
}

function ImportMany({ conn, connected, back, started }: { conn: Conn; connected: S["Connected"]; back: () => void; started: (id: number) => void }) {
  const { options: targets, targetId, setTarget, label: targetLabel } = useTarget(false);
  // "" lists every project the token reads; "?" is a name typed by hand
  const signedIn = !!connected.username;
  const [owner, setOwner] = useState(signedIn ? (connected.namespaces[0]?.path ?? "") : "?");
  // a name typed by hand is listed once it is entered, not at each keystroke
  const [typed, setTyped] = useState("");
  const [entered, setEntered] = useState("");
  const from = owner === "?" ? entered : owner;
  const listing = useQuery({
    queryKey: ["import-projects", conn.source, conn.url, from],
    queryFn: () => api.post<S["SourceProject"][]>("/imports/projects", { ...conn, owner: from }),
    enabled: owner !== "?" || from !== "",
    staleTime: 60_000,
    retry: false,
  });
  const [added, setAdded] = useState<S["SourceProject"][]>([]);
  const projects = useMemo(() => {
    const seen = new Set<string>();
    return [...(listing.data ?? []), ...added].filter((p) => !seen.has(p.id) && !!seen.add(p.id));
  }, [listing.data, added]);
  // archived projects start unpicked; a new listing picks its own
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [pickedFor, setPickedFor] = useState<unknown>(null);
  if (listing.data && pickedFor !== listing.data) {
    setPickedFor(listing.data);
    setPicked(new Set([...listing.data.filter((p) => !p.archived).map((p) => p.id), ...added.map((p) => p.id)]));
  }
  const prefix = from.replace(/^\/+|\/+$/g, "");
  const relative = (p: S["SourceProject"]) => (prefix && p.full_path.toLowerCase().startsWith(prefix.toLowerCase() + "/") ? p.full_path.slice(prefix.length + 1) : p.path);
  const [paths, setPaths] = useState<Record<string, string>>({});
  const [filter, setFilter] = useState("");
  const [adding, setAdding] = useState("");
  const [addError, setAddError] = useState<unknown>(null);
  const add = async (e: FormEvent) => {
    e.preventDefault();
    setAddError(null);
    try {
      const p = await api.post<S["SourceProject"]>("/imports/lookup", { ...conn, path: adding });
      setAdded([...added, p]);
      setPicked(new Set([...picked, p.id]));
      setAdding("");
    } catch (err) {
      setAddError(err);
    }
  };
  const [options, setOptions] = useState<S["Options"]>(ALL_OPTIONS);
  const { plan, error, busy } = usePlan(conn, started);
  const list = projects.filter((p) => p.full_path.toLowerCase().includes(filter.toLowerCase()));
  const all = list.length > 0 && list.every((p) => picked.has(p.id));
  const toggle = (id: string) => {
    const next = new Set(picked);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setPicked(next);
  };
  const owners = [
    ...connected.namespaces.map((n) => ({ value: n.path, label: n.kind === "user" ? `${n.path} (your projects)` : n.path, hint: n.name !== n.path ? n.name : undefined })),
    ...(signedIn ? [{ value: "", label: "Everything you can access" }] : []),
    { value: "?", label: "Another user or group…" },
  ];
  const groupWord = conn.source === "gitlab" ? "group" : "organization";
  return (
    <div className="stack">
      <ConnectedBar conn={conn} who={connected.username} version={connected.version} back={back} />
      <ErrorBox error={error} />
      <Section title="From" description={conn.source === "gitlab" ? "Your own projects, or a group's (with its subgroups)." : "Your own projects, or an organization's."}>
        <div className="row wrap">
          <Select label="List projects from" value={owner} onChange={setOwner} options={owners} style={{ maxWidth: 360 }} />
          {owner === "?" && (
            <input
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              onBlur={() => setEntered(typed.trim())}
              onKeyDown={(e) => {
                if (e.key === "Enter") setEntered(typed.trim());
              }}
              placeholder={conn.source === "gitlab" ? "group, group/sub or a username" : "an organization or a username"}
              aria-label={`User or ${groupWord}`}
              style={{ maxWidth: 280 }}
            />
          )}
        </div>
      </Section>
      <Section title="Into" description="The group the projects land in. Their subgroups are created under it.">
        <Select label="Target group" value={targetId} onChange={setTarget} options={targets} style={{ maxWidth: 360 }} />
      </Section>
      <Section
        title="Projects"
        description={`${picked.size} of ${projects.length} selected. Archived projects are left out unless you pick them. Any public project can be added by its path or URL.`}
      >
        <div className="stack">
        <ErrorBox error={listing.error} />
        <form className="inline-form" onSubmit={add}>
          <input
            className="grow"
            style={{ minWidth: 220 }}
            value={adding}
            onChange={(e) => setAdding(e.target.value)}
            placeholder={conn.source === "gitlab" ? "group/project or its URL" : "owner/repository or its URL"}
            aria-label="Add a project by path or URL"
          />
          <button className="btn" disabled={!adding.trim()}>
            <Plus size={14} /> Add
          </button>
        </form>
        <ErrorBox error={addError} />
        {listing.isFetching ? (
          <Empty>
            <Loader2 size={15} className="spin" /> Listing the projects…
          </Empty>
        ) : projects.length === 0 ? (
          <Empty>{from || owner === "" ? `No project to import${from ? ` under ${from}` : ""}.` : `Name a user or ${groupWord} to list their public projects.`}</Empty>
        ) : (
          <>
            <div className="row wrap">
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
                  <input type="checkbox" checked={picked.has(p.id)} onChange={() => toggle(p.id)} />
                  <ProjectLine p={p} />
                  {picked.has(p.id) && (
                    <span className="import-target" onClick={(e) => e.preventDefault()}>
                      <ArrowRight size={13} className="faint" />
                      <span className="faint mono small">{targetLabel}/</span>
                      <input className="mono" aria-label={`Path for ${p.full_path}`} value={paths[p.id] ?? relative(p)} onChange={(e) => setPaths({ ...paths, [p.id]: e.target.value })} />
                    </span>
                  )}
                </label>
              ))}
            </div>
          </>
        )}
        </div>
      </Section>
      <BringAlong options={options} setOptions={setOptions} />
      <PlanButton
        busy={busy}
        back={back}
        disabled={picked.size === 0 || !targetId}
        onClick={() =>
          plan(
            targetId,
            options,
            projects.filter((p) => picked.has(p.id)).map((p) => ({ source_id: p.id, source_path: p.full_path, target_path: paths[p.id] ?? relative(p) })),
          )
        }
      />
    </div>
  );
}

function ImportHistory() {
  const sourceName = useSourceName();
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
                {sourceName(i.source)} · {hostOf(i.base_url)}
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
  return <Loader2 size={size} className="spin ic-info" aria-label={status} />;
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
  const sourceName = useSourceName();
  const { id } = useParams();
  const qc = useQueryClient();
  const confirm = useConfirm();
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
                title={`Import from ${sourceName(d.source)}`}
                sub={
                  <span className="row wrap" style={{ gap: 6 }}>
                    {hostOf(d.base_url)} <ArrowRight size={13} /> <Link to={`/${d.target_path}`}>{d.target_path}</Link> · started {timeAgo(d.created_at)}
                  </span>
                }
              >
                {(planned || ACTIVE.includes(d.status)) && (
                  <button
                    className="btn"
                    onClick={async () => {
                      const ok = await confirm({ title: "Stop this import?", body: "Projects not imported yet are skipped.", action: "Stop import", danger: true });
                      if (ok) act("cancel", {}, "Import canceled");
                    }}
                  >
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
                    Authors are matched to accounts here by email. The others keep their name from {sourceName(d.source)}, marked “imported”.
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
