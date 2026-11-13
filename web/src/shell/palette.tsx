import { useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { createPortal } from "react-dom";
import { useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { CircleDot, FileText, FolderGit2, GitMerge, Search, Users, CornerDownLeft } from "lucide-react";
import { useDebounced } from "../components/ui";
import { api, qs, type S } from "../api/client";
import { useSession } from "../session";
import { useNav } from "./nav";
import { useGroups } from "../prefetch";

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

function Palette({ mode, close }: { mode: Mode; close: () => void }) {
  const nav = useNav();
  const navigate = useNavigate();
  const { user } = useSession();
  const [term, setTerm] = useState("");
  const [sel, setSel] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const project = nav.kind === "project" ? nav.project : null;
  const ref = nav.kind === "project" ? (nav.ref ?? project?.default_branch ?? "main") : "";

  const files = useQuery({
    queryKey: ["files", project?.id, ref],
    queryFn: () => api.get<S["FileList"]>(`/projects/${project!.id}/repository/files${qs({ ref })}`),
    enabled: !!project && project.can_read_code && !project.empty_repo,
    staleTime: 60_000,
  });
  const projects = useQuery({
    // the dashboard's key: one request for both
    queryKey: ["my-projects", ""],
    queryFn: () => api.get<S["MyProject"][]>(`/user/projects${qs({ per_page: 100 })}`),
    enabled: !!user && mode === "all",
    staleTime: 60_000,
  });
  const groups = useGroups(!!user && mode === "all");
  // issues and merge requests across the instance, once typing settles
  const settled = useDebounced(term.trim(), 200);
  const found = useQuery({
    queryKey: ["palette-search", settled],
    queryFn: () => api.get<S["SearchResults"]>(`/search${qs({ q: settled })}`),
    enabled: mode === "all" && settled.length >= 2,
    staleTime: 30_000,
    placeholderData: (prev) => prev,
  });

  const entries = useMemo(() => {
    const out: (Entry & { score: number })[] = [];
    const t = term.trim();
    if (project && files.data) {
      for (const f of files.data.paths) {
        const s = fuzzy(t, f);
        if (s >= 0) out.push({ key: `f:${f}`, label: f, hint: "file", icon: <FileText size={15} />, to: `/${project.full_path}/-/blob/${ref}/${f}`, score: s + 10 });
      }
    }
    if (mode === "all") {
      for (const p of projects.data ?? []) {
        const s = fuzzy(t, p.full_path);
        if (s >= 0) out.push({ key: `p:${p.id}`, label: p.full_path, hint: "project", icon: <FolderGit2 size={15} />, to: `/${p.full_path}`, score: s + (t ? 0 : 50) });
      }
      for (const g of groups.data ?? []) {
        const s = fuzzy(t, g.full_path);
        if (s >= 0) out.push({ key: `g:${g.id}`, label: g.full_path, hint: "group", icon: <Users size={15} />, to: `/${g.full_path}`, score: s });
      }
    }
    out.sort((a, b) => b.score - a.score);
    const top = out.slice(0, 40);
    if (mode === "all" && t.length >= 2) {
      for (const i of found.data?.issues ?? [])
        top.push({ key: `i:${i.project_id}:${i.iid}`, label: `${i.title}`, hint: `${i.project_path}#${i.iid}`, icon: <CircleDot size={15} />, to: `/${i.project_path}/-/issues/${i.iid}`, score: 0 });
      for (const m of found.data?.merge_requests ?? [])
        top.push({ key: `m:${m.project_id}:${m.iid}`, label: `${m.title}`, hint: `${m.project_path}!${m.iid}`, icon: <GitMerge size={15} />, to: `/${m.project_path}/-/merge_requests/${m.iid}`, score: 0 });
      top.push({ key: "search", label: `Search everything for “${t}”`, hint: "search", icon: <Search size={15} />, to: `/search${qs({ q: t })}`, score: 0 });
    }
    return top;
  }, [term, files.data, projects.data, groups.data, found.data, project, ref, mode]);

  useEffect(() => setSel(0), [term]);
  useEffect(() => {
    listRef.current?.querySelector(`[data-i="${sel}"]`)?.scrollIntoView({ block: "nearest" });
  }, [sel]);

  const go = (e?: Entry) => {
    if (!e) return;
    close();
    navigate(e.to);
  };
  const loading = files.isLoading || (mode === "all" && projects.isLoading);
  const placeholder = mode === "files" && project ? `Find a file in ${project.name}…` : project ? "Search files, projects, issues and merge requests…" : "Search projects, groups, issues and merge requests…";

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
              setSel((s) => Math.min(s + 1, entries.length - 1));
            } else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) {
              e.preventDefault();
              setSel((s) => Math.max(s - 1, 0));
            } else if (e.key === "Enter") {
              e.preventDefault();
              go(entries[sel]);
            }
          }}
        />
        <div className="palette-list" id="palette-list" role="listbox" ref={listRef}>
          {entries.map((e, i) => (
            <button
              key={e.key}
              id={`pal-${i}`}
              data-i={i}
              role="option"
              aria-selected={i === sel}
              className={`palette-item ${i === sel ? "active" : ""}`}
              onMouseMove={() => setSel(i)}
              onClick={() => go(e)}
            >
              {e.icon}
              <span className="grow ellipsis mono-ish">{e.label}</span>
              {i === sel ? <CornerDownLeft size={14} className="faint" /> : <span className="faint small">{e.hint}</span>}
            </button>
          ))}
          {entries.length === 0 && <div className="palette-empty muted">{loading ? "Loading…" : "No matches"}</div>}
          {files.data?.truncated && <div className="palette-empty faint small">Only the first {files.data.paths.length} files are searchable.</div>}
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

