import type { S } from "../../api/client";

/** A commit of the default branch that changed the map, as the Smart View's history tells it. */
export type Chapter = S["MapChapter"];
type LinkLike = { from: string; to: string; kind: S["MapLinkKind"] };

/** A link's key: its ends and what it is (a block's id has no space). */
export const linkKey = (l: LinkLike) => `${l.from} ${l.kind} ${l.to}`;

/** What a chapter did to one block: it came, went, was renamed or turned into another kind, or a
 *  link from it (`needs`, `drops`) or to it (`used`, `unused`) came or went. */
export type Event =
  | { what: "added" }
  | { what: "removed" }
  | { what: "renamed"; from: string }
  | { what: "kind"; from: S["MapKind"]; to: S["MapKind"] }
  | { what: "needs" | "drops" | "used" | "unused"; link: S["MapLink"] };

/** What one chapter did to a block. */
export type Told = { chapter: Chapter; events: Event[] };

export type Stories = {
  /** For each link of the latest map, the chapter that last added it. */
  since: Map<string, Chapter>;
  /** For each block, by the id it has now, what each chapter did to it, the latest first. */
  of: Map<string, Told[]>;
};

/** The history read for each link and each block: a renamed block keeps what it went through
 *  under its old id, and its links what added them. */
export function stories(h: S["MapHistory"]): Stories {
  const since = new Map<string, Chapter>();
  const of = new Map<string, Told[]>();
  const tell = (id: string, chapter: Chapter, e: Event) => {
    const list = of.get(id) ?? [];
    const last = list[list.length - 1];
    if (last?.chapter === chapter) last.events.push(e);
    else list.push({ chapter, events: [e] });
    of.set(id, list);
  };
  for (const c of h.chapters) {
    const ch = c.changes;
    // the renames first: the rest of the chapter has the new ids, but the links it removes
    const renamed = new Map(ch.blocks_renamed.map((r) => [r.from.id, r.to.id]));
    const now = (id: string) => renamed.get(id) ?? id;
    for (const r of ch.blocks_renamed) {
      const before = of.get(r.from.id) ?? [];
      of.delete(r.from.id);
      of.set(r.to.id, [...before, ...(of.get(r.to.id) ?? [])]);
      tell(r.to.id, c, { what: "renamed", from: r.from.name });
    }
    if (renamed.size) {
      for (const [k, chapter] of [...since]) {
        const [from, kind, to] = k.split(" ");
        if (renamed.has(from) || renamed.has(to)) {
          since.delete(k);
          since.set(`${now(from)} ${kind} ${now(to)}`, chapter);
        }
      }
    }
    for (const b of ch.blocks_added) tell(b.id, c, { what: "added" });
    for (const b of ch.blocks_removed) tell(b.id, c, { what: "removed" });
    for (const k of ch.kinds_changed) tell(k.id, c, { what: "kind", from: k.from, to: k.to });
    for (const l of ch.links_added) {
      since.set(linkKey(l), c);
      tell(l.from, c, { what: "needs", link: l });
      tell(l.to, c, { what: "used", link: l });
    }
    for (const l of ch.links_removed) {
      const at = { ...l, from: now(l.from), to: now(l.to) };
      since.delete(linkKey(at));
      tell(at.from, c, { what: "drops", link: at });
      tell(at.to, c, { what: "unused", link: at });
    }
  }
  for (const list of of.values()) list.reverse();
  return { since, of };
}

/** A block as a chapter knew it, when the map of now has none: what to draw it with. */
function blank(b: S["ChapterBlock"]): S["MapBlock"] {
  return {
    id: b.id,
    name: b.name,
    kind: b.kind,
    role: b.role ?? null,
    description: null,
    roots: [],
    manifests: [],
    parent: null,
    image: null,
    files: 0,
    languages: [],
    jobs: [],
    owners: [],
    people: [],
    reviewers: [],
    bus_factor: null,
    configured: false,
  };
}

/** The map as it was once the chapter `index` was made: the map of now, each later chapter
 *  undone from the latest. A block it no longer has is drawn from what the chapters knew of it. */
export function mapAt(map: S["RepoMap"], h: S["MapHistory"], index: number): S["RepoMap"] {
  const blocks = new Map(map.blocks.map((b) => [b.id, b]));
  const links = new Map(map.links.map((l) => [linkKey(l), l]));
  for (let i = h.chapters.length - 1; i > index; i--) {
    const ch = h.chapters[i].changes;
    // what the chapter did, undone the other way round
    for (const l of ch.links_added) links.delete(linkKey(l));
    for (const k of ch.kinds_changed) {
      const b = blocks.get(k.id);
      if (b) blocks.set(k.id, { ...b, kind: k.from });
    }
    for (const b of ch.blocks_added) blocks.delete(b.id);
    for (const r of ch.blocks_renamed) {
      const b = blocks.get(r.to.id);
      blocks.delete(r.to.id);
      blocks.set(r.from.id, b ? { ...b, id: r.from.id, name: r.from.name } : blank(r.from));
      for (const [key, l] of [...links]) {
        if (l.from !== r.to.id && l.to !== r.to.id) continue;
        const back = { ...l, from: l.from === r.to.id ? r.from.id : l.from, to: l.to === r.to.id ? r.from.id : l.to };
        links.delete(key);
        links.set(linkKey(back), back);
      }
    }
    for (const b of ch.blocks_removed) if (!blocks.has(b.id)) blocks.set(b.id, blank(b));
    for (const l of ch.links_removed) links.set(linkKey(l), l);
  }
  return { ...map, blocks: [...blocks.values()], links: [...links.values()] };
}

/** The blocks a chapter brought or changed, as the map after it has them: what to light. */
export function touched(c: Chapter): Set<string> {
  const ch = c.changes;
  return new Set([
    ...ch.blocks_added.map((b) => b.id),
    ...ch.blocks_renamed.map((r) => r.to.id),
    ...ch.kinds_changed.map((k) => k.id),
    ...ch.links_added.flatMap((l) => [l.from, l.to]),
  ]);
}

/** How many changes a chapter made. */
export const size = (c: Chapter) => {
  const ch = c.changes;
  return ch.blocks_added.length + ch.blocks_removed.length + ch.blocks_renamed.length + ch.kinds_changed.length + ch.links_added.length + ch.links_removed.length;
};
