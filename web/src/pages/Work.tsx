import { useState, type ReactNode, type ReactElement } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AtSign, Check, CircleDot, Eye, GitMerge, UserCheck } from "lucide-react";
import { api, timeAgo, type S } from "../api/client";
import { Empty, FilterTabs, ListHead, Q, UserLink } from "../components/ui";
import { useToast } from "../shell/toast";
import { useWork } from "../prefetch";

type Tab = "todos" | "reviews" | "mrs" | "issues";


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

function targetUrl(t: { project_path: string; target_type: string; target_iid: number }) {
  return `/${t.project_path}/-/${t.target_type === "issue" ? "issues" : "merge_requests"}/${t.target_iid}`;
}

function Todos() {
  const qc = useQueryClient();
  const toast = useToast();
  const [state, setState] = useState<"pending" | "done">("pending");
  const q = useQuery({ queryKey: ["todos", state], queryFn: () => api.get<S["Todo"][]>(`/user/todos?state=${state}`) });
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["todos"] });
    qc.invalidateQueries({ queryKey: ["work"] });
  };
  const done = async (path: string, msg: string) => {
    try {
      await api.post(path);
      toast.ok(msg);
    } catch (e) {
      toast.error(e);
    }
    refresh();
  };
  return (
    <div className="stack" style={{ gap: 12 }}>
      <div className="list-toolbar">
        <FilterTabs
          label="To-do state"
          value={state}
          onChange={setState}
          options={[
            { value: "pending", label: "Pending" },
            { value: "done", label: "Done" },
          ]}
        />
        <span className="grow" />
        {state === "pending" && (q.data?.length ?? 0) > 0 && (
          <button className="btn small" onClick={() => done("/user/todos/done", "All to-dos done")}>
            <Check size={14} /> Mark all done
          </button>
        )}
      </div>
      <Q q={q}>
        {(list) =>
          list.length === 0 ? (
            <Empty>{state === "pending" ? "Nothing to do. Assignments, review requests and mentions show up here." : "No to-do done yet."}</Empty>
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
                        {t.target_state !== "opened" && <span className="badge">{t.target_state}</span>}
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
                      <button className="icon-btn" title="Done" aria-label={`Mark done: ${t.target_title}`} onClick={() => done(`/user/todos/${t.id}/done`, "Done")}>
                        <Check size={15} />
                      </button>
                    )}
                  </div>
                );
              })}
            </div>
          )
        }
      </Q>
    </div>
  );
}

/** A review request still waiting for you (no state yet counts as waiting): the
 * tab's count and the first section use this one rule. */
const isWaiting = (m: S["WorkMr"]) => !m.review_state || m.review_state === "unreviewed";

function ListSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="stack" style={{ gap: 8 }}>
      <h2 className="section-title">{title}</h2>
      {children}
    </section>
  );
}

/** Review requests, those still waiting for you first. */
function ReviewRequests({ list }: { list: S["WorkMr"][] }) {
  if (list.length === 0) return <Empty>No review requests. When someone asks you to review a merge request, it shows up here.</Empty>;
  const reviewed = list.filter((m) => !isWaiting(m));
  return (
    <div className="stack" style={{ gap: 18 }}>
      <ListSection title="Waiting for your review">
        <MrRows list={list.filter(isWaiting)} review empty="Nothing is waiting for your review." />
      </ListSection>
      {reviewed.length > 0 && (
        <ListSection title="Reviewed by you">
          <MrRows list={reviewed} review />
        </ListSection>
      )}
    </div>
  );
}

function MrRows({ list, empty, review }: { list: S["WorkMr"][]; empty?: string; review?: boolean }) {
  if (list.length === 0) return <Empty>{empty}</Empty>;
  return (
    <div className="list rows">
      {list.map((m) => (
        <div className="item" key={`${m.project_id}-${m.iid}`}>
          <span className={`row-icon ${m.draft ? "faint" : "ic-ok"}`}>
            <GitMerge size={16} />
          </span>
          <div className="grow" style={{ minWidth: 0 }}>
            <div className="row-title">
              <Link to={`/${m.project_path}/-/merge_requests/${m.iid}`} className="ellipsis">
                {m.draft && <span className="draft-tag">Draft</span>}
                {m.title}
              </Link>
            </div>
            <div className="row-meta">
              <span>
                {m.project_path}!{m.iid}
              </span>
              <span>by {m.author_username ? <UserLink username={m.author_username} /> : "ghost"}</span>
              {review && m.review_state && <span className={isWaiting(m) ? "accent-text" : ""}>{REVIEW_TEXT[m.review_state] ?? m.review_state}</span>}
            </div>
          </div>
          <span className="row-time" title={new Date(m.updated_at).toLocaleString()}>
            {timeAgo(m.updated_at)}
          </span>
        </div>
      ))}
    </div>
  );
}

/** "Your work": to-dos, review requests, merge requests and issues, across projects. */
export function Work() {
  const w = useWork();
  const [tab, setTab] = useState<Tab>("todos");
  const d = w.data;
  return (
    <div className="page stack list-page">
      <ListHead title="Your work" sub="What is waiting for you, across your projects." />
      <FilterTabs
        label="Your work"
        value={tab}
        onChange={setTab}
        options={[
          { value: "todos", label: "To-do", count: d?.todos_pending },
          { value: "reviews", label: "Review requests", count: d?.review_requests.filter(isWaiting).length },
          { value: "mrs", label: "Merge requests", count: d ? d.assigned_merge_requests.length + d.your_merge_requests.length : undefined },
          { value: "issues", label: "Issues", count: d?.assigned_issues.length },
        ]}
      />
      {tab === "todos" ? (
        <Todos />
      ) : (
        <Q q={w}>
          {(d) =>
            tab === "reviews" ? (
              <ReviewRequests list={d.review_requests} />
            ) : tab === "mrs" ? (
              <div className="stack" style={{ gap: 18 }}>
                <ListSection title="Assigned to you">
                  <MrRows list={d.assigned_merge_requests} empty="No open merge request is assigned to you." />
                </ListSection>
                <ListSection title="Opened by you">
                  <MrRows list={d.your_merge_requests} empty="You have no open merge request." />
                </ListSection>
              </div>
            ) : d.assigned_issues.length === 0 ? (
              <Empty>No open issue is assigned to you.</Empty>
            ) : (
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
            )
          }
        </Q>
      )}
    </div>
  );
}
