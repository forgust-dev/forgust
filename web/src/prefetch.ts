import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useRef } from "react";
import { api, qs, type S } from "./api/client";

// Query keys and fetchers shared by pages and hover prefetching, so a prefetch
// fills exactly the cache entry the next page reads.

/** The groups the user can see (dashboard, pickers, the palette). */
export const useGroups = (enabled = true) => useQuery({ queryKey: ["groups"], queryFn: () => api.get<S["GroupDto"][]>("/groups"), enabled, staleTime: 60_000 });

/** "Your work": review requests, assignments, to-dos (the page and the sidebar badge). */
export const useWork = (enabled = true) => useQuery({ queryKey: ["work"], queryFn: () => api.get<S["Work"]>("/user/work"), enabled, staleTime: 15_000 });

export const keys = {
  project: (id: number) => ["project", id] as const,
  pathResolve: (path: string) => ["resolve", path] as const,
  resolve: (pid: number, refAndPath: string) => ["resolve-ref", pid, refAndPath] as const,
  tree: (pid: number, commit: string, path: string) => ["tree", pid, commit, path] as const,
  blob: (pid: number, commit: string, path: string, ref: string) => ["blob", pid, commit, path, ref] as const,
  mr: (pid: number, iid: number) => ["mr", pid, iid] as const,
};

const base = (pid: number) => `/projects/${pid}/repository`;

export const fetchers = {
  resolve: (pid: number, refAndPath: string) => api.get<S["RefResolved"]>(`${base(pid)}/resolve${qs({ path: refAndPath })}`),
  tree: (pid: number, commit: string, path: string) => api.get<S["TreeResponse"]>(`${base(pid)}/tree${qs({ ref: commit, path })}`),
  blob: (pid: number, ref: string, path: string) => api.get<S["BlobResponse"]>(`${base(pid)}/blob${qs({ ref, path })}`),
  mr: (pid: number, iid: number) => api.get<S["MrDetail"]>(`/projects/${pid}/merge_requests/${iid}`),
  project: (id: number) => api.get<S["ProjectDetail"]>(`/projects/${id}`),
  pathResolve: (path: string) => api.get<S["Resolved"]>(`/resolve${qs({ path })}`),
};

const STALE = 30_000;

async function prefetchCode(qc: QueryClient, pid: number, kind: "tree" | "blob", refAndPath: string) {
  const res = await qc.fetchQuery({ queryKey: keys.resolve(pid, refAndPath), queryFn: () => fetchers.resolve(pid, refAndPath), staleTime: 5_000 });
  if (kind === "tree") {
    await qc.prefetchQuery({ queryKey: keys.tree(pid, res.commit, res.path), queryFn: () => fetchers.tree(pid, res.commit, res.path), staleTime: STALE });
  } else {
    await qc.prefetchQuery({ queryKey: keys.blob(pid, res.commit, res.path, res.ref), queryFn: () => fetchers.blob(pid, res.ref, res.path), staleTime: STALE });
  }
}

export type PrefetchTarget =
  | { kind: "tree" | "blob"; pid: number; refAndPath: string }
  | { kind: "mr"; pid: number; iid: number }
  | { kind: "project"; id: number; path: string };

export function prefetch(qc: QueryClient, t: PrefetchTarget) {
  const run = () => {
    switch (t.kind) {
      case "tree":
      case "blob":
        return prefetchCode(qc, t.pid, t.kind, t.refAndPath);
      case "mr":
        return qc.prefetchQuery({ queryKey: keys.mr(t.pid, t.iid), queryFn: () => fetchers.mr(t.pid, t.iid), staleTime: STALE });
      case "project":
        return Promise.all([
          qc.prefetchQuery({ queryKey: keys.pathResolve(t.path), queryFn: () => fetchers.pathResolve(t.path), staleTime: STALE }),
          qc.prefetchQuery({ queryKey: keys.project(t.id), queryFn: () => fetchers.project(t.id), staleTime: STALE }),
        ]);
    }
  };
  run()?.catch(() => {
    /* a failed prefetch is just a cache miss */
  });
}

/**
 * Handlers for a link: prefetch after the pointer rests ~80 ms (or on keyboard
 * focus), so navigation usually finds its data already loaded.
 */
export function usePrefetch(target: PrefetchTarget | null) {
  const qc = useQueryClient();
  const timer = useRef<number | null>(null);
  if (!target) return {};
  const start = () => {
    if (timer.current) return;
    timer.current = window.setTimeout(() => prefetch(qc, target), 80);
  };
  const cancel = () => {
    if (timer.current) window.clearTimeout(timer.current);
    timer.current = null;
  };
  return { onMouseEnter: start, onMouseLeave: cancel, onFocus: () => prefetch(qc, target) };
}
