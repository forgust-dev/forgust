/** The administrator's pages, in the order the sidebar lists them under "Admin area";
 *  each one takes its title from here. `path` is under /admin ("" is the overview). */
export const ADMIN_PAGES: { path: string; label: string }[] = [
  { path: "", label: "Overview" },
  { path: "users", label: "Users" },
  { path: "projects", label: "Projects" },
  { path: "storage", label: "Storage" },
  { path: "runners", label: "Runners" },
  { path: "jobs", label: "Jobs" },
  { path: "health", label: "Health" },
  { path: "backups", label: "Backups" },
  { path: "settings", label: "Settings" },
  { path: "dependency-updates", label: "Dependency updates" },
  { path: "security-scanning", label: "Security scanning" },
  { path: "ai-review", label: "AI review" },
  { path: "audit", label: "Audit log" },
];

/** The page of /admin/… a path is on (the overview for /admin itself). */
export function adminPage(pathname: string) {
  const rest = pathname.replace(/^\/admin\/?/, "").split("/")[0];
  return ADMIN_PAGES.find((p) => p.path === rest);
}
