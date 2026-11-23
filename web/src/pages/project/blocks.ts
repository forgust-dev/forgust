import type { S } from "../../api/client";

// What the Smart View's pages and the others (a file's, a merge request's) say of a block. Kept
// apart from SmartView.tsx, which brings the graph's libraries with it.

/** A block's title: its name, but its id when the name is only its path (an image, a schema). */
export const blockTitle = (b: S["MapBlock"]) => (b.name === b.roots[0]?.path ? b.id : b.name);

/** Who to ask about a block, by username or name: its declared owners, then who reviewed it,
 *  then who changed it, at most `max`. */
export function whoKnows(b: S["MapBlock"], max: number): string[] {
  const names = [
    ...b.owners.filter((o) => o.startsWith("@")).map((o) => o.slice(1)),
    ...b.reviewers.map((r) => r.username),
    ...b.people.map((p) => p.username ?? p.name),
  ];
  const seen = new Set<string>();
  return names.filter((n) => !seen.has(n.toLowerCase()) && !!seen.add(n.toLowerCase())).slice(0, max);
}
