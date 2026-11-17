import { useEffect, useMemo, useRef, type ReactElement } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { CircleDot, GitCommitHorizontal, GitMerge, MessageSquare, ThumbsUp } from "lucide-react";
import { api, enc, qs, type S } from "../api/client";
import { Q } from "./ui";
import { Timeline, type LogEntry } from "./timeline";

const DAY = 86_400_000;
const WEEKS = 53;
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** Local `YYYY-MM-DD` of a date (the server counts days in the viewer's time zone). */
function iso(d: Date) {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** 0-4 by quartiles of the busiest day, like GitHub. */
function level(n: number, max: number) {
  if (n <= 0) return 0;
  if (max <= 4) return Math.min(4, n);
  return Math.min(4, Math.ceil((n / max) * 4));
}

/**
 * The last 53 weeks as a grid, one column per week (Sunday on top), today in
 * the last column; the newest weeks are scrolled into view on narrow screens.
 */
export function ContributionCalendar({ days }: { days: S["Day"][] }) {
  const scroller = useRef<HTMLDivElement>(null);
  const { weeks, months, max } = useMemo(() => {
    const counts = new Map(days.map((d) => [d.date, d.count]));
    const today = new Date();
    today.setHours(12, 0, 0, 0);
    const start = new Date(today.getTime() - ((WEEKS - 1) * 7 + today.getDay()) * DAY);
    const weeks: { date: Date; count: number; future: boolean }[][] = [];
    const months: { week: number; label: string }[] = [];
    for (let w = 0; w < WEEKS; w++) {
      const col = [];
      for (let d = 0; d < 7; d++) {
        const date = new Date(start.getTime() + (w * 7 + d) * DAY);
        col.push({ date, count: counts.get(iso(date)) ?? 0, future: date > today });
      }
      const first = col[0].date;
      if (w === 0 || first.getDate() <= 7) {
        const label = MONTHS[first.getMonth()];
        if (months.length === 0 || months[months.length - 1].label !== label) months.push({ week: w, label });
      }
      weeks.push(col);
    }
    return { weeks, months: months[0]?.week === 0 && months[1]?.week <= 2 ? months.slice(1) : months, max: Math.max(0, ...days.map((d) => d.count)) };
  }, [days]);
  useEffect(() => {
    const el = scroller.current;
    if (el) el.scrollLeft = el.scrollWidth;
  }, [weeks]);
  const total = days.reduce((n, d) => n + d.count, 0);
  return (
    <figure className="contrib">
      <figcaption className="contrib-caption">
        <strong>{total.toLocaleString()}</strong> contribution{total === 1 ? "" : "s"} in the last year
      </figcaption>
      <div className="contrib-scroll" ref={scroller}>
        {/* one grid: a label column and 53 week columns that share the width */}
        <div className="contrib-grid" role="grid" aria-label="Contributions per day">
          {months.map((m) => (
            <span key={`${m.week}-${m.label}`} className="contrib-month" aria-hidden="true" style={{ gridColumn: m.week + 2, gridRow: 1 }}>
              {m.label}
            </span>
          ))}
          {/* every row gets a label cell: the column stays opaque when it sticks on phones */}
          {["", "Mon", "", "Wed", "", "Fri", ""].map((label, d) => (
            <span key={d} className="contrib-weekday" aria-hidden="true" style={{ gridColumn: 1, gridRow: d + 2 }}>
              {label}
            </span>
          ))}
          {weeks.map((col, w) =>
            col.map(({ date, count, future }, d) =>
              future ? (
                <span key={`${w}-${d}`} className="contrib-cell future" style={{ gridColumn: w + 2, gridRow: d + 2 }} />
              ) : (
                <span
                  key={`${w}-${d}`}
                  role="gridcell"
                  className={`contrib-cell l${level(count, max)}`}
                  style={{ gridColumn: w + 2, gridRow: d + 2 }}
                  title={`${count === 0 ? "No" : count} contribution${count === 1 ? "" : "s"} on ${date.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric", year: "numeric" })}`}
                  aria-label={`${iso(date)}: ${count}`}
                />
              ),
            ),
          )}
        </div>
      </div>
      <div className="contrib-legend" aria-hidden="true">
        Less
        {[0, 1, 2, 3, 4].map((l) => (
          <span key={l} className={`contrib-cell l${l}`} />
        ))}
        More
      </div>
    </figure>
  );
}

/** Icon, tone and verb of each kind of activity, as in a merge request's log. */
const ACTIVITY: Record<string, { icon: ReactElement; verb: string; tone?: string }> = {
  pushed: { icon: <GitCommitHorizontal size={12} />, verb: "pushed to" },
  opened_merge_request: { icon: <GitMerge size={12} />, verb: "opened" },
  merged_merge_request: { icon: <GitMerge size={12} />, verb: "merged", tone: "accent" },
  opened_issue: { icon: <CircleDot size={12} />, verb: "opened" },
  closed_issue: { icon: <CircleDot size={12} />, verb: "closed" },
  commented_merge_request: { icon: <MessageSquare size={12} />, verb: "commented on" },
  commented_issue: { icon: <MessageSquare size={12} />, verb: "commented on" },
  approved: { icon: <ThumbsUp size={12} />, verb: "approved", tone: "ok-solid" },
};

function activityEntry(a: S["Activity"], i: number): LogEntry {
  const k = ACTIVITY[a.action] ?? ACTIVITY.pushed;
  const isMr = a.action.endsWith("merge_request") || a.action === "approved";
  const target =
    a.action === "pushed" ? (
      <>
        <code>{a.title}</code> ({a.count} commit{a.count === 1 ? "" : "s"})
      </>
    ) : (
      <Link to={`/${a.project_path}/-/${isMr ? "merge_requests" : "issues"}/${a.iid}`}>
        {isMr ? "!" : "#"}
        {a.iid} {a.title}
      </Link>
    );
  return {
    at: a.at,
    key: `${i}:${a.at}`,
    icon: k.icon,
    tone: k.tone,
    body: (
      <>
        {k.verb} {target} in <Link to={`/${a.project_path}`}>{a.project_path}</Link>
      </>
    ),
  };
}

/** A user's contributions per day and latest activity, in the viewer's time zone. */
export function useContributions(username: string) {
  const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
  return useQuery({
    queryKey: ["contributions", username, tz],
    queryFn: () => api.get<S["Contributions"]>(`/users/${enc(username)}/contributions${qs({ tz })}`),
  });
}

/** A user's calendar and latest activity (only what the viewer may see), the
 *  activity as a merge request's log: by day, each with its time. */
export function UserContributions({ username }: { username: string }) {
  const q = useContributions(username);
  return (
    <Q q={q}>
      {(c) => (
        <section className="stack user-contrib" aria-label="Contributions">
          <ContributionCalendar days={c.days} />
          {c.activity.length > 0 && (
            <section className="activity user-activity" aria-label="Recent activity">
              <div className="activity-head">
                <h2>Recent activity</h2>
                <span className="rule" />
              </div>
              <Timeline entries={c.activity.map(activityEntry)} />
            </section>
          )}
        </section>
      )}
    </Q>
  );
}
