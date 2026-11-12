// Job log parsing, for what runner 17-19 sends:
//  - optional per-line prefix `<RFC3339> <2-hex stream><O|E><+| >` (`+`: this
//    line continues the previous one, `E`: stderr)
//  - `section_start:<unix>:<name>[options]\r\x1b[0K<header>` and
//    `section_end:<unix>:<name>\r\x1b[0K` markers
//  - `\r` progress rewrites and ANSI SGR colors.

export type Style = { fg?: string; bg?: string; bold?: boolean; italic?: boolean; underline?: boolean; faint?: boolean };
export type Span = { text: string; style: Style };
export type LogLine = {
  /** Line number (1-based) as shown. */
  n: number;
  spans: Span[];
  time?: string;
  stderr?: boolean;
  /** Section this line belongs to (innermost). */
  section?: string;
  /** This line is a section header. */
  header?: { name: string; collapsed: boolean; start: number; end?: number };
};

const PREFIX = /^(\d{4}-\d\d-\d\dT[\d:.]+Z) ([0-9a-f]{2})([OE])([+ ])/;
const SECTION = /^section_(start|end):(\d+):([A-Za-z0-9_.-]+)(\[[^\]]*\])?\r?(\x1b\[0K)?/;

const BASIC = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];

function color256(n: number): string {
  if (n < 8) return `var(--term-${BASIC[n]})`;
  if (n < 16) return `var(--term-bright-${BASIC[n - 8]})`;
  if (n < 232) {
    const c = n - 16;
    const v = (x: number) => (x === 0 ? 0 : 55 + x * 40);
    return `rgb(${v(Math.floor(c / 36))},${v(Math.floor(c / 6) % 6)},${v(c % 6)})`;
  }
  const g = 8 + (n - 232) * 10;
  return `rgb(${g},${g},${g})`;
}

/** Apply one SGR sequence (`1;31`) to a style. */
function sgr(style: Style, params: string): Style {
  const codes = params === "" ? [0] : params.split(";").map((x) => Number(x) || 0);
  let s: Style = { ...style };
  for (let i = 0; i < codes.length; i++) {
    const c = codes[i];
    if (c === 0) s = {};
    else if (c === 1) s.bold = true;
    else if (c === 2) s.faint = true;
    else if (c === 3) s.italic = true;
    else if (c === 4) s.underline = true;
    else if (c === 22) s.bold = s.faint = false;
    else if (c === 23) s.italic = false;
    else if (c === 24) s.underline = false;
    else if (c >= 30 && c <= 37) s.fg = `var(--term-${BASIC[c - 30]})`;
    else if (c === 39) s.fg = undefined;
    else if (c >= 40 && c <= 47) s.bg = `var(--term-${BASIC[c - 40]})`;
    else if (c === 49) s.bg = undefined;
    else if (c >= 90 && c <= 97) s.fg = `var(--term-bright-${BASIC[c - 90]})`;
    else if (c >= 100 && c <= 107) s.bg = `var(--term-bright-${BASIC[c - 100]})`;
    else if ((c === 38 || c === 48) && codes[i + 1] === 5) {
      const col = color256(codes[i + 2] ?? 0);
      if (c === 38) s.fg = col;
      else s.bg = col;
      i += 2;
    } else if ((c === 38 || c === 48) && codes[i + 1] === 2) {
      const col = `rgb(${codes[i + 2] ?? 0},${codes[i + 3] ?? 0},${codes[i + 4] ?? 0})`;
      if (c === 38) s.fg = col;
      else s.bg = col;
      i += 4;
    }
  }
  return s;
}

/** ANSI text → styled spans (non-SGR escapes are dropped). */
export function spans(text: string, start: Style = {}): { spans: Span[]; style: Style } {
  const out: Span[] = [];
  let style = start;
  let buf = "";
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\x1b" && text[i + 1] === "[") {
      let j = i + 2;
      while (j < text.length && !/[@-~]/.test(text[j])) j++;
      const cmd = text[j];
      if (cmd === "m") {
        if (buf) out.push({ text: buf, style });
        buf = "";
        style = sgr(style, text.slice(i + 2, j));
      }
      i = j;
      continue;
    }
    buf += ch;
  }
  if (buf) out.push({ text: buf, style });
  return { spans: out, style };
}

/** `a\rb` shows `b` (progress bars); a trailing `\r` is ignored. */
function applyCarriageReturns(s: string): string {
  const t = s.endsWith("\r") ? s.slice(0, -1) : s;
  const i = t.lastIndexOf("\r");
  return i < 0 ? t : t.slice(i + 1);
}

export type Parsed = { lines: LogLine[]; sections: Map<string, LogLine> };

type State = { lines: LogLine[]; sections: Map<string, LogLine>; stack: string[]; style: Style };
type Pending = { text: string; time?: string; stderr?: boolean };

/** One log record (after `+` continuations are joined) into `st`. */
function emit(st: State, content: string, time?: string, stderr?: boolean) {
  let body = content;
  // section markers may sit anywhere at the start of a line
  for (;;) {
    const m = SECTION.exec(body);
    if (!m) break;
    const [all, kind, ts, name, opts] = m;
    body = body.slice(all.length);
    if (kind === "start") {
      const s = spans(applyCarriageReturns(body), st.style);
      st.style = s.style;
      const header: LogLine = {
        n: st.lines.length + 1,
        spans: s.spans,
        time,
        stderr,
        section: st.stack[st.stack.length - 1],
        header: { name, collapsed: /collapsed=true/.test(opts ?? ""), start: Number(ts) },
      };
      st.lines.push(header);
      st.sections.set(name, header);
      st.stack.push(name);
      return;
    }
    const h = st.sections.get(name);
    if (h?.header) h.header.end = Number(ts);
    const idx = st.stack.lastIndexOf(name);
    if (idx >= 0) st.stack.splice(idx);
    if (!body.replace(/\x1b\[0K/g, "").trim()) return;
  }
  const s = spans(applyCarriageReturns(body), st.style);
  st.style = s.style;
  st.lines.push({ n: st.lines.length + 1, spans: s.spans, time, stderr, section: st.stack[st.stack.length - 1] });
}

/**
 * Incremental parser: `feed` only the new text. A record is final once the
 * next line shows it is not continued (`+`); `snapshot` also shows the
 * record and partial line still open, without committing them.
 */
export class LogParser {
  private st: State = { lines: [], sections: new Map(), stack: [], style: {} };
  private pending: Pending | null = null;
  private partial = "";

  feed(chunk: string) {
    const parts = (this.partial + chunk).split("\n");
    this.partial = parts.pop() ?? "";
    for (const r of parts) this.record(this.st, r);
  }

  private record(st: State, raw: string) {
    const m = PREFIX.exec(raw);
    if (!m) {
      this.flush(st);
      emit(st, raw);
      return;
    }
    const [all, time, , stream, cont] = m;
    const content = raw.slice(all.length);
    if (cont === "+" && this.pending) {
      this.pending.text += content;
    } else {
      this.flush(st);
      this.pending = { text: content, time, stderr: stream === "E" };
    }
  }

  private flush(st: State) {
    if (this.pending) emit(st, this.pending.text, this.pending.time, this.pending.stderr);
    this.pending = null;
  }

  snapshot(): Parsed {
    if (!this.pending && !this.partial) return { lines: this.st.lines, sections: this.st.sections };
    // finish the open record on a copy (line objects are shared, not copied)
    const st: State = { lines: [...this.st.lines], sections: new Map(this.st.sections), stack: [...this.st.stack], style: this.st.style };
    const saved = this.pending ? { ...this.pending } : null;
    if (this.partial) this.record(st, this.partial);
    this.flush(st);
    this.pending = saved;
    return { lines: st.lines, sections: st.sections };
  }
}


/** Duration of a section in seconds, when it ended. */
export function sectionDuration(h: LogLine["header"]): number | undefined {
  return h?.end != null ? h.end - h.start : undefined;
}
