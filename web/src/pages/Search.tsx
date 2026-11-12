import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { CircleDot, FolderGit2, GitMerge } from "lucide-react";
import { api, qs, timeAgo, type S } from "../api/client";
import { Empty, FilterTabs, ListHead, Q, SearchField, Pager, useDebounced } from "../components/ui";

type Scope = "projects" | "issues" | "merge_requests";
const PER_PAGE = 20;

/** Projects, issues and merge requests across the instance, by title. */
export function SearchPage() {
  const [params, setParams] = useSearchParams();
  const q = params.get("q") ?? "";
  const scope = (params.get("scope") as Scope) || "issues";
  const state = params.get("state") ?? "all";
  const page = Number(params.get("page") ?? "1");
  const [term, setTerm] = useState(q);
  useEffect(() => setTerm(q), [q]);
  // typing settles before it searches
  const settled = useDebounced(term.trim(), 300);
  useEffect(() => {
    if (settled !== q) setParams(settled ? { q: settled, scope } : {}, { replace: true });
  }, [settled]);
  const set = (k: string, v: string) => {
    const next = new URLSearchParams(params);
    next.set(k, v);
    next.delete("page");
    if (k === "scope") next.delete("state");
    setParams(next, { replace: true });
  };
  const r = useQuery({
    queryKey: ["search", q, scope, state, page],
    queryFn: () => api.get<S["SearchResults"]>(`/search${qs({ q, scope, state: scope === "projects" ? undefined : state, page, per_page: PER_PAGE })}`),
    enabled: !!q,
    placeholderData: (prev) => prev,
  });
  const items = scope === "projects" ? (r.data?.projects.length ?? 0) : scope === "issues" ? (r.data?.issues.length ?? 0) : (r.data?.merge_requests.length ?? 0);
  return (
    <div className="page">
      <div className="stack list-page">
        <ListHead title="Search" sub="Projects, issues and merge requests you can see, by title." />
        <SearchField value={term} onChange={setTerm} placeholder="Search…" />
        {!q ? (
          <Empty>Type to search.</Empty>
        ) : (
          <>
            <div className="list-toolbar">
              <FilterTabs
                label="What"
                value={scope}
                onChange={(v) => set("scope", v)}
                options={[
                  { value: "issues", label: "Issues", count: r.data?.counts.issues },
                  { value: "merge_requests", label: "Merge requests", count: r.data?.counts.merge_requests },
                  { value: "projects", label: "Projects", count: r.data?.counts.projects },
                ]}
              />
              {scope !== "projects" && (
                <div className="toolbar-end">
                  <FilterTabs
                    label="State"
                    value={state}
                    onChange={(v) => set("state", v)}
                    options={[
                      { value: "all", label: "All" },
                      { value: "opened", label: "Open" },
                      ...(scope === "merge_requests" ? [{ value: "merged", label: "Merged" }] : []),
                      { value: "closed", label: "Closed" },
                    ]}
                  />
                </div>
              )}
            </div>
            <Q q={r}>
              {(d) => {
                if (scope === "projects")
                  return d.projects.length === 0 ? (
                    <Empty>No project matches “{q}”.</Empty>
                  ) : (
                    <div className="list rows">
                      {d.projects.map((p) => (
                        <div className="item" key={p.id}>
                          <FolderGit2 size={16} className="faint" />
                          <div className="grow" style={{ minWidth: 0 }}>
                            <div className="row-title">
                              <Link to={`/${p.full_path}`} className="ellipsis">
                                {p.full_path}
                              </Link>
                            </div>
                            {p.description && <div className="row-meta ellipsis">{p.description}</div>}
                          </div>
                          <span className="row-time">{timeAgo(p.last_activity_at)}</span>
                        </div>
                      ))}
                    </div>
                  );
                const list = scope === "issues" ? d.issues : d.merge_requests;
                const kind = scope === "issues" ? "issues" : "merge_requests";
                const sigil = scope === "issues" ? "#" : "!";
                return list.length === 0 ? (
                  <Empty>Nothing matches “{q}”.</Empty>
                ) : (
                  <div className="list rows">
                    {list.map((i) => (
                      <div className="item" key={`${i.project_id}-${i.iid}`}>
                        {scope === "issues" ? (
                          <CircleDot size={15} className={i.state === "opened" ? "ic-ok" : "faint"} />
                        ) : (
                          <GitMerge size={15} className={i.state === "opened" ? "ic-ok" : i.state === "merged" ? "ic-accent" : "faint"} />
                        )}
                        <div className="grow" style={{ minWidth: 0 }}>
                          <div className="row-title">
                            <Link to={`/${i.project_path}/-/${kind}/${i.iid}`} className="ellipsis">
                              {i.title}
                            </Link>
                          </div>
                          <div className="row-meta">
                            <span>
                              {i.project_path}
                              {sigil}
                              {i.iid}
                            </span>
                            <span>{i.state}</span>
                            {i.author_username && <span>by {i.author_username}</span>}
                          </div>
                        </div>
                        <span className="row-time">{timeAgo(i.updated_at)}</span>
                      </div>
                    ))}
                  </div>
                );
              }}
            </Q>
            <Pager page={page} count={items} perPage={PER_PAGE} onPage={(n) => set("page", String(n))} />
          </>
        )}
      </div>
    </div>
  );
}
