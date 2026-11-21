import { useEffect, useId, useMemo, useRef, useState, type ReactElement } from "react";
import { createPortal } from "react-dom";
import { useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { CircleDot, FileSearch, FileText, FolderGit2, GitMerge, Search, Users, CornerDownLeft } from "lucide-react";
import { useDebounced } from "../components/ui";
import { api, qs, refLink, type S } from "../api/client";
import { useSession } from "../session";
import { useNav } from "./nav";
import { fetchers, keys, useGroups, useMyProjects } from "../prefetch";
import { useRecentProjects } from "./recent";
import { globalPages, groupItems, newItems, projectPages, type Item } from "./navItems";
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

/** `keys`: held together (+), or either of them (`or`). */
export const SHORTCUTS: { keys: string[]; join?: "or"; what: string; where: string }[] = [
  { keys: [MOD, "K"], what: "Search or go to", where: "Everywhere" },
  { keys: ["/"], what: "Search or go to", where: "Signed in" },
  { keys: ["t"], what: "Find a file in this project", where: "Project" },
  { keys: ["?"], what: "Show or hide keyboard shortcuts", where: "Everywhere" },
  { keys: ["j", "k"], join: "or", what: "Move down / up the file list", where: "Repository" },
  { keys: ["Enter"], what: "Open the selected entry", where: "Repository" },
  { keys: ["]", "["], join: "or", what: "Next / previous file", where: "Diffs" },
  { keys: ["n", "p"], join: "or", what: "Next / previous unresolved thread", where: "Merge request" },
  { keys: ["j", "k"], join: "or", what: "Move the line cursor (Shift: select lines)", where: "Merge request changes" },
  { keys: ["c"], what: "Comment on the selected lines", where: "Merge request changes" },
  { keys: ["Shift", "click"], what: "Select a line range (or drag over line numbers)", where: "File view, changes" },
  { keys: [MOD, "Enter"], what: "Send or save what you are writing", where: "Comments, descriptions" },
];

// ---------------------------------------------------------------- palette

type Mode = "all" | "files";
const OPEN = "forgust:palette";

export function openPalette(mode: Mode = "all") {
  window.dispatchEvent(new CustomEvent(OPEN, { detail: mode }));
}

/** `hint`: faint, on the right; `file`: a path, its folder faint. */
type Entry = { key: string; label: string; hint?: string; file?: boolean; icon: ReactElement; to: string };
/** A titled run of entries; `more` counts the matches left out. */
type Section = { key: string; title: string; entries: Entry[]; more?: number; moreHint?: string; start?: number };

/** At most this many of each, once something is typed (the file finder lists more). */
const CAP = { pages: 4, files: 5, finder: 50, projects: 6, groups: 4, found: 5 };

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

/** The best `cap` of `items` by `score` (below 0: left out), and how many more scored.
 *  Keeps only the best few while scanning: a short term matches most of a large repository. */
function best<T>(items: readonly T[], score: (t: T) => number, cap: number): { top: T[]; more: number } {
  const top: { it: T; s: number }[] = [];
  let hits = 0;
  for (const it of items) {
    const s = score(it);
    if (s < 0) continue;
    hits++;
    if (top.length === cap && s <= top[cap - 1].s) continue;
    let k = top.length;
    while (k > 0 && top[k - 1].s < s) k--;
    top.splice(k, 0, { it, s });
    if (top.length > cap) top.pop();
  }
  return { top: top.map((h) => h.it), more: hits - top.length };
}

/** The best `cap` of `items` for `term` (in their order when nothing is typed). One or
 *  two letters scattered through a path name nearly anything: those have to follow
 *  each other ("re" finds README.md, not partners). */
function ranked<T>(items: readonly T[], term: string, hay: (t: T) => string, cap: number): { top: T[]; more: number } {
  if (!term) return { top: items.slice(0, cap), more: Math.max(0, items.length - cap) };
  const lc = term.toLowerCase();
  return best(items, (it) => (term.length < 3 && !hay(it).toLowerCase().includes(lc) ? -1 : fuzzy(term, hay(it))), cap);
}

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** How well `term` (lower case) names a page: at the start of a word of its name, the
 *  earlier the better ("re" finds Releases, not Explore), else one of its other names
 *  ("mr" finds Merge requests; from 3 letters, their start: "webh" finds Integrations);
 *  below 0 when it does not. */
function pageScore(term: string, it: Item): number {
  const label = it.label.toLowerCase();
  const at = label.search(new RegExp(`(^|[^\\p{L}\\p{N}])${escapeRe(term)}`, "u"));
  if (at >= 0) return 1000 - at - label.length / 100;
  // the start of a word there, or of several ("deploy keys")
  const words = ` ${it.words ?? ""} `;
  return words.includes(` ${term} `) || (term.length >= 3 && words.includes(` ${term}`)) ? 500 - label.length / 100 : -1;
}

type Jump = { kind: "issue" | "mr"; path: string; iid: number };

/** `#12` or `!12` in the project shown, `group/project#12` anywhere: one issue or merge request. */
function jumpOf(term: string, project: S["ProjectDetail"] | null): Jump | null {
  const m = /^([\w.-]+(?:\/[\w.-]+)+)?([#!])(\d{1,9})$/.exec(term);
  const path = m ? (m[1] ?? project?.full_path) : undefined;
  return m && path ? { kind: m[2] === "#" ? "issue" : "mr", path, iid: Number(m[3]) } : null;
}

const pageEntry = (it: Item): Entry => ({ key: `n:${it.to}:${it.label}`, label: it.label, icon: <>{it.icon}</>, to: it.to });

/** A file's path, its folder faint: the folder is cut first when the row is short, so
 *  the name shows. */
function FilePath({ path }: { path: string }) {
  const cut = path.lastIndexOf("/") + 1;
  return (
    <span className="grow palette-path">
      {cut > 0 && <span className="palette-dir">{path.slice(0, cut)}</span>}
      <span className="palette-name">{path.slice(cut)}</span>
    </span>
  );
}

function Palette({ mode, close }: { mode: Mode; close: () => void }) {
  const nav = useNav();
  const navigate = useNavigate();
  const { user } = useSession();
  const [term, setTerm] = useState("");
  // the selected entry, by key: results that arrive later do not move it; null: the first
  const [selKey, setSelKey] = useState<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  // each section's title names it, by an id of its own (a title holds spaces)
  const uid = useId();
  const project = nav.kind === "project" ? nav.project : null;
  const ref = nav.kind === "project" ? (nav.ref ?? project?.default_branch ?? "main") : "";
  const inIde = nav.kind === "project" && !!nav.ide;
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

  // `#12`: the title of that issue, once typing settles (the hover cards' requests)
  const jump = mode === "all" ? jumpOf(settled, project) : null;
  const elsewhere = jump && jump.path !== project?.full_path ? jump.path : null;
  const resolved = useQuery({ queryKey: keys.pathResolve(elsewhere ?? ""), queryFn: () => fetchers.pathResolve(elsewhere!), enabled: !!elsewhere, staleTime: 60_000, retry: false });
  const jumpPid = !jump ? undefined : !elsewhere ? project?.id : resolved.data?.kind === "project" ? resolved.data.id : undefined;
  const item = useQuery<{ title: string }>({
    queryKey: jump?.kind === "mr" ? keys.mr(jumpPid ?? 0, jump.iid) : ["issue", jumpPid, jump?.iid],
    queryFn: () => (jump!.kind === "mr" ? fetchers.mr(jumpPid!, jump!.iid) : api.get<S["IssueDetail"]>(`/projects/${jumpPid}/issues/${jump!.iid}`)),
    enabled: !!jump && jumpPid != null,
    staleTime: 30_000,
    retry: false,
  });
  // what the answer says of what is typed now: its title, or that there is none
  const current = !!jump && settled === t;
  const jumpTitle = current ? item.data?.title : undefined;
  const jumpGone = current && (item.isError || resolved.isError || (!!resolved.data && resolved.data.kind !== "project"));

  // the pages of the project or group shown, and what can be made in it
  const here = useMemo(() => {
    if (nav.kind === "project") {
      const { top, all } = projectPages(nav.project, ref);
      return { title: nav.project.name, top, all: [...all, ...newItems(nav).filter((i) => i.here)] };
    }
    if (nav.kind === "group") {
      const pages = groupItems(nav.group);
      return { title: nav.group.name, top: pages, all: [...pages, ...newItems(nav).filter((i) => i.here)] };
    }
    return null;
  }, [nav, ref]);
  // the rest of the site: the main menu's pages, your settings, the admin area, and
  // what can be made anywhere
  const pages = useMemo(() => (user ? [...globalPages(user), ...newItems(nav).filter((i) => !i.here)] : []), [user, nav]);

  // the files on their own: a search response must not rank the whole repository again
  const fileHits = useMemo(() => {
    if (!project || !files.data || (mode === "all" && !t)) return null;
    return ranked(files.data.paths, t, (f) => f, mode === "files" ? CAP.finder : CAP.files);
  }, [project, files.data, t, mode]);

  const { sections, entries, searchAll, searchCode } = useMemo(() => {
    const out: Section[] = [];
    const push = (key: string, title: string, entries: Entry[], more?: number, moreHint?: string) => {
      if (entries.length) out.push({ key, title, entries, more, moreHint });
    };
    // from the web IDE, a file opens in it
    const kind = inIde ? "ide" : "blob";
    const fileEntry = (f: string): Entry => ({ key: `f:${f}`, label: f, file: true, icon: <FileText size={15} />, to: refLink(`/${project!.full_path}`, kind, ref, f) });
    const projectEntry = (p: { id: number; full_path: string }): Entry => ({ key: `p:${p.id}`, label: p.full_path, icon: <FolderGit2 size={15} />, to: `/${p.full_path}` });
    const groupEntry = (g: { id: number; full_path: string }): Entry => ({ key: `g:${g.id}`, label: g.full_path, icon: <Users size={15} />, to: `/${g.full_path}` });
    // your projects as they are now (renamed, moved, archived)
    const mine = new Map((projects.data ?? []).map((p) => [p.id, p]));
    const lc = t.toLowerCase();
    let searchAll: Entry | undefined;
    let searchCode: Entry | undefined;

    if (mode === "files") {
      if (fileHits) push("files", "Files", fileHits.top.map(fileEntry), fileHits.more, "keep typing to narrow");
    } else if (!t) {
      // nothing typed: where to go next, not every file of the repository
      if (here) push("here", here.title, here.top.map(pageEntry));
      // the sidebar's: archived ones left out
      const rec = recent
        .map((r) => mine.get(r.id) ?? r)
        .filter((r) => r.id !== project?.id && !("archived" in r && r.archived))
        .slice(0, 5);
      push("recent", "Recent projects", rec.map(projectEntry));
      if (!project) {
        const seen = new Set(rec.map((r) => r.id));
        push("projects", "Projects", (projects.data ?? []).filter((p) => !seen.has(p.id)).slice(0, 5).map(projectEntry));
        push("groups", "Groups", (groups.data ?? []).slice(0, 5).map(groupEntry));
      }
    } else {
      const typed = mode === "all" ? jumpOf(t, project) : null;
      if (typed && !jumpGone) {
        const sigil = typed.kind === "issue" ? "#" : "!";
        push("jump", "Go to", [
          {
            key: "jump",
            label: jumpTitle ?? `${typed.kind === "issue" ? "Issue" : "Merge request"} ${sigil}${typed.iid}`,
            hint: `${typed.path}${sigil}${typed.iid}`,
            icon: typed.kind === "issue" ? <CircleDot size={15} /> : <GitMerge size={15} />,
            to: `/${typed.path}/-/${typed.kind === "issue" ? "issues" : "merge_requests"}/${typed.iid}`,
          },
        ]);
      }
      if (here) {
        // a page by its name, not by letters scattered through "Code › Tags"
        const hits = best(here.all, (it) => pageScore(lc, it), CAP.pages);
        push("here", here.title, hits.top.map(pageEntry), hits.more);
      }
      const hits = best(pages, (it) => pageScore(lc, it), CAP.pages);
      push("pages", "Pages", hits.top.map(pageEntry), hits.more);
      if (fileHits) push("files", "Files", fileHits.top.map(fileEntry), fileHits.more, "press t for the file finder");
      // yours first, then those the search finds that you are not a member of
      const pr = ranked(projects.data ?? [], t, (p) => p.full_path, CAP.projects);
      // not the last term's answer while this one's comes: those would not match
      const others = t.length >= 2 && settled === t && !found.isPlaceholderData ? (found.data?.projects ?? []).filter((p) => !mine.has(p.id)) : [];
      const room = Math.max(0, CAP.projects - pr.top.length);
      push("projects", "Projects", [...pr.top, ...others.slice(0, room)].map(projectEntry), pr.more + Math.max(0, others.length - room));
      const gr = ranked(groups.data ?? [], t, (g) => g.full_path, CAP.groups);
      push("groups", "Groups", gr.top.map(groupEntry), gr.more);
      if (t.length >= 2 && found.data) {
        push(
          "issues",
          "Issues",
          (found.data.issues ?? []).slice(0, CAP.found).map((i) => ({ key: `i:${i.project_id}:${i.iid}`, label: i.title, hint: `${i.project_path}#${i.iid}`, icon: <CircleDot size={15} />, to: `/${i.project_path}/-/issues/${i.iid}` })),
        );
        push(
          "mrs",
          "Merge requests",
          (found.data.merge_requests ?? []).slice(0, CAP.found).map((m) => ({ key: `m:${m.project_id}:${m.iid}`, label: m.title, hint: `${m.project_path}!${m.iid}`, icon: <GitMerge size={15} />, to: `/${m.project_path}/-/merge_requests/${m.iid}` })),
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
      push("search", "Search", search);
    }

    let n = 0;
    for (const s of out) {
      s.start = n;
      n += s.entries.length;
    }
    return { sections: out, entries: out.flatMap((s) => s.entries), searchAll, searchCode };
  }, [t, mode, fileHits, projects.data, groups.data, found.data, found.isPlaceholderData, settled, project, ref, inIde, here, pages, recent, jumpTitle, jumpGone]);

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
  const searching = mode === "all" && t.length >= 2 && (settled !== t || found.isFetching || item.isFetching);
  const placeholder =
    mode === "files" && project
      ? `Find a file in ${project.name}…`
      : project
        ? "Search files, pages, projects, issues and merge requests…"
        : "Search pages, projects, groups, issues and merge requests…";

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
          {sections.map((s, si) => (
            <div key={s.key} role="group" aria-labelledby={`${uid}-${si}`} className="palette-group">
              <div className="palette-group-title" id={`${uid}-${si}`}>
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
                    {e.file ? <FilePath path={e.label} /> : <span className="grow ellipsis">{e.label}</span>}
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
            <kbd>Tab</kbd> section
          </span>
          <span>
            <kbd>↵</kbd> open
          </span>
          {searchAll && (
            <span>
              <kbd>⇧ ↵</kbd> search all
            </span>
          )}
          {searchCode && (
            <span>
              <kbd>{MOD} ↵</kbd> {searchCode.key === "code-here" ? "search code here" : "search code"}
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
        <div className="help-body">
          <table className="shortcuts">
            <tbody>
              {SHORTCUTS.map((s, i) => (
                <tr key={i}>
                  <td>
                    {s.keys.map((k, j) => (
                      <span key={j}>
                        {j > 0 && <span className="faint"> {s.join === "or" ? "/" : "+"} </span>}
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
    </div>
  );
}

/** Mount once: owns the palette, the help dialog, and the global keys. */
export function PaletteHost() {
  const [mode, setMode] = useState<Mode | null>(null);
  const [help, setHelp] = useState(false);
  const nav = useNav();
  const { user } = useSession();
  // what the key handler, registered once, reads when a key comes
  const now = useRef({ nav, user, help });
  now.current = { nav, user, help };
  useEffect(() => {
    const onOpen = (e: Event) => setMode((e as CustomEvent<Mode>).detail);
    const onKey = (e: KeyboardEvent) => {
      const { nav, user, help } = now.current;
      // Mod+K alone, and not one an editor took for itself (Mod+Shift+K deletes a line there)
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && !e.defaultPrevented && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setHelp(false);
        setMode((m) => (m ? null : "all"));
        return;
      }
      // in the web IDE, Mod+P finds a file, from the editor too (`t` is a letter there)
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && !e.defaultPrevented && e.key.toLowerCase() === "p" && nav.kind === "project" && nav.ide) {
        e.preventDefault();
        setHelp(false);
        setMode("files");
        return;
      }
      if (isTyping(e) || e.ctrlKey || e.metaKey || e.altKey) return;
      // the help open: "?" again, or Escape, closes it
      if (help) {
        if (e.key === "?" || e.key === "Escape") {
          e.preventDefault();
          setHelp(false);
        }
        return;
      }
      if (document.querySelector(".dialog-backdrop")) return;
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
  }, []);
  return createPortal(
    <>
      {mode && <Palette mode={mode} close={() => setMode(null)} />}
      {help && <Help close={() => setHelp(false)} />}
    </>,
    document.body,
  );
}
