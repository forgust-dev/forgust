import { useRef, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Bell, CircleAlert, CircleCheck, GitPullRequest, CircleDot, ShieldAlert } from "lucide-react";
import { api, timeAgo, type S } from "../api/client";
import { useUserEvents } from "../shell/live";
import { useToast } from "../shell/toast";
import { Popover } from "./controls";

type Item = S["Notification"];

function url(n: Item) {
  const base = `/${n.project_path}/-`;
  switch (n.target_type) {
    case "issue":
      return `${base}/issues/${n.target_iid}`;
    case "merge_request":
      return `${base}/merge_requests/${n.target_iid}`;
    case "pipeline":
      return `${base}/pipelines/${n.target_iid}`;
    case "security":
      return `${base}/security`;
    default:
      // a kind of target this build does not know yet: its project
      return `/${n.project_path}`;
  }
}

function Icon({ n }: { n: Item }) {
  if (n.kind === "pipeline_failed") return <CircleAlert size={16} className="inbox-icon bad" />;
  if (n.kind === "pipeline_fixed") return <CircleCheck size={16} className="inbox-icon ok" />;
  if (n.target_type === "security") return <ShieldAlert size={16} className="inbox-icon bad" />;
  if (n.target_type === "issue") return <CircleDot size={16} className="inbox-icon" />;
  return <GitPullRequest size={16} className="inbox-icon" />;
}

/** "!12 Fix the login page", "#3 Crash on start", "Pipeline #8 on main". */
function title(n: Item) {
  if (n.target_type === "merge_request") return `!${n.target_iid} ${n.title}`;
  if (n.target_type === "issue") return `#${n.target_iid} ${n.title}`;
  return n.title;
}

/** "rita approved", or what happened on its own ("failed"). */
function what(n: Item) {
  const who = n.actor_name || n.actor_username;
  return who ? `${who} ${n.summary}` : n.summary;
}

/** The bell: what reached you in the app, live. */
export function InboxBell() {
  const qc = useQueryClient();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  // the event stream says when it changes; the interval covers a missed event
  const q = useQuery({ queryKey: ["inbox"], queryFn: () => api.get<S["Inbox"]>("/user/notifications"), refetchInterval: 120_000 });
  // one action often tells the same person twice (assigned and mentioned): a
  // burst of events, and a mark-read with its own event, refetch once
  const refetch = useRef<number | undefined>(undefined);
  const refresh = () => {
    clearTimeout(refetch.current);
    refetch.current = window.setTimeout(() => qc.invalidateQueries({ queryKey: ["inbox"] }), 250);
  };
  const unread = q.data?.unread ?? 0;
  const items = q.data?.items ?? [];
  const read = async (id?: number) => {
    try {
      await api.post(id ? `/user/notifications/${id}/read` : "/user/notifications/read");
      refresh();
    } catch (e) {
      toast.error(e);
    }
  };
  // what arrives while the app is open also shows on screen, when the person
  // kept that kind "on screen" (the server says so with the event)
  useUserEvents((ev) => {
    refresh();
    if (ev.type !== "notification" || !ev.screen) return;
    const n = ev.notification;
    toast.notice(`${what(n)}: ${title(n)}`, { to: url(n), onOpen: () => read(n.id) });
  });
  return (
    <Popover
      open={open}
      onOpenChange={setOpen}
      label="Notifications"
      trigger={
        <button className="icon-btn inbox-btn" aria-label={unread ? `Notifications: ${unread} unread` : "Notifications"} title="Notifications">
          <Bell size={17} />
          {unread > 0 && <span className="inbox-count">{unread > 99 ? "99+" : unread}</span>}
        </button>
      }
    >
      <div className="inbox">
        <div className="inbox-head">
          <strong>Notifications</strong>
          <span className="grow" />
          {unread > 0 && (
            <button className="link-btn small" onClick={() => read()}>
              Mark all read
            </button>
          )}
          <Link className="small" to="/profile/notifications" onClick={() => setOpen(false)}>
            Settings
          </Link>
        </div>
        {items.length === 0 ? (
          <p className="muted small inbox-empty">{q.isLoading ? "Loading…" : "Nothing yet. Reviews, mentions and pipelines of yours show up here."}</p>
        ) : (
          <ul className="inbox-list">
            {items.map((n) => (
              <li key={n.id}>
                <Link
                  className={`inbox-item ${n.read ? "" : "unread"}`}
                  to={url(n)}
                  onClick={() => {
                    setOpen(false);
                    if (!n.read) read(n.id);
                  }}
                >
                  <Icon n={n} />
                  <span className="inbox-text">
                    <span className="inbox-title ellipsis">{title(n)}</span>
                    <span className="muted small">
                      {what(n)} · {n.project_path} · {timeAgo(n.created_at)}
                    </span>
                  </span>
                  {!n.read && <span className="inbox-dot" aria-label="Unread" />}
                </Link>
              </li>
            ))}
          </ul>
        )}
      </div>
    </Popover>
  );
}
