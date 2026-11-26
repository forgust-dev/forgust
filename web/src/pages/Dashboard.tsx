import { useState, type ReactElement, type ReactNode } from "react";
import { Link, NavLink } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { GitMerge, Globe, Lock, Shield } from "lucide-react";
import { api, qs, timeAgo, type S } from "../api/client";
import { Empty, ListHead, Q, SearchField, Tile, useDebounced } from "../components/ui";
import { Segmented, Select } from "../components/controls";
import { useGroups, useMyProjects, useMyProjectStates, usePrefetch } from "../prefetch";
import { useSession } from "../session";
import { usePref } from "../shell/theme";
import { StatusIcon, statusLabel } from "./project/ci/Status";

const VIS_ICON: Record<string, ReactElement> = {
  private: <Lock size={12} />,
  internal: <Shield size={12} />,
  public: <Globe size={12} />,
};

export function Visibility({ v }: { v: string }) {
  return (
    <span className="vis" title={`${v[0].toUpperCase()}${v.slice(1)}`}>
      {VIS_ICON[v]}
      {v}
    </span>
  );
}

export function ProjectRow({ p, level, aside }: { p: S["ProjectDto"]; level?: string; aside?: ReactNode }) {
  const pf = usePrefetch({ kind: "project", id: p.id, path: p.full_path });
  const slash = p.full_path.lastIndexOf("/");
  return (
    <div className="item project-row">
      <Tile name={p.name} src={p.avatar_url} />
      <div className="grow" style={{ minWidth: 0 }}>
        <div className="row-title">
          <Link to={`/${p.full_path}`} className="ellipsis" {...pf}>
            <span className="ns">{p.full_path.slice(0, slash + 1)}</span>
            {p.full_path.slice(slash + 1)}
          </Link>
          <Visibility v={p.visibility} />
          {level && <span className="pill">{level}</span>}
        </div>
        {p.description && <div className="row-desc ellipsis">{p.description}</div>}
      </div>
      {aside}
      <span className="row-time" title={new Date(p.last_activity_at).toLocaleString()}>
        {timeAgo(p.last_activity_at)}
      </span>
    </div>
  );
}

export function GroupRow({ g }: { g: S["GroupDto"] }) {
  return (
    <Link to={`/${g.full_path}`} className="item group-row">
      <Tile name={g.name} src={g.avatar_url} group />
      <span className="grow" style={{ minWidth: 0 }}>
        <span className="row-title">
          <span className="ellipsis">{g.name}</span>
        </span>
        <span className="row-meta">
          <span>{g.full_path}</span>
        </span>
      </span>
      <Visibility v={g.visibility} />
    </Link>
  );
}

/** How a project is doing, beside it in a list: its open merge requests and its default
 *  branch's latest pipeline (a group's page, your projects). Nothing when neither. */
export function ProjectStateBadges({ p, st, plain }: { p: S["ProjectDto"]; st?: S["ProjectState"]; plain?: boolean }) {
  if (!st) return null;
  const open = st.open_merge_requests;
  const pipe = st.pipeline_iid != null && st.pipeline_status ? { iid: st.pipeline_iid, status: st.pipeline_status, ref: st.pipeline_ref ?? "" } : null;
  if (!pipe && open === 0) return null;
  return (
    <span className="project-state">
      {open > 0 && (
        <StatePart plain={plain} to={`/${p.full_path}/-/merge_requests`} title={`${open} open merge request${open === 1 ? "" : "s"}`}>
          <GitMerge size={13} aria-hidden /> {open}
        </StatePart>
      )}
      {pipe && (
        <StatePart plain={plain} className={`st-${pipe.status}`} to={`/${p.full_path}/-/pipelines/${pipe.iid}`} title={`Pipeline #${pipe.iid} on ${pipe.ref}: ${statusLabel(pipe.status)}`}>
          <StatusIcon status={pipe.status} size={15} />
        </StatePart>
      )}
    </span>
  );
}

/** A part of a project's state: a link to it, or (`plain`: inside a link already, a card) only what it says. */
function StatePart({ plain, to, className, title, children }: { plain?: boolean; to: string; className?: string; title: string; children: ReactNode }) {
  return plain ? (
    <span className={className} title={title}>
      {children}
    </span>
  ) : (
    <Link to={to} className={className} title={title}>
      {children}
    </Link>
  );
}

type Scope = "all" | "owned" | "personal" | "archived";
type Sort = "activity" | "name" | "created";
type View = "list" | "namespace";

const SORTS: Record<Sort, (a: S["MyProject"], b: S["MyProject"]) => number> = {
  activity: (a, b) => b.last_activity_at.localeCompare(a.last_activity_at),
  name: (a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" }) || a.full_path.localeCompare(b.full_path),
  created: (a, b) => b.created_at.localeCompare(a.created_at),
};

/** Where a project lives: its namespace's path. */
const nsOf = (p: { full_path: string }) => p.full_path.slice(0, p.full_path.lastIndexOf("/"));

/** A project of yours in the list: your role when you do not own it, how it is doing. */
function MyProjectRow({ p, st }: { p: S["MyProject"]; st?: S["ProjectState"] }) {
  return <ProjectRow p={p} level={p.access_level === "owner" ? undefined : p.access_level} aside={<ProjectStateBadges p={p} st={st} />} />;
}

/** The list cut by namespace: each organization, subgroup or personal namespace with
 *  its projects, in the list's order (the one with the first project first). */
function ByNamespace({ list, stateOf, groups, username }: { list: S["MyProject"][]; stateOf: Map<number, S["ProjectState"]>; groups: S["GroupDto"][]; username?: string }) {
  const sections = new Map<string, S["MyProject"][]>();
  for (const p of list) sections.set(nsOf(p), [...(sections.get(nsOf(p)) ?? []), p]);
  // each organization where its first project comes, its subgroups right under it
  const rootOf = (ns: string) => ns.split("/")[0];
  const firstRoot = new Map<string, number>();
  [...sections.keys()].forEach((ns, i) => firstRoot.has(rootOf(ns)) || firstRoot.set(rootOf(ns), i));
  const order = [...sections].sort(([a], [b]) => firstRoot.get(rootOf(a))! - firstRoot.get(rootOf(b))! || a.localeCompare(b));
  const top = new Map(groups.map((g) => [g.full_path, g]));
  return (
    <div className="stack dash-namespaces">
      {order.map(([ns, projects]) => {
        const personal = ns === username;
        const g = top.get(ns);
        return (
          <section key={ns} className="dash-ns" aria-label={ns}>
            <div className="dash-ns-head">
              <Tile name={g?.name ?? ns.split("/").at(-1) ?? ns} src={g?.avatar_url} group={!personal} />
              <Link to={`/${ns}`} className="dash-ns-name">
                {personal ? "Personal" : (g?.name ?? ns)}
              </Link>
              {personal && <span className="dash-ns-path">{ns}</span>}
              <span className="section-count">{projects.length}</span>
            </div>
            <div className="list rows">
              {projects.map((p) => (
                <MyProjectRow key={p.id} p={p} st={stateOf.get(p.id)} />
              ))}
            </div>
          </section>
        );
      })}
    </div>
  );
}

function ProjectsTab() {
  const { user } = useSession();
  const [search, setSearch] = useState("");
  const term = useDebounced(search, 250);
  const [scope, setScope] = useState<Scope>("all");
  const [sort, setSort] = usePref<Sort>("projects.sort", "activity");
  const [view, setView] = usePref<View>("projects.view", "list");
  const q = useMyProjects(term);
  const states = useMyProjectStates();
  const groups = useGroups();
  const stateOf = new Map((states.data ?? []).map((s) => [s.project_id, s]));
  const mine = user ? `${user.username}/` : null;
  const all = q.data ?? [];
  const live = all.filter((p) => !p.archived);
  const lists: Record<Scope, S["MyProject"][]> = {
    all: live,
    owned: live.filter((p) => p.access_level === "owner"),
    personal: mine ? live.filter((p) => p.full_path.startsWith(mine)) : [],
    archived: all.filter((p) => p.archived),
  };
  const list = [...lists[scope]].sort(SORTS[sort]);
  const counted = (v: Scope) => (q.data ? lists[v].length : undefined);
  // a filter with nothing in it only shows while it is chosen
  const shown = (v: Scope) => v === "all" || v === "owned" || v === scope || lists[v].length > 0;
  const options = (
    [
      { value: "all", label: "All" },
      { value: "owned", label: "Owned" },
      { value: "personal", label: "Personal" },
      { value: "archived", label: "Archived" },
    ] as { value: Scope; label: string }[]
  )
    .filter((o) => shown(o.value))
    .map((o) => ({ ...o, count: counted(o.value) }));
  return (
    <div className="stack" style={{ gap: 14 }}>
      <div className="list-toolbar plain dash-toolbar">
        <nav className="scope-chips" aria-label="Which projects">
          {options.map((o) => (
            <button key={o.value} type="button" aria-pressed={o.value === scope} onClick={() => setScope(o.value)}>
              {o.label}
              {o.count != null && <span className="chip-count">{o.count}</span>}
            </button>
          ))}
        </nav>
        <div className="dash-controls">
          <Select
            label="Sort by"
            value={sort}
            onChange={(v) => setSort(v as Sort)}
            options={[
              { value: "activity", label: "Last active first" },
              { value: "name", label: "Name, A to Z" },
              { value: "created", label: "Newest first" },
            ]}
          />
          <Segmented
            label="Show"
            value={view}
            onChange={setView}
            options={[
              { value: "list", label: "List" },
              { value: "namespace", label: "By organization" },
            ]}
          />
          <SearchField value={search} onChange={setSearch} placeholder="Filter projects" />
        </div>
      </div>
      <Q q={q}>
        {() =>
          list.length === 0 ? (
            <Empty>
              {search ? (
                <>No project matches “{search}”.</>
              ) : scope === "owned" ? (
                <>You don't own a project yet.</>
              ) : scope === "personal" ? (
                <>You have no personal project.</>
              ) : scope === "archived" ? (
                <>No archived project.</>
              ) : (
                <>
                  No projects yet. <Link to="/new/project">Create one</Link>, <Link to="/import">import one</Link> or ask to be added to an organization.
                </>
              )}
            </Empty>
          ) : view === "namespace" ? (
            <ByNamespace list={list} stateOf={stateOf} groups={groups.data ?? []} username={user?.username} />
          ) : (
            <div className="list rows">
              {list.map((p) => (
                <MyProjectRow key={p.id} p={p} st={stateOf.get(p.id)} />
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}

/** An organization as a card: its picture, what it is, how many of your projects it holds. */
function OrgCard({ g, projects }: { g: S["GroupDto"]; projects: number }) {
  const pf = usePrefetch({ kind: "group", id: g.id, path: g.full_path });
  return (
    <Link to={`/${g.full_path}`} className="project-card" title={g.full_path} {...pf}>
      <span className="project-card-head">
        <Tile name={g.name} src={g.avatar_url} group />
        <span className="grow" style={{ minWidth: 0 }}>
          <span className="project-card-name ellipsis">{g.name}</span>
          <span className="project-card-ns ellipsis">{g.full_path}</span>
        </span>
      </span>
      <span className={`project-card-desc${g.description ? "" : " none"}`}>{g.description || "No description"}</span>
      <span className="project-card-foot">
        <Visibility v={g.visibility} />
        <span className="grow" />
        <span>{projects === 0 ? "None of your projects" : `${projects} of your project${projects === 1 ? "" : "s"}`}</span>
      </span>
    </Link>
  );
}

function OrganizationsTab({ groups, projects }: { groups: ReturnType<typeof useGroups>; projects: S["MyProject"][] }) {
  const [search, setSearch] = useState("");
  const match = (s: string) => s.toLowerCase().includes(search.trim().toLowerCase());
  return (
    <div className="stack" style={{ gap: 14 }}>
      {(groups.data?.length ?? 0) > 6 && (
        <div className="list-toolbar plain">
          <SearchField value={search} onChange={setSearch} placeholder="Filter organizations" />
        </div>
      )}
      <Q q={groups}>
        {(all) => {
          const list = all.filter((g) => match(g.name) || match(g.full_path));
          if (all.length === 0)
            return (
              <Empty>
                No organizations yet. <Link to="/new/group">Create one</Link> to share projects with a team.
              </Empty>
            );
          if (list.length === 0) return <Empty>No organization matches “{search}”.</Empty>;
          return (
            <div className="project-cards">
              {list.map((g) => (
                <OrgCard key={g.id} g={g} projects={projects.filter((p) => p.full_path.startsWith(`${g.full_path}/`) && !p.archived).length} />
              ))}
            </div>
          );
        }}
      </Q>
    </div>
  );
}

/** Each tab's title and what it shows. */
const TABS = {
  projects: { title: "Projects", sub: "Every project you are a member of, and how it is doing." },
  groups: { title: "Organizations", sub: "The organizations you can see, and your projects in each." },
  explore: { title: "Explore", sub: "What others share: the internal and public projects." },
};

/**
 * The projects page, in three tabs: your projects (/dashboard/projects) and how each is
 * doing, to filter, sort and see by organization; your organizations (/dashboard/groups),
 * as cards; and what others share (/explore).
 */
export function Dashboard({ tab = "projects" }: { tab?: keyof typeof TABS }) {
  // the whole lists, for the tabs' counts (the projects tab filters its own)
  const projects = useMyProjects();
  const groups = useGroups();
  const live = projects.data?.filter((p) => !p.archived).length;
  return (
    <div className="page stack list-page">
      <ListHead title={TABS[tab].title} sub={TABS[tab].sub}>
        {tab !== "groups" ? (
          <>
            <Link to="/import" className="btn">
              Import
            </Link>
            <Link to="/new/project" className="btn primary">
              New project
            </Link>
          </>
        ) : (
          <Link to="/new/group" className="btn primary">
            New organization
          </Link>
        )}
      </ListHead>
      <nav className="tabs dash-tabs" aria-label="Projects, organizations and what others share">
        <NavLink to="/dashboard/projects" end>
          Projects{live != null && <span className="tab-count">{live}</span>}
        </NavLink>
        <NavLink to="/dashboard/groups" end>
          Organizations{groups.data && <span className="tab-count">{groups.data.length}</span>}
        </NavLink>
        <NavLink to="/explore" end>
          Explore
        </NavLink>
      </nav>
      {tab === "projects" ? <ProjectsTab /> : tab === "groups" ? <OrganizationsTab groups={groups} projects={projects.data ?? []} /> : <ExploreList />}
    </div>
  );
}

/** What others share: the instance's internal and public projects, to search. */
function ExploreList() {
  const [search, setSearch] = useState("");
  const term = useDebounced(search, 250);
  const q = useQuery({ queryKey: ["explore", term], queryFn: () => api.get<S["ProjectDto"][]>(`/projects${qs({ search: term, per_page: 100 })}`) });
  return (
    <div className="stack" style={{ gap: 14 }}>
      <div className="list-toolbar plain">
        <SearchField value={search} onChange={setSearch} placeholder="Search projects" />
      </div>
      <Q q={q}>
        {(list) =>
          list.length === 0 ? (
            <Empty>{search ? <>No project matches “{search}”.</> : "Nothing to explore yet."}</Empty>
          ) : (
            <div className="list rows">
              {list.map((p) => (
                <ProjectRow key={p.id} p={p} />
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}

/** Explore for someone signed out: the public projects, on their own. */
function Explore() {
  return (
    <div className="page stack list-page">
      <ListHead title="Explore" sub="Internal and public projects." />
      <ExploreList />
    </div>
  );
}

/** `/explore`: a tab of the projects page once signed in, a page of its own before. */
export function ExploreRoute() {
  const { user, loading } = useSession();
  if (loading) return null;
  return user ? <Dashboard tab="explore" /> : <Explore />;
}
