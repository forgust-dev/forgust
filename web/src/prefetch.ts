import { queryOptions, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useRef } from "react";
import { api, qs, type S } from "./api/client";

// Query keys and fetchers shared by pages and hover prefetching, so a prefetch
// fills exactly the cache entry the next page reads.

const groupsQuery = queryOptions({ queryKey: ["groups"], queryFn: () => api.get<S["GroupDto"][]>("/groups"), staleTime: 60_000 });

/** The groups the user can see (dashboard, pickers, the palette). */
export const useGroups = (enabled = true) => useQuery({ ...groupsQuery, enabled });

const myGroupsQuery = queryOptions({ queryKey: ["groups", "mine"], queryFn: () => api.get<S["GroupDto"][]>("/groups?member=true"), staleTime: 60_000 });

/** The top-level groups the user belongs to: the sidebar's organizations. */
export const useMyGroups = (enabled = true) => useQuery({ ...myGroupsQuery, enabled });

/** A project or a group was made, renamed, moved, archived or deleted: the lists of
 *  yours (kept a minute) are read again where they show. */
export function refreshMine(qc: QueryClient) {
  for (const queryKey of [["my-projects"], ["my-project-states"], ["groups"]]) qc.invalidateQueries({ queryKey });
}

const myProjectsQuery = (search: string) =>
  queryOptions({
    queryKey: ["my-projects", search],
    queryFn: () => api.get<S["MyProject"][]>(`/user/projects${qs({ search, per_page: 100 })}`),
    staleTime: 60_000,
  });

/** The projects the user is a member of, the latest active first (the dashboard and its
 *  filter, the palette, the sidebar's recent projects, your work's cards). */
export const useMyProjects = (search = "", enabled = true) => useQuery({ ...myProjectsQuery(search), enabled });

const myProjectStatesQuery = queryOptions({
  queryKey: ["my-project-states"],
  queryFn: () => api.get<S["ProjectState"][]>("/user/project_states"),
  staleTime: 30_000,
  retry: false,
});

/** How your projects are doing (open merge requests, latest pipeline), one request for
 *  the list (the server reports on the first, most active): the projects page, home's cards. */
export const useMyProjectStates = () => useQuery(myProjectStatesQuery);

const workQuery = queryOptions({ queryKey: ["work"], queryFn: () => api.get<S["Work"]>("/user/work"), staleTime: 15_000 });

/** Home: review requests, assignments, to-dos (the page and the sidebar badge). */
export const useWork = (enabled = true) => useQuery({ ...workQuery, enabled });

/** Where each kind of notification goes (the settings tab, and the bell for what shows on screen). */
export const useNotificationChannels = () =>
  useQuery({ queryKey: ["notifications", "channels"], queryFn: () => api.get<S["ChannelDto"][]>("/user/notification_channels"), staleTime: 60_000 });

/** The sign-in providers configured here (the sign-in page, and who is linked to which). */
export const useProviders = () => useQuery({ queryKey: ["providers"], queryFn: () => api.get<S["Provider"][]>("/auth/providers") });

/** What a signed-out page needs to know of the instance: whether it still waits for its first administrator, whether it sends email. */
export const useSetup = () => useQuery({ queryKey: ["setup"], queryFn: () => api.get<S["SetupStatus"]>("/setup") });

export const keys = {
  project: (id: number) => ["project", id] as const,
  pathResolve: (path: string) => ["resolve", path] as const,
  resolve: (pid: number, refAndPath: string) => ["resolve-ref", pid, refAndPath] as const,
  tree: (pid: number, commit: string, path: string) => ["tree", pid, commit, path] as const,
  blob: (pid: number, commit: string, path: string, ref: string) => ["blob", pid, commit, path, ref] as const,
  mr: (pid: number, iid: number) => ["mr", pid, iid] as const,
  group: (id: number) => ["group", id] as const,
};

const base = (pid: number) => `/projects/${pid}/repository`;

export const fetchers = {
  resolve: (pid: number, refAndPath: string) => api.get<S["RefResolved"]>(`${base(pid)}/resolve${qs({ path: refAndPath })}`),
  tree: (pid: number, commit: string, path: string) => api.get<S["TreeResponse"]>(`${base(pid)}/tree${qs({ ref: commit, path })}`),
  blob: (pid: number, ref: string, path: string) => api.get<S["BlobResponse"]>(`${base(pid)}/blob${qs({ ref, path })}`),
  mr: (pid: number, iid: number) => api.get<S["MrDetail"]>(`/projects/${pid}/merge_requests/${iid}`),
  project: (id: number) => api.get<S["ProjectDetail"]>(`/projects/${id}`),
  pathResolve: (path: string) => api.get<S["Resolved"]>(`/resolve${qs({ path })}`),
  group: (id: number) => api.get<S["GroupDetail"]>(`/groups/${id}`),
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
  | { kind: "project" | "group"; id: number; path: string }
  // home's page and the main menu's lists (the to-do count, recent projects, organizations)
  | { kind: "home" };

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
      case "group":
        return Promise.all([
          qc.prefetchQuery({ queryKey: keys.pathResolve(t.path), queryFn: () => fetchers.pathResolve(t.path), staleTime: STALE }),
          qc.prefetchQuery({ queryKey: keys.group(t.id), queryFn: () => fetchers.group(t.id), staleTime: STALE }),
        ]);
      case "home":
        return Promise.all([qc.prefetchQuery(workQuery), qc.prefetchQuery(myProjectsQuery("")), qc.prefetchQuery(myGroupsQuery), qc.prefetchQuery(myProjectStatesQuery)]);
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
