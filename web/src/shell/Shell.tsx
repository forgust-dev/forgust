import { useEffect, useState, type ReactNode } from "react";
import { Link, NavLink, useLocation } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  AlertTriangle,
  UserCheck,
  Info,
  X,
  CircleDot,
  Boxes,
  Package,
  Container,
  Rocket,
  ChevronsLeft,
  ChevronsRight,
  Code2,
  Compass,
  FolderGit2,
  Inbox,
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
} from "lucide-react";
import { api, qs, type S } from "../api/client";
import { useSession } from "../session";
import { useToast } from "./toast";
import { usePref, useTheme } from "./theme";
import { openPalette } from "./palette";
import { useNav } from "./nav";
import { HoverCards } from "../components/HoverCards";
import { Avatar, Tile } from "../components/ui";
import { ResendLink } from "../components/ResendLink";
import { useWork } from "../prefetch";

/** A nav entry; `children` show under it while its section is active. */
type Item = { to: string; label: string; icon?: ReactNode; end?: boolean; count?: number; match?: RegExp; children?: Item[] };

function SideLink({ item, collapsed, onNavigate, sub }: { item: Item; collapsed: boolean; onNavigate: () => void; sub?: boolean }) {
  const loc = useLocation();
  const active = item.match ? item.match.test(loc.pathname) : undefined;
  return (
    <NavLink
      to={item.to}
      end={item.end}
      onClick={onNavigate}
      title={collapsed ? item.label : undefined}
      className={({ isActive }) => `side-link ${sub ? "sub" : ""} ${(active ?? isActive) ? "active" : ""}`}
    >
      {item.icon}
      {!collapsed && <span className="grow">{item.label}</span>}
      {!collapsed && item.count != null && item.count > 0 && <span className="count">{item.count}</span>}
    </NavLink>
  );
}

/** A top-level entry, plus its sub-entries when its section is open. */
function SideEntry({ item, collapsed, onNavigate }: { item: Item; collapsed: boolean; onNavigate: () => void }) {
  const loc = useLocation();
  const open = !!item.match?.test(loc.pathname) && !collapsed && (item.children?.length ?? 0) > 1;
  return (
    <>
      <SideLink item={item} collapsed={collapsed} onNavigate={onNavigate} />
      {open && (
        <div className="side-sub">
          {item.children!.map((c) => (
            <SideLink key={c.to + c.label} item={c} collapsed={false} onNavigate={onNavigate} sub />
          ))}
        </div>
      )}
    </>
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
  const security = developer ? [{ to: `${web}/-/security`, label: "Security", icon: <ShieldAlert size={16} />, match: sec("security") }] : [];
  return [
    {
      to: web,
      label: "Code",
      icon: <Code2 size={16} />,
      match: code,
      children: [
        { to: web, label: "Files", match: files },
        { to: `${web}/-/commits/${ref}`, label: "Commits", match: sec("commits?") },
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
      icon: <Rocket size={16} />,
      match: sec("pipelines|jobs|pipeline_schedules|environments"),
      children: [
        { to: `${web}/-/pipelines`, label: "Pipelines", match: sec("pipelines|jobs") },
        { to: `${web}/-/environments`, label: "Environments", match: sec("environments") },
        ...schedules,
      ],
    },
    ...security,
    { to: `${web}/-/releases`, label: "Releases", icon: <Package size={16} />, match: sec("releases") },
    { to: `${web}/-/container_registry`, label: "Container registry", icon: <Container size={16} />, match: sec("container_registry") },
    { to: `${web}/-/members`, label: "Members", icon: <Users size={16} />, match: sec("members") },
    ...(p.can_admin
      ? [
          {
            to: `${web}/-/settings`,
            label: "Settings",
            icon: <Settings size={16} />,
            match: sec("settings"),
            children: [
              { to: `${web}/-/settings`, label: "General", match: new RegExp(`^/${esc}/-/settings/?$`) },
              { to: `${web}/-/settings/ci_cd`, label: "CI/CD", match: sec("settings/ci_cd") },
            ],
          },
        ]
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
  const nav = useNav();
  const { user } = useSession();
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
  const work = useWork(!!user && nav.kind === "global");
  const waiting = work.data ? work.data.todos_pending : undefined;
  const c = collapsed && !mobileOpen;

  let header: ReactNode = null;
  let items: Item[];
  if (nav.kind === "project") {
    header = (
      <Link to={`/${nav.project.full_path}`} className="side-context" title={nav.project.full_path} onClick={closeMobile}>
        <Tile name={nav.project.name} src={nav.project.avatar_url} />
        {!c && <span className="ellipsis">{nav.project.name}</span>}
      </Link>
    );
    items = projectItems(nav.project, nav.ref ?? nav.project.default_branch ?? "main", mrs.data?.opened, issues.data?.opened);
  } else if (nav.kind === "group") {
    header = (
      <Link to={`/${nav.group.full_path}`} className="side-context" title={nav.group.full_path} onClick={closeMobile}>
        <Tile name={nav.group.name} src={nav.group.avatar_url} group />
        {!c && <span className="ellipsis">{nav.group.name}</span>}
      </Link>
    );
    items = groupItems(nav.group);
  } else {
    items = [
      { to: "/", label: "Your work", icon: <Inbox size={16} />, end: true, count: waiting },
      { to: "/dashboard/projects", label: "Projects", icon: <FolderGit2 size={16} /> },
      { to: "/explore", label: "Explore", icon: <Compass size={16} /> },
      ...(user?.is_admin ? [{ to: "/admin", label: "Admin area", icon: <Shield size={16} /> }] : []),
    ];
  }

  return (
    <aside className={`sidebar ${c ? "collapsed" : ""} ${mobileOpen ? "open" : ""}`} aria-label="Navigation">
      {nav.kind !== "global" && (
        <Link to="/" className="side-link back" onClick={closeMobile} title="Your work">
          <Home size={16} />
          {!c && <span>Your work</span>}
        </Link>
      )}
      {header}
      <nav className="side-items">
        {items.map((it) => (
          <SideEntry key={it.to + it.label} item={it} collapsed={c} onNavigate={closeMobile} />
        ))}
      </nav>
      <div className="spacer" />
      <button className="side-link collapse-btn" onClick={() => setCollapsed(!collapsed)} title={collapsed ? "Expand sidebar" : "Collapse sidebar"} aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}>
        {collapsed ? <ChevronsRight size={16} /> : <ChevronsLeft size={16} />}
        {!c && <span>Collapse</span>}
      </button>
      {!c && <SideVersion />}
    </aside>
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

function ThemeButton() {
  const { theme, setTheme } = useTheme();
  const next = theme === "system" ? "light" : theme === "light" ? "dark" : "system";
  const icon = theme === "light" ? <Sun size={16} /> : theme === "dark" ? <Moon size={16} /> : <Monitor size={16} />;
  return (
    <button className="icon-btn" onClick={() => setTheme(next)} title={`Theme: ${theme} (click for ${next})`} aria-label={`Theme: ${theme}`}>
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
          <Link role="menuitem" className="menu-head" to={`/${user.username}`} onClick={() => setOpen(false)} title="Your profile">
            <strong>{user.name || user.username}</strong>
            <span className="faint small">@{user.username}</span>
          </Link>
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
  const toast = useToast();
  const [dismissed, setDismissed] = usePref("update.dismissed", "");
  const v = status?.available;
  const url = status?.url;
  useEffect(() => {
    if (!v || dismissed === v) return;
    toast.notice({
      key: `update:${v}`,
      title: "Update available",
      text: `Forgust ${v} is available.`,
      action: url ? { label: "View release notes", href: url } : undefined,
      onDismiss: () => setDismissed(v),
    });
  }, [v, url, dismissed]);
  return null;
}

/** The bottom of the sidebar, for administrators: this server's version, and a newer one. */
function SideVersion() {
  const status = useUpdateStatus();
  if (!status) return null;
  return (
    <div className="side-version">
      <span>v{status.current}</span>
      {status.available &&
        (status.url ? (
          <a href={status.url} target="_blank" rel="noreferrer" title={`Forgust ${status.available} is available`}>
            <span className="dot" />
            Update available
          </a>
        ) : (
          <span title={`Forgust ${status.available} is available`}>Update available</span>
        ))}
    </div>
  );
}

function EmailBanner() {
  const { user } = useSession();
  if (!user || user.email_verified !== false) return null;
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
export function Shell({ children }: { children: ReactNode }) {
  const { user } = useSession();
  const [collapsed, setCollapsed] = usePref("sidebar.collapsed", false);
  const [mobileOpen, setMobileOpen] = useState(false);
  const loc = useLocation();
  useEffect(() => setMobileOpen(false), [loc.pathname]);
  // Escape closes the menu on small screens
  useEffect(() => {
    if (!mobileOpen) return;
    const key = (e: KeyboardEvent) => e.key === "Escape" && setMobileOpen(false);
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [mobileOpen]);

  if (!user) {
    return (
      <>
        <header className="topbar">
          <Link to="/" className="brand">
            <img src="/favicon.svg" width={24} height={24} alt="" />
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
          <img src="/favicon.svg" width={24} height={24} alt="" />
          <span className="brand-name">Forgust</span>
        </Link>
        <Breadcrumbs />
        <button className="search-btn" onClick={() => openPalette()} aria-label="Search or go to (Ctrl+K)">
          <Search size={15} />
          <span className="search-label">Search or go to…</span>
          <kbd>{IS_MAC ? "⌘K" : "Ctrl K"}</kbd>
        </button>
        <NewMenu />
        <ThemeButton />
        <UserMenu />
      </header>
      {mobileOpen && <div className="scrim" onClick={() => setMobileOpen(false)} />}
      <Sidebar collapsed={collapsed} setCollapsed={setCollapsed} mobileOpen={mobileOpen} closeMobile={() => setMobileOpen(false)} />
      <main className="main">
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
