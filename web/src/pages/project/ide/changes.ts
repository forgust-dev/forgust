// The change set of the web IDE: what differs from the commit it started from, as
// plain data. Nothing here knows of the editor or of the page: the tree, the tabs
// and (later) the commit all read it.

import { nameOf, type S } from "../../../api/client";

/** One file that differs from the base commit, under the path it has now. */
export type Draft = {
  /** where the file is in the base commit; none: a new file */
  origin?: string;
  /** removed (always a file of the base commit, under its own path) */
  deleted?: true;
  /** its text now; none: that of `origin`, untouched (a deletion, a plain rename) */
  text?: string;
};

/** Every file that differs, by its path now. */
export type Drafts = Record<string, Draft>;

export type Status = "added" | "modified" | "deleted" | "renamed";

export const statusOf = (path: string, d: Draft): Status => (d.deleted ? "deleted" : d.origin === undefined ? "added" : d.origin !== path ? "renamed" : "modified");

/** The base commit's paths that a rename emptied, each with where its file went. */
export function renamedAway(drafts: Drafts): Map<string, string> {
  const gone = new Map<string, string>();
  for (const [path, d] of Object.entries(drafts)) if (d.origin !== undefined && d.origin !== path) gone.set(d.origin, path);
  return gone;
}

/**
 * The changes without their texts: all the tree and the tabs show. `outlineKey` is
 * the same for as long as only texts change, so what is built from the outline can
 * be kept while the author types.
 */
export const outline = (drafts: Drafts): Drafts => Object.fromEntries(Object.entries(drafts).map(([path, { text: _, ...d }]) => [path, d]));
export const outlineKey = (drafts: Drafts) =>
  Object.entries(drafts)
    .map(([path, d]) => `${path}\0${d.origin ?? ""}\0${d.deleted ? 1 : 0}`)
    .join("\n");

/** Why the changes so far leave no room for a file at a path, or null. */
export function clash(drafts: Drafts, path: string): string | null {
  const live = Object.keys(drafts).filter((k) => !drafts[k].deleted);
  if (live.includes(path)) return `${path} already exists`;
  if (live.some((k) => k.startsWith(`${path}/`) || path.startsWith(`${k}/`))) return "A file and a folder cannot have the same name";
  return null;
}

/** Whether anything under a directory differs (`""` is the root). */
export function touches(drafts: Drafts, dir: string): boolean {
  const prefix = dir ? `${dir}/` : "";
  return Object.entries(drafts).some(([path, d]) => path.startsWith(prefix) || (d.origin?.startsWith(prefix) ?? false));
}

/** A line of the file tree: an entry of a directory, as the changes leave it. */
export type Row = {
  name: string;
  path: string;
  /** the base commit's kind of entry (`tree`, `blob`, `executable`, `symlink`, `submodule`); a folder is a `tree` */
  kind: string;
  /** a directory of the base commit (one that only new files make has nothing to fetch) */
  tracked: boolean;
  status?: Status;
};

export const isDir = (row: Row) => row.kind === "tree";

/**
 * What a directory shows: the base commit's entries, less the files renamed away,
 * plus the new and renamed files that landed in it and the folders they make.
 * Deleted files stay listed (struck out), so they can be restored. Folders first.
 */
export function listing(entries: { name: string; path: string; kind: string }[], drafts: Drafts, dir: string): Row[] {
  const prefix = dir ? `${dir}/` : "";
  const away = renamedAway(drafts);
  const rows = new Map<string, Row>();
  for (const e of entries) {
    if (e.kind === "tree") rows.set(e.name, { name: e.name, path: e.path, kind: "tree", tracked: true });
    else if (drafts[e.path] || !away.has(e.path)) rows.set(e.name, { name: e.name, path: e.path, kind: e.kind, tracked: true });
  }
  for (const [path, d] of Object.entries(drafts)) {
    if (!path.startsWith(prefix)) continue;
    const rest = path.slice(prefix.length);
    const name = rest.split("/")[0];
    if (rest.includes("/")) {
      if (!rows.has(name)) rows.set(name, { name, path: prefix + name, kind: "tree", tracked: false });
    } else {
      rows.set(name, { name, path, kind: rows.get(name)?.kind ?? "blob", tracked: d.origin === path, status: statusOf(path, d) });
    }
  }
  return [...rows.values()].sort((a, b) => Number(isDir(b)) - Number(isDir(a)) || (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
}

export type Action =
  /** every change dropped */
  | { type: "clear" }
  /** the text of a file changed; `base` is the text of its origin in the base commit (none for a new file) */
  | { type: "edit"; path: string; text: string; base?: string }
  /** a new, empty file (the caller checked that nothing is at this path) */
  | { type: "create"; path: string }
  | { type: "remove"; path: string }
  /** the caller checked that nothing is at `to`, nor was in the base commit */
  | { type: "rename"; from: string; to: string }
  /** back to what the base commit has: an edit undone, a deleted file restored, a renamed one back in place, a new one gone */
  | { type: "discard"; path: string };

const without = (drafts: Drafts, path: string): Drafts => {
  const { [path]: _, ...rest } = drafts;
  return rest;
};

export function reduce(drafts: Drafts, a: Action): Drafts {
  switch (a.type) {
    case "clear":
      return {};
    case "edit": {
      const d = drafts[a.path];
      // a file of the base commit, untouched so far
      if (!d) return a.text === a.base ? drafts : { ...drafts, [a.path]: { origin: a.path, text: a.text } };
      if (d.deleted) return drafts;
      if (d.origin === undefined) return { ...drafts, [a.path]: { text: a.text } };
      // typed back to what the base commit has: no longer a change (a rename stays one)
      if (a.text === a.base) return d.origin === a.path ? without(drafts, a.path) : { ...drafts, [a.path]: { origin: d.origin } };
      return { ...drafts, [a.path]: { origin: d.origin, text: a.text } };
    }
    case "create":
      // over a file deleted here: the same file, emptied
      return { ...drafts, [a.path]: drafts[a.path]?.deleted ? { origin: a.path, text: "" } : { text: "" } };
    case "remove": {
      const d = drafts[a.path];
      if (d?.deleted) return drafts;
      if (d && d.origin === undefined) return without(drafts, a.path);
      const origin = d?.origin ?? a.path;
      return { ...without(drafts, a.path), [origin]: { origin, deleted: true } };
    }
    case "rename": {
      if (a.from === a.to) return drafts;
      const d = drafts[a.from];
      if (d?.deleted) return drafts;
      const rest = without(drafts, a.from);
      if (!d) return { ...rest, [a.to]: { origin: a.from } };
      // renamed back to where it was: an edit at most
      if (d.origin === a.to) return d.text === undefined ? rest : { ...rest, [a.to]: { origin: a.to, text: d.text } };
      return { ...rest, [a.to]: d };
    }
    case "discard":
      return without(drafts, a.path);
  }
}

/** The most files one commit takes (the server's limit). */
export const MAX_ACTIONS = 100;

/**
 * The changes as one commit's actions. The server checks each against the base
 * commit, not against the actions before it; the workbench keeps the changes such
 * that both agree (no new file on a path of the commit, renamed away or not).
 */
export function toActions(drafts: Drafts): S["FileAction"][] {
  return Object.keys(drafts)
    .sort()
    .map((path): S["FileAction"] => {
      const d = drafts[path];
      if (d.deleted) return { action: "delete", file_path: path };
      if (d.origin === undefined) return { action: "create", file_path: path, content: d.text ?? "" };
      if (d.origin === path) return { action: "update", file_path: path, content: d.text ?? "" };
      return { action: "move", file_path: path, previous_path: d.origin, content: d.text };
    });
}

/** A commit message to start from: what the changes do, in a line. */
export function describe(drafts: Drafts): string {
  const paths = Object.keys(drafts);
  if (paths.length === 0) return "";
  if (paths.length > 1) return `Update ${paths.length} files`;
  const [path] = paths;
  const d = drafts[path];
  const verbs: Record<Status, string> = { added: `Add ${nameOf(path)}`, modified: `Update ${nameOf(path)}`, deleted: `Delete ${nameOf(path)}`, renamed: `Rename ${nameOf(d.origin ?? path)} to ${nameOf(path)}` };
  return verbs[statusOf(path, d)];
}

/** Why a file cannot be named so, or null when it can. */
export function badPath(path: string): string | null {
  if (!path) return "Give the file a path";
  if (path.endsWith("/")) return "Give the file a name after the folder";
  const parts = path.split("/");
  if (parts.some((s) => s === "" || s === "." || s === "..")) return "A path is made of names separated by single slashes";
  if (parts.some((s) => s.toLowerCase() === ".git")) return "Nothing can be named .git";
  if (/[\0-\x1f\x7f]/.test(path)) return "A path cannot hold control characters";
  return null;
}
