import { useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";

/**
 * Horizontal scrolling for long code lines in virtualized lists.
 *
 * A virtualized list is as tall as the whole file, so a native horizontal
 * scrollbar on it sits at the very bottom, out of reach. Instead the code
 * column is clipped and shifted by `--sx` (elements with `.hs-x`), driven by
 * a scrollbar pinned to the bottom of the window, the trackpad / shift+wheel,
 * and horizontal swipes. Line numbers stay put.
 */

const FONT_PX = 12.5;
let cachedCh = 0;

function measureCh(): number {
  const mono = getComputedStyle(document.documentElement).getPropertyValue("--mono").trim() || "monospace";
  const c = document.createElement("canvas").getContext("2d");
  if (!c) return FONT_PX * 0.6;
  c.font = `${FONT_PX}px ${mono}`;
  return c.measureText("0".repeat(100)).width / 100;
}

/** Width of one column of the code font (12.5px monospace), once the font is loaded. */
export function useCharWidth(): number {
  const [ch, setCh] = useState(() => cachedCh || FONT_PX * 0.6);
  useEffect(() => {
    let live = true;
    const mono = getComputedStyle(document.documentElement).getPropertyValue("--mono").trim();
    const ready = document.fonts?.load ? document.fonts.load(`${FONT_PX}px ${mono}`).catch(() => null) : Promise.resolve(null);
    ready.then(() => {
      cachedCh = measureCh();
      if (live) setCh(cachedCh);
    });
    return () => {
      live = false;
    };
  }, []);
  return ch;
}

/** Display columns of a line: tabs to the next multiple of 4, wide characters count twice. */
export function columns(text: string): number {
  let col = 0;
  for (const chr of text) {
    if (chr === "\t") col += 4 - (col % 4);
    else {
      const cp = chr.codePointAt(0)!;
      col += (cp >= 0x1100 && cp <= 0x115f) || (cp >= 0x2e80 && cp <= 0xa4cf) || (cp >= 0xac00 && cp <= 0xd7a3) || (cp >= 0xf900 && cp <= 0xfaff) || (cp >= 0xfe30 && cp <= 0xfe4f) || (cp >= 0xff00 && cp <= 0xff60) || cp >= 0x1f300 ? 2 : 1;
    }
  }
  return col;
}

/** Text of a line of highlighted HTML, for measuring. */
export function htmlText(html: string): string {
  return html.replace(/<[^>]*>/g, "").replace(/&(#\d+|#x[0-9a-f]+|\w+);/gi, "x");
}

/**
 * Drop a virtualizer's sizes (they came from another width or layout) and
 * measure the rows on screen again: their own size may not have changed, so
 * nothing else would tell the virtualizer.
 */
export function remeasure(v: { measure: () => void; getVirtualItems: () => unknown; measureElement: (el: Element | null) => void }, root: HTMLElement | null) {
  v.measure();
  // rebuild the measurements from the estimates first, or the rows below
  // would be compared with their old sizes and skipped
  v.getVirtualItems();
  root?.querySelectorAll("[data-index]").forEach((el) => v.measureElement(el));
}

/** The element's content width, kept up to date. */
export function useWidth(ref: RefObject<HTMLElement | null>): number {
  const [w, setW] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setW(el.clientWidth);
    const ro = new ResizeObserver(() => setW(el.clientWidth));
    ro.observe(el);
    return () => ro.disconnect();
  }, [ref]);
  return w;
}

export type HScroll = {
  /** Move group `g` (a file) to `x` px. */
  set: (g: number, x: number) => void;
  get: (g: number) => number;
  range: (g: number) => number;
};

/**
 * Sideways scrolling, one offset per group (a file of a diff). Rows of group
 * `g` carry `data-hg={g}` and `style={hsVar(g)}`; `scope` contains them and
 * every bar. Gestures: trackpad / shift+wheel and swipes over a row, its
 * bars, and dragging a text selection past either edge of the code.
 */
export function useHScroll(scope: RefObject<HTMLElement | null>, ranges: number[]): HScroll {
  const xs = useRef(new Map<number, number>());
  const rs = useRef(ranges);
  rs.current = ranges;
  const range = (g: number) => Math.max(0, Math.ceil(rs.current[g] ?? 0));
  const set = (g: number, nx: number) => {
    const max = range(g);
    const x = Math.max(0, Math.min(max, nx));
    xs.current.set(g, x);
    const el = scope.current;
    if (!el) return;
    el.style.setProperty(`--sx-${g}`, `${x}px`);
    el.querySelectorAll<HTMLElement>(`[data-hbar="${g}"] > .hscroll-thumb`).forEach((t) => t.style.setProperty("--p", String(max ? x / max : 0)));
  };
  const ctl = useRef<HScroll>({ set, get: (g) => xs.current.get(g) ?? 0, range });
  ctl.current.set = set;
  ctl.current.range = range;
  // keep offsets inside their (new) ranges
  useLayoutEffect(() => {
    for (const g of xs.current.keys()) set(g, xs.current.get(g)!);
  }, [ranges.join(",")]);

  useEffect(() => {
    const el = scope.current;
    if (!el) return;
    const groupAt = (t: EventTarget | null) => {
      const n = (t as Element | null)?.closest?.("[data-hg],[data-hbar]") as HTMLElement | null;
      if (!n) return -1;
      const g = Number(n.dataset.hg ?? n.dataset.hbar);
      return range(g) > 0 ? g : -1;
    };
    const onWheel = (e: WheelEvent) => {
      const dx = e.deltaX !== 0 ? e.deltaX : e.shiftKey ? e.deltaY : 0;
      if (dx === 0 || (!e.shiftKey && Math.abs(e.deltaY) > Math.abs(e.deltaX))) return;
      const g = groupAt(e.target);
      if (g < 0) return;
      e.preventDefault();
      set(g, (xs.current.get(g) ?? 0) + dx * (e.deltaMode === 1 ? 20 : 1));
    };
    // horizontal swipes on touch screens; vertical ones scroll the page
    let touch: { g: number; x: number; y: number; lock: "x" | "y" | null } | null = null;
    const onStart = (e: TouchEvent) => {
      const t = e.touches[0];
      const g = groupAt(e.target);
      touch = e.touches.length === 1 && g >= 0 ? { g, x: t.clientX, y: t.clientY, lock: null } : null;
    };
    const onMove = (e: TouchEvent) => {
      if (!touch) return;
      const t = e.touches[0];
      const dx = touch.x - t.clientX;
      const dy = touch.y - t.clientY;
      if (!touch.lock && Math.hypot(dx, dy) > 6) touch.lock = Math.abs(dx) > Math.abs(dy) ? "x" : "y";
      if (touch.lock !== "x") return;
      e.preventDefault();
      set(touch.g, (xs.current.get(touch.g) ?? 0) + dx);
      touch.x = t.clientX;
    };
    const onEnd = () => (touch = null);
    // selecting text past the edge of the code column scrolls it, and the
    // selection follows
    const onDown = (e: MouseEvent) => {
      if (e.button !== 0) return;
      const target = e.target as Element;
      const col = target.closest?.(".t, .src") as HTMLElement | null;
      const g = groupAt(target);
      if (!col || g < 0 || target.closest("a, button, input, textarea")) return;
      const box = col.getBoundingClientRect();
      let px = e.clientX;
      let py = e.clientY;
      let raf = 0;
      const tick = () => {
        raf = 0;
        const edge = 24;
        const speed = px > box.right - edge ? px - (box.right - edge) : px < box.left + edge ? px - (box.left + edge) : 0;
        if (speed === 0) return;
        const before = xs.current.get(g) ?? 0;
        set(g, before + speed * 0.5);
        if ((xs.current.get(g) ?? 0) === before) return;
        // extend the selection to what is now under the pointer
        const sel = window.getSelection();
        const at = Math.min(Math.max(px, box.left + 1), box.right - 1);
        const caret = (document as any).caretRangeFromPoint?.(at, py) as Range | null;
        if (sel && caret && sel.rangeCount) sel.extend(caret.startContainer, caret.startOffset);
        raf = requestAnimationFrame(tick);
      };
      const move = (ev: MouseEvent) => {
        px = ev.clientX;
        py = ev.clientY;
        if (!raf) raf = requestAnimationFrame(tick);
      };
      const up = () => {
        cancelAnimationFrame(raf);
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    el.addEventListener("touchstart", onStart, { passive: true });
    el.addEventListener("touchmove", onMove, { passive: false });
    el.addEventListener("touchend", onEnd);
    el.addEventListener("mousedown", onDown);
    return () => {
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("touchstart", onStart);
      el.removeEventListener("touchmove", onMove);
      el.removeEventListener("touchend", onEnd);
      el.removeEventListener("mousedown", onDown);
    };
  }, [scope]);
  return ctl.current;
}

/** The style a row of group `g` needs for its code to follow the group's offset. */
export const hsVar = (g: number) => ({ "--sx": `var(--sx-${g}, 0px)` }) as React.CSSProperties;

/** A scrollbar for group `g`: the thumb is `view` wide out of `view + range`. */
export function HBar({ ctl, g, view, className = "" }: { ctl: HScroll; g: number; view: number; className?: string }) {
  const r = ctl.range(g);
  const bar = useRef<HTMLDivElement>(null);
  if (r === 0) return null;
  const share = Math.max(0.06, Math.min(1, view / (view + r)));
  const pct = `${share * 100}%`;
  const drag = (e: React.PointerEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    let last = e.clientX;
    const k = r / Math.max(1, (bar.current?.clientWidth ?? 1) * (1 - share));
    const move = (ev: PointerEvent) => {
      ctl.set(g, ctl.get(g) + (ev.clientX - last) * k);
      last = ev.clientX;
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  };
  // a click on the track moves the thumb there
  const jump = (e: React.PointerEvent) => {
    const rect = bar.current!.getBoundingClientRect();
    const w = rect.width * share;
    ctl.set(g, ((e.clientX - rect.left - w / 2) / Math.max(1, rect.width - w)) * r);
  };
  return (
    <div className={`hscroll ${className}`} ref={bar} data-hbar={g} onPointerDown={jump} aria-hidden>
      <div className="hscroll-thumb" style={{ width: pct, "--w": pct, "--p": String(ctl.get(g) / r) } as React.CSSProperties} onPointerDown={drag} />
    </div>
  );
}
