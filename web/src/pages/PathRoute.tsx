import { lazy, useEffect } from "react";
import { Navigate, useLocation, useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { fetchers, keys } from "../prefetch";
import { NotFound, Spinner } from "../components/ui";
const GroupPage = lazy(() => import("./Group").then((m) => ({ default: m.GroupPage })));
const ProjectPage = lazy(() => import("./project/Project").then((m) => ({ default: m.ProjectPage })));
const UserProfilePage = lazy(() => import("./UserProfile").then((m) => ({ default: m.UserProfilePage })));
import { useSession } from "../session";
import { KeepNav } from "../shell/nav";
import { useForgetProject } from "../shell/recent";
import { ApiError } from "../api/client";

/**
 * A URL path decoded; a malformed escape (a `%` typed or pasted as is) is
 * kept as written rather than failing the page.
 */
export function decodePath(p: string): string {
  try {
    return decodeURIComponent(p);
  } catch {
    return p
      .split("/")
      .map((s) => {
        try {
          return decodeURIComponent(s);
        } catch {
          return s;
        }
      })
      .join("/");
  }
}

/**
 * GitLab-shaped URLs: `/<full/path>` and `/<full/path>/-/<rest>`. The path is
 * resolved through the API (which also reports old paths that moved).
 */
export function PathRoute() {
  const loc = useLocation();
  const nav = useNavigate();
  const { user } = useSession();
  const raw = decodePath(loc.pathname).replace(/^\/+|\/+$/g, "");
  const [base, rest = ""] = raw.split(/\/-\/(.*)/s);
  const q = useQuery({
    queryKey: keys.pathResolve(base),
    queryFn: () => fetchers.pathResolve(base),
    enabled: !!base,
  });

  // an address not found: a project there leaves your recent projects
  useForgetProject(user?.id, q.error instanceof ApiError && q.error.status === 404 ? base : null);

  useEffect(() => {
    if (q.data?.redirected) {
      nav(`/${q.data.full_path}${rest ? `/-/${rest}` : ""}${loc.search}`, { replace: true });
    }
  }, [q.data]);

  if (!base) return <NotFound />;
  // the sidebar keeps the menu it shows until the page's own context comes
  const loading = (
    <>
      <KeepNav />
      <Spinner />
    </>
  );
  if (q.isLoading) return loading;
  if (q.error) {
    if (!user) return <Navigate to={`/login?next=${encodeURIComponent(loc.pathname + loc.search)}`} replace />;
    return <NotFound />;
  }
  if (!q.data || q.data.redirected) return loading;
  if (q.data.kind === "project") return <ProjectPage key={`project-${q.data.id}`} id={q.data.id} fullPath={q.data.full_path} rest={rest} />;
  // a person's page is their profile; their namespace's settings stay below `/-/`
  if (q.data.kind === "user" && !rest) return <UserProfilePage key={`user-${q.data.id}`} username={q.data.full_path} />;
  return <GroupPage key={`group-${q.data.id}`} id={q.data.id} fullPath={q.data.full_path} rest={rest} />;
}
