import { useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Globe, Lock, Shield } from "lucide-react";
import { api, qs, timeAgo, type S } from "../api/client";
import { Empty, FilterTabs, ListHead, Q, SearchField, Tile, useDebounced } from "../components/ui";
import { useGroups, usePrefetch } from "../prefetch";

const VIS_ICON: Record<string, JSX.Element> = {
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

export function ProjectRow({ p, level }: { p: S["ProjectDto"]; level?: string }) {
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
          {level && <span className="pill accent">{level}</span>}
        </div>
        {p.description && <div className="row-desc ellipsis">{p.description}</div>}
      </div>
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

export function Dashboard() {
  const [search, setSearch] = useState("");
  const term = useDebounced(search, 250);
  const [scope, setScope] = useState<"all" | "owned">("all");
  const projects = useQuery({
    queryKey: ["my-projects", term],
    queryFn: () => api.get<S["MyProject"][]>(`/user/projects${qs({ search: term, per_page: 100 })}`),
  });
  const groups = useGroups();
  const owned = (projects.data ?? []).filter((p) => p.access_level === "owner");

  return (
    <div className="page stack list-page">
      <ListHead title="Your projects">
        <Link to="/new/group" className="btn">
          New organization
        </Link>
        <Link to="/new/project" className="btn primary">
          New project
        </Link>
      </ListHead>
      <div className="dash-layout">
        <div className="stack" style={{ gap: 14 }}>
          <div className="list-toolbar">
            <FilterTabs
              label="Projects"
              value={scope}
              onChange={setScope}
              options={[
                { value: "all", label: "All", count: projects.data?.length },
                { value: "owned", label: "Owned", count: projects.data ? owned.length : undefined },
              ]}
            />
            <SearchField value={search} onChange={setSearch} placeholder="Filter projects" />
          </div>
          <Q q={projects}>
            {(all) => {
              const list = scope === "owned" ? owned : all;
              return list.length === 0 ? (
                <Empty>
                  {search ? (
                    <>No project matches “{search}”.</>
                  ) : scope === "owned" ? (
                    <>You don't own a project yet.</>
                  ) : (
                    <>
                      No projects yet. <Link to="/new/project">Create one</Link> or ask to be added to an organization.
                    </>
                  )}
                </Empty>
              ) : (
                <div className="list rows">
                  {list.map((p) => (
                    <ProjectRow key={p.id} p={p} level={p.access_level} />
                  ))}
                </div>
              );
            }}
          </Q>
        </div>
        <aside className="dash-side">
          <h2>Organizations</h2>
          <Q q={groups}>
            {(list) =>
              list.length === 0 ? (
                <p className="muted small">
                  No organizations yet. <Link to="/new/group">Create one</Link> to share projects with a team.
                </p>
              ) : (
                <div className="list rows">
                  {list.map((g) => (
                    <GroupRow key={g.id} g={g} />
                  ))}
                </div>
              )
            }
          </Q>
        </aside>
      </div>
    </div>
  );
}

export function Explore() {
  const [search, setSearch] = useState("");
  const term = useDebounced(search, 250);
  const q = useQuery({ queryKey: ["explore", term], queryFn: () => api.get<S["ProjectDto"][]>(`/projects${qs({ search: term, per_page: 100 })}`) });
  return (
    <div className="page stack list-page">
      <ListHead title="Explore" sub="Internal and public projects." />
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
