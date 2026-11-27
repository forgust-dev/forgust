import { useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { useWindowVirtualizer } from "@tanstack/react-virtual";
import { api, bytes, qs, refLink, timeAgo, type S, filesLink } from "../../api/client";
import { Avatar, Badge, CopyField, GitPerson, Q, useCopy } from "../../components/ui";
import { Popover, Segmented, Select } from "../../components/controls";
import { type Ctx } from "./Project";
import { BookOpen, Box, Boxes, Check, ChevronDown, Code2, Copy, CornerLeftUp, Download, FileCode, FileText, Folder, GitBranch, History, Link2, Search, Tag, WrapText, FilePlus2, Pencil, Trash2 } from "lucide-react";
import { fetchers, keys, usePrefetch } from "../../prefetch";
import { openPalette, useShortcuts } from "../../shell/palette";
import { usePref } from "../../shell/theme";
import { columns, HBar, hsVar, htmlText, remeasure, useCharWidth, useHScroll, useWidth } from "../../components/hscroll";
import { Markdown, Outline } from "../../components/Markdown";
import { SignatureBadge, useSignatures } from "./Signatures";
import { EDITORS } from "./editors";
import { blockTitle, whoKnows } from "./blocks";

/** Resolve `<ref>/<path>` (refs may contain slashes) once per URL. */
export function useResolved(ctx: Ctx, refAndPath: string) {
  const q = useQuery({
    queryKey: keys.resolve(ctx.p.id, refAndPath),
    queryFn: () => fetchers.resolve(ctx.p.id, refAndPath),
    staleTime: 5_000,
  });
  const resolvedRef = q.data?.ref;
  const { setRef } = ctx;
  useEffect(() => {
    if (resolvedRef) setRef?.(resolvedRef);
  }, [resolvedRef, setRef]);
  return q;
}

export function RefPicker({ ctx, current, path, kind }: { ctx: Ctx; current: string; path: string; kind: "tree" | "blob" | "commits" }) {
  const nav = useNavigate();
  const branches = useBranches(ctx);
  const tags = useTags(ctx);
  const options = [
    ...(branches.data ?? []).map((b) => ({ value: b.name, label: b.name, group: "Branches" })),
    ...(tags.data ?? []).filter((t) => !(branches.data ?? []).some((b) => b.name === t.name)).map((t) => ({ value: t.name, label: t.name, group: "Tags" })),
  ];
  if (!options.some((o) => o.value === current)) options.unshift({ value: current, label: current, group: "Commit" });
  return (
    <Select
      style={{ width: "auto", maxWidth: 260, fontFamily: "var(--mono)", fontSize: 13, fontWeight: 600 }}
      value={current}
      label="Branch or tag"
      onChange={(v) => nav(kind === "tree" ? filesLink(ctx.web, v, ctx.p.default_branch, path) : `${ctx.web}/-/${kind}/${v}${path ? `/${path}` : ""}`)}
      options={options}
    />
  );
}

/** A project's branches and tags (the first 100), shared by every page that offers them. */
export const useBranches = (ctx: Ctx, enabled = true) =>
  // every picker shares this list; the Branches page asks for its details on its own key
  useQuery({ queryKey: ["branches", ctx.p.id, ""], queryFn: () => api.get<S["BranchDto"][]>(`${ctx.base}/branches${qs({ per_page: 100 })}`), enabled });
export const useTags = (ctx: Ctx, enabled = true) => useQuery({ queryKey: ["tags", ctx.p.id], queryFn: () => api.get<S["TagDto"][]>(`${ctx.base}/tags${qs({ per_page: 100 })}`), enabled });
const atLeast = (n: number) => (n >= 100 ? "100+" : String(n));

/** "4 branches · 1 tag" next to the ref picker. */
function RefCounts({ ctx }: { ctx: Ctx }) {
  const branches = useBranches(ctx);
  const tags = useTags(ctx);
  const nb = branches.data?.length ?? 0;
  const nt = tags.data?.length ?? 0;
  return (
    <span className="ref-counts">
      <Link to={`${ctx.web}/-/branches`}>
        <GitBranch size={14} /> <strong>{atLeast(nb)}</strong> {nb === 1 ? "branch" : "branches"}
      </Link>
      <Link to={`${ctx.web}/-/tags`}>
        <Tag size={14} /> <strong>{atLeast(nt)}</strong> {nt === 1 ? "tag" : "tags"}
      </Link>
    </span>
  );
}

/** Clone URLs (HTTPS or SSH), editors that clone them, and archive downloads, in one menu. */
function CloneMenu({ ctx, gitRef }: { ctx: Ctx; gitRef: string }) {
  const [proto, setProto] = useState<"https" | "ssh">("https");
  const url = proto === "https" ? ctx.p.http_url : ctx.p.ssh_url;
  return (
    <Popover
      label="Clone"
      trigger={
        <button className="btn primary">
          <Code2 size={14} /> Clone <ChevronDown size={14} />
        </button>
      }
    >
      <div className="clone-menu">
        <div className="row" style={{ justifyContent: "space-between" }}>
          <strong>Clone</strong>
          <Segmented
            label="Protocol"
            value={proto}
            onChange={setProto}
            options={[
              { value: "https", label: "HTTPS" },
              { value: "ssh", label: "SSH" },
            ]}
          />
        </div>
        <CopyField value={url} />
        <div className="clone-row clone-editors">
          <span className="muted small">Open in</span>
          {EDITORS.map((e) => (
            <a key={e.name} className="btn small" href={e.link(url)}>
              <svg viewBox="0 0 24 24" width={13} height={13} fill="currentColor" aria-hidden="true">
                <path d={e.logo} />
              </svg>
              {e.name}
            </a>
          ))}
        </div>
        <div className="clone-row">
          <span className="muted small">Download {gitRef}</span>
          <a className="btn small" href={`/api/v1${ctx.base}/archive${qs({ ref: gitRef, format: "zip" })}`}>
            <Download size={13} /> zip
          </a>
          <a className="btn small" href={`/api/v1${ctx.base}/archive${qs({ ref: gitRef, format: "tar.gz" })}`}>
            <Download size={13} /> tar.gz
          </a>
        </div>
      </div>
    </Popover>
  );
}

/** The Smart View's block a file or folder is in, and who to ask about it: to its details. */
function BlockChip({ ctx, path }: { ctx: Ctx; path: string }) {
  const q = useQuery({
    // by the default branch's map, whatever the ref browsed
    queryKey: ["path-block", ctx.p.id, path],
    queryFn: () => api.get<S["PathBlock"]>(`${ctx.base}/map/block${qs({ path })}`),
    staleTime: 60_000,
  });
  const b = q.data?.block;
  if (!b) return null;
  const who = whoKnows(b, 2);
  const name = blockTitle(b);
  return (
    <Link
      className="block-chip"
      to={`${ctx.web}/-/smart_view${qs({ block: b.id })}`}
      title={`In the block ${name}${b.description ? `: ${b.description}` : ""}${who.length ? `. Ask ${who.join(" or ")}` : ""}`}
    >
      <Boxes size={13} aria-hidden />
      <span className="block-chip-name">{name}</span>
      {who.length > 0 && <span className="block-chip-who">{who.join(", ")}</span>}
    </Link>
  );
}

export function PathCrumbs({ ctx, gitRef, path }: { ctx: Ctx; gitRef: string; path: string }) {
  const parts = path ? path.split("/") : [];
  return (
    <div className="crumbs" style={{ margin: 0 }}>
      <Link to={filesLink(ctx.web, gitRef, ctx.p.default_branch)}>
        <strong>{ctx.p.path}</strong>
      </Link>
      {parts.map((part, i) => (
        <span key={i} className="row" style={{ gap: 4 }}>
          <span className="sep">/</span>
          {i === parts.length - 1 ? <strong>{part}</strong> : <Link to={filesLink(ctx.web, gitRef, ctx.p.default_branch, parts.slice(0, i + 1).join("/"))}>{part}</Link>}
        </span>
      ))}
    </div>
  );
}

const ICONS: Record<string, ReactElement> = {
  tree: <Folder size={16} className="ic-folder" />,
  blob: <FileText size={16} />,
  executable: <FileCode size={16} />,
  symlink: <Link2 size={16} />,
  submodule: <Box size={16} />,
};

function TreeLink({ ctx, e, gitRef, selected }: { ctx: Ctx; e: S["TreeEntry"]; gitRef: string; selected: boolean }) {
  const kind = e.kind === "tree" ? "tree" : "blob";
  const pf = usePrefetch({ kind, pid: ctx.p.id, refAndPath: `${gitRef}/${e.path}` });
  return (
    <Link to={refLink(ctx.web, kind, gitRef, e.path)} className="tree-name ellipsis" data-selected={selected || undefined} {...pf}>
      {e.name}
    </Link>
  );
}

/** Subject and age of the last commit that touched a tree entry. */
function EntryCommit({ ctx, c, loading }: { ctx: Ctx; c?: S["CommitInfo"] | null; loading: boolean }) {
  if (!c)
    return (
      <>
        <span className="tree-msg">{loading && <span className="skeleton" style={{ width: "40%" }} />}</span>
        <span className="tree-time">{loading && <span className="skeleton" style={{ width: 56 }} />}</span>
      </>
    );
  return (
    <>
      <Link to={`${ctx.web}/-/commit/${c.id}`} className="tree-msg ellipsis" title={c.subject} tabIndex={-1}>
        {c.subject}
      </Link>
      <span className="tree-time" title={new Date(c.committer.time * 1000).toLocaleString()}>
        {timeAgo(c.committer.time)}
      </span>
    </>
  );
}

/** The last commit of a directory or file: author, subject, sha, age, and its history. */
function LastCommitBar({ ctx, c, history }: { ctx: Ctx; c: S["CommitInfo"]; history: string }) {
  const signature = useSignatures(ctx, [c.id])[c.id];
  return (
    <div className="item last-commit">
      <Avatar name={c.author.name} username={c.author.username} />
      <strong className="nowrap">
        <GitPerson p={c.author} />
      </strong>
      <Link to={`${ctx.web}/-/commit/${c.id}`} className="muted grow ellipsis" title={c.subject}>
        {c.subject}
      </Link>
      {/* on a phone the row has no room for it: the commit page shows it */}
      <SignatureBadge v={signature} className="hide-narrow" />
      <Link to={`${ctx.web}/-/commit/${c.id}`} className="sha">
        {c.id.slice(0, 8)}
      </Link>
      <span className="faint small nowrap hide-narrow">{timeAgo(c.committer.time)}</span>
      <Link className="history-link" to={history}>
        <History size={14} /> History
      </Link>
    </div>
  );
}

/** Search the text of this project's files (its default branch), in the folder shown. */
function CodeSearchBox({ ctx, path }: { ctx: Ctx; path: string }) {
  const navigate = useNavigate();
  const [text, setText] = useState("");
  return (
    <form
      className="search-field repo-code-search"
      role="search"
      onSubmit={(e) => {
        e.preventDefault();
        if (text.trim()) navigate(`/search${qs({ q: text.trim(), scope: "code", project_id: ctx.p.full_path, path: path ? `${path}/` : undefined })}`);
      }}
    >
      <Search size={14} aria-hidden />
      <input type="search" value={text} placeholder={path ? "Search this folder…" : "Search code…"} aria-label="Search this project's code" onChange={(e) => setText(e.target.value)} />
    </form>
  );
}

export function Tree({ ctx, refAndPath }: { ctx: Ctx; refAndPath: string }) {
  const r = useResolved(ctx, refAndPath);
  return <Q q={r}>{(res) => <TreeView ctx={ctx} res={res} />}</Q>;
}

function TreeView({ ctx, res }: { ctx: Ctx; res: S["RefResolved"] }) {
  const tree = useQuery({
    queryKey: keys.tree(ctx.p.id, res.commit, res.path),
    queryFn: () => fetchers.tree(ctx.p.id, res.commit, res.path),
  });
  const readme = useQuery({
    queryKey: ["readme", ctx.p.id, res.commit, res.path, res.ref],
    queryFn: () => api.get<S["ReadmeResponse"] | null>(`${ctx.base}/readme${qs({ ref: res.ref, path: res.path })}`),
  });
  // per-entry last commits come after the listing (slower on big histories);
  // an incomplete answer is asked again until the server has them all
  const lastCommits = useQuery({
    queryKey: ["last-commits", ctx.p.id, res.commit, res.path],
    queryFn: () => api.get<S["LastCommits"]>(`${ctx.base}/last_commits${qs({ ref: res.commit, path: res.path })}`),
    refetchInterval: (q) => (q.state.data && !q.state.data.complete ? 4000 : false),
    staleTime: Infinity,
  });
  const lastOf = useMemo(() => new Map((lastCommits.data?.entries ?? []).map((e) => [e.path, e.commit])), [lastCommits.data]);
  const listRef = useRef<HTMLDivElement>(null);
  const entries = tree.data?.entries ?? [];
  const virtual = entries.length > 300;
  // asked once a render, not once per row the virtualizer estimates
  const rowH = matchMedia("(pointer: coarse)").matches ? 36 : 30;
  const v = useWindowVirtualizer({ count: entries.length, estimateSize: () => rowH, overscan: 20, scrollMargin: listRef.current?.offsetTop ?? 0 });
  const navigate = useNavigate();
  const [sel, setSel] = useState(-1);
  useEffect(() => setSel(-1), [res.commit, res.path]);
  const move = (d: number) => {
    if (!entries.length) return;
    const n = Math.max(0, Math.min(entries.length - 1, sel + d));
    setSel(n);
    if (virtual) v.scrollToIndex(n, { align: "auto" });
    else listRef.current?.querySelector(`[data-row="${n}"]`)?.scrollIntoView({ block: "nearest" });
  };
  useShortcuts({
    j: () => move(1),
    k: () => move(-1),
    ArrowDown: () => (sel >= 0 ? move(1) : false),
    ArrowUp: () => (sel >= 0 ? move(-1) : false),
    Enter: () => {
      const e = entries[sel];
      if (!e || e.kind === "submodule") return false;
      navigate(`${ctx.web}/-/${e.kind === "tree" ? "tree" : "blob"}/${res.ref}/${e.path}`);
    },
  });

  const row = (e: S["TreeEntry"], i: number, style?: React.CSSProperties) => (
    <div className={`tree-row ${i === sel ? "selected" : ""}`} key={e.path} style={style} data-row={i} onMouseDown={() => setSel(i)}>
      <span className="icon">{ICONS[e.kind] ?? ICONS.blob}</span>
      {e.kind === "submodule" ? <span className="tree-name">{e.name}</span> : <TreeLink ctx={ctx} e={e} gitRef={res.ref} selected={i === sel} />}
      <EntryCommit ctx={ctx} c={lastOf.get(e.path)} loading={lastCommits.isLoading || (!lastOf.has(e.path) && !lastCommits.data?.complete)} />
    </div>
  );

  return (
    <div className="stack">
      <div className="repo-toolbar">
        <RefPicker ctx={ctx} current={res.ref} path={res.path} kind="tree" />
        {res.path ? <PathCrumbs ctx={ctx} gitRef={res.ref} path={res.path} /> : <RefCounts ctx={ctx} />}
        {res.path && <BlockChip ctx={ctx} path={res.path} />}
        <div className="spacer" />
        <button className="btn" onClick={() => openPalette("files")} title="Find a file (t)" aria-label="Go to file">
          <Search size={14} /> <span className="repo-wide-only">Go to file</span> <kbd>t</kbd>
        </button>
        <CodeSearchBox ctx={ctx} path={res.path} />
        {ctx.p.can_write_repository && (
          <Link className="btn" to={`${ctx.web}/-/new/${res.ref}${res.path ? `/${res.path}` : ""}`} title="Create a file here" aria-label="New file">
            <FilePlus2 size={14} /> <span className="hide-narrow repo-wide-only">New file</span>
          </Link>
        )}
        <CloneMenu ctx={ctx} gitRef={res.ref} />
      </div>
      <Q q={tree}>
        {(t) => (
          <div className="list file-table" ref={listRef}>
            {t.last_commit && <LastCommitBar ctx={ctx} c={t.last_commit} history={refLink(ctx.web, "commits", res.ref, res.path)} />}
            {res.path && (
              <div className="tree-row">
                <span className="icon">
                  <CornerLeftUp size={16} />
                </span>
                <Link to={filesLink(ctx.web, res.ref, ctx.p.default_branch, res.path.split("/").slice(0, -1).join("/"))}>..</Link>
              </div>
            )}
            {virtual ? (
              <div style={{ height: v.getTotalSize(), position: "relative" }}>
                {v.getVirtualItems().map((vi) =>
                  row(entries[vi.index], vi.index, { position: "absolute", top: 0, left: 0, right: 0, transform: `translateY(${vi.start - v.options.scrollMargin}px)` }),
                )}
              </div>
            ) : (
              entries.map((e, i) => row(e, i))
            )}
          </div>
        )}
      </Q>
      {readme.data && (
        <div className="card readme" id="readme">
          <div className="card-head">
            <BookOpen size={15} className="muted" />
            <strong>{readme.data.path.split("/").pop()}</strong>
            <div className="spacer" />
            <Outline html={readme.data.html} />
            {ctx.p.can_write_repository && (
              <Link className="icon-btn small-icon" to={`${ctx.web}/-/edit/${res.ref}/${readme.data.path}`} title="Edit this file" aria-label="Edit this file">
                <Pencil size={16} />
              </Link>
            )}
          </div>
          <Markdown className="card-body markdown" html={readme.data.html} />
        </div>
      )}
    </div>
  );
}

/** `#L12` or `#L12-20` → [12, 20]. */
export function parseLineHash(hash: string): [number, number] | null {
  const m = hash.match(/^#L(\d+)(?:-L?(\d+))?$/);
  if (!m) return null;
  const a = Number(m[1]);
  const b = m[2] ? Number(m[2]) : a;
  return [Math.min(a, b), Math.max(a, b)];
}

/**
 * Lines of highlighted HTML, virtualized when long. `#L12` highlights a line,
 * `#L12-20` a range (drag over line numbers, or shift-click a second one).
 */
export function CodeLines({ lines, gutter, gutterWidth = 0, wrap = false }: { lines: string[]; gutter?: (i: number) => React.ReactNode; gutterWidth?: number; wrap?: boolean }) {
  const loc = useLocation();
  const navigate = useNavigate();
  const range = parseLineHash(loc.hash);
  const target = range?.[0] ?? 0;
  const ref = useRef<HTMLDivElement>(null);
  const width = useWidth(ref);
  const ch = useCharWidth();
  const virtual = lines.length > 400;
  // 56px of line numbers, then the code with 24px of padding on the right
  const codePx = Math.max(0, width - 56 - gutterWidth);
  const perRow = Math.max(8, Math.floor((codePx - 24) / ch));
  const lens = useMemo(() => lines.map((l) => columns(htmlText(l))), [lines]);
  const v = useWindowVirtualizer({
    count: lines.length,
    estimateSize: (i) => (wrap ? 20 * Math.max(1, Math.ceil(lens[i] / perRow)) : 20),
    overscan: 40,
    scrollMargin: ref.current?.offsetTop ?? 0,
  });
  useEffect(() => remeasure(v, ref.current), [wrap, perRow]);
  const widest = useMemo(() => lens.reduce((a, b) => Math.max(a, b), 0), [lens]);
  const scope = useRef<HTMLDivElement>(null);
  const hs = useHScroll(scope, [wrap || width === 0 ? 0 : widest * ch + 24 - codePx]);
  const scrolled = useRef<string | null>(null);
  useEffect(() => {
    // scroll on arrival, not on every range tweak made by clicking
    if (!target || scrolled.current === loc.key) return;
    scrolled.current = loc.key;
    if (virtual) v.scrollToIndex(target - 1, { align: "center" });
    else document.getElementById(`L${target}`)?.scrollIntoView({ block: "center" });
  }, [target, lines.length, loc.key]);
  const select = (a: number, b: number) => {
    scrolled.current = "click";
    const hash = a === b ? `#L${a}` : `#L${Math.min(a, b)}-${Math.max(a, b)}`;
    if (hash !== loc.hash) navigate({ hash }, { replace: true, preventScrollReset: true });
  };
  const click = (e: React.MouseEvent, n: number) => {
    e.preventDefault();
    select(e.shiftKey && range ? range[0] : n, n);
  };
  // drag over line numbers to select a range
  const anchor = useRef<number | null>(null);
  useEffect(() => {
    const up = () => (anchor.current = null);
    window.addEventListener("mouseup", up);
    return () => window.removeEventListener("mouseup", up);
  }, []);
  const down = (e: React.MouseEvent, n: number) => {
    if (e.button !== 0 || e.shiftKey) return;
    e.preventDefault(); // no text selection while dragging
    anchor.current = n;
    select(n, n);
  };
  const enter = (n: number) => anchor.current != null && select(anchor.current, n);
  const line = (i: number, style?: React.CSSProperties, measure?: (el: Element | null) => void) => {
    const inRange = range && i + 1 >= range[0] && i + 1 <= range[1];
    return (
      <div key={i} id={`L${i + 1}`} data-index={i} ref={measure} className={`code-line ${inRange ? "target" : ""}`} style={style} onMouseEnter={() => enter(i + 1)}>
        {gutter?.(i)}
        <span className="ln">
          <a href={`#L${i + 1}`} onClick={(e) => click(e, i + 1)} onMouseDown={(e) => down(e, i + 1)}>
            {i + 1}
          </a>
        </span>
        <span className="src">
          <span className="hs-x" dangerouslySetInnerHTML={{ __html: lines[i] || " " }} />
        </span>
      </div>
    );
  };
  return (
    <div ref={scope}>
      <div className={`code ${wrap ? "code-wrap" : ""}`} ref={ref} data-hg={0} style={hsVar(0)}>
        {virtual ? (
          <div style={{ height: v.getTotalSize(), position: "relative" }}>
            {v.getVirtualItems().map((vi) =>
              line(vi.index, { position: "absolute", top: 0, left: 0, right: 0, transform: `translateY(${vi.start - v.options.scrollMargin}px)` }, wrap ? v.measureElement : undefined),
            )}
          </div>
        ) : (
          lines.map((_, i) => line(i))
        )}
      </div>
      <HBar ctl={hs} g={0} view={codePx} />
    </div>
  );
}

export function Blob({ ctx, refAndPath }: { ctx: Ctx; refAndPath: string }) {
  const r = useResolved(ctx, refAndPath);
  return <Q q={r}>{(res) => <BlobView ctx={ctx} res={res} />}</Q>;
}

function BlobView({ ctx, res }: { ctx: Ctx; res: S["RefResolved"] }) {
  const b = useQuery({
    queryKey: keys.blob(ctx.p.id, res.commit, res.path, res.ref),
    queryFn: () => fetchers.blob(ctx.p.id, res.ref, res.path),
  });
  const last = useQuery({
    queryKey: ["commits", ctx.p.id, res.commit, res.path, "last"],
    queryFn: () => api.get<S["CommitInfo"][]>(`${ctx.base}/commits${qs({ ref: res.commit, path: res.path, per_page: 1 })}`),
    staleTime: Infinity,
  });
  const [view, setView] = useState<"preview" | "code">("preview");
  const [wrap, setWrap] = usePref("code.wrap", false);
  const [copied, copyText] = useCopy(1500);
  const [pathCopied, copyPath] = useCopy(1500);
  const [linkCopied, copyLink] = useCopy(1500);
  const raw = refLink(ctx.web, "raw", res.ref, res.path);
  // a link that keeps showing this, whatever the branch becomes: this commit, and the lines picked
  const permalink = () => `${location.origin}${refLink(ctx.web, "blob", res.commit, res.path)}${/^#L\d/.test(location.hash) ? location.hash : ""}`;
  const copy = () => copyText(() => fetch(raw, { credentials: "same-origin" }).then((r) => r.text()));
  const lastCommit = last.data?.[0];
  return (
    <div className="stack">
      <div className="repo-toolbar">
        <RefPicker ctx={ctx} current={res.ref} path={res.path} kind="blob" />
        <PathCrumbs ctx={ctx} gitRef={res.ref} path={res.path} />
        <button className="icon-btn" onClick={() => copyPath(res.path)} title="Copy the path" aria-label="Copy the path">
          {pathCopied ? <Check size={14} /> : <Copy size={14} />}
        </button>
        <BlockChip ctx={ctx} path={res.path} />
        <div className="spacer" />
        <button className="btn hide-narrow" onClick={() => openPalette("files")} title="Find a file (t)" aria-label="Go to file">
          <Search size={14} /> <span className="repo-wide-only">Go to file</span> <kbd>t</kbd>
        </button>
      </div>
      {lastCommit && (
        <div className="list file-table">
          <LastCommitBar ctx={ctx} c={lastCommit} history={refLink(ctx.web, "commits", res.ref, res.path)} />
        </div>
      )}
      <Q q={b}>
        {(blob) => {
          const preview = !!blob.markdown_html && view === "preview";
          const text = !blob.binary && !blob.too_large && !blob.lfs && !blob.image;
          return (
            <div className="card blob">
              <div className="blob-head">
                {blob.markdown_html ? (
                  <Segmented
                    label="View"
                    value={view}
                    onChange={setView}
                    options={[
                      { value: "preview", label: "Preview" },
                      { value: "code", label: "Code" },
                    ]}
                  />
                ) : null}
                <span className="blob-meta">
                  {/* what is read, still said once the path above has scrolled away */}
                  <strong className="blob-name ellipsis" title={res.path}>
                    {blob.name}
                  </strong>
                  {blob.lines.length > 0 && (
                    <span>
                      {blob.lines.length} line{blob.lines.length === 1 ? "" : "s"}
                    </span>
                  )}
                  <span>{bytes(blob.lfs ? blob.lfs.size : blob.size)}</span>
                  {blob.language && <span className="hide-narrow">{blob.language}</span>}
                </span>
                {blob.lfs && <Badge>LFS</Badge>}
                <div className="spacer" />
                {preview && <Outline html={blob.markdown_html!} />}
                {text && !preview && (
                  <button className={`icon-btn ${wrap ? "on" : ""}`} onClick={() => setWrap(!wrap)} aria-pressed={wrap} title="Wrap long lines" aria-label="Wrap long lines">
                    <WrapText size={16} />
                  </button>
                )}
                <div className="btn-group">
                  <a className="btn small" href={raw}>
                    Raw
                  </a>
                  {!blob.binary && !blob.too_large && (
                    <Link className="btn small" to={refLink(ctx.web, "blame", res.ref, res.path)}>
                      Blame
                    </Link>
                  )}
                  <Link className="btn small hide-narrow" to={refLink(ctx.web, "commits", res.ref, res.path)}>
                    History
                  </Link>
                </div>
                {ctx.p.can_write_repository && text && (
                  <Link className="icon-btn" to={`${ctx.web}/-/edit/${res.ref}/${res.path}`} title="Edit this file" aria-label="Edit this file">
                    <Pencil size={16} />
                  </Link>
                )}
                {ctx.p.can_write_repository && (
                  <Link className="icon-btn" to={`${ctx.web}/-/delete/${res.ref}/${res.path}`} title="Delete this file" aria-label="Delete this file">
                    <Trash2 size={16} />
                  </Link>
                )}
                {text && (
                  <button className="icon-btn" onClick={copy} title="Copy the file's contents" aria-label="Copy the file's contents">
                    {copied ? <Check size={16} /> : <Copy size={16} />}
                  </button>
                )}
                <button className="icon-btn" onClick={() => copyLink(permalink())} title="Copy a permanent link: this commit, and the lines picked" aria-label="Copy a permanent link">
                  {linkCopied ? <Check size={16} /> : <Link2 size={16} />}
                </button>
                <a className="icon-btn" href={raw} download={blob.name} title="Download" aria-label="Download">
                  <Download size={16} />
                </a>
              </div>
              {preview ? (
                <Markdown className="markdown blob-body" html={blob.markdown_html!} />
              ) : blob.image && (!blob.lfs || blob.lfs.stored) ? (
                <div className="blob-body" style={{ textAlign: "center" }}>
                  <img src={raw} alt={blob.name} style={{ maxWidth: "100%" }} />
                </div>
              ) : blob.lfs ? (
                <div className="empty">
                  Stored in Git LFS ({bytes(blob.lfs.size)}). {blob.lfs.stored ? <a href={raw}>Download</a> : "The object has not been uploaded."}
                </div>
              ) : blob.too_large ? (
                <div className="empty">
                  This file is too large to display. <a href={raw}>View raw</a>.
                </div>
              ) : blob.binary ? (
                <div className="empty">
                  Binary file. <a href={raw}>Download</a>.
                </div>
              ) : (
                <CodeLines lines={blob.lines} wrap={wrap} />
              )}
            </div>
          );
        }}
      </Q>
    </div>
  );
}

export function Blame({ ctx, refAndPath }: { ctx: Ctx; refAndPath: string }) {
  const r = useResolved(ctx, refAndPath);
  return <Q q={r}>{(res) => <BlameView ctx={ctx} res={res} />}</Q>;
}

function BlameView({ ctx, res }: { ctx: Ctx; res: S["RefResolved"] }) {
  const q = useQuery({
    queryKey: ["blame", ctx.p.id, res.commit, res.path],
    queryFn: () => api.get<S["BlameResponse"]>(`${ctx.base}/blame${qs({ ref: res.commit, path: res.path })}`),
  });
  // which hunk starts at each line
  const starts = useMemo(() => {
    const m = new Map<number, S["BlameHunk"]>();
    for (const h of q.data?.hunks ?? []) m.set(h.start_line - 1, h);
    return m;
  }, [q.data]);
  return (
    <div className="stack">
      <div className="row wrap">
        <PathCrumbs ctx={ctx} gitRef={res.ref} path={res.path} />
        <div className="spacer" />
        <Link className="btn small" to={refLink(ctx.web, "blob", res.ref, res.path)}>
          Normal view
        </Link>
      </div>
      <Q q={q}>
        {(b) => (
          <div className="card" style={{ overflow: "clip" }}>
            <CodeLines
              lines={b.lines}
              gutterWidth={300}
              gutter={(i) => {
                const h = starts.get(i);
                return (
                  <span className="small ellipsis" style={{ flex: "none", width: 300, padding: "0 10px", borderRight: "1px solid var(--border)", borderTop: h ? "1px solid var(--border)" : undefined }}>
                    {h && (
                      <>
                        <Link to={`${ctx.web}/-/commit/${h.commit.id}`} className="sha">
                          {h.commit.id.slice(0, 8)}
                        </Link>{" "}
                        <span title={h.commit.subject}>{h.commit.subject}</span> <span className="faint">· {h.commit.author_name}</span>
                      </>
                    )}
                  </span>
                );
              }}
            />
          </div>
        )}
      </Q>
    </div>
  );
}
