import { useQuery, type QueryClient } from "@tanstack/react-query";
import { api, type S } from "../../api/client";

/**
 * A project's CI settings, one object the server takes whole. Several sections edit parts of it
 * (CI/CD's pipelines, runners and job token access, and the merge checks of Merge requests), so
 * each saves the latest object with its own fields changed, and nobody's change is lost.
 */
const key = (pid: number) => ["ci-settings", pid];
const get = (pid: number) => api.get<S["ProjectCiSettings"]>(`/projects/${pid}/ci/settings`);

export function useCiSettings(pid: number) {
  return useQuery({ queryKey: key(pid), queryFn: () => get(pid) });
}

export async function saveCiSettings(qc: QueryClient, pid: number, patch: Partial<S["ProjectCiSettings"]>) {
  const latest = await qc.fetchQuery({ queryKey: key(pid), queryFn: () => get(pid), staleTime: 0 });
  const body = { ...latest, ...patch };
  await api.put(`/projects/${pid}/ci/settings`, body);
  qc.setQueryData(key(pid), body);
}
