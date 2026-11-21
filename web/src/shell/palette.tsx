import { useEffect, useMemo, useRef, useState, type ReactElement, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { CircleDot, FileSearch, FileText, FolderGit2, GitMerge, Search, Users, CornerDownLeft } from "lucide-react";
import { useDebounced } from "../components/ui";
import { api, qs, refLink, type S } from "../api/client";
import { useSession } from "../session";
import { useNav } from "./nav";
import { useGroups, useMyProjects } from "../prefetch";
import { useRecentProjects } from "./recent";
import { projectPages } from "./projectNav";
import { MOD } from "./platform";

// ---------------------------------------------------------------- shortcuts

/** True when a key press belongs to a text field, not to the page. */
export function isTyping(e: KeyboardEvent): boolean {
  const t = e.target as HTMLElement | null;
  if (!t) return false;
  const tag = t.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t.isContentEditable;
}

/** Return `false` to leave the key to the browser. */
type Handler = (e: KeyboardEvent) => boolean | void;

/**
 * Page-level single-key shortcuts (`j`, `]`, `n`…). Ignored while typing, with
 * modifiers held, or while a dialog is open.
 */
export function useShortcuts(map: Record<string, Handler>, enabled = true) {
  const ref = useRef(map);
  ref.current = map;
  useEffect(() => {
    if (!enabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || isTyping(e) || e.ctrlKey || e.metaKey || e.altKey) return;
      if (document.querySelector(".dialog-backdrop")) return;
      if ((e.key === "Enter" || e.key === " ") && (e.target as HTMLElement | null)?.closest?.("a,button,summary,[role=button],[role=tab]")) return;
      const h = ref.current[e.key];
      if (h && h(e) !== false) e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [enabled]);
}

export const SHORTCUTS: { keys: string[]; what: string; where: string }[] = [
  { keys: ["Ctrl", "K"], what: "Search or go to", where: "Everywhere" },
  { keys: ["t"], what: "Find a file in this project", where: "Project" },
  { keys: ["?"], what: "Show keyboard shortcuts", where: "Everywhere" },
  { keys: ["j", "k"], what: "Move down / up the file list", where: "Repository" },
  { keys: ["Enter"], what: "Open the selected entry", where: "Repository" },
  { keys: ["]", "["], what: "Next / previous file", where: "Diffs" },
  { keys: ["n", "p"], what: "Next / previous unresolved thread", where: "Merge request" },
  { keys: ["j", "k"], what: "Move the line cursor (Shift: select lines)", where: "Merge request changes" },
  { keys: ["c"], what: "Comment on the selected lines", where: "Merge request changes" },
  { keys: ["Shift", "click"], what: "Select a line range (or drag over line numbers)", where: "File view, changes" },
];

// ---------------------------------------------------------------- palette

type Mode = "all" | "files";
const OPEN = "forgust:palette";

export function openPalette(mode: Mode = "all") {
  window.dispatchEvent(new CustomEvent(OPEN, { detail: mode }));
}

type Entry = { key: string; label: string; hint?: string; icon: ReactElement; to: string };
/** A titled run of entries; `more` counts the matches left out. */
type Section = { key: string; title: string; entries: Entry[]; more?: number; moreHint?: string; start?: number };

/** Subsequence match, scored so contiguous and basename hits rank first. */
function fuzzy(needle: string, hay: string): number {
  if (!needle) return 1;
  const n = needle.toLowerCase();
  const h = hay.toLowerCase();
  const idx = h.indexOf(n);
  if (idx >= 0) {
    const base = h.lastIndexOf("/") + 1;
    return 1000 - idx + (idx >= base ? 500 : 0) - h.length / 100;
  }
  let score = 0;
  let j = 0;
  let last = -2;
  for (let i = 0; i < h.length && j < n.length; i++) {
    if (h[i] === n[j]) {
      score += i === last + 1 ? 5 : 1;
      last = i;
      j++;
    }
  }
  return j === n.length ? score - h.length / 100 : -1;
}

/** The best `cap` of `items` for `term`, and how many more matched. Keeps only the
 *  best few while scanning: a short term matches most of a large repository. */
function ranked<T>(items: readonly T[], term: string, hay: (t: T) => string, cap: number): { top: T[]; more: number } {
  // nothing typed: the given order
  if (!term) return { top: items.slice(0, cap), more: Math.max(0, items.length - cap) };
  const best: { it: T; s: number }[] = [];
  let hits = 0;
  for (const it of items) {
    const s = fuzzy(term, hay(it));
    if (s < 0) continue;
    hits++;
    if (best.length === cap && s <= best[cap - 1].s) continue;
    let k = best.length;
    while (k > 0 && best[k - 1].s < s) k--;
    best.splice(k, 0, { it, s });
    if (best.length > cap) best.pop();
  }
  return { top: best.map((h) => h.it), more: hits - best.length };
}

function Palette({ mode, close }: { mode: Mode; close: () => void }) {
  const nav = useNav();
  const navigate = useNavigate();
  const { user } = useSession();
  const [term, setTerm] = useState("");
  // the selected entry, by key: results that arrive later do not move it; null: the first
  const [selKey, setSelKey] = useState<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const project = nav.kind === "project" ? nav.project : null;
  const ref = nav.kind === "project" ? (nav.ref ?? project?.default_branch ?? "main") : "";
  const recent = useRecentProjects(user?.id);

  const files = useQuery({
    queryKey: ["files", project?.id, ref],
    queryFn: () => api.get<S["FileList"]>(`/projects/${project!.id}/repository/files${qs({ ref })}`),
    enabled: !!project && project.can_read_code && !project.empty_repo,
    staleTime: 60_000,
  });
  // the dashboard's request
  const projects = useMyProjects("", !!user && mode === "all");
  const groups = useGroups(!!user && mode === "all");
  // issues and merge requests across the instance, once typing settles
  const t = term.trim();
  const settled = useDebounced(t, 200);
  const found = useQuery({
    queryKey: ["palette-search", settled],
    queryFn: () => api.get<S["SearchResults"]>(`/search${qs({ q: settled })}`),
    enabled: mode === "all" && settled.length >= 2,
    staleTime: 30_000,
    placeholderData: (prev) => prev,
  });

  const pages = useMemo(() => {
    if (!project) return { top: [] as Entry[], all: [] as Entry[] };
    const entry = (it: { to: string; label: string; icon?: ReactNode }): Entry => ({ key: `n:${it.to}:${it.label}`, label: it.label, icon: <>{it.icon}</>, to: it.to });
    const { top, all } = projectPages(project, ref);
    return { top: top.map(entry), all: all.map(entry) };
  }, [project, ref]);

  // the files on their own: a search response must not rank the whole repository again
  const fileHits = useMemo(() => {
    if (!project || !files.data || (mode === "all" && !t)) return null;
    return ranked(files.data.paths, t, (f) => f, mode === "files" ? 50 : 8);
  }, [project, files.data, t, mode]);

  const { sections, entries, searchAll, searchCode } = useMemo(() => {
    const out: Section[] = [];
    const push = (title: string, entries: Entry[], more?: number, moreHint?: string) => {
      if (entries.length) out.push({ key: title, title, entries, more, moreHint });
    };
    const fileEntry = (f: string): Entry => ({ key: `f:${f}`, label: f, icon: <FileText size={15} />, to: refLink(`/${project!.full_path}`, "blob", ref, f) });
    const projectEntry = (p: { id: number; full_path: string }): Entry => ({ key: `p:${p.id}`, label: p.full_path, icon: <FolderGit2 size={15} />, to: `/${p.full_path}` });
    const groupEntry = (g: { id: number; full_path: string }): Entry => ({ key: `g:${g.id}`, label: g.full_path, icon: <Users size={15} />, to: `/${g.full_path}` });
    let searchAll: Entry | undefined;
    let searchCode: Entry | undefined;

    if (mode === "files") {
      if (fileHits) push("Files", fileHits.top.map(fileEntry), fileHits.more, "keep typing to narrow");
    } else if (!t) {
      // nothing typed: where to go next, not every file of the repository
      if (project) push(project.name, pages.top);
      const rec = recent.filter((r) => r.id !== project?.id).slice(0, 5);
      push("Recent projects", rec.map(projectEntry));
      if (!project) {
        const seen = new Set(rec.map((r) => r.id));
        push("Projects", (projects.data ?? []).filter((p) => !seen.has(p.id)).slice(0, 5).map(projectEntry));
        push("Groups", (groups.data ?? []).slice(0, 5).map(groupEntry));
      }
    } else {
      if (project) {
        // a page by its name, not by letters scattered through "Code › Tags"
        const lc = t.toLowerCase();
        push(`Go to in ${project.name}`, ranked(pages.all.filter((e) => e.label.toLowerCase().includes(lc)), t, (e) => e.label, 4).top);
      }
      if (fileHits) push("Files", fileHits.top.map(fileEntry), fileHits.more, "press t for the file finder");
      const pr = ranked(projects.data ?? [], t, (p) => p.full_path, 6);
      push("Projects", pr.top.map(projectEntry), pr.more);
      const gr = ranked(groups.data ?? [], t, (g) => g.full_path, 4);
      push("Groups", gr.top.map(groupEntry), gr.more);
      if (t.length >= 2 && found.data) {
        push(
          "Issues",
          (found.data.issues ?? []).slice(0, 5).map((i) => ({ key: `i:${i.project_id}:${i.iid}`, label: i.title, hint: `${i.project_path}#${i.iid}`, icon: <CircleDot size={15} />, to: `/${i.project_path}/-/issues/${i.iid}` })),
        );
        push(
          "Merge requests",
          (found.data.merge_requests ?? []).slice(0, 5).map((m) => ({ key: `m:${m.project_id}:${m.iid}`, label: m.title, hint: `${m.project_path}!${m.iid}`, icon: <GitMerge size={15} />, to: `/${m.project_path}/-/merge_requests/${m.iid}` })),
        );
      }
      // Shift+Enter and Ctrl/⌘+Enter open these from anywhere in the list
      searchAll = { key: "search", label: `Search everything for “${t}”`, hint: "⇧↵", icon: <Search size={15} />, to: `/search${qs({ q: t })}` };
      const search = [searchAll];
      if (t.length >= 3) {
        const codeAll: Entry = { key: "code", label: `Search all code for “${t}”`, icon: <FileSearch size={15} />, to: `/search${qs({ q: t, scope: "code" })}` };
        // in a project: its code first
        searchCode = project ? { key: "code-here", label: `Search ${project.name}'s code for “${t}”`, icon: <FileSearch size={15} />, to: `/search${qs({ q: t, scope: "code", project_id: project.full_path })}` } : codeAll;
        searchCode.hint = `${MOD}↵`;
        search.push(...(project ? [searchCode, codeAll] : [codeAll]));
      }
      push("Search", search);
    }

    let n = 0;
    for (const s of out) {
      s.start = n;
      n += s.entries.length;
    }
    return { sections: out, entries: out.flatMap((s) => s.entries), searchAll, searchCode };
  }, [t, mode, fileHits, projects.data, groups.data, found.data, project, ref, pages, recent]);

  useEffect(() => setSelKey(null), [term]);
  const sel = Math.max(0, entries.findIndex((e) => e.key === selKey));
  const setSel = (i: number) => setSelKey(entries[i]?.key ?? null);
  useEffect(() => {
    listRef.current?.querySelector(`[data-i="${sel}"]`)?.scrollIntoView({ block: "nearest" });
  }, [sel]);

  const go = (e?: Entry) => {
    if (!e) return;
    close();
    navigate(e.to);
  };
  const loading = files.isLoading || (mode === "all" && projects.isLoading);
  const searching = mode === "all" && t.length >= 2 && (settled !== t || found.isFetching);
  const placeholder = mode === "files" && project ? `Find a file in ${project.name}…` : project ? "Search files, pages, projects, issues and merge requests…" : "Search projects, groups, issues and merge requests…";

  return (
    <div className="dialog-backdrop" onMouseDown={close}>
      <div className="palette" role="dialog" aria-modal="true" aria-label="Search" onMouseDown={(e) => e.stopPropagation()}>
        <input
          autoFocus
          className="palette-input"
          placeholder={placeholder}
          value={term}
          onChange={(e) => setTerm(e.target.value)}
          aria-controls="palette-list"
          aria-activedescendant={entries[sel] ? `pal-${sel}` : undefined}
          onKeyDown={(e) => {
            if (e.key === "Escape") close();
            else if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n")) {
              e.preventDefault();
              setSel(Math.min(sel + 1, entries.length - 1));
            } else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) {
              e.preventDefault();
              setSel(Math.max(sel - 1, 0));
            } else if (e.key === "Tab") {
              // section to section; back: this section's head first, then the previous one
              e.preventDefault();
              const n = sections.length;
              if (!n) return;
              let cur = 0;
              while (cur + 1 < n && sections[cur + 1].start! <= sel) cur++;
              const next = e.shiftKey ? (sel > sections[cur].start! ? cur : (cur - 1 + n) % n) : (cur + 1) % n;
              setSel(sections[next].start!);
            } else if (e.key === "Enter") {
              e.preventDefault();
              go(e.shiftKey && searchAll ? searchAll : (e.ctrlKey || e.metaKey) && searchCode ? searchCode : entries[sel]);
            }
          }}
        />
        <div className="palette-list" id="palette-list" role="listbox" ref={listRef}>
          {sections.map((s) => (
            <div key={s.key} role="group" aria-labelledby={`palg-${s.key}`} className="palette-group">
              <div className="palette-group-title" id={`palg-${s.key}`}>
                {s.title}
              </div>
              {s.entries.map((e, j) => {
                const n = s.start! + j;
                return (
                  <button
                    key={e.key}
                    id={`pal-${n}`}
                    data-i={n}
                    role="option"
                    tabIndex={-1}
                    aria-selected={n === sel}
                    className={`palette-item ${n === sel ? "active" : ""}`}
                    onMouseMove={() => setSel(n)}
                    onClick={() => go(e)}
                  >
                    {e.icon}
                    <span className="grow ellipsis mono-ish">{e.label}</span>
                    {e.hint && <span className="faint small ellipsis palette-hint">{e.hint}</span>}
                    {n === sel && <CornerDownLeft size={14} className="faint" />}
                  </button>
                );
              })}
              {!!s.more && (
                <div className="palette-more faint small">
                  {s.more} more{s.moreHint ? ` — ${s.moreHint}` : ""}
                </div>
              )}
            </div>
          ))}
          {entries.length === 0 && <div className="palette-empty muted">{loading ? "Loading…" : "No matches"}</div>}
          {mode === "files" && files.data?.truncated && <div className="palette-empty faint small">Only the first {files.data.paths.length} files are searchable.</div>}
        </div>
        <div className="palette-foot faint small">
          <span>
            <kbd>↑</kbd> <kbd>↓</kbd> move
          </span>
          <span>
            <kbd>Tab</kbd> next section
          </span>
          <span>
            <kbd>↵</kbd> open
          </span>
          {searchAll && (
            <span>
              <kbd>⇧ ↵</kbd> search everything
            </span>
          )}
          {searchCode && (
            <span>
              <kbd>{MOD} ↵</kbd> search {searchCode.key === "code-here" ? "this project's" : "all"} code
            </span>
          )}
          {searching && <span className="grow palette-busy">Searching…</span>}
        </div>
      </div>
    </div>
  );
}

function Help({ close }: { close: () => void }) {
  return (
    <div className="dialog-backdrop" onMouseDown={close}>
      <div className="palette help" role="dialog" aria-modal="true" aria-label="Keyboard shortcuts" onMouseDown={(e) => e.stopPropagation()}>
        <div className="card-head">
          <strong className="grow">Keyboard shortcuts</strong>
          <button className="btn small ghost" onClick={close} autoFocus>
            Close
          </button>
        </div>
        <table className="shortcuts">
          <tbody>
            {SHORTCUTS.map((s) => (
              <tr key={s.what}>
                <td>
                  {s.keys.map((k, i) => (
                    <span key={k}>
                      {i > 0 && <span className="faint"> {s.keys.length === 2 && ["j", "]", "n"].includes(s.keys[0]) ? "/" : "+"} </span>}
                      <kbd>{k}</kbd>
                    </span>
                  ))}
                </td>
                <td>{s.what}</td>
                <td className="faint small">{s.where}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

/** Mount once: owns the palette, the help dialog, and the global keys. */
export function PaletteHost() {
  const [mode, setMode] = useState<Mode | null>(null);
  const [help, setHelp] = useState(false);
  const nav = useNav();
  const { user } = useSession();
  useEffect(() => {
    const onOpen = (e: Event) => setMode((e as CustomEvent<Mode>).detail);
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setMode((m) => (m ? null : "all"));
        return;
      }
      if (isTyping(e) || e.ctrlKey || e.metaKey || e.altKey || document.querySelector(".dialog-backdrop")) return;
      if (e.key === "?") {
        e.preventDefault();
        setHelp(true);
      } else if (e.key === "t" && nav.kind === "project") {
        e.preventDefault();
        setMode("files");
      } else if (e.key === "/" && user) {
        e.preventDefault();
        setMode("all");
      }
    };
    window.addEventListener(OPEN, onOpen);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener(OPEN, onOpen);
      window.removeEventListener("keydown", onKey);
    };
  }, [nav.kind, user]);
  return createPortal(
    <>
      {mode && <Palette mode={mode} close={() => setMode(null)} />}
      {help && <Help close={() => setHelp(false)} />}
    </>,
    document.body,
  );
}

