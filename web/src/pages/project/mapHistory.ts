import type { S } from "../../api/client";

/** A commit of the default branch that changed the map, as the Smart View's history tells it. */
export type Chapter = S["MapChapter"];
type LinkLike = { from: string; to: string; kind: S["MapLinkKind"] };

/** A block's title as a chapter knew it: its name, but its id when the name is only its path (an
 *  image, a schema), as `blockTitle` says it. */
export const chapterTitle = (b: S["ChapterBlock"]) => (b.name.includes("/") ? b.id : b.name);

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
      tell(r.to.id, c, { what: "renamed", from: chapterTitle(r.from) });
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
    // a name that is only its path is its root, as for a block of the map (`blockTitle`)
    roots: b.name.includes("/") ? [{ path: b.name, dir: false }] : [],
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

/** The map once a chapter was made: its blocks and links by the ids they have at the end of the
 *  history (a renamed block keeps one id, and so its place), each as it was then. */
export type Step = { blocks: Map<string, S["MapBlock"]>; links: Map<string, S["MapLink"]> };

/** The history replayed: the map after each chapter (`steps`), before the first (`base`), and every
 *  block and link it ever had (`all`), to lay out once so that each keeps its place. */
export type Replay = { steps: Step[]; base: Step; all: S["RepoMap"] };

/** The history replayed from the map of now, each chapter undone from the latest. */
export function replay(map: S["RepoMap"], h: S["MapHistory"]): Replay {
  const blocks = new Map(map.blocks.map((b) => [b.id, b]));
  const links = new Map(map.links.map((l) => [linkKey(l), l]));
  // a block's id then → the one it has at the end
  const last = new Map<string, string>();
  const at = (id: string) => last.get(id) ?? id;
  const snapshot = (): Step => {
    const step: Step = { blocks: new Map(), links: new Map() };
    for (const [id, b] of blocks) step.blocks.set(at(id), b);
    for (const l of links.values()) {
      const kept = { ...l, from: at(l.from), to: at(l.to) };
      step.links.set(linkKey(kept), kept);
    }
    return step;
  };
  const steps: Step[] = [];
  steps[h.chapters.length - 1] = snapshot();
  for (let i = h.chapters.length - 1; i >= 0; i--) {
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
      last.set(r.from.id, at(r.to.id));
      for (const [key, l] of [...links]) {
        if (l.from !== r.to.id && l.to !== r.to.id) continue;
        const back = { ...l, from: l.from === r.to.id ? r.from.id : l.from, to: l.to === r.to.id ? r.from.id : l.to };
        links.delete(key);
        links.set(linkKey(back), back);
      }
    }
    for (const b of ch.blocks_removed) if (!blocks.has(b.id)) blocks.set(b.id, blank(b));
    for (const l of ch.links_removed) links.set(linkKey(l), l);
    if (i > 0) steps[i - 1] = snapshot();
  }
  const base = snapshot();
  // every block and link, each as it last was
  const every = new Map<string, S["MapBlock"]>();
  const ties = new Map<string, S["MapLink"]>();
  for (const s of [...steps].reverse().concat(base)) {
    for (const [id, b] of s.blocks) if (!every.has(id)) every.set(id, { ...b, id });
    for (const [key, l] of s.links) if (!ties.has(key)) ties.set(key, l);
  }
  return { steps, base, all: { ...map, blocks: [...every.values()], links: [...ties.values()] } };
}

/** The history in a line's numbers: how many blocks at its start, and when; how many changes, by
 *  how many people. `now`: the blocks of the map of now. */
export function storyOf(h: S["MapHistory"], now: number) {
  const net = h.chapters.reduce((n, c) => n + c.changes.blocks_added.length - c.changes.blocks_removed.length, 0);
  return {
    // from nothing, when it goes back to the first commit
    from: h.start ? now - net : (h.chapters[0]?.changes.blocks_added.length ?? 0),
    since: h.start?.at ?? h.chapters[0]?.at,
    changes: h.chapters.length,
    people: new Set(h.chapters.map((c) => c.author.username ?? c.author.name)).size,
  };
}

/** How many changes a chapter made. */
export const size = (c: Chapter) => {
  const ch = c.changes;
  return ch.blocks_added.length + ch.blocks_removed.length + ch.blocks_renamed.length + ch.kinds_changed.length + ch.links_added.length + ch.links_removed.length;
};
