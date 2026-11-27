import { lazy, memo, Suspense, useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import { Eye, FileDiff, FilePlus2, FolderTree, GitBranch, Search, TriangleAlert, X } from "lucide-react";
import { ancestors, api, ApiError, bytes, nameOf, parentOf, plural, qs, refLink, type S } from "../../../api/client";
import { fetchers, keys } from "../../../prefetch";
import { Markdown } from "../../../components/Markdown";
import { ErrorBox, Q, Spinner, useDebounced } from "../../../components/ui";
import { useSession } from "../../../session";
import { useConfirm, usePrompt } from "../../../shell/dialog";
import { openPalette } from "../../../shell/palette";
import { MOD } from "../../../shell/platform";
import { usePref } from "../../../shell/theme";
import { useToast } from "../../../shell/toast";
import { useResolved } from "../Code";
import type { Ctx } from "../Project";
import { badPath, clash, outline, outlineKey, reduce, renamedAway, statusOf, type Action, type Draft, type Drafts } from "./changes";
import type { Landed } from "../commitTarget";
import { Changes } from "./Changes";
import { FileTree, StatusMark, type TreeOps } from "./FileTree";
import { loadDrafts, saveDrafts, type Desk, type Saved } from "./store";

// fetched as the page loads, alongside the first file rather than after it
const editorChunk = import("../../../components/CodeEditor");
const CodeEditor = lazy(() => editorChunk.then((m) => ({ default: m.CodeEditor })));

/** Past this, a file is not opened as text. */
const MAX_TEXT = 2 * 1024 * 1024;

const isMarkdown = (path: string) => /\.(md|markdown)$/i.test(path);

/**
 * The web IDE: the repository's files beside an editor with tabs. Changes to
 * several files gather in this browser until they are committed together.
 */
export function Ide({ ctx, refAndPath }: { ctx: Ctx; refAndPath: string }) {
  // resolved once per ref: going from file to file changes the address, not the ref
  const [first, setFirst] = useState(refAndPath);
  const r = useResolved(ctx, first);
  const gitRef = r.data?.ref;
  const within = gitRef !== undefined && (refAndPath === gitRef || refAndPath.startsWith(`${gitRef}/`));
  useEffect(() => {
    if (gitRef === undefined || within) return;
    // another ref: nothing of this one's workbench carries over
    setRound((r) => ({ n: r.n, kept: null }));
    setFirst(refAndPath);
  }, [gitRef, within, refAndPath]);
  // after a commit, or once the changes are brought onto a newer one: the workbench
  // starts again on the ref's head, with what the one before it leaves
  const [round, setRound] = useState<{ n: number; kept: Kept | null }>({ n: 0, kept: null });
  const { refetch } = r;
  const restart = useCallback(
    async (kept: Kept) => {
      await refetch();
      setRound((r) => ({ n: r.n + 1, kept }));
    },
    [refetch],
  );
  // the changes handed over are taken once: a workbench drawn again in the same round
  // (the ref failing to load a moment) reads what this browser kept since
  const taken = useCallback(() => setRound((r) => (r.kept?.saved === undefined ? r : { ...r, kept: { ...r.kept, saved: undefined } })), []);
  return (
    <Q q={r}>
      {(res) => (within ? <Session key={`${res.ref}:${round.n}`} ctx={ctx} res={res} path={refAndPath.slice(res.ref.length + 1)} kept={round.kept} restart={restart} taken={taken} refresh={refetch} /> : <Spinner />)}
    </Q>
  );
}

/**
 * What a workbench leaves to the one that starts over after it: the changes still to
 * commit (null: none, it starts on the ref's head; undefined: taken already, read what
 * this browser kept), the open files, the side panel as it was. The changes are handed
 * over in memory: without storage in this browser, nothing is lost either.
 */
type Kept = { saved?: Saved | null; tabs: string[]; side: "files" | "changes"; sideOpen: boolean };

/** What the workbench is given to start over, and to learn where the ref is now. */
type Rounds = { kept: Kept | null; restart: (keep: Kept) => Promise<void>; taken: () => void; refresh: () => Promise<unknown> };

/** Reads what this browser kept for the ref (or what the workbench before it left), then opens the workbench on it. */
function Session({ ctx, res, path, ...rounds }: { ctx: Ctx; res: S["RefResolved"]; path: string } & Rounds) {
  const userId = useSession().user?.id ?? 0;
  const desk = useMemo<Desk>(() => ({ userId, projectId: ctx.p.id, ref: res.ref }), [userId, ctx.p.id, res.ref]);
  // what the workbench before it left, as this one starts
  const [handed] = useState(rounds.kept?.saved);
  const [saved, setSaved] = useState<Saved | null | undefined>(handed);
  const { taken } = rounds;
  useEffect(() => {
    if (handed !== undefined) {
      taken();
      return;
    }
    let current = true;
    loadDrafts(desk).then((s) => current && setSaved(s));
    return () => {
      current = false;
    };
  }, [desk, handed, taken]);
  if (saved === undefined) return <Spinner />;
  return <Workbench ctx={ctx} res={res} path={path} desk={desk} saved={saved} {...rounds} />;
}

function Workbench({ ctx, res, path, desk, saved, kept, restart, refresh }: { ctx: Ctx; res: S["RefResolved"]; path: string; desk: Desk; saved: Saved | null } & Rounds) {
  const nav = useNavigate();
  const qc = useQueryClient();
  const toast = useToast();
  const confirm = useConfirm();
  const prompt = usePrompt();
  const pid = ctx.p.id;
  // the commit the changes are made on, pinned: kept work stays on the commit it was
  // started from, and a push meanwhile must not move the files under the author
  const [base] = useState(saved?.base ?? res.commit);
  const [initial] = useState<Drafts>(saved?.drafts ?? {});
  const [drafts, dispatch] = useReducer(reduce, initial);
  // the changes without their texts: the same object for as long as only texts change,
  // so the tree is not drawn again with each key typed
  const shapeKey = outlineKey(drafts);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const shape = useMemo(() => outline(drafts), [shapeKey]);

  // kept in the browser shortly after each change, and at once when leaving
  const pending = useRef<Drafts | null>(null);
  // set once the workbench hands over (a commit, an update): what is kept is then written by hand (`seal`)
  const over = useRef(false);
  const flush = useCallback(() => {
    const d = pending.current;
    if (!d || over.current) return;
    pending.current = null;
    void saveDrafts(desk, base, d);
  }, [desk, base]);
  useEffect(() => {
    if (drafts === initial) return;
    pending.current = drafts;
    const t = setTimeout(flush, 300);
    return () => clearTimeout(t);
  }, [drafts, initial, flush]);
  useEffect(() => {
    window.addEventListener("pagehide", flush);
    return () => {
      window.removeEventListener("pagehide", flush);
      flush();
    };
  }, [flush]);

  const [opened, setTabs] = useState<string[]>(kept?.tabs ?? []);
  // the address names the file in front: it has a tab from the first render on
  const tabs = path && !opened.includes(path) ? [...opened, path] : opened;
  useEffect(() => {
    if (path) setTabs((t) => (t.includes(path) ? t : [...t, path]));
  }, [path]);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set(ancestors(path)));
  useEffect(() => {
    setExpanded((s) => (ancestors(path).every((d) => s.has(d)) ? s : new Set([...s, ...ancestors(path)])));
  }, [path]);
  const [cursor, setCursor] = useState<string | null>(path || null);
  // on a narrow screen the files cover the editor until one is picked
  const [treeOpen, setTreeOpen] = useState(kept?.sideOpen ?? !path);
  const [side, setSide] = useState<Kept["side"]>(kept?.side ?? "files");
  // what differs from the commit shows in the editors
  const [diff, setDiff] = useState(false);
  // Markdown files show as they will read, instead of their text
  const [preview, setPreview] = useState(false);
  const [vim, setVim] = usePref("editor.vim", false);

  const go = useCallback((to: string, replace = false) => nav(refLink(ctx.web, "ide", res.ref, to), { replace }), [nav, ctx.web, res.ref]);
  const open = useCallback(
    (to: string) => {
      setTreeOpen(false);
      go(to);
    },
    [go],
  );
  const close = (tab: string) => {
    const at = tabs.indexOf(tab);
    const left = tabs.filter((t) => t !== tab);
    setTabs(left);
    if (tab === path) go(left[Math.min(at, left.length - 1)] ?? "", true);
  };
  /** A file's tab follows it to its new path. */
  const retab = (from: string, to: string) => {
    setTabs([...new Set(tabs.map((t) => (t === from ? to : t)))]);
    if (from === path) go(to, true);
  };

  /** What a commit has at a path: the entry of its folder's listing, or null. */
  const entryAt = async (commit: string, p: string) => {
    const dir = parentOf(p);
    try {
      const t = await qc.fetchQuery({ queryKey: keys.tree(pid, commit, dir), queryFn: () => fetchers.tree(pid, commit, dir), staleTime: Infinity });
      return t.entries.find((e) => e.name === nameOf(p)) ?? null;
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) return null;
      throw e;
    }
  };
  /** The kind of what the base commit has at a path, or null. */
  const inBase = async (p: string) => (await entryAt(base, p))?.kind ?? null;
  /** Why the changes so far, or a file of the commit above it, leave no room for a file at a path; or null. */
  const taken = async (p: string): Promise<string | null> => {
    const why = clash(drafts, p);
    if (why) return why;
    const dirs = ancestors(p);
    const kinds = await Promise.all(dirs.map(inBase));
    const file = dirs.find((_, i) => kinds[i] !== null && kinds[i] !== "tree");
    return file === undefined ? null : `${file} is a file: nothing can go under it`;
  };

  /** A path asked for, without the slashes it may start with; undefined when the author cancels. */
  const askPath = async (title: string, action: string, value: string, more: { body?: string; placeholder?: string } = {}) =>
    (await prompt({ title, body: more.body, action, input: { label: "File path", value, placeholder: more.placeholder, mono: true } }))?.trim().replace(/^\/+/, "");

  const create = async (dir: string) => {
    const to = await askPath("New file", "Create", dir ? `${dir}/` : "", { placeholder: "docs/guide.md" });
    if (to === undefined) return;
    try {
      const kind = badPath(to) ? null : await inBase(to);
      const there = () => {
        // a file of the commit leaves its path free once it is deleted here (renamed, its path waits for the commit)
        if (kind === null || (kind !== "tree" && drafts[to]?.deleted)) return null;
        if (renamedAway(drafts).has(to) && !drafts[to]) return `${to} is renamed here, and still in the commit: commit the rename first`;
        return `${to} already exists`;
      };
      const why = badPath(to) ?? (await taken(to)) ?? there();
      if (why) return toast.error(why);
      dispatch({ type: "create", path: to });
      open(to);
    } catch (e) {
      toast.error(e);
    }
  };
  const rename = async (from: string) => {
    const to = await askPath(`Rename ${nameOf(from)}`, "Rename", from, { body: "A new folder in the path moves the file there." });
    if (to === undefined || to === from) return;
    try {
      // back to where the commit has it is the one path of the commit a file can take
      const home = drafts[from]?.origin === to;
      const why = badPath(to) ?? (await taken(to)) ?? (!home && (await inBase(to)) !== null ? `${to} is in the commit: a file cannot be renamed over it` : null);
      if (why) return toast.error(why);
      dispatch({ type: "rename", from, to });
      retab(from, to);
      setCursor(to);
    } catch (e) {
      toast.error(e);
    }
  };
  /** A change was dropped: its file is back where the commit has it (or gone, if it never was in it), and so its tab. */
  const settle = (p: string, d: Draft) => {
    if (d.origin === p) return;
    if (d.origin === undefined) close(p);
    else retab(p, d.origin);
    setCursor(d.origin ?? parentOf(p));
  };
  /** Whether a change may go: only typed text is lost for good (a deletion or a plain rename is undone as it was done), so that asks first. */
  const mayLose = async (d: Draft | undefined, title: string, action: string) =>
    d?.text === undefined || (await confirm({ title, body: d.origin === undefined ? "The file was never committed: it is gone for good." : "What you typed is gone for good.", action, danger: true }));
  const remove = async (p: string) => {
    const d = drafts[p];
    if (!(await mayLose(d, `Delete ${nameOf(p)}?`, "Delete"))) return;
    // a file of the commit stays listed, struck out, under the path the commit has
    dispatch({ type: "remove", path: p });
    if (d) settle(p, d);
  };
  const discard = async (p: string) => {
    const d = drafts[p];
    if (!d || !(await mayLose(d, `Discard the changes of ${nameOf(p)}?`, "Discard"))) return;
    dispatch({ type: "discard", path: p });
    settle(p, d);
  };
  const discardAll = async () => {
    if (!(await confirm({ title: "Discard every change?", body: `${plural(count, "file")} go back to what the commit has. What you typed is gone for good.`, action: "Discard all", danger: true }))) return;
    dispatch({ type: "clear" });
    const kept = tabs.filter((t) => !drafts[t] || drafts[t].origin === t);
    setTabs(kept);
    if (path && !kept.includes(path)) go(kept[0] ?? "", true);
    setCursor(null);
  };

  /** The workbench hands over: what is kept is `next` on `onto` from now on, not what it types. */
  const seal = (onto: string, next: Drafts) => {
    over.current = true;
    return saveDrafts(desk, onto, next);
  };
  /** Starts over with what is kept now (`next` on `onto`), the open files still open. */
  const handOver = async (onto: string, next: Drafts, open: string[]) => {
    const sealed = seal(onto, next);
    if (path && !open.includes(path)) go(open[0] ?? "", true);
    // the next workbench is handed the changes in memory: it need not wait for the write
    await Promise.all([sealed, restart({ saved: Object.keys(next).length > 0 ? { base: onto, drafts: next } : null, tabs: open, side, sideOpen: treeOpen })]);
  };
  const committed = async ({ target, branch, mrTarget }: Landed) => {
    if (target === "new") {
      await seal(base, {});
      return nav(`${ctx.web}/-/merge_requests/new${qs({ source: branch, target: mrTarget })}`);
    }
    toast.ok(`Committed to ${branch}`);
    // what was deleted has no tab left; the rest is where the commit put it
    await handOver(base, {}, tabs.filter((t) => !drafts[t]?.deleted));
  };
  /**
   * Brings the changes onto the ref's latest commit. A file changed there too cannot
   * follow (nothing here merges two versions of a text): its change is dropped, once
   * the author agreed.
   */
  const update = async () => {
    const head = res.commit;
    // the same object in both commits: the listings tell, no file is fetched
    const same = async (p: string) => {
      const [a, b] = await Promise.all([entryAt(base, p), entryAt(head, p)]);
      return a !== null && b !== null && a.id === b.id;
    };
    const entries = Object.entries(drafts);
    const clashes = await Promise.all(
      entries.map(async ([p, d]) => {
        // the file it was made from changed, or went
        if (d.origin !== undefined && !(await same(d.origin))) return true;
        // a new file, or a renamed one: its place was taken
        return d.origin !== p && (await entryAt(head, p)) !== null;
      }),
    );
    const lost = entries.filter((_, i) => clashes[i]).map(([p]) => p);
    if (lost.length > 0) {
      const ok = await confirm({
        title: `${plural(lost.length, "file")} changed on ${res.ref} too`,
        body: (
          <>
            <p>
              Updating drops your changes to {lost.length === 1 ? "it" : "them"}: <code>{lost.join(", ")}</code>.
            </p>
            <p>To keep everything, cancel and commit to a new branch: the merge request shows the conflict.</p>
          </>
        ),
        action: lost.length === 1 ? "Drop it and update" : "Drop them and update",
        danger: true,
      });
      if (!ok) return;
    }
    const next = Object.fromEntries(entries.filter((_, i) => !clashes[i]));
    // a dropped rename leaves its file where the commit has it; a dropped new file is gone
    const open = tabs.flatMap((t) => (lost.includes(t) ? (drafts[t].origin !== undefined ? [drafts[t].origin as string] : []) : [t]));
    toast.ok(`Updated to ${head.slice(0, 8)}`);
    await handOver(head, next, [...new Set(open)]);
  };
  const show = (p: string) => {
    setDiff(true);
    open(p);
  };

  // what the tree's rows do, as the workbench is now, behind functions that stay the same
  const acts = useRef({ create, rename, remove, discard });
  acts.current = { create, rename, remove, discard };
  const ops = useMemo<TreeOps>(
    () => ({
      projectId: pid,
      base,
      drafts: shape,
      active: path,
      expanded,
      cursor,
      setCursor,
      toggle: (dir) => {
        setExpanded((s) => {
          const next = new Set(s);
          if (!next.delete(dir)) next.add(dir);
          return next;
        });
      },
      open,
      create: (dir) => {
        setExpanded((s) => new Set([...s, ...ancestors(`${dir}/x`)]));
        void acts.current.create(dir);
      },
      rename: (p) => void acts.current.rename(p),
      remove: (p) => void acts.current.remove(p),
      discard: (p) => void acts.current.discard(p),
    }),
    [pid, base, shape, path, expanded, cursor, open],
  );
  const count = Object.keys(drafts).length;
  const names = tabs.map(nameOf);
  // where each file renamed here went
  const movedTo = useMemo(() => renamedAway(shape), [shape]);

  return (
    <div className="ide" data-tree={treeOpen ? "open" : undefined}>
      <aside className="ide-side" aria-label="Files and changes">
        <div className="ide-side-head">
          <div className="ide-side-tabs" role="tablist" aria-label="Side panel">
            <button type="button" role="tab" aria-selected={side === "files"} onClick={() => setSide("files")}>
              Files
            </button>
            <button type="button" role="tab" aria-selected={side === "changes"} onClick={() => setSide("changes")}>
              Changes {count > 0 && <span className="tab-count">{count}</span>}
            </button>
          </div>
          <span className="grow" />
          {side === "files" && (
            <>
              <button type="button" className="icon-btn" title={`Go to file (${MOD}+P)`} aria-label="Go to file" onClick={() => openPalette("files")}>
                <Search size={15} />
              </button>
              <button type="button" className="icon-btn" title="New file" aria-label="New file" onClick={() => void create(parentOf(path))}>
                <FilePlus2 size={15} />
              </button>
            </>
          )}
        </div>
        {/* both stay as they are while the other shows: a commit message being typed is not lost to a look at the files */}
        <div className="ide-side-body" hidden={side !== "files"}>
          <FileTree ops={ops} />
        </div>
        <div className="ide-side-body" hidden={side !== "changes"}>
          <Changes ctx={ctx} res={res} base={base} drafts={drafts} active={path} show={show} discard={(p) => void discard(p)} update={update} refresh={refresh} committed={committed} />
        </div>
      </aside>
      <section className="ide-main">
        <div className="ide-tabs">
          <button type="button" className="icon-btn ide-tree-toggle" title="Files and changes" aria-label="Files and changes" aria-pressed={treeOpen} onClick={() => setTreeOpen(!treeOpen)}>
            <FolderTree size={16} />
          </button>
          <div className="ide-tablist" role="tablist" aria-label="Open files">
            {tabs.map((t, i) => (
              <div key={t} className="ide-tab" data-active={t === path || undefined} onAuxClick={(e) => e.button === 1 && close(t)}>
                <button type="button" role="tab" aria-selected={t === path} title={t} onClick={() => open(t)}>
                  {drafts[t] && <StatusMark status={statusOf(t, drafts[t])} />}
                  <span className={drafts[t]?.deleted ? "ide-deleted" : undefined}>{names[i]}</span>
                  {/* two files of one name: the folder tells them apart */}
                  {names.indexOf(names[i]) !== names.lastIndexOf(names[i]) && <span className="faint small">{parentOf(t) || "/"}</span>}
                </button>
                <button type="button" className="icon-btn ide-tab-close" aria-label={`Close ${t}`} title="Close" onClick={() => close(t)}>
                  <X size={13} />
                </button>
              </div>
            ))}
          </div>
          {isMarkdown(path) && (
            <button type="button" className={`icon-btn ide-toggle ${preview ? "on" : ""}`} title="Show Markdown files as they will read" aria-label="Preview" aria-pressed={preview} onClick={() => setPreview(!preview)}>
              <Eye size={16} />
            </button>
          )}
          <button type="button" className={`icon-btn ide-toggle ${diff ? "on" : ""}`} title="Show what changed in the files" aria-label="Show changes" aria-pressed={diff} onClick={() => setDiff(!diff)}>
            <FileDiff size={16} />
          </button>
        </div>
        <div className="ide-panes">
          {tabs.length === 0 && (
            <div className="empty">
              <h2>Web IDE</h2>
              <p>Pick a file in the list, or create one. Your changes stay in this browser until you commit them.</p>
            </div>
          )}
          {/* every open file keeps its editor (its undo history, its place): only the one in front shows */}
          {tabs.map((t) => (
            <Pane key={t} ctx={ctx} gitRef={res.ref} base={base} path={t} draft={drafts[t]} movedTo={drafts[t] ? undefined : movedTo.get(t)} open={open} active={t === path} diff={diff} preview={preview && isMarkdown(t)} vim={vim} dispatch={dispatch} />
          ))}
        </div>
        <footer className="ide-status-bar small">
          <span className="row" style={{ gap: 4 }}>
            <GitBranch size={13} /> {res.ref}
          </span>
          <Link to={`${ctx.web}/-/commit/${base}`} className="sha" title="The commit these files are read from">
            {base.slice(0, 8)}
          </Link>
          {base !== res.commit && (
            <span className="row warn-text" style={{ gap: 4 }} title={`${res.ref} is now at ${res.commit.slice(0, 8)}`}>
              <TriangleAlert size={13} /> {res.ref} has moved since these changes were started
            </span>
          )}
          <span className="grow" />
          <button type="button" className="ide-vim" title="Vim's modes and keys in the editor" aria-pressed={vim} onClick={() => setVim(!vim)}>
            Vim
          </button>
          {count === 0 ? (
            <span>No changes</span>
          ) : (
            <>
              <button
                type="button"
                className="link-btn small"
                title="Kept in this browser until you commit them"
                onClick={() => {
                  setSide("changes");
                  setTreeOpen(true);
                }}
              >
                {plural(count, "changed file")}
              </button>
              <button type="button" className="link-btn small" onClick={() => void discardAll()}>
                Discard all
              </button>
            </>
          )}
        </footer>
      </section>
    </div>
  );
}

/** A file of the base commit, as the editor can take it. */
type Base = { kind: "text"; text: string } | { kind: "binary" } | { kind: "lfs" } | { kind: "large"; size: number };

async function loadBase(projectId: number, commit: string, path: string): Promise<Base> {
  // what the file is, before any of it is fetched. One kept in Git LFS is a pointer in
  // the repository: its content, committed from here, would replace the pointer
  const blob = await fetchers.blob(projectId, commit, path);
  if (blob.lfs) return { kind: "lfs" };
  if (blob.binary) return { kind: "binary" };
  if (blob.too_large || blob.size > MAX_TEXT) return { kind: "large", size: blob.size };
  const r = await fetch(`/api/v1/projects/${projectId}/repository/raw${qs({ ref: commit, path })}`, { credentials: "same-origin" });
  if (!r.ok) throw new ApiError(r.status, "error", `Could not load the file (${r.status})`);
  try {
    // byte for byte (a byte order mark stays): what is not UTF-8 text is not edited here
    const text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(await r.arrayBuffer());
    return text.includes("\0") ? { kind: "binary" } : { kind: "text", text };
  } catch {
    return { kind: "binary" };
  }
}

/** A Markdown file as the server renders it, a moment after the last key. */
function Preview({ projectId, text }: { projectId: number; text: string }) {
  const settled = useDebounced(text, 300);
  const q = useQuery({
    queryKey: ["markdown", projectId, settled],
    queryFn: () => api.post<S["MarkdownHtml"]>(`/projects/${projectId}/markdown`, { text: settled }),
    enabled: settled.trim() !== "",
    staleTime: Infinity,
    // a rendering per pause in the typing: none is kept once it is replaced
    gcTime: 0,
    // the last rendering stays while the next one comes
    placeholderData: keepPreviousData,
  });
  return (
    <div className="ide-preview markdown">
      {settled.trim() === "" ? <span className="faint">Nothing to preview.</span> : q.error ? <ErrorBox error={q.error} /> : q.data ? <Markdown html={q.data.html} /> : <Spinner />}
    </div>
  );
}

/** One open file: its editor, or why there is none. Drawn again only when its own file changes, not with each key typed in another. */
const Pane = memo(function Pane({
  ctx,
  gitRef,
  base,
  path,
  draft,
  movedTo,
  open,
  active,
  diff,
  preview,
  vim,
  dispatch,
}: {
  ctx: Ctx;
  gitRef: string;
  base: string;
  path: string;
  draft?: Draft;
  /** the file was renamed here: where it is now */
  movedTo?: string;
  open: (path: string) => void;
  active: boolean;
  diff: boolean;
  preview: boolean;
  vim: boolean;
  dispatch: (a: Action) => void;
}) {
  // with no change, the path is a file of the commit; a new file has no origin
  const origin = draft ? draft.origin : path;
  const q = useQuery({
    queryKey: ["ide-base", ctx.p.id, base, origin],
    queryFn: () => loadBase(ctx.p.id, base, origin!),
    enabled: origin !== undefined && !draft?.deleted && movedTo === undefined,
    staleTime: Infinity,
    retry: (n, e) => n < 2 && !(e instanceof ApiError && e.status === 404),
  });

  // under a preview the editor stays as it is (its undo history, its place), out of sight
  const editor = (text: string, baseText?: string) => (
    <>
      <div className="ide-fill" hidden={preview}>
        <Suspense fallback={<Spinner />}>
          <CodeEditor value={text} onChange={(t) => dispatch({ type: "edit", path, text: t, base: baseText })} path={path} label={`Content of ${path}`} autoFocus={active} original={diff && active ? (baseText ?? "") : undefined} vim={vim} />
        </Suspense>
      </div>
      {preview && <Preview projectId={ctx.p.id} text={text} />}
    </>
  );
  let body;
  if (draft?.deleted) {
    body = (
      <div className="empty">
        <p>
          <code>{path}</code> is deleted by your changes.
        </p>
        <button type="button" className="btn" onClick={() => dispatch({ type: "discard", path })}>
          Restore it
        </button>
      </div>
    );
  } else if (movedTo !== undefined) {
    body = (
      <div className="empty">
        <p>
          <code>{path}</code> is renamed by your changes.
        </p>
        <button type="button" className="btn" onClick={() => open(movedTo)}>
          Open {movedTo}
        </button>
      </div>
    );
  } else if (origin === undefined) {
    body = editor(draft?.text ?? "");
  } else if (q.isLoading) {
    body = <Spinner />;
  } else if (q.error instanceof ApiError && q.error.status === 404) {
    body = (
      <div className="empty">
        <p>
          There is no <code>{path}</code> in this commit.
        </p>
      </div>
    );
  } else if (q.error) {
    body = <ErrorBox error={q.error} />;
  } else if (q.data && q.data.kind !== "text") {
    body = (
      <div className="empty">
        <p>{q.data.kind === "binary" ? "This file is not text: it cannot be edited here." : q.data.kind === "lfs" ? "This file is kept in Git LFS: it cannot be edited here." : `This file is too large to edit here (${bytes(q.data.size)}).`}</p>
        <Link to={refLink(ctx.web, "blob", gitRef, origin)}>View it in the repository</Link>
      </div>
    );
  } else if (q.data) {
    body = editor(draft?.text ?? q.data.text, q.data.text);
  }
  return (
    <div className="ide-pane" role="tabpanel" aria-label={path} data-active={active || undefined}>
      {body}
    </div>
  );
});
