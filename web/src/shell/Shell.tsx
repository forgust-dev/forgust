import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { Menu as BMenu } from "@base-ui/react/menu";
import { Link, NavLink, useLocation } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  AlertTriangle,
  UserCheck,
  Info,
  X,
  CircleDot,
  Boxes,
  Rocket,
  ChevronLeft,
  ChevronsLeft,
  ChevronsRight,
  ChevronsUpDown,
  Code2,
  Compass,
  Ellipsis,
  FolderGit2,
  GitMerge,
  Home,
  KeyRound,
  LogOut,
  Menu,
  Milestone as MilestoneIcon,
  Monitor,
  Moon,
  Plus,
  Search,
  Settings,
  Shield,
  ShieldAlert,
  Sun,
  User,
  Users,
  Bell,
  DatabaseBackup,
  FileKey,
  Gauge,
  HardDrive,
  HeartPulse,
  ListChecks,
  LockKeyhole,
  RefreshCw,
  ScrollText,
  Server,
  ShieldCheck,
  Sparkles,
  Ticket,
  Workflow,
} from "lucide-react";
import { api, qs, refLink, type S } from "../api/client";
import { useSession, wasSignedIn } from "../session";
import { useToast } from "./toast";
import { usePref, useTheme } from "./theme";
import { openPalette } from "./palette";
import { useNav, useSettleNav, useSideView, type Area, type Nav } from "./nav";
import { HoverCards } from "../components/HoverCards";
import { Avatar, Spinner, Tile } from "../components/ui";
import { ResendLink } from "../components/ResendLink";
import { Logo } from "../components/Logo";
import { fetchers, keys, useMyGroups, useMyProjects, usePrefetch, useWork, type PrefetchTarget } from "../prefetch";
import { useRecentProjects, useRememberProject, type RecentProject } from "./recent";
import { SETTINGS_PAGES } from "../pages/project/settingsPages";
import { ADMIN_PAGES, type AdminPath } from "../pages/adminPages";
import { PROFILE_PAGES, type ProfilePath } from "../pages/profilePages";
import { InboxBell } from "../components/Inbox";

/** A nav entry; `children` show under it while its section is active. `title`: the
 *  entry's tip, folded or not (a project's full path); `prefetch`: what it opens. */
type Item = { to: string; label: string; icon?: ReactNode; end?: boolean; count?: number; match?: RegExp; children?: Item[]; title?: string; prefetch?: PrefetchTarget };

function SideLink({ item, collapsed, onNavigate, sub }: { item: Item; collapsed: boolean; onNavigate: () => void; sub?: boolean }) {
  const loc = useLocation();
  const active = item.match ? item.match.test(loc.pathname) : undefined;
  const pf = usePrefetch(item.prefetch ?? null);
  return (
    <NavLink
      to={item.to}
      end={item.end}
      {...pf}
      onClick={onNavigate}
      title={collapsed ? (item.title ?? item.label) : item.title}
      className={({ isActive }) => `side-link ${sub ? "sub" : ""} ${(active ?? isActive) ? "active" : ""}`}
    >
      {item.icon}
      {/* kept while folded: the sidebar clips and fades them (styles.css) */}
      <span className="side-label grow">{item.label}</span>
      {item.count != null && item.count > 0 && <span className="side-label count">{item.count}</span>}
    </NavLink>
  );
}

/** A top-level entry, plus its sub-entries when its section is open. */
function SideEntry({ item, collapsed, onNavigate }: { item: Item; collapsed: boolean; onNavigate: () => void }) {
  const loc = useLocation();
  const open = !!item.match?.test(loc.pathname) && (item.children?.length ?? 0) > 1;
  return (
    <>
      <SideLink item={item} collapsed={collapsed} onNavigate={onNavigate} />
      {open && (
        // folded, not removed, while the sidebar is: it unfolds again with it
        <div className={`side-sub${collapsed ? " folded" : ""}`} inert={collapsed}>
          <div className="side-sub-inner">
            {item.children!.map((c) => (
              <SideLink key={c.to + c.label} item={c} collapsed={false} onNavigate={onNavigate} sub />
            ))}
          </div>
        </div>
      )}
    </>
  );
}

/** A titled run of entries under the main ones (recent projects, organizations);
 *  folded, its title becomes a short rule above its tiles. */
function SideGroup({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="side-group" role="group" aria-label={label}>
      <div className="side-heading" aria-hidden="true">
        <span className="side-label">{label}</span>
      </div>
      {children}
    </div>
  );
}

/** Shown in the sidebar under each heading. */
const SIDE_GROUP_MAX = 5;

/** The projects you opened last, then (to fill up) your latest active ones; archived
 *  ones left out (one that is gone leaves the list when its address is not found). */
function recentItems(recent: RecentProject[], mine: S["MyProject"][] | undefined): Item[] {
  const fresh = new Map((mine ?? []).map((p) => [p.id, p]));
  const seen = new Set<number>();
  const out: Item[] = [];
  for (const p of [...recent.map((r) => fresh.get(r.id) ?? r), ...(mine ?? [])]) {
    if (seen.has(p.id) || out.length >= SIDE_GROUP_MAX || ("archived" in p && p.archived)) continue;
    seen.add(p.id);
    out.push({ to: `/${p.full_path}`, label: p.name, title: p.full_path, icon: <Tile name={p.name} src={p.avatar_url} />, prefetch: { kind: "project", id: p.id, path: p.full_path } });
  }
  return out;
}

/** What the sidebar shows: the main menu (0), or a group or a project, the deeper the
 *  more groups it sits in. Going deeper slides the menu in from the right, back up from
 *  the left, and across (a sibling) it fades in. Pending (no menu yet), it just shows. */
function levelOf(nav: Nav, area: Area | null): { key: string; depth: number } {
  if (nav.kind === "project") return { key: `project-${nav.project.id}`, depth: 1 + nav.project.ancestors.length };
  if (nav.kind === "group") return { key: `group-${nav.group.id}`, depth: 1 + nav.group.ancestors.length };
  if (area) return { key: area, depth: 1 };
  if (nav.kind === "pending") return { key: "pending", depth: 0 };
  return { key: "global", depth: 0 };
}


// a page without an icon would leave a blank row on the folded rail: the type wants each
const ADMIN_ICONS: Record<AdminPath, ReactNode> = {
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
  audit: <ScrollText size={16} />,
};

const PROFILE_ICONS: Record<ProfilePath, ReactNode> = {
  "": <User size={16} />,
  keys: <KeyRound size={16} />,
  gpg_keys: <FileKey size={16} />,
  tokens: <Ticket size={16} />,
  security: <LockKeyhole size={16} />,
  notifications: <Bell size={16} />,
};

/** An area's pages as entries: `base` is its path ("/admin"), each page's under it. */
function areaItems<P extends string>(base: string, pages: readonly { path: P; label: string }[], icons: Record<P, ReactNode>): Item[] {
  return pages.map(({ path, label }) => ({
    to: path ? `${base}/${path}` : base,
    label,
    icon: icons[path],
    match: path ? new RegExp(`^${base}/${path}(/|$)`) : new RegExp(`^${base}/?$`),
  }));
}

/** A level shown less than this is left without a slide: changes in quick succession don't slide one after the other. */
const SETTLED_MS = 400;

/** How long the first menu waits for its lists, to come in one piece (Sidebar). */
const MENU_WAIT_MS = 300;

/** A group's kind, as its card in the sidebar names it. */
function groupKind(g: S["GroupDetail"]) {
  if (g.kind === "user") return "Personal namespace";
  if (g.is_org) return "Organization";
  return g.ancestors.length > 0 ? "Subgroup" : "Group";
}

/** A destination in the switcher: prefetched when it is pointed at or reached by keyboard. */
function SwitchLink({ to, icon, label, title, prefetch, onNavigate }: { to: string; icon: ReactNode; label: string; title?: string; prefetch?: PrefetchTarget; onNavigate: () => void }) {
  const pf = usePrefetch(prefetch ?? null);
  return (
    <BMenu.LinkItem className="dropdown-item" closeOnClick render={<Link to={to} />} title={title} {...pf} onClick={onNavigate}>
      {icon}
      <span className="ellipsis">{label}</span>
    </BMenu.LinkItem>
  );
}

/**
 * The card of the project or group the sidebar is in: what it is, and a menu to go
 * elsewhere without going back first: home, your recent projects (this one left
 * out), the search. (The groups above it are in the breadcrumbs and the back link.)
 */
function ContextSwitcher({
  tile,
  kind,
  name,
  sub,
  title,
  projectId,
  collapsed,
  onNavigate,
}: {
  tile: ReactNode;
  kind: string;
  name: string;
  sub?: string;
  title: string;
  projectId?: number;
  collapsed: boolean;
  onNavigate: () => void;
}) {
  const { user } = useSession();
  const [open, setOpen] = useState(false);
  const recent = useRecentProjects(user?.id);
  // your latest active projects fill the list up: asked for once it opens
  const mine = useMyProjects("", !!user && open);
  const recents = recentItems(
    recent.filter((r) => r.id !== projectId),
    mine.data?.filter((p) => p.id !== projectId),
  );
  // the search opens once the menu has closed: focus goes to its field, not back here
  const toSearch = useRef(false);
  return (
    <BMenu.Root
      modal={false}
      open={open}
      onOpenChange={(o) => {
        // reset when it opens again: the close asks for it (finalFocus) after it completes
        if (o) toSearch.current = false;
        setOpen(o);
      }}
      onOpenChangeComplete={(o) => {
        if (!o && toSearch.current) openPalette();
      }}
    >
      <BMenu.Trigger className="side-context switcher" title={title}>
        {tile}
        <span className="side-context-text">
          <span className="side-context-kind">{kind}</span>
          <span className="ellipsis">{name}</span>
          {sub && <span className="side-context-ns ellipsis">{sub}</span>}
        </span>
        <ChevronsUpDown size={14} className="side-context-chevron" aria-hidden />
      </BMenu.Trigger>
      <BMenu.Portal>
        {/* folded, beside the rail rather than over the page */}
        <BMenu.Positioner className="positioner" side={collapsed ? "right" : "bottom"} align="start" sideOffset={6}>
          <BMenu.Popup className="dropdown switcher-menu" finalFocus={() => !toSearch.current}>
            <SwitchLink to="/" icon={<Home size={15} />} label="Home" onNavigate={onNavigate} />
            {recents.length > 0 && (
              <>
                <BMenu.Separator className="dropdown-sep" />
                <BMenu.Group>
                  <BMenu.GroupLabel className="select-label">Recent projects</BMenu.GroupLabel>
                  {recents.map((it) => (
                    <SwitchLink key={it.to} to={it.to} icon={it.icon} label={it.label} title={it.title} prefetch={it.prefetch} onNavigate={onNavigate} />
                  ))}
                </BMenu.Group>
              </>
            )}
            <BMenu.Separator className="dropdown-sep" />
            <BMenu.Item
              className="dropdown-item"
              onClick={() => {
                toSearch.current = true;
                onNavigate();
              }}
            >
              <Search size={15} />
              <span className="grow">Search…</span>
              <kbd>{IS_MAC ? "⌘K" : "Ctrl K"}</kbd>
            </BMenu.Item>
          </BMenu.Popup>
        </BMenu.Positioner>
      </BMenu.Portal>
    </BMenu.Root>
  );
}

function projectItems(p: S["ProjectDetail"], ref: string, mrCount?: number, issueCount?: number): Item[] {
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

function groupItems(g: S["GroupDetail"]): Item[] {
  const web = `/${g.full_path}`;
  return [
    { to: web, label: g.kind === "user" ? "Projects" : "Overview", icon: <Boxes size={16} />, end: true },
    ...(g.kind === "group" ? [{ to: `${web}/-/members`, label: "Members", icon: <Users size={16} /> }] : []),
    ...(g.can_admin ? [{ to: `${web}/-/settings`, label: "Settings", icon: <Settings size={16} /> }] : []),
  ];
}

function Sidebar({ collapsed, setCollapsed, mobileOpen, closeMobile }: { collapsed: boolean; setCollapsed: (c: boolean) => void; mobileOpen: boolean; closeMobile: () => void }) {
  const { user } = useSession();
  const loc = useLocation();
  // the admin area and your settings open their own menu, as a project does; while a
  // page loads, the sidebar keeps what it showed (its context and area)
  const { nav, area } = useSideView((n) =>
    (n.kind !== "global" && n.kind !== "pending") || !user
      ? null
      : user.is_admin && /^\/admin(\/|$)/.test(loc.pathname)
        ? "admin"
        : /^\/profile(\/|$)/.test(loc.pathname)
          ? "settings"
          : null,
  );
  const mrs = useQuery({
    // under ["mrs", id]: the merge request pages' invalidations refresh it
    // the same key as the merge request list's unfiltered count: one request
    queryKey: ["mrs", nav.kind === "project" ? nav.project.id : 0, "count", ""],
    queryFn: () => api.get<S["MrCount"]>(`/projects/${nav.kind === "project" ? nav.project.id : 0}/merge_requests_count`),
    enabled: nav.kind === "project" && nav.project.can_read_code,
    staleTime: 30_000,
  });
  const issues = useQuery({
    queryKey: ["issues", nav.kind === "project" ? nav.project.id : 0, "count"],
    queryFn: () => api.get<S["IssueList"]>(`/projects/${nav.kind === "project" ? nav.project.id : 0}/issues${qs({ per_page: 1 })}`),
    enabled: nav.kind === "project",
    staleTime: 30_000,
  });
  // the main menu: home, projects, the admin area, recent projects, organizations
  const global = nav.kind === "global" && !area;
  // opened on a project's or a group's address: no menu until its context comes
  const pending = nav.kind === "pending" && !area;
  const work = useWork(!!user && global);
  const waiting = work.data ? work.data.todos_pending : undefined;
  const c = collapsed && !mobileOpen;
  useRememberProject(user?.id, nav.kind === "project" ? nav.project : null);
  const recent = useRecentProjects(user?.id);
  const mine = useMyProjects("", !!user && global);
  // your organizations: the top-level groups you belong to, not every one you may see
  const groups = useMyGroups(!!user && global);
  const orgs = groups.data ?? [];
  // the first menu comes in one piece: the main menu waits for its lists (the to-do
  // count, recent projects, organizations) rather than growing as each one answers,
  // MENU_WAIT_MS at most; later, a menu that slides in brings what it has
  const [opened, setOpened] = useState(false);
  const [waitedOut, setWaitedOut] = useState(false);
  const hold = global && !!user && !opened && !waitedOut && (work.isLoading || mine.isLoading || groups.isLoading);
  useEffect(() => {
    if (!hold) return;
    const t = setTimeout(() => setWaitedOut(true), MENU_WAIT_MS);
    return () => clearTimeout(t);
  }, [hold]);
  if (!opened && !hold && !pending) setOpened(true);
  const recents = global && user && !hold ? recentItems(recent, mine.data) : [];

  // back one level: to the group the project or subgroup is in, else home; a personal
  // project's namespace is its owner's page, not a menu (home then)
  const top = nav.kind === "project" && nav.project.ancestors.length === 1 ? nav.project.ancestors[0] : undefined;
  const topKind = useQuery({
    queryKey: keys.pathResolve(top?.full_path ?? ""),
    queryFn: () => fetchers.pathResolve(top!.full_path),
    enabled: !!top && top.full_path !== user?.username,
    staleTime: 300_000,
  });
  const personal = !!top && (top.full_path === user?.username || topKind.data?.kind === "user");
  const parent = nav.kind === "project" ? nav.project.ancestors.at(-1) : nav.kind === "group" ? nav.group.ancestors.at(-1) : undefined;
  const up = parent && !personal ? parent : undefined;
  const back = global || pending ? null : up ? { to: `/${up.full_path}`, label: up.name, title: `Back to ${up.name}` } : { to: "/", label: "Home", title: "Back to Home" };
  // back to home: its page and the main menu's lists, so the menu slides in whole
  const backPf = usePrefetch(up ? { kind: "group", id: up.id, path: up.full_path } : back ? { kind: "home" } : null);

  // the level shown, and how the menu came in when it changed (derived while rendering)
  const level = levelOf(nav, area);
  const [shown, setShown] = useState(() => ({ ...level, since: Date.now(), enter: "" }));
  if (shown.key !== level.key) {
    const settled = Date.now() - shown.since > SETTLED_MS;
    // the first menu of a page opened from its address just shows: nothing was left
    const enter = !settled || shown.key === "pending" ? "" : level.depth > shown.depth ? "in-deeper" : level.depth < shown.depth ? "in-back" : "in-across";
    setShown({ ...level, since: Date.now(), enter });
  }

  let header: ReactNode = null;
  let items: Item[];
  if (nav.kind === "project") {
    header = (
      <ContextSwitcher
        tile={<Tile name={nav.project.name} src={nav.project.avatar_url} />}
        kind="Project"
        name={nav.project.name}
        sub={nav.project.ancestors.map((a) => a.name).join(" / ") || undefined}
        title={nav.project.full_path}
        projectId={nav.project.id}
        collapsed={c}
        onNavigate={closeMobile}
      />
    );
    items = projectItems(nav.project, nav.ref ?? nav.project.default_branch ?? "main", mrs.data?.opened, issues.data?.opened);
  } else if (nav.kind === "group") {
    header = (
      <ContextSwitcher
        tile={<Tile name={nav.group.name} src={nav.group.avatar_url} group />}
        kind={groupKind(nav.group)}
        name={nav.group.name}
        sub={nav.group.ancestors.map((a) => a.name).join(" / ") || undefined}
        title={nav.group.full_path}
        collapsed={c}
        onNavigate={closeMobile}
      />
    );
    items = groupItems(nav.group);
  } else if (area === "admin") {
    header = (
      <div className="side-context area" title="The instance's administration">
        <span className="tile area-tile" aria-hidden="true">
          <Shield size={16} />
        </span>
        <span className="side-context-text">
          <span className="side-context-kind">Instance</span>
          <span className="ellipsis">Admin area</span>
        </span>
      </div>
    );
    items = areaItems("/admin", ADMIN_PAGES, ADMIN_ICONS);
  } else if (area === "settings" && user) {
    header = (
      <div className="side-context area" title={`@${user.username}`}>
        <Avatar name={user.name || user.username} src={user.avatar_url} />
        <span className="side-context-text">
          <span className="side-context-kind">Account</span>
          <span className="ellipsis">Your settings</span>
        </span>
      </div>
    );
    items = areaItems("/profile", PROFILE_PAGES, PROFILE_ICONS);
  } else if (pending || hold) {
    items = [];
  } else if (!user) {
    // signed out: what is public
    items = [{ to: "/explore", label: "Explore", icon: <Compass size={16} /> }];
  } else {
    items = [
      { to: "/", label: "Home", icon: <Home size={16} />, end: true, count: waiting },
      // your projects, your organizations and what others share (Explore): one page, three tabs
      { to: "/dashboard/projects", label: "Projects", icon: <FolderGit2 size={16} />, match: /^\/(dashboard\/(projects|groups)|explore)\/?$/ },
      ...(user.is_admin ? [{ to: "/admin", label: "Admin area", icon: <Shield size={16} />, match: /^\/admin(\/|$)/ }] : []),
    ];
  }

  return (
    // folded, its icons' tips show beside the rail rather than over the page
    <aside className={`sidebar ${c ? "collapsed" : ""} ${mobileOpen ? "open" : ""}`} aria-label="Navigation" data-tip-side={c ? "right" : undefined}>
      {/* one level of the menu: a new one comes in sliding (levelOf) */}
      <div key={level.key} className={`side-level${shown.key === level.key && shown.enter ? ` ${shown.enter}` : ""}`}>
        {back && (
          <Link to={back.to} className="side-link back" {...backPf} onClick={closeMobile} title={back.title} aria-label={back.title}>
            <ChevronLeft size={16} />
            <span className="side-label grow">{back.label}</span>
          </Link>
        )}
        {header}
        <nav className="side-items">
          {items.map((it) => (
            <SideEntry key={it.to + it.label} item={it} collapsed={c} onNavigate={closeMobile} />
          ))}
          {recents.length > 0 && (
            <SideGroup label="Recent">
              {recents.map((it) => (
                <SideLink key={it.to} item={it} collapsed={c} onNavigate={closeMobile} />
              ))}
            </SideGroup>
          )}
          {global && user && !hold && orgs.length > 0 && (
            <SideGroup label="Organizations">
              {orgs.slice(0, SIDE_GROUP_MAX).map((g) => (
                <SideLink key={g.id} item={{ to: `/${g.full_path}`, label: g.name, title: g.full_path, icon: <Tile name={g.name} src={g.avatar_url} group /> }} collapsed={c} onNavigate={closeMobile} />
              ))}
              {orgs.length > SIDE_GROUP_MAX && (
                <SideLink item={{ to: "/dashboard/groups", label: `All ${orgs.length} organizations`, icon: <Ellipsis size={16} />, match: /(?!)/ }} collapsed={c} onNavigate={closeMobile} />
              )}
            </SideGroup>
          )}
        </nav>
      </div>
      <div className="spacer" />
      <CollapseButton collapsed={collapsed} setCollapsed={setCollapsed} />
      <div inert={c}>
        <SideVersion />
      </div>
    </aside>
  );
}

function CollapseButton({ collapsed, setCollapsed }: { collapsed: boolean; setCollapsed: (c: boolean) => void }) {
  return (
    <button className="side-link collapse-btn" onClick={() => setCollapsed(!collapsed)} title={collapsed ? "Expand sidebar" : "Collapse sidebar"} aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}>
      {collapsed ? <ChevronsRight size={16} /> : <ChevronsLeft size={16} />}
      <span className="side-label">Collapse</span>
    </button>
  );
}

function Breadcrumbs() {
  const nav = useNav();
  let items: { label: string; to?: string }[] = [];
  if (nav.kind === "project") {
    items = [...nav.project.ancestors.map((a) => ({ label: a.name, to: `/${a.full_path}` })), { label: nav.project.name, to: `/${nav.project.full_path}` }, ...(nav.crumbs ?? [])];
  } else if (nav.kind === "group") {
    items = [...nav.group.ancestors.map((a) => ({ label: a.name, to: `/${a.full_path}` })), { label: nav.group.name, to: `/${nav.group.full_path}` }, ...(nav.crumbs ?? [])];
  }
  if (items.length === 0) return <div className="grow" />;
  return (
    <nav className="breadcrumbs grow" aria-label="Breadcrumb">
      {items.map((c, i) => (
        <span key={i} className="crumb">
          {i > 0 && <span className="sep">/</span>}
          {c.to && i < items.length - 1 ? <Link to={c.to}>{c.label}</Link> : <span className={i === items.length - 1 ? "current" : ""}>{c.label}</span>}
        </span>
      ))}
    </nav>
  );
}

export function ThemeButton() {
  const { theme, setTheme } = useTheme();
  // the icon a change brings turns in (styles.css); the one shown on load just shows
  const [changed, setChanged] = useState(false);
  const next = theme === "system" ? "light" : theme === "light" ? "dark" : "system";
  const cls = changed ? "theme-icon" : undefined;
  const icon = theme === "light" ? <Sun size={16} className={cls} /> : theme === "dark" ? <Moon size={16} className={cls} /> : <Monitor size={16} className={cls} />;
  return (
    <button
      className="icon-btn"
      onClick={() => {
        setChanged(true);
        setTheme(next);
      }}
      title={`Theme: ${theme} (click for ${next})`}
      aria-label={`Theme: ${theme}`}
    >
      {icon}
    </button>
  );
}

function UserMenu() {
  const { user, logout } = useSession();
  const [open, setOpen] = useState(false);
  if (!user) return null;
  return (
    <div className="menu-wrap" onBlur={(e) => !e.currentTarget.contains(e.relatedTarget as Node) && setOpen(false)}>
      <button className="avatar-btn" onClick={() => setOpen(!open)} aria-haspopup="menu" aria-expanded={open} title={user.username}>
        <Avatar name={user.name || user.username} src={user.avatar_url} />
      </button>
      {open && (
        <div className="menu" role="menu">
          {/* who is signed in; "Your profile" below goes to their page */}
          <div className="menu-head" role="presentation">
            <strong>{user.name || user.username}</strong>
            <span className="faint small">@{user.username}</span>
          </div>
          <Link role="menuitem" to={`/${user.username}`} onClick={() => setOpen(false)}>
            <User size={15} /> Your profile
          </Link>
          <Link role="menuitem" to="/profile" onClick={() => setOpen(false)}>
            <Settings size={15} /> Settings
          </Link>
          <Link role="menuitem" to="/profile/keys" onClick={() => setOpen(false)}>
            <KeyRound size={15} /> SSH keys and tokens
          </Link>
          <button role="menuitem" onClick={() => logout()}>
            <LogOut size={15} /> Sign out
          </button>
        </div>
      )}
    </div>
  );
}

/** While the account's address is unconfirmed: notifications wait for it. */
/** An administrator signed in as someone else: who, and a way back. */
function ImpersonationBanner() {
  const { user, impersonator } = useSession();
  const toast = useToast();
  if (!user || !impersonator) return null;
  return (
    <div className="banner impersonation-banner" role="status">
      <UserCheck size={16} />
      <span className="grow">
        You are signed in as <strong>{user.name || user.username}</strong> (@{user.username}) by impersonation, as {impersonator.username}. Everything you do is audited as
        yours; keys, tokens and account settings are off limits.
      </span>
      <button
        className="btn small"
        onClick={async () => {
          try {
            await api.post("/auth/stop_impersonating");
            window.location.assign("/admin/users");
          } catch (e) {
            toast.error(e);
          }
        }}
      >
        Stop impersonating
      </button>
    </div>
  );
}

/** The administrator's message for everyone (Admin > Settings); dismissed until it changes. */
function SiteBanner() {
  const q = useQuery({ queryKey: ["banner"], queryFn: () => api.get<S["Banner"] | null>("/banner"), staleTime: 60_000 });
  const b = q.data;
  const key = b ? `banner.dismissed:${b.text}` : "";
  const [hidden, setHidden] = useState(false);
  let dismissed = false;
  try {
    dismissed = !!key && localStorage.getItem(key) === "1";
  } catch {
    // storage blocked: the banner just stays
  }
  if (!b || hidden || dismissed) return null;
  return (
    <div className={`banner site-banner ${b.tone}`} role="status">
      {b.tone === "warn" ? <AlertTriangle size={16} /> : <Info size={16} />}
      <span className="grow">{b.text}</span>
      <button
        className="icon-btn"
        aria-label="Dismiss"
        title="Dismiss"
        onClick={() => {
          try {
            localStorage.setItem(key, "1");
          } catch {
            // not remembered: hidden for this page only
          }
          setHidden(true);
        }}
      >
        <X size={15} />
      </button>
    </div>
  );
}

/** This server's version and the latest release, for administrators (Admin > Settings turns the check off). */
function useUpdateStatus() {
  const { user } = useSession();
  const q = useQuery({
    queryKey: ["admin-update"],
    queryFn: () => api.get<S["UpdateStatus"]>("/admin/update"),
    enabled: !!user?.is_admin,
    staleTime: 3_600_000,
  });
  return user?.is_admin ? q.data : undefined;
}

/** For administrators: a newer Forgust release is out, in a notice that stays until dismissed (then until the next one). */
function UpdateNotice() {
  const status = useUpdateStatus();
  const { announce, withdraw } = useToast();
  const v = status?.available;
  const url = status?.url;
  useEffect(() => {
    if (!v) return;
    // remembered per release, as the site banner is per text: a notice left
    // open in another tab, for an older release, cannot bring this one back
    const seen = `forgust.update.dismissed:${v}`;
    try {
      if (localStorage.getItem(seen) === "1") return;
    } catch {
      // storage blocked: shown on each load
    }
    const key = `update:${v}`;
    announce({
      key,
      title: "Update available",
      text: `Forgust ${v} is available.`,
      action: url ? { label: "View release notes", href: url } : undefined,
      onDismiss: () => {
        try {
          localStorage.setItem(seen, "1");
        } catch {
          // not remembered: back on the next load
        }
      },
    });
    // gone with what it says: signed out, the check turned off, a newer release
    return () => withdraw(key);
  }, [v, url, announce, withdraw]);
  return null;
}

/** The bottom of the sidebar, for administrators: this server's version, and a newer one. */
function SideVersion() {
  const status = useUpdateStatus();
  if (!status) return null;
  const title = `Forgust ${status.available} is available`;
  return (
    <div className="side-version">
      <span>v{status.current}</span>
      {status.available &&
        (status.url ? (
          <a href={status.url} target="_blank" rel="noreferrer" title={title}>
            <span className="dot" />
            Update available
          </a>
        ) : (
          <span title={title}>Update available</span>
        ))}
    </div>
  );
}

function EmailBanner() {
  const { user, emailEnabled } = useSession();
  // no email here: no link was sent, and none can be
  if (!user || user.email_verified !== false || !emailEnabled) return null;
  return (
    <div className="banner email-banner" role="status">
      <span className="grow">
        Confirm <strong>{user.email}</strong>: open the link we sent. Until then you get no notifications by email.
      </span>
      <ResendLink />
    </div>
  );
}

function NewMenu() {
  const nav = useNav();
  const [open, setOpen] = useState(false);
  const items: { to: string; label: string }[] = [];
  if (nav.kind === "project") items.push({ to: `/${nav.project.full_path}/-/issues/new`, label: "New issue" });
  if (nav.kind === "project" && nav.project.can_push) items.push({ to: `/${nav.project.full_path}/-/merge_requests/new`, label: "New merge request" });
  if (nav.kind === "group" && nav.group.can_create_project) items.push({ to: `/new/project?namespace=${encodeURIComponent(nav.group.full_path)}`, label: "New project in this group" });
  if (nav.kind === "group" && nav.group.can_create_subgroup) items.push({ to: `/new/group?parent=${nav.group.id}`, label: "New subgroup" });
  items.push({ to: "/new/project", label: "New project" }, { to: "/new/group", label: "New organization" }, { to: "/import", label: "Import projects" });
  return (
    <div className="menu-wrap" onBlur={(e) => !e.currentTarget.contains(e.relatedTarget as Node) && setOpen(false)}>
      <button className="icon-btn" onClick={() => setOpen(!open)} aria-haspopup="menu" aria-expanded={open} title="Create new…" aria-label="Create new">
        <Plus size={18} />
      </button>
      {open && (
        <div className="menu" role="menu">
          {items.map((i) => (
            <Link key={i.to} role="menuitem" to={i.to} onClick={() => setOpen(false)}>
              {i.label}
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}

const IS_MAC = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

/** The app frame: sidebar, slim top bar with breadcrumbs, and the page. */
/** Pages a signed-out visitor reaches to get in: they replace the top bar with their own frame. */
const AUTH_PATHS = ["/login", "/setup", "/forgot-password", "/reset-password", "/verify-email"];

export function Shell({ children }: { children: ReactNode }) {
  const { user } = useSession();
  const [collapsed, setCollapsed] = usePref("sidebar.collapsed", false);
  const [mobileOpen, setMobileOpen] = useState(false);
  const loc = useLocation();
  useSettleNav();
  useEffect(() => setMobileOpen(false), [loc.pathname]);
  // the page slides along as the sidebar folds or unfolds (FLIP: its margin
  // changes at once, a transform eases it over); the sidebar itself is clipped
  const mainRef = useRef<HTMLElement>(null);
  const mainLeft = useRef<number | null>(null);
  useLayoutEffect(() => {
    const el = mainRef.current;
    if (!el) return;
    el.getAnimations().forEach((a) => a.cancel());
    const left = el.getBoundingClientRect().left;
    const from = mainLeft.current;
    mainLeft.current = left;
    if (from == null || from === left || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    // the sidebar's own curve and duration (styles.css)
    const easing = getComputedStyle(el).getPropertyValue("--ease-in-out").trim() || "ease-in-out";
    el.animate([{ transform: `translateX(${from - left}px)` }, { transform: "none" }], { duration: 240, easing });
  }, [collapsed, !!user]);
  // Escape closes the menu on small screens
  useEffect(() => {
    if (!mobileOpen) return;
    const key = (e: KeyboardEvent) => e.key === "Escape" && setMobileOpen(false);
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [mobileOpen]);

  // the sign-in pages are full screen: they bring their own frame (AuthLayout)
  if (!user && AUTH_PATHS.includes(loc.pathname)) {
    return (
      <main className="main-solo">
        <SiteBanner />
        {children}
      </main>
    );
  }
  if (!user) {
    return (
      <>
        <header className="topbar">
          <Link to="/" className="brand">
            <Logo size={24} />
            Forgust
          </Link>
          <div className="grow" />
          <ThemeButton />
          <Link to="/login" className="btn small">
            Sign in
          </Link>
        </header>
        <main className="main-solo">
          <SiteBanner />
          {children}
        </main>
      </>
    );
  }
  return (
    <div className={`app ${collapsed ? "side-collapsed" : ""}`}>
      <header className="topbar">
        <button className="icon-btn mobile-only" onClick={() => setMobileOpen(!mobileOpen)} aria-label="Open navigation">
          <Menu size={18} />
        </button>
        <Link to="/" className="brand">
          <Logo size={24} />
          <span className="brand-name">Forgust</span>
        </Link>
        <Breadcrumbs />
        <button className="search-btn" onClick={() => openPalette()} aria-label="Search or go to (Ctrl+K)">
          <Search size={15} />
          <span className="search-label">Search or go to…</span>
          <kbd>{IS_MAC ? "⌘K" : "Ctrl K"}</kbd>
        </button>
        <NewMenu />
        <InboxBell />
        <ThemeButton />
        <UserMenu />
      </header>
      {mobileOpen && <div className="scrim" onClick={() => setMobileOpen(false)} />}
      <Sidebar collapsed={collapsed} setCollapsed={setCollapsed} mobileOpen={mobileOpen} closeMobile={() => setMobileOpen(false)} />
      <main className="main" ref={mainRef}>
        <ImpersonationBanner />
        <UpdateNotice />
        <SiteBanner />
        <EmailBanner />
        {children}
      </main>
      <HoverCards />
    </div>
  );
}

/**
 * The shell's frame while the app learns who is signed in (main.tsx): the top bar and,
 * if someone was signed in here last time, the sidebar, each where the shell puts it, so
 * nothing moves when the page comes; a slow answer shows the loader where the page goes.
 */
export function ShellFrame() {
  const [collapsed, setCollapsed] = usePref("sidebar.collapsed", false);
  const loc = useLocation();
  if (!wasSignedIn()) {
    // the sign-in pages bring their own frame
    if (AUTH_PATHS.includes(loc.pathname)) {
      return (
        <main className="main-solo">
          <Spinner />
        </main>
      );
    }
    return (
      <>
        <header className="topbar">
          <Link to="/" className="brand">
            <Logo size={24} />
            Forgust
          </Link>
          <div className="grow" />
          <ThemeButton />
          <Link to="/login" className="btn small">
            Sign in
          </Link>
        </header>
        <main className="main-solo">
          <Spinner />
        </main>
      </>
    );
  }
  return (
    <div className={`app ${collapsed ? "side-collapsed" : ""}`}>
      <header className="topbar">
        {/* in place for the brand's sake on small screens; the menu has nothing yet */}
        <button className="icon-btn mobile-only" aria-label="Open navigation">
          <Menu size={18} />
        </button>
        <Link to="/" className="brand">
          <Logo size={24} />
          <span className="brand-name">Forgust</span>
        </Link>
      </header>
      <aside className={`sidebar ${collapsed ? "collapsed" : ""}`} aria-label="Navigation">
        <div className="spacer" />
        <CollapseButton collapsed={collapsed} setCollapsed={setCollapsed} />
      </aside>
      <main className="main">
        <Spinner />
      </main>
    </div>
  );
}
