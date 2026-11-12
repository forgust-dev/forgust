import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useWindowVirtualizer } from "@tanstack/react-virtual";
import { ChevronDown, ChevronRight, ChevronsDownUp, ChevronsUpDown, Columns2, FileDiff, FolderTree, Rows2, WrapText } from "lucide-react";
import { api, ApiError, type S } from "../../api/client";
import { ErrorBox, Spinner } from "../../components/ui";
import { columns, HBar, hsVar, remeasure, useCharWidth, useHScroll, useWidth } from "../../components/hscroll";
import { usePref } from "../../shell/theme";
import { useShortcuts } from "../../shell/palette";

/** Where a comment sits: new-side lines use `n`, removed lines use `o`. */
export type LineKey = string;
export const lineKey = (path: string, side: "n" | "o", line: number): LineKey => `${path}\u0000${side}\u0000${line}`;

export type NewComment = {
  old_path: string;
  new_path: string;
  old_line?: number;
  new_line?: number;
  /** a comment on several lines: where they start */
  start_old_line?: number;
  start_new_line?: number;
  /** the new-side text of the lines (to start a suggestion) */
  text?: string;
  /** new-side lines above the last one, for ```suggestion:-N+0 */
  above?: number;
};

/** Review mode: threads placed on lines, and a "+" to start one. */
export type Review = {
  threads: Map<LineKey, ReactNode>;
  /** Keys holding at least one unresolved thread, for `n` / `p`. */
  unresolved?: Set<LineKey>;
  composer: (key: LineKey, pos: NewComment, close: () => void) => ReactNode;
};

type L = S["DiffLine"];
type F = S["DiffFile"];
type Side = { line: L; key: LineKey | null };

type Row = { id: string; fi: number } & (
  | { kind: "file"; file: F }
  | { kind: "note"; text: string }
  | { kind: "hunk"; text: string }
  | { kind: "line"; line: L; file: F; key: LineKey | null }
  | { kind: "pair"; left: Side | null; right: Side | null; file: F }
  | { kind: "thread"; key: LineKey }
  | { kind: "composer"; key: LineKey; pos: NewComment }
  | { kind: "bar" }
  | { kind: "gap" }
);

const TOPBAR = 56;
const SCROLL_KEYS = new Set(["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "]);

const HEIGHT: Record<Row["kind"], number> = { file: 40, note: 40, hunk: 20, line: 20, pair: 20, thread: 120, composer: 140, bar: 14, gap: 20 };
/** Gutter widths, in px (see .diff-line .n / .s). */
const NUM = 48;
const SIGN = 20;
const PAD = 24;

function label(f: F) {
  if (f.status === "renamed") return `${f.old_path} → ${f.new_path}`;
  return f.new_path || f.old_path;
}

function keyOf(f: F, l: L): LineKey | null {
  if (l.new != null) return lineKey(f.new_path || f.old_path, "n", l.new);
  if (l.old != null) return lineKey(f.old_path || f.new_path, "o", l.old);
  return null;
}

type Composing = { key: LineKey; pos: NewComment } | null;

/** Flatten the diff into rows so thousands of lines can be virtualized. */
function rows(diff: S["Diff"], split: boolean, review?: Review, composing?: Composing, collapsed?: Set<number>, bars?: Set<number>): Row[] {
  const out: Row[] = [];
  const extras = (keys: (LineKey | null)[], fi: number) => {
    for (const key of keys) {
      if (key && review?.threads.has(key)) out.push({ kind: "thread", id: `t${key}`, key, fi });
      if (key && composing?.key === key) out.push({ kind: "composer", id: `c${key}`, key, pos: composing.pos, fi });
    }
  };
  diff.files.forEach((f, fi) => {
    out.push({ kind: "file", id: `f${fi}`, file: f, fi });
    if (collapsed?.has(fi)) {
      out.push({ kind: "gap", id: `g${fi}`, fi });
      return;
    }
    const note = (text: string) => out.push({ kind: "note", id: `n${fi}`, text, fi });
    if (f.binary) note("Binary file");
    else if (f.collapsed) note("Large diff: not shown. Use git locally or the raw view.");
    else if (f.hunks.length === 0) note(f.status === "renamed" ? "File renamed without changes" : "No content changes");
    f.hunks.forEach((h, hi) => {
      const at = `${fi}:${hi}`;
      out.push({ kind: "hunk", id: `h${at}`, text: h.header, fi });
      if (!split) {
        h.lines.forEach((l, li) => {
          const key = keyOf(f, l);
          out.push({ kind: "line", id: `l${at}:${li}`, line: l, file: f, key, fi });
          extras([key], fi);
        });
        return;
      }
      // split: pair each run of removals with the following run of additions
      const ls = h.lines;
      for (let i = 0; i < ls.length; ) {
        const l = ls[i];
        if (l.kind === "del" || l.kind === "add") {
          const first = i;
          const dels: L[] = [];
          const adds: L[] = [];
          while (i < ls.length && ls[i].kind === "del") dels.push(ls[i++]);
          while (i < ls.length && ls[i].kind === "add") adds.push(ls[i++]);
          for (let k = 0; k < Math.max(dels.length, adds.length); k++) {
            const left = dels[k] ? { line: dels[k], key: keyOf(f, dels[k]) } : null;
            const right = adds[k] ? { line: adds[k], key: keyOf(f, adds[k]) } : null;
            out.push({ kind: "pair", id: `p${at}:${first}:${k}`, left, right, file: f, fi });
            extras([left?.key ?? null, right?.key ?? null], fi);
          }
        } else if (l.kind === "nonewline") {
          out.push({ kind: "line", id: `l${at}:${i}`, line: l, file: f, key: null, fi });
          i++;
        } else {
          const key = keyOf(f, l);
          const side = { line: l, key };
          out.push({ kind: "pair", id: `p${at}:${i}`, left: side, right: side, file: f, fi });
          extras([key], fi);
          i++;
        }
      }
    });
    // files wider than the window get their own scrollbar under their last line
    if (bars?.has(fi)) out.push({ kind: "bar", id: `b${fi}`, fi });
    out.push({ kind: "gap", id: `g${fi}`, fi });
  });
  return out;
}

function Code({ l }: { l: L }) {
  return l.html != null ? <span className="code-text hs-x" dangerouslySetInnerHTML={{ __html: l.html || " " }} /> : <span className="code-text hs-x">{l.text}</span>;
}

const sign = (l: L) => (l.kind === "add" ? "+" : l.kind === "del" ? "−" : "");

/** Display width of each line, computed once per line object. */
const widths = new WeakMap<L, number>();
const cols = (l: L) => {
  let w = widths.get(l);
  if (w == null) widths.set(l, (w = columns(l.text)));
  return w;
};

/** File list on the side of a diff: every changed file, even before its page loads. */
function FileTree({ diff, current, go }: { diff: S["Diff"]; current: number; go: (i: number) => void }) {
  const list = diff.summary.length ? diff.summary : diff.files.map((f) => ({ old_path: f.old_path, new_path: f.new_path, additions: f.additions, deletions: f.deletions }));
  return (
    <nav className="diff-tree" aria-label="Changed files">
      {list.map((f, i) => {
        const path = f.new_path || f.old_path;
        const slash = path.lastIndexOf("/");
        return (
          <button key={i} className={`diff-tree-item ${i === current ? "active" : ""}`} onClick={() => go(i)} title={path}>
            <FileDiff size={14} className="faint" />
            <span className="grow ellipsis">
              <span>{path.slice(slash + 1)}</span>
              {slash > 0 && <span className="faint small"> {path.slice(0, slash)}</span>}
            </span>
            {f.additions != null && <span className="stat-add small">+{f.additions}</span>}
            {f.deletions != null && <span className="stat-del small">−{f.deletions}</span>}
          </button>
        );
      })}
    </nav>
  );
}

export function DiffView({ diff, onMore, review, loadUntil }: { diff: S["Diff"]; onMore?: () => void; review?: Review; loadUntil?: (fileIndex: number) => void }) {
  const [composing, setComposing] = useState<Composing>(null);
  const [split, setSplit] = usePref("diff.split", false);
  const [wrapPref, setWrap] = usePref("diff.wrap", true);
  const [showTree, setShowTree] = usePref("diff.tree", true);
  const [collapsed, setCollapsed] = useState<Set<number>>(() => new Set());
  const narrow = useNarrow();
  const isSplit = split && !narrow;
  // phones always wrap: there is no room to scroll sideways through code
  const wrap = wrapPref || narrow;
  const ref = useRef<HTMLDivElement>(null);
  const mainRef = useRef<HTMLDivElement>(null);
  const width = useWidth(ref);
  const ch = useCharWidth();
  // the code column: what is left of a line (or half a line) after its gutters
  const codePx = Math.max(0, (isSplit ? (width - 1) / 2 - NUM : width - 2 * NUM) - SIGN);
  // no wrap: each file scrolls sideways on its own, as far as its longest line
  const widest = useMemo(() => diff.files.map((f) => f.hunks.reduce((m, h) => h.lines.reduce((mm, l) => Math.max(mm, cols(l)), m), 0)), [diff]);
  const ranges = useMemo(() => widest.map((w) => (wrap || width === 0 ? 0 : Math.max(0, Math.ceil(w * ch + PAD - codePx)))), [widest, wrap, width, ch, codePx]);
  const hs = useHScroll(mainRef, ranges);
  const bars = useMemo(() => new Set(ranges.flatMap((r, fi) => (r > 0 ? [fi] : []))), [ranges]);
  const all = useMemo(() => rows(diff, isSplit, review, composing, collapsed, bars), [diff, isSplit, review, composing, collapsed, bars]);
  const fileRow = useMemo(() => {
    const m: number[] = [];
    all.forEach((r, i) => r.kind === "file" && (m[r.fi] = i));
    return m;
  }, [all]);
  const perRow = Math.max(8, Math.floor((codePx - PAD) / ch));
  const lines = (l: L | undefined) => (l ? Math.max(1, Math.ceil(cols(l) / perRow)) : 1);
  const estimate = useCallback(
    (i: number) => {
      const r = all[i];
      if (!wrap) return HEIGHT[r.kind];
      if (r.kind === "line") return 20 * lines(r.line);
      if (r.kind === "pair") return 20 * Math.max(lines(r.left?.line), lines(r.right?.line));
      return HEIGHT[r.kind];
    },
    [all, wrap, perRow],
  );
  const getItemKey = useCallback((i: number) => all[i].id, [all]);
  const v = useWindowVirtualizer({
    count: all.length,
    estimateSize: estimate,
    getItemKey,
    overscan: 40,
    scrollMargin: ref.current?.offsetTop ?? 0,
    scrollPaddingStart: 60,
  });
  // wrapped heights depend on the width: measure again when it changes
  useEffect(() => remeasure(v, ref.current), [wrap, perRow, isSplit]);

  const items = v.getVirtualItems();
  // the file whose rows are at the top of the viewport
  const viewTop = window.scrollY + TOPBAR;
  const topIndex = items.find((vi) => vi.end > viewTop + 8)?.index ?? 0;
  // a file picked in the list stays current until the reader scrolls (the
  // last files can't reach the top of the viewport)
  const [picked, setPicked] = useState<number | null>(null);
  useEffect(() => {
    if (picked == null) return;
    const clear = () => setPicked(null);
    const onKey = (e: KeyboardEvent) => SCROLL_KEYS.has(e.key) && clear();
    const opts = { passive: true } as const;
    window.addEventListener("wheel", clear, opts);
    window.addEventListener("touchmove", clear, opts);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("wheel", clear);
      window.removeEventListener("touchmove", clear);
      window.removeEventListener("keydown", onKey);
    };
  }, [picked]);
  const current = picked ?? all[topIndex]?.fi ?? 0;
  // overlay the current file's name once its own header has scrolled away
  const headerVisible = items.some((vi) => all[vi.index].kind === "file" && all[vi.index].fi === current && vi.start >= viewTop - 1);
  // the file at the bottom of the window: while its own scrollbar (under its
  // last line) is below the window, a copy floats at the bottom
  const viewBottom = window.scrollY + window.innerHeight;
  const low = items.find((vi) => vi.start <= viewBottom && vi.end > viewBottom);
  const lowFile = low ? all[low.index].fi : -1;
  const lowBar = useMemo(() => {
    const m: number[] = [];
    all.forEach((r, i) => r.kind === "bar" && (m[r.fi] = i));
    return m;
  }, [all]);
  const floating = lowFile >= 0 && lowBar[lowFile] != null && (v.measurementsCache[lowBar[lowFile]]?.end ?? 0) > viewBottom;
  const shown = diff.offset + diff.files.length;

  const [pending, setPending] = useState<number | null>(null);
  const goFile = (i: number) => {
    if (i < 0) return;
    setPicked(i);
    if (fileRow[i] != null) v.scrollToIndex(fileRow[i], { align: "start" });
    else if (loadUntil && i < diff.total_files) {
      setPending(i);
      loadUntil(i);
    }
  };
  useEffect(() => {
    if (pending != null && fileRow[pending] != null) {
      v.scrollToIndex(fileRow[pending], { align: "start" });
      setPending(null);
    }
  }, [pending, fileRow]);

  const toggleFile = (fi: number) => {
    const next = new Set(collapsed);
    if (next.has(fi)) next.delete(fi);
    else next.add(fi);
    setCollapsed(next);
    // keep the header in place when the file above the viewport folds
    if (fileRow[fi] != null && (v.measurementsCache[fileRow[fi]]?.start ?? 0) < window.scrollY + TOPBAR - v.options.scrollMargin)
      requestAnimationFrame(() => v.scrollToIndex(fileRow[fi], { align: "start" }));
  };
  const allCollapsed = diff.files.length > 0 && collapsed.size >= diff.files.length;

  const threadRows = useMemo(
    () => all.flatMap((r, i) => (r.kind === "thread" && (!review?.unresolved || review.unresolved.has(r.key)) ? [i] : [])),
    [all, review],
  );
  const goThread = (dir: 1 | -1) => {
    if (!threadRows.length) return;
    const next = dir === 1 ? (threadRows.find((i) => i > topIndex + 1) ?? threadRows[0]) : ([...threadRows].reverse().find((i) => i < topIndex) ?? threadRows[threadRows.length - 1]);
    v.scrollToIndex(next, { align: "center" });
  };

  const startComment = (f: F, l: L, key: LineKey) => {
    const pos: NewComment = { old_path: f.old_path || f.new_path, new_path: f.new_path || f.old_path };
    if (l.new != null) {
      pos.new_line = l.new;
      pos.text = l.text;
    } else if (l.old != null) pos.old_line = l.old;
    setComposing({ key, pos });
  };

  // ---- line selection: a cursor moved with j/k, extended with J/K or by
  // dragging over line numbers; `c` comments on it. Rows are kept by id (a
  // composer or thread row shifts the indexes).
  const [sel, setSel] = useState<{ from: string; to: string } | null>(null);
  const rowIndex = useMemo(() => new Map(all.map((r, i) => [r.id, i])), [all]);
  const pick = (i: number): { file: F; line: L; key: LineKey } | null => {
    const r = all[i];
    if (!r) return null;
    if (r.kind === "line") return r.key && r.line.kind !== "nonewline" ? { file: r.file, line: r.line, key: r.key } : null;
    if (r.kind === "pair") {
      const s = r.right?.key ? r.right : r.left?.key ? r.left : null;
      return s ? { file: r.file, line: s.line, key: s.key! } : null;
    }
    return null;
  };
  const selRange = (() => {
    if (!sel) return null;
    const a = rowIndex.get(sel.from);
    const b = rowIndex.get(sel.to);
    if (a == null || b == null) return null;
    return { lo: Math.min(a, b), hi: Math.max(a, b), cursor: b, fi: all[b].fi };
  })();
  const commentRange = (lo: number, hi: number) => {
    const picks: { file: F; line: L; key: LineKey }[] = [];
    for (let i = lo; i <= hi; i++) {
      const p = pick(i);
      if (p && all[i].fi === all[hi].fi) picks.push(p);
    }
    const last = picks[picks.length - 1];
    if (!last) return;
    const f = last.file;
    const pos: NewComment = { old_path: f.old_path || f.new_path, new_path: f.new_path || f.old_path };
    const news = picks.filter((p) => p.line.new != null && p.line.kind !== "del");
    if (last.line.new != null) {
      pos.new_line = last.line.new;
      if (news.length > 1) pos.start_new_line = news[0].line.new!;
      pos.text = news.map((p) => p.line.text).join("\n");
      pos.above = Math.max(0, news.length - 1);
    } else if (last.line.old != null) {
      pos.old_line = last.line.old;
      const olds = picks.filter((p) => p.line.old != null);
      if (olds.length > 1) pos.start_old_line = olds[0].line.old!;
    }
    setComposing({ key: last.key, pos });
  };
  const step = (from: number, dir: 1 | -1) => {
    for (let i = from + dir; i >= 0 && i < all.length; i += dir) if (pick(i)) return i;
    return null;
  };
  const moveCursor = (dir: 1 | -1, extend: boolean) => {
    if (!review) return false;
    if (!selRange) {
      const start = pick(topIndex) ? topIndex : step(topIndex, 1);
      if (start == null) return;
      setSel({ from: all[start].id, to: all[start].id });
      v.scrollToIndex(start, { align: "auto" });
      return;
    }
    const n = step(selRange.cursor, dir);
    if (n == null) return;
    if (extend && all[n].fi !== selRange.fi) return; // a range stays in one file
    setSel(extend ? { from: sel!.from, to: all[n].id } : { from: all[n].id, to: all[n].id });
    v.scrollToIndex(n, { align: "auto" });
  };
  const dragging = useRef(false);
  useEffect(() => {
    const up = () => {
      if (!dragging.current) return;
      dragging.current = false;
      if (selRange && selRange.lo !== selRange.hi) commentRange(selRange.lo, selRange.hi);
    };
    window.addEventListener("mouseup", up);
    return () => window.removeEventListener("mouseup", up);
  });
  const gutterDown = (e: React.MouseEvent, i: number) => {
    if (!review || e.button !== 0 || (e.target as HTMLElement).closest("button") || !pick(i)) return;
    e.preventDefault(); // no text selection while dragging
    dragging.current = true;
    setSel(e.shiftKey && sel ? { from: sel.from, to: all[i].id } : { from: all[i].id, to: all[i].id });
  };
  const rowEnter = (i: number) => {
    if (dragging.current && sel && pick(i) && all[i].fi === selRange?.fi) setSel({ from: sel.from, to: all[i].id });
  };
  const selClass = (i: number) =>
    selRange && i >= selRange.lo && i <= selRange.hi && all[i].fi === selRange.fi ? (i === selRange.cursor ? " selected cursor" : " selected") : "";
  const plus = (f: F, s: Side) =>
    review && s.key ? (
      <button className="add-comment" title="Comment on this line" aria-label="Comment on this line" onClick={() => startComment(f, s.line, s.key!)}>
        +
      </button>
    ) : null;

  useShortcuts({
    "]": () => goFile(Math.min(current + 1, diff.total_files - 1)),
    "[": () => goFile(Math.max(current - 1, 0)),
    n: () => (review ? goThread(1) : false),
    p: () => (review ? goThread(-1) : false),
    j: () => moveCursor(1, false),
    k: () => moveCursor(-1, false),
    J: () => moveCursor(1, true),
    K: () => moveCursor(-1, true),
    c: () => (review && selRange ? commentRange(selRange.lo, selRange.hi) : false),
    Escape: () => (sel ? setSel(null) : false),
  });

  const cur = diff.files[current];
  return (
    <div className="stack">
      <div className="row wrap small diff-toolbar">
        {!narrow && (
          <button className="btn small ghost" onClick={() => setShowTree(!showTree)} aria-pressed={showTree} title="Show the file list">
            <FolderTree size={15} /> Files
          </button>
        )}
        <span className="muted">
          {diff.total_files} file{diff.total_files === 1 ? "" : "s"} changed
        </span>
        <span className="stat-add">+{diff.additions}</span>
        <span className="stat-del">−{diff.deletions}</span>
        <div className="spacer" />
        <span className="faint hide-narrow">
          <kbd>[</kbd> <kbd>]</kbd> files
          {review ? (
            <>
              {" "}
              · <kbd>n</kbd> <kbd>p</kbd> threads · <kbd>j</kbd> <kbd>k</kbd> lines · <kbd>c</kbd> comment
            </>
          ) : null}
        </span>
        <button className="btn small ghost" onClick={() => setCollapsed(allCollapsed ? new Set() : new Set(diff.files.map((_, i) => i)))} title={allCollapsed ? "Expand every file" : "Collapse every file"}>
          {allCollapsed ? <ChevronsUpDown size={15} /> : <ChevronsDownUp size={15} />} {allCollapsed ? "Expand all" : "Collapse all"}
        </button>
        {!narrow && (
          <button className="btn small ghost" onClick={() => setWrap(!wrapPref)} aria-pressed={wrapPref} title="Wrap long lines">
            <WrapText size={15} /> Wrap
          </button>
        )}
        {!narrow && (
          <div className="seg" role="group" aria-label="Diff layout">
            <button className={!split ? "active" : ""} onClick={() => setSplit(false)} aria-pressed={!split}>
              <Rows2 size={14} /> Unified
            </button>
            <button className={split ? "active" : ""} onClick={() => setSplit(true)} aria-pressed={split}>
              <Columns2 size={14} /> Split
            </button>
          </div>
        )}
      </div>
      <div className={`diff-layout ${showTree && !narrow ? "with-tree" : ""}`}>
        {showTree && !narrow && <FileTree diff={diff} current={current} go={goFile} />}
        <div className="diff-main" ref={mainRef}>
          <div className="diff-sticky">
            {cur && !headerVisible && (
              <div className="diff-head">
                <button className="diff-fold" onClick={() => toggleFile(current)} aria-label={collapsed.has(current) ? "Expand file" : "Collapse file"}>
                  {collapsed.has(current) ? <ChevronRight size={15} /> : <ChevronDown size={15} />}
                </button>
                <span className="grow ellipsis">{label(cur)}</span>
                <span className="stat-add">+{cur.additions}</span>
                <span className="stat-del">−{cur.deletions}</span>
              </div>
            )}
          </div>
          <div ref={ref} className={`diff-rows ${isSplit ? "diff-split" : ""} ${wrap ? "diff-wrap" : "diff-nowrap"}`} style={{ height: v.getTotalSize() }}>
            {items.map((vi) => {
              const r = all[vi.index];
              // wrapped lines, threads and composers vary in height: measure them
              const measured = r.kind === "thread" || r.kind === "composer" || (wrap && (r.kind === "line" || r.kind === "pair"));
              const style: React.CSSProperties = {
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                height: measured ? undefined : HEIGHT[r.kind],
                transform: `translateY(${vi.start - v.options.scrollMargin}px)`,
                ...hsVar(r.fi),
              };
              const common = { "data-index": vi.index, "data-hg": r.fi, ref: measured ? v.measureElement : undefined, style };
              if (r.kind === "file") {
                const folded = collapsed.has(r.fi);
                return (
                  <div key={vi.key} {...common} data-file={r.fi}>
                    <div className={`diff-head ${folded ? "folded" : ""}`}>
                      <button className="diff-fold" onClick={() => toggleFile(r.fi)} aria-expanded={!folded} aria-label={folded ? "Expand file" : "Collapse file"}>
                        {folded ? <ChevronRight size={15} /> : <ChevronDown size={15} />}
                      </button>
                      <span className="grow ellipsis" title={label(r.file)}>
                        {label(r.file)}
                      </span>
                      <span className="badge">{r.file.status.replace("_", " ")}</span>
                      <span className="stat-add">+{r.file.additions}</span>
                      <span className="stat-del">−{r.file.deletions}</span>
                    </div>
                  </div>
                );
              }
              if (r.kind === "note")
                return (
                  <div key={vi.key} {...common} className="diff-note muted small">
                    {r.text}
                  </div>
                );
              if (r.kind === "bar")
                return (
                  <div key={vi.key} {...common} className="diff-bar-row">
                    <HBar ctl={hs} g={r.fi} view={codePx} />
                  </div>
                );
              if (r.kind === "gap")
                return (
                  <div key={vi.key} {...common}>
                    {!collapsed.has(r.fi) && <div className="diff-end" />}
                  </div>
                );
              if (r.kind === "thread")
                return (
                  <div key={vi.key} {...common}>
                    <div className="thread-row">{review!.threads.get(r.key)}</div>
                  </div>
                );
              if (r.kind === "composer")
                return (
                  <div key={vi.key} {...common}>
                    <div className="thread-row">{review!.composer(r.key, r.pos, () => setComposing(null))}</div>
                  </div>
                );
              if (r.kind === "hunk")
                return (
                  <div key={vi.key} {...common} className="diff-line hunk">
                    <span className="n" />
                    {!isSplit && <span className="n" />}
                    <span className="t">{r.text}</span>
                  </div>
                );
              if (r.kind === "pair") {
                const half = (s: Side | null, side: "old" | "new") => {
                  if (!s) return <div className="half empty" />;
                  const k = s.line.kind;
                  const cls = k === "add" ? "add" : k === "del" ? "del" : "";
                  return (
                    <div className={`half ${cls}`}>
                      <span className="n">
                        {plus(r.file, s)}
                        {side === "old" ? (s.line.old ?? "") : (s.line.new ?? "")}
                      </span>
                      <span className="s">{sign(s.line)}</span>
                      <span className="t">
                        <Code l={s.line} />
                      </span>
                    </div>
                  );
                };
                return (
                  <div key={vi.key} {...common} className={`diff-line pair${selClass(vi.index)}`} onMouseDown={(e) => (e.target as HTMLElement).closest(".n") && gutterDown(e, vi.index)} onMouseEnter={() => rowEnter(vi.index)}>
                    {half(r.left, "old")}
                    {half(r.right, "new")}
                  </div>
                );
              }
              const l = r.line;
              const cls = l.kind === "add" ? "add" : l.kind === "del" ? "del" : l.kind === "nonewline" ? "eof" : "";
              return (
                <div key={vi.key} {...common} className={`diff-line ${cls}${selClass(vi.index)}`} onMouseDown={(e) => (e.target as HTMLElement).closest(".n") && gutterDown(e, vi.index)} onMouseEnter={() => rowEnter(vi.index)}>
                  <span className="n">{l.old ?? ""}</span>
                  <span className="n">
                    {r.key && plus(r.file, { line: l, key: r.key })}
                    {l.new ?? ""}
                  </span>
                  <span className="s">{sign(l)}</span>
                  <span className="t">
                    <Code l={l} />
                  </span>
                </div>
              );
            })}
          </div>
          <div className="diff-sticky-bottom">{floating && <HBar ctl={hs} g={lowFile} view={codePx} className="floating" />}</div>
          {shown < diff.total_files && onMore && (
            <button className="btn" style={{ marginTop: 12 }} onClick={onMore}>
              Show next files ({diff.total_files - shown} more)
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

function useNarrow() {
  const q = "(max-width: 760px)";
  const [narrow, setNarrow] = useState(() => typeof window !== "undefined" && window.matchMedia(q).matches);
  useEffect(() => {
    const m = window.matchMedia(q);
    const on = () => setNarrow(m.matches);
    m.addEventListener("change", on);
    return () => m.removeEventListener("change", on);
  }, []);
  return narrow;
}

/** Load a diff page by page (the server caps files per page) and show them together. */
export function PagedDiff({ url, pick = (v: any) => v as S["Diff"], review }: { url: (offset: number) => string; pick?: (v: any) => S["Diff"]; review?: Review }) {
  const [diff, setDiff] = useState<S["Diff"] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(false);
  const gen = useRef(0);
  const busy = useRef(false);
  const load = async (offset: number, until?: number) => {
    // a fresh load (offset 0) always wins; "more" pages wait for the current one
    if (offset > 0 && busy.current) return;
    const mine = offset === 0 ? ++gen.current : gen.current;
    busy.current = true;
    setLoading(true);
    try {
      let off = offset;
      let acc = null as S["Diff"] | null;
      // keep paging until file `until` is loaded (file-tree jumps)
      do {
        const page = pick(await api.get<unknown>(url(off)));
        if (mine !== gen.current) return; // the URL changed meanwhile
        acc = acc ? { ...acc, files: [...acc.files, ...page.files] } : page;
        off += page.files.length;
        if (page.files.length === 0) break;
      } while (until != null && off <= until && off < acc.total_files);
      const got = acc!;
      setDiff((prev) => (prev && offset > 0 ? { ...got, offset: 0, summary: prev.summary, files: [...prev.files, ...got.files] } : got));
    } catch (e) {
      if (mine === gen.current) setError(e);
    } finally {
      if (mine === gen.current) {
        busy.current = false;
        setLoading(false);
      }
    }
  };
  useEffect(() => {
    setDiff(null);
    setError(null);
    load(0);
  }, [url(0)]);
  if (error) return error instanceof ApiError && error.status === 404 ? <div className="empty">Not found.</div> : <ErrorBox error={error} />;
  if (!diff) return <Spinner />;
  return (
    <DiffView
      diff={diff}
      onMore={loading ? undefined : () => load(diff.files.length)}
      loadUntil={(i) => load(diff.files.length, i)}
      review={review}
    />
  );
}
