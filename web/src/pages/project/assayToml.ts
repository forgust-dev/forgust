// Corrections of the Smart View's map, written into assay.toml (crates/forgust-assay's README):
// a block's keys, a referent added to it, someone left out of the people to ask. Only the lines
// concerned change: the file keeps its order, its comments and the rest of its text. The editor
// applies a correction to the file as it loads it, so that a push made since is kept.

export type AssayEdit =
  /** Keys of a block's table: `name`, `description`, `merge_into`, `ignore`. */
  | { kind: "set"; block: string; title: string; keys: Record<string, string | boolean> }
  /** Someone added to a block's `referents`: `@username`. */
  | { kind: "referent"; block: string; title: string; who: string }
  /** Someone left out of every block's people, by a name or an address their commits bear. */
  | { kind: "exclude"; who: string };

/** The commit message that says what a correction does. */
export function assayMessage(e: AssayEdit): string {
  switch (e.kind) {
    case "set":
      if (typeof e.keys.merge_into === "string") return `Smart View: ${e.title} is part of ${e.keys.merge_into}`;
      if (e.keys.ignore === true) return `Smart View: ${e.title} is not a block`;
      if (typeof e.keys.name === "string") return `Smart View: rename ${e.title} to ${e.keys.name}`;
      return `Smart View: say what ${e.title} is for`;
    case "referent":
      return `Smart View: ${e.who} knows ${e.title}`;
    case "exclude":
      return `Smart View: leave ${e.who} out of the people to ask`;
  }
}

/** `text` (assay.toml, or nothing yet) with the correction made. */
export function applyAssayEdit(text: string, e: AssayEdit): string {
  const nl = text.includes("\r\n") ? "\r\n" : "\n";
  const lines = text.trim() === "" ? ["version = 1"] : text.replace(/\r\n/g, "\n").replace(/\n+$/, "").split("\n");
  const name = e.kind === "exclude" ? ["people"] : ["blocks", e.block];
  if (!findTable(lines, name)) {
    if (lines[lines.length - 1].trim() !== "") lines.push("");
    lines.push(`[${name.map(key).join(".")}]`);
  }
  if (e.kind === "set") for (const [k, v] of Object.entries(e.keys)) setKey(lines, name, k, typeof v === "boolean" ? String(v) : str(v));
  else addItem(lines, name, e.kind === "referent" ? "referents" : "exclude", e.who);
  return lines.join(nl) + nl;
}

const BARE = /^[A-Za-z0-9_-]+$/;
const str = (s: string) =>
  `"${s.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n").replace(/\r/g, "\\r").replace(/\t/g, "\\t")}"`;
const key = (k: string) => (BARE.test(k) ? k : str(k));
const blank = (line: string) => line.trim() === "" || line.trim().startsWith("#");

/** A dotted key's parts (`blocks."a.b"`: blocks, then a.b); none when it does not read. */
function parts(dotted: string): string[] | null {
  const out: string[] = [];
  const re = /\s*(?:"((?:[^"\\]|\\.)*)"|'([^']*)'|([A-Za-z0-9_-]+))\s*(\.|$)/y;
  let at = 0;
  while (at < dotted.length) {
    re.lastIndex = at;
    const m = re.exec(dotted);
    if (!m) return null;
    out.push(m[1] !== undefined ? unescape(m[1]) : (m[2] ?? m[3]));
    at = re.lastIndex;
    if (m[4] === "") break;
  }
  return out;
}

function unescape(s: string): string {
  try {
    return JSON.parse(`"${s}"`) as string;
  } catch {
    return s;
  }
}

type Table = { start: number; end: number };

/** Where the table `name` is: its header's line, and the next header's (or the end). */
function findTable(lines: string[], name: string[]): Table | null {
  let found: Table | null = null;
  for (let i = 0; i < lines.length; i++) {
    const m = /^\s*(\[\[?)(.*?)\]\]?\s*(#.*)?$/.exec(lines[i]);
    if (!m) {
      // a value's lines are skipped: a `[` there opens an array, not a table
      i = valueEnd(lines, i);
      continue;
    }
    if (found) {
      found.end = i;
      return found;
    }
    const p = m[1] === "[" ? parts(m[2]) : null;
    if (p && p.length === name.length && p.every((x, j) => x === name[j])) found = { start: i, end: lines.length };
  }
  return found;
}

/** The line a key of the table is on, or -1. */
function findKey(lines: string[], t: Table, k: string): number {
  for (let i = t.start + 1; i < t.end; i++) {
    if (blank(lines[i])) continue;
    const m = /^\s*("(?:[^"\\]|\\.)*"|'[^']*'|[A-Za-z0-9_-]+)\s*=/.exec(lines[i]);
    if (m && parts(m[1])?.join(".") === k) return i;
    i = valueEnd(lines, i);
  }
  return -1;
}

/** The last line of the value on line `i`: an array or an inline table may go on, as a
 *  multi-line string does. */
function valueEnd(lines: string[], i: number): number {
  if (blank(lines[i])) return i;
  const eq = lines[i].indexOf("=");
  if (eq < 0 || /^\s*\[/.test(lines[i])) return i;
  const rest = lines[i].slice(eq + 1).trimStart();
  const triple = rest.startsWith('"""') ? '"""' : rest.startsWith("'''") ? "'''" : null;
  if (triple) {
    if (rest.indexOf(triple, 3) >= 0) return i;
    for (let j = i + 1; j < lines.length; j++) if (lines[j].includes(triple)) return j;
    return lines.length - 1;
  }
  if (!rest.startsWith("[") && !rest.startsWith("{")) return i;
  return close(lines, i, lines[i].indexOf(rest[0], eq)).line;
}

/** Where the bracket or brace at `lines[i][col]` closes: strings and comments skipped. */
function close(lines: string[], i: number, col: number): { line: number; col: number } {
  let depth = 0;
  for (let j = i; j < lines.length; j++) {
    const s = lines[j];
    for (let c = j === i ? col : 0; c < s.length; c++) {
      const ch = s[c];
      if (ch === "#") break;
      if (ch === '"' || ch === "'") {
        // to the string's end on this line
        for (c++; c < s.length && s[c] !== ch; c++) if (ch === '"' && s[c] === "\\") c++;
      } else if (ch === "[" || ch === "{") depth++;
      else if (ch === "]" || ch === "}") {
        depth--;
        if (depth === 0) return { line: j, col: c };
      }
    }
  }
  return { line: lines.length - 1, col: lines[lines.length - 1].length };
}

/** After the table's last key (comments after it may be about the next table). */
function endOf(lines: string[], t: Table): number {
  let at = t.start + 1;
  for (let i = t.start + 1; i < t.end; i++) {
    if (blank(lines[i])) continue;
    i = valueEnd(lines, i);
    at = i + 1;
  }
  return at;
}

function setKey(lines: string[], table: string[], k: string, v: string) {
  const t = findTable(lines, table)!;
  const i = findKey(lines, t, k);
  if (i < 0) {
    lines.splice(endOf(lines, t), 0, `${key(k)} = ${v}`);
    return;
  }
  const indent = /^\s*/.exec(lines[i])![0];
  lines.splice(i, valueEnd(lines, i) - i + 1, `${indent}${key(k)} = ${v}`);
}

/** `item` added to the array `k` of the table, unless it is in it (any case). */
function addItem(lines: string[], table: string[], k: string, item: string) {
  const t = findTable(lines, table)!;
  const i = findKey(lines, t, k);
  if (i < 0) {
    lines.splice(endOf(lines, t), 0, `${key(k)} = [${str(item)}]`);
    return;
  }
  const eq = lines[i].indexOf("=");
  const open = lines[i].slice(eq + 1).trimStart().startsWith("[") ? lines[i].indexOf("[", eq) : -1;
  if (open < 0) {
    // not an array: what it says goes, the new one with it
    const indent = /^\s*/.exec(lines[i])![0];
    lines.splice(i, valueEnd(lines, i) - i + 1, `${indent}${key(k)} = [${str(item)}]`);
    return;
  }
  const end = close(lines, i, open);
  const within = [...lines.slice(i, end.line), lines[end.line].slice(0, end.col)].join("\n").slice(open + 1);
  const inside = [...within.matchAll(/"((?:[^"\\]|\\.)*)"|'([^']*)'/g)].map((m) =>
    m[1] !== undefined ? unescape(m[1]) : m[2],
  );
  if (inside.some((x) => x.toLowerCase() === item.toLowerCase())) return;
  const before = lines[end.line].slice(0, end.col);
  const after = lines[end.line].slice(end.col);
  if (end.line > i && before.trim() === "") {
    // the bracket on its own line: the item on one before it, after a comma
    const last = lastContent(lines, i, end.line);
    if (last.line >= 0 && !/[,[]$/.test(last.text)) lines[last.line] = last.text + "," + lines[last.line].slice(last.text.length);
    const indent = /^\s*/.exec(lines[end.line - 1] ?? "")![0] || "  ";
    lines.splice(end.line, 0, `${indent}${str(item)},`);
    return;
  }
  const head = before.trimEnd();
  const sep = head.endsWith("[") ? "" : head.endsWith(",") ? " " : ", ";
  lines[end.line] = head + sep + str(item) + after;
}

/** The last line holding an array's items before its closing line, comments left out. */
function lastContent(lines: string[], from: number, to: number): { line: number; text: string } {
  for (let j = to - 1; j >= from; j--) {
    const text = lines[j].replace(/\s*#.*$/, "");
    if (text.trim() !== "") return { line: j, text: text.trimEnd() };
  }
  return { line: -1, text: "" };
}
