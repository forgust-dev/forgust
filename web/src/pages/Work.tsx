import { useMemo, useState, type ReactElement, type ReactNode } from "react";
import { Link } from "react-router-dom";
import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowRight, AtSign, Check, CircleDot, Eye, UserCheck, CircleCheck, Clock, GitPullRequestDraft } from "lucide-react";
import type { UseQueryResult } from "@tanstack/react-query";
import { api, timeAgo, type S } from "../api/client";
import { Empty, Q, ListHead, Tile, UserLink, StateBadge } from "../components/ui";
import { ProjectStateBadges, Visibility } from "./Dashboard";
import { InstanceCard } from "./InstanceCard";
import { DecideCard } from "../components/decide";
import { readinessChecks, readinessScore } from "../components/readiness";
import { useSession } from "../session";
import { useToast } from "../shell/toast";
import { useMyProjects, useMyProjectStates, usePrefetch, useWork } from "../prefetch";
import { StateMark, mrLook } from "../components/states";

const ACTION: Record<string, { icon: ReactElement; text: string }> = {
  assigned: { icon: <UserCheck size={14} />, text: "assigned you" },
  review_requested: { icon: <Eye size={14} />, text: "asked you to review" },
  mentioned: { icon: <AtSign size={14} />, text: "mentioned you" },
};

const REVIEW_TEXT: Record<string, string> = {
  unreviewed: "Waiting for you",
  reviewed: "You commented",
  requested_changes: "You requested changes",
  approved: "You approved",
};

/** Merge requests whose checks are counted here: each costs a request, as on its page. */
const STATUS_LIMIT = 12;

const mrUrl = (m: { project_path: string; iid: number }) => `/${m.project_path}/-/merge_requests/${m.iid}`;

function targetUrl(t: { project_path: string; target_type: string; target_iid: number }) {
  return `/${t.project_path}/-/${t.target_type === "issue" ? "issues" : "merge_requests"}/${t.target_iid}`;
}

/** A review request still waiting for you (no state yet counts as waiting): the
 * section, the queue and the sidebar's count use this one rule. */
const isWaiting = (m: S["WorkMr"]) => !m.review_state || m.review_state === "unreviewed";

/** One thing waiting for you, in the order you would take them. */
type Next = { key: string; icon: ReactElement; why: ReactNode; title: string; to: string; ref: string; at: string; cta: string; todo?: S["Todo"] };

/**
 * What waits for you, first things first: reviews others wait on (the longest
 * waiting first), then what someone asked of you, then the issues assigned to
 * you. Each thing once: a review request is also a to-do, an assignment too.
 */
function queueOf(d: S["Work"], todos: S["Todo"][]): Next[] {
  const seen = new Set<string>();
  const out: Next[] = [];
  const add = (target: string, n: Next) => {
    if (seen.has(target)) return;
    seen.add(target);
    out.push(n);
  };
  const oldest = <T,>(l: T[], at: (x: T) => string) => [...l].sort((a, b) => at(a).localeCompare(at(b)));
  for (const m of oldest(d.review_requests.filter(isWaiting), (m) => m.updated_at)) {
    add(`${m.project_id}!${m.iid}`, {
      key: `review-${m.project_id}-${m.iid}`,
      icon: <Eye size={14} />,
      why: <>Waiting for your review{m.author_username ? ` · opened by ${m.author_username}` : ""}</>,
      title: m.title,
      to: mrUrl(m),
      ref: `${m.project_path}!${m.iid}`,
      at: m.updated_at,
      cta: `Review !${m.iid}`,
    });
  }
  for (const t of oldest(todos, (t) => t.created_at)) {
    const sigil = t.target_type === "issue" ? "#" : "!";
    const a = ACTION[t.action] ?? ACTION.mentioned;
    add(`${t.project_id}${sigil}${t.target_iid}`, {
      key: `todo-${t.id}`,
      icon: a.icon,
      why: (
        <>
          {t.author_name || t.author_username || "Someone"} {a.text}
        </>
      ),
      title: t.target_title,
      to: targetUrl(t),
      ref: `${t.project_path}${sigil}${t.target_iid}`,
      at: t.created_at,
      cta: `Open ${sigil}${t.target_iid}`,
      todo: t,
    });
  }
  for (const i of oldest(d.assigned_issues, (i) => i.updated_at)) {
    add(`${i.project_id}#${i.iid}`, {
      key: `issue-${i.project_id}-${i.iid}`,
      icon: <CircleDot size={14} />,
      why: "Assigned to you",
      title: i.title,
      to: `/${i.project_path}/-/issues/${i.iid}`,
      ref: `${i.project_path}#${i.iid}`,
      at: i.updated_at,
      cta: `Open #${i.iid}`,
    });
  }
  return out;
}

/** The page's emphasized card: the first thing waiting for you, and what comes after it. */
function NextCard({ queue, onDone }: { queue: Next[]; onDone: (t: S["Todo"]) => void }) {
  const [first, ...rest] = queue;
  const shown = rest.slice(0, 3);
  const more = rest.length - shown.length;
  return (
    <DecideCard
      label="Waiting for you"
      eyebrow={
        <>
          {first.icon} {first.why}
        </>
      }
      title={first.title}
      sub={
        <>
          <span className="mono">{first.ref}</span> · {timeAgo(first.at)}
        </>
      }
      actions={
        <>
          <Link className="btn run" to={first.to}>
            {first.cta}
          </Link>
          {first.todo && (
            <button type="button" className="link-btn" onClick={() => onDone(first.todo!)}>
              <Check size={14} /> Mark done
            </button>
          )}
        </>
      }
      rowsTitle={shown.length > 0 ? "Then" : undefined}
      rows={shown.map((n) => ({
        key: n.key,
        tone: "item",
        icon: <span className="row-ic">{n.icon}</span>,
        text: <Link to={n.to}>{n.title}</Link>,
        aside: n.ref,
      }))}
      foot={rest.length === 0 ? "Nothing else waits for you." : more > 0 ? `And ${more} more below.` : undefined}
    />
  );
}

function Section({ id, title, count, actions, children }: { id: string; title: string; count?: number; actions?: ReactNode; children: ReactNode }) {
  return (
    <section className="stack work-section" id={id} aria-label={title} style={{ gap: 8 }}>
      <div className="section-head">
        <h2 className="section-title">
          {title}
          {count != null && <span className="section-count"> {count}</span>}
        </h2>
        {actions && <div className="section-actions">{actions}</div>}
      </div>
      {children}
    </section>
  );
}

/** Your to-dos (or the done ones): what someone asked of you. `quiet`: none is
 *  waiting and the page has other things, so an empty list takes one line;
 *  `clear`: nothing at all waits for you, said once in place of the lists. */
function Todos({ pending, onDone, quiet, clear }: { pending: S["Todo"][]; onDone: (path: string, msg: string) => void; quiet?: boolean; clear?: boolean }) {
  const [state, setState] = useState<"pending" | "done">("pending");
  const done = useQuery({ queryKey: ["todos", "done"], queryFn: () => api.get<S["Todo"][]>(`/user/todos?state=done`), enabled: state === "done" });
  const toggle = (
    <button type="button" className="link-btn small" onClick={() => setState(state === "pending" ? "done" : "pending")}>
      {state === "pending" ? (clear ? "Show done to-dos" : "Show done") : "Show pending"}
    </button>
  );
  if (clear && state === "pending")
    return (
      <div className="all-clear" role="status">
        <span className="all-clear-icon" aria-hidden="true">
          <Check size={16} />
        </span>
        <div className="grow" style={{ minWidth: 0 }}>
          <div className="all-clear-title">Nothing is waiting for you</div>
          <div className="all-clear-sub">Review requests, assignments and mentions show up here as they come.</div>
        </div>
        {toggle}
      </div>
    );
  const rows = (list: S["Todo"][]) =>
    list.length === 0 ? (
      quiet && state === "pending" ? (
        <p className="muted small" style={{ margin: 0 }}>
          No to-do waiting.
        </p>
      ) : (
        <Empty>{state === "pending" ? "Nothing to do. Assignments, review requests and mentions show up here." : "No to-do done yet."}</Empty>
      )
    ) : (
      <div className="list rows">
        {list.map((t) => {
          const a = ACTION[t.action] ?? ACTION.mentioned;
          return (
            <div className="item todo-row" key={t.id}>
              <span className="row-icon faint">{a.icon}</span>
              <div className="grow" style={{ minWidth: 0 }}>
                <div className="row-title">
                  <Link to={targetUrl(t)} className="ellipsis">
                    {t.target_title}
                  </Link>
                  {t.target_state !== "opened" && (
                    <StateBadge tone={t.target_state === "merged" ? "merged" : "neutral"}>{t.target_state[0].toUpperCase() + t.target_state.slice(1)}</StateBadge>
                  )}
                </div>
                <div className="row-meta">
                  <span>
                    {t.project_path}
                    {t.target_type === "issue" ? "#" : "!"}
                    {t.target_iid}
                  </span>
                  <span>
                    {t.author_username ? <UserLink username={t.author_username}>{t.author_name || t.author_username}</UserLink> : "someone"} {a.text} · {timeAgo(t.created_at)}
                  </span>
                </div>
                {t.body && <div className="row-desc ellipsis">{t.body}</div>}
              </div>
              {t.state === "pending" && (
                <button className="icon-btn" title="Done" aria-label={`Mark done: ${t.target_title}`} onClick={() => onDone(`/user/todos/${t.id}/done`, "Done")}>
                  <Check size={15} />
                </button>
              )}
            </div>
          );
        })}
      </div>
    );
  return (
    <Section
      id="todos"
      title={state === "pending" ? "To-do" : "Done to-dos"}
      count={state === "pending" ? pending.length : undefined}
      actions={
        <>
          {state === "pending" && pending.length > 1 && (
            <button className="btn small" onClick={() => onDone("/user/todos/done", "All to-dos done")}>
              <Check size={14} /> Mark all done
            </button>
          )}
          {toggle}
        </>
      }
    >
      {state === "pending" ? rows(pending) : <Q q={done}>{rows}</Q>}
    </Section>
  );
}

/** How far a merge request is from its merge: the checks of its page, as a small ring. */
function MrReady({ s, draft }: { s: S["MergeStatus"]; draft: boolean }) {
  const { passed, total } = readinessScore(readinessChecks(s));
  const left = total - passed;
  if (draft) return <StateBadge tone="neutral" icon={GitPullRequestDraft}>Draft</StateBadge>;
  if (s.can_merge) return <StateBadge tone="ok" icon={CircleCheck}>Ready to merge</StateBadge>;
  return (
    <StateBadge tone="warn" icon={Clock} title={`${passed} of ${total} checks pass`}>
      {left > 0 ? `${left} check${left === 1 ? "" : "s"} left` : "Not ready"}
    </StateBadge>
  );
}

function MrRows({ list, review, status, me }: { list: S["WorkMr"][]; review?: boolean; status?: (m: S["WorkMr"]) => S["MergeStatus"] | undefined; me?: string }) {
  return (
    <div className="list rows">
      {list.map((m) => {
        const s = status?.(m);
        return (
          <div className="item" key={`${m.project_id}-${m.iid}`}>
            <StateMark look={mrLook("opened", m.draft)} className="row-icon" />
            <div className="grow" style={{ minWidth: 0 }}>
              <div className="row-title">
                <Link to={mrUrl(m)} className="ellipsis">
                  {m.draft && <span className="draft-tag">Draft</span>}
                  {m.title}
                </Link>
              </div>
              <div className="row-meta">
                <span>
                  {m.project_path}!{m.iid}
                </span>
                {me && m.author_username === me ? <span>opened by you</span> : <span>by {m.author_username ? <UserLink username={m.author_username} /> : "ghost"}</span>}
                {me && m.author_username !== me && <span>assigned to you</span>}
                {review && m.review_state && <span className={isWaiting(m) ? "accent-text" : ""}>{REVIEW_TEXT[m.review_state] ?? m.review_state}</span>}
                {status && <span title={new Date(m.updated_at).toLocaleString()}>updated {timeAgo(m.updated_at)}</span>}
              </div>
            </div>
            {status ? (
              s && <MrReady s={s} draft={m.draft} />
            ) : (
              <span className="row-time" title={new Date(m.updated_at).toLocaleString()}>
                {timeAgo(m.updated_at)}
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
}

/** Projects shown as cards: two rows on a wide screen. */
const CARDS = 6;

/** One of your projects as a card: its picture, where it lives, what it is, how it is
 *  doing (open merge requests, latest pipeline) and how lately it moved. */
function ProjectCard({ p, st }: { p: S["MyProject"]; st?: S["ProjectState"] }) {
  const pf = usePrefetch({ kind: "project", id: p.id, path: p.full_path });
  const slash = p.full_path.lastIndexOf("/");
  return (
    <Link to={`/${p.full_path}`} className="project-card" title={p.full_path} {...pf}>
      <span className="project-card-head">
        <Tile name={p.name} src={p.avatar_url} />
        <span className="grow" style={{ minWidth: 0 }}>
          <span className="project-card-name ellipsis">{p.name}</span>
          <span className="project-card-ns ellipsis">{p.full_path.slice(0, slash)}</span>
        </span>
      </span>
      <span className={`project-card-desc${p.description ? "" : " none"}`}>{p.description || "No description"}</span>
      <span className="project-card-foot">
        <Visibility v={p.visibility} />
        <span className="grow" />
        <ProjectStateBadges p={p} st={st} plain />
        <span title={new Date(p.last_activity_at).toLocaleString()}>{timeAgo(p.last_activity_at)}</span>
      </span>
    </Link>
  );
}

/** Your latest active projects, a click away. */
function YourProjects({ q }: { q: UseQueryResult<S["MyProject"][]> }) {
  // the projects page's request
  const states = useMyProjectStates();
  const stateOf = new Map((states.data ?? []).map((s) => [s.project_id, s]));
  const live = q.data?.filter((p) => !p.archived);
  const total = live?.length;
  return (
    <Section
      id="projects"
      title="Your projects"
      count={total}
      actions={
        total != null &&
        total > 0 && (
          <Link className="link-btn small section-link" to="/dashboard/projects">
            {total > CARDS ? `All ${total} projects` : "All projects"} <ArrowRight size={13} />
          </Link>
        )
      }
    >
      <Q q={q}>
        {() =>
          !live || live.length === 0 ? (
            <p className="muted small" style={{ margin: 0 }}>
              No project yet. <Link to="/new/project">Create one</Link>, <Link to="/import">import one</Link> or <Link to="/explore">explore</Link> what others share.
            </p>
          ) : (
            <div className="project-cards">
              {live.slice(0, CARDS).map((p) => (
                <ProjectCard key={p.id} p={p} st={stateOf.get(p.id)} />
              ))}
            </div>
          )
        }
      </Q>
    </Section>
  );
}

/** Home: what waits for you across projects (the first thing in its own card), then
 *  your projects; an administrator has the instance beside them. */
export function Work() {
  const w = useWork();
  const { user } = useSession();
  const qc = useQueryClient();
  const toast = useToast();
  const pending = useQuery({ queryKey: ["todos", "pending"], queryFn: () => api.get<S["Todo"][]>(`/user/todos?state=pending`) });
  // asked for with the rest, not once the lists are in: they show together
  const projects = useMyProjects();
  const admin = !!user?.is_admin;
  // yours and those assigned to you, once each
  const mine = useMemo(() => {
    const d = w.data;
    if (!d) return [];
    const all = [...d.your_merge_requests, ...d.assigned_merge_requests];
    return all.filter((m, i) => all.findIndex((x) => x.project_id === m.project_id && x.iid === m.iid) === i);
  }, [w.data]);
  const statuses = useQueries({
    queries: mine.slice(0, STATUS_LIMIT).map((m) => ({
      // the key of the merge request's page: one request for both
      queryKey: ["mr", m.project_id, m.iid, "status"],
      queryFn: () => api.get<S["MergeStatus"]>(`/projects/${m.project_id}/merge_requests/${m.iid}/merge_status`),
      staleTime: 30_000,
    })),
  });
  const statusOf = (m: S["WorkMr"]) => {
    const i = mine.indexOf(m);
    return i >= 0 && i < STATUS_LIMIT ? statuses[i]?.data : undefined;
  };
  const done = async (path: string, msg: string) => {
    try {
      await api.post(path);
      toast.ok(msg);
    } catch (e) {
      toast.error(e);
    }
    qc.invalidateQueries({ queryKey: ["todos"] });
    qc.invalidateQueries({ queryKey: ["work"] });
    // a to-do done reads its notifications: the bell follows
    qc.invalidateQueries({ queryKey: ["inbox"] });
  };
  // the card needs both: shown once, in place, rather than moving the page when the to-dos arrive
  const both = {
    isLoading: w.isLoading || pending.isLoading,
    error: w.error ?? pending.error,
    data: w.data && pending.data ? { d: w.data, todos: pending.data } : undefined,
  };
  return (
    <div className="page stack list-page work-page">
      <ListHead title="Home" sub="What is waiting for you, across your projects." />
      <div className={`work-layout${admin ? "" : " solo"}`}>
        <div className="stack work-main">
          <Q q={both}>
            {({ d, todos }) => {
              const queue = queueOf(d, todos);
              const waiting = d.review_requests.filter(isWaiting);
              const reviewed = d.review_requests.filter((m) => !isWaiting(m));
              // no to-do but other things: the to-dos go last, in a line
              const others = d.review_requests.length > 0 || mine.length > 0 || d.assigned_issues.length > 0;
              const quiet = todos.length === 0 && others;
              const todoSection = <Todos pending={todos} onDone={done} quiet={quiet} clear={todos.length === 0 && !others} />;
              return (
                <>
                  {queue.length > 0 && <NextCard queue={queue} onDone={(t) => done(`/user/todos/${t.id}/done`, "Done")} />}
                  {waiting.length > 0 && (
                    <Section id="reviews" title="Waiting for your review" count={waiting.length}>
                      <MrRows list={waiting} review />
                    </Section>
                  )}
                  {!quiet && todoSection}
                  {mine.length > 0 && (
                    <Section id="mrs" title="Your merge requests" count={mine.length}>
                      <MrRows list={mine} status={statusOf} me={user?.username} />
                    </Section>
                  )}
                  {d.assigned_issues.length > 0 && (
                    <Section id="issues" title="Issues assigned to you" count={d.assigned_issues.length}>
                      <div className="list rows">
                        {d.assigned_issues.map((i) => (
                          <div className="item" key={`${i.project_id}-${i.iid}`}>
                            <span className="row-icon ic-ok">
                              <CircleDot size={16} />
                            </span>
                            <div className="grow" style={{ minWidth: 0 }}>
                              <div className="row-title">
                                <Link to={`/${i.project_path}/-/issues/${i.iid}`} className="ellipsis">
                                  {i.title}
                                </Link>
                              </div>
                              <div className="row-meta">
                                <span>
                                  {i.project_path}#{i.iid}
                                </span>
                                <span>by {i.author_username ? <UserLink username={i.author_username} /> : "ghost"}</span>
                              </div>
                            </div>
                            <span className="row-time">{timeAgo(i.updated_at)}</span>
                          </div>
                        ))}
                      </div>
                    </Section>
                  )}
                  {reviewed.length > 0 && (
                    <Section id="reviewed" title="Reviewed by you" count={reviewed.length}>
                      <MrRows list={reviewed} review />
                    </Section>
                  )}
                  {quiet && todoSection}
                  <YourProjects q={projects} />
                </>
              );
            }}
          </Q>
        </div>
        {admin && (
          <aside className="repo-about work-side" aria-label="This instance">
            <InstanceCard />
          </aside>
        )}
      </div>
    </div>
  );
}
