import { useEffect, useSyncExternalStore } from "react";

/** A project as the sidebar lists it: what it needs to draw the link without a request. */
export type RecentProject = { id: number; name: string; full_path: string; avatar_url?: string | null };

/** Kept per browser and per user; the sidebar shows the first few. */
const MAX = 8;
const key = (uid: number) => `forgust.recent-projects.${uid}`;

const listeners = new Set<() => void>();
// the last list read, so a render gets the same array while nothing changed; `raw`
// is what storage held then (null without storage: the list lives here only)
let cache: { uid: number; raw: string | null; list: RecentProject[] } | null = null;

const valid = (p: unknown): p is RecentProject =>
  !!p && typeof p === "object" && typeof (p as RecentProject).id === "number" && typeof (p as RecentProject).full_path === "string" && typeof (p as RecentProject).name === "string";

function load(uid: number): RecentProject[] {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(key(uid));
  } catch {
    /* private mode: the list in memory */
  }
  if (cache && cache.uid === uid && cache.raw === raw) return cache.list;
  let list: RecentProject[] = [];
  try {
    const v: unknown = raw ? JSON.parse(raw) : [];
    if (Array.isArray(v)) list = v.filter(valid).slice(0, MAX);
  } catch {
    /* a broken entry starts over */
  }
  cache = { uid, raw, list };
  return list;
}

function save(uid: number, list: RecentProject[]) {
  let raw: string | null = null;
  try {
    raw = JSON.stringify(list);
    localStorage.setItem(key(uid), raw);
  } catch {
    raw = null;
  }
  cache = { uid, raw, list };
  listeners.forEach((l) => l());
}

function subscribe(onChange: () => void) {
  listeners.add(onChange);
  // another tab opened a project
  const onStorage = (e: StorageEvent) => {
    if (e.key?.startsWith("forgust.recent-projects.")) onChange();
  };
  window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(onChange);
    window.removeEventListener("storage", onStorage);
  };
}

const NONE: RecentProject[] = [];

/** The projects this user opened last in this browser, the latest first. */
export function useRecentProjects(uid: number | undefined): RecentProject[] {
  return useSyncExternalStore(subscribe, () => (uid == null ? NONE : load(uid)));
}

/** Puts the open project first in the user's recent projects (and refreshes its name,
 *  path and picture there, after a rename). */
export function useRememberProject(uid: number | undefined, p: RecentProject | null) {
  const id = p?.id;
  const name = p?.name;
  const path = p?.full_path;
  const avatar = p?.avatar_url ?? null;
  useEffect(() => {
    if (uid == null || id == null || name == null || path == null) return;
    const list = load(uid);
    const first = list[0];
    if (first && first.id === id && first.name === name && first.full_path === path && (first.avatar_url ?? null) === avatar) return;
    save(uid, [{ id, name, full_path: path, avatar_url: avatar }, ...list.filter((x) => x.id !== id)].slice(0, MAX));
  }, [uid, id, name, path, avatar]);
}

/** Takes a project that is not found any more (deleted, moved away, no longer yours)
 *  out of the user's recent projects: `full_path` is the address that was not found. */
export function useForgetProject(uid: number | undefined, fullPath: string | null) {
  useEffect(() => {
    if (uid == null || !fullPath) return;
    const list = load(uid);
    if (list.some((x) => x.full_path === fullPath)) save(uid, list.filter((x) => x.full_path !== fullPath));
  }, [uid, fullPath]);
}
