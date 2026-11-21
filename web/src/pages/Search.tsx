import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { CircleDot, FolderGit2, GitMerge } from "lucide-react";
import { api, qs, timeAgo, type S } from "../api/client";
import { Empty, FilterTabs, ListHead, Q, SearchField, Pager, Spinner, useDebounced } from "../components/ui";
import { Segmented } from "../components/controls";
import { CodeSearch } from "./SearchCode";

type Scope = "projects" | "issues" | "merge_requests" | "code";
const SCOPES: readonly Scope[] = ["issues", "merge_requests", "projects", "code"];
/** The tabs searched by title, in their order: with none picked, the page shows the
 *  first that finds something. */
const TITLED = ["issues", "merge_requests", "projects"] as const;
const PER_PAGE = 20;

/** A state as the state tabs name it. */
const STATE: Record<string, string> = { opened: "open", closed: "closed", merged: "merged" };

/** `text` with what was searched for marked, as the code results mark theirs (the
 *  search ignores case). */
function Marked({ text, term }: { text: string; term: string }) {
  if (!term) return <>{text}</>;
  const parts = text.split(new RegExp(`(${term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`, "gi"));
  // the separator is captured: every other part is a match
  return <>{parts.map((p, i) => (i % 2 ? <mark key={i}>{p}</mark> : p))}</>;
}

/** Projects, issues and merge requests across the instance, by title; code by its text. */
export function SearchPage() {
  const [params, setParams] = useSearchParams();
  const q = params.get("q") ?? "";
  // the tab picked; none: the first that finds something, once the counts come
  const picked = SCOPES.find((s) => s === params.get("scope")) ?? null;
  const [first, setFirst] = useState<{ q: string; scope: Scope } | null>(null);
  const scope: Scope = picked ?? (first?.q === q ? first.scope : "issues");
  const state = params.get("state") ?? "all";
  const page = Number(params.get("page") ?? "1");
  const [term, setTerm] = useState(q);
  useEffect(() => setTerm(q), [q]);
  // typing settles before it searches
  const settled = useDebounced(term.trim(), 300);
  useEffect(() => {
    if (settled === q) return;
    // the filters of a code search stay while its text changes, as a tab picked does;
    // else the new text opens on what it finds
    const next = new URLSearchParams(picked === "code" ? params : {});
    next.delete("page");
    if (settled) {
      next.set("q", settled);
      if (picked) next.set("scope", picked);
    } else next.delete("q");
    setParams(next, { replace: true });
  }, [settled]);
  /** Change one parameter of the address ("" removes it). */
  const set = (k: string, v: string) => {
    const next = new URLSearchParams(params);
    if (v) next.set(k, v);
    else next.delete(k);
    next.delete("page");
    if (k === "scope") next.delete("state");
    setParams(next, { replace: true });
  };
  // on the code tab this only fetches the counts of the other tabs (titles
  // hold 200 characters at most); the code itself is `CodeSearch`'s request
  const titles = scope === "code" ? "all" : scope;
  const r = useQuery({
    queryKey: ["search", q, titles, state, page],
    queryFn: () => api.get<S["SearchResults"]>(`/search${qs({ q, scope: titles, state: titles === "projects" || titles === "all" ? undefined : state, page, per_page: PER_PAGE })}`),
    enabled: !!q && q.length <= 200,
    placeholderData: (prev) => prev,
  });
  // any tab's answer brings the counts of all: with none picked, the first with results
  // (worked out while rendering, so an empty tab never shows on the way)
  if (!picked && r.data && !r.isPlaceholderData && first?.q !== q) {
    const c = r.data.counts;
    setFirst({ q, scope: TITLED.find((s) => c[s] > 0) ?? "issues" });
  }
  const items = scope === "projects" ? (r.data?.projects.length ?? 0) : scope === "issues" ? (r.data?.issues.length ?? 0) : (r.data?.merge_requests.length ?? 0);
  // what the list holds, in words, and so where the search looks
  const noun = scope === "projects" ? "projects" : scope === "issues" ? "issues" : "merge requests";
  const which = scope !== "projects" && state !== "all" ? `${STATE[state] ?? state} ${noun}` : noun;
  const about = `${which[0].toUpperCase()}${which.slice(1)} with “${q}” in ${scope === "projects" ? "their name or path" : "their title"}`;
  return (
    <div className="page">
      <div className="stack list-page">
        <ListHead title="Search" />
        <div className="search-page-field">
          <SearchField value={term} onChange={setTerm} placeholder="Search…" autoFocus={!q} />
        </div>
        {!q ? (
          <Empty>Search the titles of the issues, merge requests and projects you can see, and the code of their default branches.</Empty>
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
                  { value: "code", label: "Code" },
                ]}
              />
            </div>
            {scope === "code" ? (
              <CodeSearch params={params} set={set} />
            ) : (
              <>
                {/* as tall on every tab as the code's filters are: the list stays put */}
                <div className="search-bar">
                  <span className={`search-about muted${scope === "projects" ? "" : " with-state"}`} title={about}>
                    {about}
                  </span>
                  {scope !== "projects" && (
                    <Segmented
                      label="State"
                      value={state}
                      onChange={(v) => set("state", v === "all" ? "" : v)}
                      options={[
                        { value: "all", label: "All" },
                        { value: "opened", label: "Open" },
                        ...(scope === "merge_requests" ? [{ value: "merged", label: "Merged" }] : []),
                        { value: "closed", label: "Closed" },
                      ]}
                    />
                  )}
                </div>
                <Q q={r}>
                  {(d) => {
                    const list = scope === "projects" ? d.projects : scope === "issues" ? d.issues : d.merge_requests;
                    // another tab's answer, shown while this one's comes: not "nothing"
                    if (list.length === 0 && r.isPlaceholderData) return <Spinner />;
                    if (list.length === 0) {
                      const none = d.counts.issues + d.counts.merge_requests + d.counts.projects === 0;
                      const what = scope === "projects" ? "project" : `${state === "all" ? "" : `${STATE[state] ?? state} `}${scope === "issues" ? "issue" : "merge request"}`;
                      return none ? (
                        <Empty>
                          Nothing has “{q}” in its title. <Link to={`/search${qs({ q, scope: "code" })}`}>Search the code</Link> instead.
                        </Empty>
                      ) : (
                        <Empty>
                          No {what} matches “{q}”.
                        </Empty>
                      );
                    }
                    if (scope === "projects")
                      return (
                        <div className="list rows search-results">
                          {d.projects.map((p) => (
                            <div className="item" key={p.id}>
                              <FolderGit2 size={16} className="faint" />
                              <div className="grow" style={{ minWidth: 0 }}>
                                <div className="row-title">
                                  <Link to={`/${p.full_path}`} className="ellipsis">
                                    <Marked text={p.full_path} term={q} />
                                  </Link>
                                </div>
                                {p.description && <div className="row-meta ellipsis">{p.description}</div>}
                              </div>
                              <span className="row-time">{timeAgo(p.last_activity_at)}</span>
                            </div>
                          ))}
                        </div>
                      );
                    const kind = scope === "issues" ? "issues" : "merge_requests";
                    const sigil = scope === "issues" ? "#" : "!";
                    return (
                      <div className="list rows search-results">
                        {(scope === "issues" ? d.issues : d.merge_requests).map((i) => (
                          <div className="item" key={`${i.project_id}-${i.iid}`}>
                            {scope === "issues" ? (
                              <CircleDot size={15} className={i.state === "opened" ? "ic-ok" : "faint"} />
                            ) : (
                              <GitMerge size={15} className={i.state === "opened" ? "ic-ok" : i.state === "merged" ? "ic-accent" : "faint"} />
                            )}
                            <div className="grow" style={{ minWidth: 0 }}>
                              <div className="row-title">
                                <Link to={`/${i.project_path}/-/${kind}/${i.iid}`} className="ellipsis">
                                  <Marked text={i.title} term={q} />
                                </Link>
                              </div>
                              <div className="row-meta">
                                <span>
                                  {i.project_path}
                                  {sigil}
                                  {i.iid}
                                </span>
                                <span>{STATE[i.state] ?? i.state}</span>
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
          </>
        )}
      </div>
    </div>
  );
}
