import type { ReactNode } from "react";
import {
  Bell,
  Boxes,
  CircleDot,
  Code2,
  Compass,
  DatabaseBackup,
  FileKey,
  FolderGit2,
  Gauge,
  GitMerge,
  HardDrive,
  HeartPulse,
  Home,
  Import,
  KeyRound,
  ListChecks,
  LockKeyhole,
  Milestone as MilestoneIcon,
  Plus,
  RefreshCw,
  Rocket,
  ScrollText,
  Server,
  Settings,
  ShieldAlert,
  ShieldCheck,
  Sparkles,
  Ticket,
  User,
  Users,
  Workflow,
} from "lucide-react";
import { refLink, type S, filesLink } from "../api/client";
import type { PrefetchTarget } from "../prefetch";
import type { Nav } from "./nav";
import { SETTINGS_PAGES, SETTINGS_SECTIONS, type SettingsSlug } from "../pages/project/settingsPages";
import { ADMIN_PAGES, type AdminPath } from "../pages/adminPages";
import { PROFILE_PAGES, type ProfilePath } from "../pages/profilePages";

/** A nav entry; `children` show under it while its section is active. `title`: the
 *  entry's tip, folded or not (a project's full path); `prefetch`: what it opens;
 *  `words`: other names the palette finds it by. */
export type Item = {
  to: string;
  label: string;
  icon?: ReactNode;
  end?: boolean;
  count?: number;
  match?: RegExp;
  children?: Item[];
  title?: string;
  prefetch?: PrefetchTarget;
  words?: string;
};

/** What each settings page holds, for the palette ("webhooks" finds Integrations). */
const SETTINGS_WORDS: Partial<Record<SettingsSlug, string>> = {
  "": "rename archive delete visibility picture",
  repository: "protected branches tags push rules deploy keys tokens locked lfs",
  merge_requests: "approvals merge checks pipeline must succeed merge trains",
  ci_cd: "runners variables environments pages job token",
  automation: "security scanning dependency updates renovate ai review",
  integrations: "webhooks access tokens",
};

/** A project's pages, as its sidebar lists them. */
export function projectItems(p: S["ProjectDetail"], ref: string, mrCount?: number, issueCount?: number): Item[] {
  const web = `/${p.full_path}`;
  const esc = p.full_path.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const sec = (s: string) => new RegExp(`^/${esc}/-/(${s})(/|$)`);
  const files = new RegExp(`^/${esc}(/-/(tree|blob|blame|raw)(/|$)|/?$)`);
  const code = new RegExp(`${files.source}|${sec("commits?|branches|tags|compare|smart_view|ide").source}`);
  // the web IDE commits: for those who can change the repository (not a pull mirror)
  const ide = p.can_write_repository ? [{ to: refLink(web, "ide", ref), label: "Web IDE", match: sec("ide"), words: "editor edit files" }] : [];
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
          children: [{ to: `${web}/-/security`, label: "Findings", match: sec("security"), words: "vulnerabilities" }, ...audit],
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
        { to: filesLink(web, ref, p.default_branch), label: "Files", match: files, words: "repository tree" },
        { to: `${web}/-/smart_view`, label: "Smart View", match: sec("smart_view"), words: "map blocks architecture dependencies people owners experts" },
        { to: refLink(web, "commits", ref), label: "Commits", match: sec("commits?"), words: "history" },
        { to: `${web}/-/branches`, label: "Branches", match: sec("branches") },
        { to: `${web}/-/tags`, label: "Tags", match: sec("tags") },
        { to: `${web}/-/compare`, label: "Compare", match: sec("compare") },
        ...ide,
      ],
    },
    { to: `${web}/-/issues`, label: "Issues", icon: <CircleDot size={16} />, count: issueCount, match: sec("issues") },
    { to: `${web}/-/merge_requests`, label: "Merge requests", icon: <GitMerge size={16} />, count: mrCount, match: sec("merge_requests"), words: "mr pr pull" },
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
      words: "ci jobs builds",
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
        { to: `${web}/-/container_registry`, label: "Container registry", match: sec("container_registry"), words: "docker images" },
        { to: `${web}/-/environments`, label: "Environments", match: sec("environments") },
      ],
    },
    ...security,
    { to: `${web}/-/members`, label: "Members", icon: <Users size={16} />, match: sec("members"), words: "people team" },
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
              return { to: `${web}/-/${path}`, label, match: new RegExp(`^/${esc}/-/${path}/?$`), words: SETTINGS_WORDS[slug] };
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
  // each section of the settings, straight to it ("Settings › Deploy keys")
  if (p.can_admin) {
    const web = `/${p.full_path}`;
    for (const sec of SETTINGS_SECTIONS) {
      if (sec.own && !p.can_own) continue;
      all.push({ to: `${web}/-/settings${sec.slug ? `/${sec.slug}` : ""}#${sec.id}`, label: `Settings › ${sec.label}`, icon: <Settings size={16} />, words: sec.words });
    }
  }
  return { top, all };
}

/** A group's pages, as its sidebar lists them. */
export function groupItems(g: S["GroupDetail"]): Item[] {
  const web = `/${g.full_path}`;
  return [
    { to: web, label: g.kind === "user" ? "Projects" : "Overview", icon: <Boxes size={16} />, end: true },
    ...(g.kind === "group" ? [{ to: `${web}/-/members`, label: "Members", icon: <Users size={16} />, words: "people team" }] : []),
    ...(g.can_admin ? [{ to: `${web}/-/settings`, label: "Settings", icon: <Settings size={16} /> }] : []),
  ];
}

// a page without an icon would leave a blank row on the folded rail: the type wants each
export const ADMIN_ICONS: Record<AdminPath, ReactNode> = {
  "": <Gauge size={16} />,
  users: <Users size={16} />,
  projects: <FolderGit2 size={16} />,
  storage: <HardDrive size={16} />,
  runners: <Server size={16} />,
  jobs: <ListChecks size={16} />,
  health: <HeartPulse size={16} />,
  backups: <DatabaseBackup size={16} />,
  settings: <Settings size={16} />,
  "dependency-updates": <RefreshCw size={16} />,
  "security-scanning": <ShieldCheck size={16} />,
  "ai-review": <Sparkles size={16} />,
  "smart-view": <Boxes size={16} />,
  audit: <ScrollText size={16} />,
};

export const PROFILE_ICONS: Record<ProfilePath, ReactNode> = {
  "": <User size={16} />,
  keys: <KeyRound size={16} />,
  gpg_keys: <FileKey size={16} />,
  tokens: <Ticket size={16} />,
  security: <LockKeyhole size={16} />,
  notifications: <Bell size={16} />,
};

/** Other names of the admin area's and your settings' pages, for the palette. */
const AREA_WORDS: Record<string, string> = {
  "/admin/dependency-updates": "renovate",
  "/admin/security-scanning": "sast",
  "/admin/audit": "events",
  "/profile": "name picture",
  "/profile/tokens": "personal api",
  "/profile/security": "two-factor passkeys security keys webauthn",
  "/profile/notifications": "email",
};

/** An area's pages as entries: `base` is its path ("/admin"), each page's under it. */
export function areaItems<P extends string>(base: string, pages: readonly { path: P; label: string }[], icons: Record<P, ReactNode>): Item[] {
  return pages.map(({ path, label }) => {
    const to = path ? `${base}/${path}` : base;
    return { to, label, icon: icons[path], match: path ? new RegExp(`^${base}/${path}(/|$)`) : new RegExp(`^${base}/?$`), words: AREA_WORDS[to] };
  });
}

/** The pages that are no project's or group's, for the palette: the main menu's, your
 *  settings' and (for administrators) the admin area's. */
export function globalPages(user: { username: string; is_admin?: boolean | null }): Item[] {
  return [
    { to: "/", label: "Home", icon: <Home size={16} />, words: "work to-dos todos reviews" },
    { to: "/dashboard/projects", label: "Projects", icon: <FolderGit2 size={16} /> },
    { to: "/dashboard/groups", label: "Organizations", icon: <Users size={16} />, words: "groups" },
    { to: "/explore", label: "Explore", icon: <Compass size={16} /> },
    { to: `/${user.username}`, label: "Your profile", icon: <User size={16} /> },
    ...areaItems("/profile", PROFILE_PAGES, PROFILE_ICONS).map((it) => ({ ...it, label: `Your settings › ${it.label}` })),
    ...(user.is_admin ? areaItems("/admin", ADMIN_PAGES, ADMIN_ICONS).map((it) => ({ ...it, label: it.to === "/admin" ? "Admin area" : `Admin area › ${it.label}` })) : []),
  ];
}

/** What the "+" menu makes: first what goes in the page's project or group (`here`),
 *  then what goes anywhere. */
export function newItems(nav: Nav): (Item & { here?: boolean })[] {
  const out: (Item & { here?: boolean })[] = [];
  if (nav.kind === "project") {
    const web = `/${nav.project.full_path}/-`;
    out.push({ to: `${web}/issues/new`, label: "New issue", icon: <Plus size={16} />, here: true });
    if (nav.project.can_push) out.push({ to: `${web}/merge_requests/new`, label: "New merge request", icon: <Plus size={16} />, here: true, words: "mr pr pull" });
  }
  if (nav.kind === "group" && nav.group.can_create_project)
    out.push({ to: `/new/project?namespace=${encodeURIComponent(nav.group.full_path)}`, label: "New project in this group", icon: <Plus size={16} />, here: true });
  if (nav.kind === "group" && nav.group.can_create_subgroup) out.push({ to: `/new/group?parent=${nav.group.id}`, label: "New subgroup", icon: <Plus size={16} />, here: true });
  out.push(
    { to: "/new/project", label: "New project", icon: <Plus size={16} />, words: "create repository" },
    { to: "/new/group", label: "New organization", icon: <Plus size={16} />, words: "create group" },
    { to: "/import", label: "Import projects", icon: <Import size={16} />, words: "migrate gitlab github" },
  );
  return out;
}
