import { useEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import { Link, useNavigate } from "react-router-dom";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { ChevronDown, ChevronRight, FileText, Folder, PanelLeftClose, PanelLeftOpen } from "lucide-react";
import { filesLink, refLink, type S } from "../../api/client";
import { fetchers, keys } from "../../prefetch";
import { usePref } from "../../shell/theme";
import type { Ctx } from "./Project";

/** A folder shows this many entries here at most; its page shows them all. */
const MAX = 500;

/** The folders above a path: `a/b/c` → `a`, `a/b`. */
const above = (path: string) => {
  const parts = path.split("/");
  return parts.slice(0, -1).map((_, i) => parts.slice(0, i + 1).join("/"));
};

type Where = { ctx: Ctx; gitRef: string; commit: string };

/**
 * A folder, a file or its blame, with the project's files beside them on a wide screen. It stays
 * mounted from one file to the next (`kind` and `refAndPath` change, it does not), so the folders
 * opened in it stay open. At a ref's root, the Files page, it adds nothing.
 */
export function CodeLayout({ ctx, kind, refAndPath, children }: { ctx: Ctx; kind: "tree" | "blob"; refAndPath: string; children: ReactNode }) {
  // the same request as the page's own (same key); the last answer is kept while the next one comes
  const r = useQuery({ queryKey: keys.resolve(ctx.p.id, refAndPath), queryFn: () => fetchers.resolve(ctx.p.id, refAndPath), staleTime: 5_000, placeholderData: keepPreviousData });
  const [shown, setShown] = usePref("code.filetree", true);
  const res = r.data;
  const panel = res && !(kind === "tree" && res.path === "");
  return (
    <div className={`code-layout${panel ? (shown ? "" : " tree-hidden") : " no-tree"}`}>
      {panel ? <FileTreePanel where={{ ctx, gitRef: res.ref, commit: res.commit }} path={res.path} folder={kind === "tree"} shown={shown} setShown={setShown} /> : null}
      <div className="code-main">{children}</div>
    </div>
  );
}

function FileTreePanel({ where, path, folder, shown, setShown }: { where: Where; path: string; folder: boolean; shown: boolean; setShown: (v: boolean) => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const navigate = useNavigate();
  // the folders open: those above what is read, and those opened by hand, until the ref changes
  const opening = (p: string) => (folder ? [...above(p), p] : above(p));
  const [open, setOpen] = useState<Set<string>>(() => new Set(opening(path)));
  const [focus, setFocus] = useState(path);
  const last = useRef({ commit: where.commit, path });
  useEffect(() => {
    const changedRef = last.current.commit !== where.commit;
    last.current = { commit: where.commit, path };
    setOpen((prev) => new Set([...(changedRef ? [] : prev), ...opening(path)]));
    setFocus(path);
    // what is read, in view in the panel (the panel scrolls, not the page)
    requestAnimationFrame(() => {
      const box = ref.current?.querySelector<HTMLElement>(".ft-items");
      const cur = box?.querySelector<HTMLElement>('[aria-current="page"]');
      if (box && cur && (cur.offsetTop < box.scrollTop || cur.offsetTop > box.scrollTop + box.clientHeight - 30)) box.scrollTop = cur.offsetTop - box.clientHeight / 3;
    });
  }, [where.commit, path]);
  const toggle = (p: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (next.has(p)) next.delete(p);
      else next.add(p);
      return next;
    });

  if (!shown)
    return (
      <div className="file-tree hidden-tree">
        <button type="button" className="icon-btn" onClick={() => setShown(true)} title="Show the files" aria-label="Show the files">
          <PanelLeftOpen size={16} />
        </button>
      </div>
    );

  // keys while focus is in it: the page's own (j, k, arrows on the folder list) wait
  const keys_ = (e: KeyboardEvent) => {
    const items = [...(ref.current?.querySelectorAll<HTMLElement>('[role="treeitem"]') ?? [])];
    const at = items.findIndex((x) => x === document.activeElement);
    if (at < 0) return;
    const item = items[at];
    const p = item.dataset.path!;
    const dir = item.dataset.dir === "1";
    const go = (i: number) => {
      const t = items[Math.max(0, Math.min(items.length - 1, i))];
      setFocus(t.dataset.path!);
      t.focus();
    };
    const handled = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    if (e.key === "ArrowDown") (handled(), go(at + 1));
    else if (e.key === "ArrowUp") (handled(), go(at - 1));
    else if (e.key === "Home") (handled(), go(0));
    else if (e.key === "End") (handled(), go(items.length - 1));
    else if (e.key === "ArrowRight" && dir) {
      handled();
      if (!open.has(p)) toggle(p);
      else go(at + 1);
    } else if (e.key === "ArrowLeft") {
      handled();
      if (dir && open.has(p)) toggle(p);
      else {
        const up = items.findIndex((x) => x.dataset.path === p.split("/").slice(0, -1).join("/"));
        if (up >= 0) go(up);
      }
    } else if (e.key === "Enter" || e.key === " ") {
      handled();
      if (dir && !open.has(p)) toggle(p);
      navigate(item.dataset.to!);
    }
  };

  return (
    <nav className="file-tree" aria-label="Files" ref={ref}>
      <div className="ft-head">
        <span>Files</span>
        <button type="button" className="icon-btn" onClick={() => setShown(false)} title="Hide the files" aria-label="Hide the files">
          <PanelLeftClose size={16} />
        </button>
      </div>
      <div className="ft-items" role="tree" aria-label="The project's files" onKeyDown={keys_}>
        <Entries where={where} dir="" depth={0} open={open} toggle={toggle} current={path} focus={focus} />
      </div>
    </nav>
  );
}

function Entries({ where, dir, depth, open, toggle, current, focus }: { where: Where; dir: string; depth: number; open: Set<string>; toggle: (p: string) => void; current: string; focus: string }) {
  const { ctx, commit } = where;
  const q = useQuery({ queryKey: keys.tree(ctx.p.id, commit, dir), queryFn: () => fetchers.tree(ctx.p.id, commit, dir), staleTime: 60_000 });
  const depthStyle = { "--depth": depth } as CSSProperties;
  if (q.isLoading)
    return (
      <div className="ft-row ft-note" style={depthStyle}>
        Loading…
      </div>
    );
  if (!q.data) return null;
  const entries = q.data.entries;
  return (
    <>
      {entries.slice(0, MAX).map((e) => (
        <Entry key={e.path} where={where} e={e} depth={depth} open={open} toggle={toggle} current={current} focus={focus} />
      ))}
      {entries.length > MAX && (
        <Link className="ft-row ft-note" style={depthStyle} to={filesLink(ctx.web, where.gitRef, ctx.p.default_branch, dir)}>
          {entries.length - MAX} more in this folder
        </Link>
      )}
    </>
  );
}

function Entry({ where, e, depth, open, toggle, current, focus }: { where: Where; e: S["TreeEntry"]; depth: number; open: Set<string>; toggle: (p: string) => void; current: string; focus: string }) {
  const { ctx, gitRef } = where;
  const dir = e.kind === "tree";
  const expanded = dir && open.has(e.path);
  const to = dir ? filesLink(ctx.web, gitRef, ctx.p.default_branch, e.path) : refLink(ctx.web, "blob", gitRef, e.path);
  // a submodule is another repository: named, not followed
  if (e.kind === "submodule") return null;
  return (
    <>
      <div
        role="treeitem"
        aria-level={depth + 1}
        aria-expanded={dir ? expanded : undefined}
        aria-current={e.path === current ? "page" : undefined}
        aria-label={e.name}
        tabIndex={e.path === focus ? 0 : -1}
        className="ft-row"
        style={{ "--depth": depth } as CSSProperties}
        data-path={e.path}
        data-dir={dir ? "1" : undefined}
        data-to={to}
      >
        {dir ? (
          <button type="button" tabIndex={-1} className="ft-toggle" aria-hidden onClick={() => toggle(e.path)}>
            {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          </button>
        ) : (
          <span className="ft-toggle" />
        )}
        <Link tabIndex={-1} className="ft-name" to={to} onClick={() => dir && !expanded && toggle(e.path)}>
          {dir ? <Folder size={15} className="ic-folder" aria-hidden /> : <FileText size={15} aria-hidden />}
          <span className="ellipsis">{e.name}</span>
        </Link>
      </div>
      {expanded && (
        <div role="group">
          <Entries where={where} dir={e.path} depth={depth + 1} open={open} toggle={toggle} current={current} focus={focus} />
        </div>
      )}
    </>
  );
}
