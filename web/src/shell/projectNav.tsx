import type { ReactNode } from "react";
import { CircleDot, Code2, GitMerge, Milestone as MilestoneIcon, Rocket, Settings, ShieldAlert, Users, Workflow } from "lucide-react";
import { refLink, type S } from "../api/client";
import type { PrefetchTarget } from "../prefetch";
import { SETTINGS_PAGES } from "../pages/project/settingsPages";

/** A nav entry; `children` show under it while its section is active. `title`: the
 *  entry's tip, folded or not (a project's full path); `prefetch`: what it opens. */
export type Item = { to: string; label: string; icon?: ReactNode; end?: boolean; count?: number; match?: RegExp; children?: Item[]; title?: string; prefetch?: PrefetchTarget };

/** A project's pages, as its sidebar lists them. */
export function projectItems(p: S["ProjectDetail"], ref: string, mrCount?: number, issueCount?: number): Item[] {
  const web = `/${p.full_path}`;
  const esc = p.full_path.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const sec = (s: string) => new RegExp(`^/${esc}/-/(${s})(/|$)`);
  const files = new RegExp(`^/${esc}(/-/(tree|blob|blame|raw)(/|$)|/?$)`);
  const code = new RegExp(`${files.source}|${sec("commits?|branches|tags|compare").source}`);
  const schedules = p.can_push ? [{ to: `${web}/-/pipeline_schedules`, label: "Schedules", match: sec("pipeline_schedules") }] : [];
  // findings are for Developers and above
  const developer = p.can_push || p.can_admin || ["developer", "maintainer", "owner"].includes(p.access_level);
  // the audit log is for Maintainers and above, with the findings
  const audit = p.can_admin ? [{ to: `${web}/-/audit`, label: "Audit log", match: sec("audit") }] : [];
  const security = developer
    ? [
        {
          to: `${web}/-/security`,
          label: "Security",
          icon: <ShieldAlert size={16} />,
          match: sec("security|audit"),
          children: [{ to: `${web}/-/security`, label: "Findings", match: sec("security") }, ...audit],
        },
      ]
    : [];
  return [
    {
      to: web,
      label: "Code",
      icon: <Code2 size={16} />,
      match: code,
      children: [
        { to: web, label: "Files", match: files },
        { to: refLink(web, "commits", ref), label: "Commits", match: sec("commits?") },
        { to: `${web}/-/branches`, label: "Branches", match: sec("branches") },
        { to: `${web}/-/tags`, label: "Tags", match: sec("tags") },
        { to: `${web}/-/compare`, label: "Compare", match: sec("compare") },
      ],
    },
    { to: `${web}/-/issues`, label: "Issues", icon: <CircleDot size={16} />, count: issueCount, match: sec("issues") },
    { to: `${web}/-/merge_requests`, label: "Merge requests", icon: <GitMerge size={16} />, count: mrCount, match: sec("merge_requests") },
    // milestones and labels gather issues and merge requests alike
    {
      to: `${web}/-/milestones`,
      label: "Plan",
      icon: <MilestoneIcon size={16} />,
      match: sec("labels|milestones"),
      children: [
        { to: `${web}/-/milestones`, label: "Milestones", match: sec("milestones") },
        { to: `${web}/-/labels`, label: "Labels", match: sec("labels") },
      ],
    },
    {
      to: `${web}/-/pipelines`,
      label: "Pipelines",
      icon: <Workflow size={16} />,
      match: sec("pipelines|jobs|pipeline_schedules"),
      children: [{ to: `${web}/-/pipelines`, label: "Pipelines", match: sec("pipelines|jobs") }, ...schedules],
    },
    // what the project ships, and where: its releases, its images, its environments
    {
      to: `${web}/-/releases`,
      label: "Deploy",
      icon: <Rocket size={16} />,
      match: sec("releases|container_registry|environments"),
      children: [
        { to: `${web}/-/releases`, label: "Releases", match: sec("releases") },
        { to: `${web}/-/container_registry`, label: "Container registry", match: sec("container_registry") },
        { to: `${web}/-/environments`, label: "Environments", match: sec("environments") },
      ],
    },
    ...security,
    { to: `${web}/-/members`, label: "Members", icon: <Users size={16} />, match: sec("members") },
    ...(p.can_admin
      ? [
          {
            to: `${web}/-/settings`,
            label: "Settings",
            icon: <Settings size={16} />,
            match: sec("settings"),
            // only the page's own path: deeper ones are not found
            children: SETTINGS_PAGES.map(({ slug, label }) => {
              const path = slug ? `settings/${slug}` : "settings";
              return { to: `${web}/-/${path}`, label, match: new RegExp(`^/${esc}/-/${path}/?$`) };
            }),
          },
        ]
      : // archived, its Owner keeps the general page (rename, move, delete, unarchive)
        p.archived && p.can_own
        ? [{ to: `${web}/-/settings`, label: "Settings", icon: <Settings size={16} />, match: sec("settings") }]
        : []),
  ];
}


/** The same pages for the palette: one per section (`top`), and every one (`all`), a
 *  child named after its section ("Settings › Webhooks"); a child named like its
 *  section is the section itself. */
export function projectPages(p: S["ProjectDetail"], ref: string): { top: Item[]; all: Item[] } {
  const top: Item[] = [];
  const all: Item[] = [];
  for (const it of projectItems(p, ref)) {
    top.push(it);
    const kids = it.children?.filter((c) => c.label !== it.label) ?? [];
    // "Pipelines" stays itself next to "Pipelines › Schedules"
    if (!kids.length || it.children!.some((c) => c.label === it.label)) all.push(it);
    for (const c of kids) all.push({ ...c, label: `${it.label} › ${c.label}`, icon: it.icon });
  }
  return { top, all };
}
