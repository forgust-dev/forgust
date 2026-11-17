import type { ReactNode } from "react";

/** A time of day, on 24 hours. */
export const hhmm = (at: string) => new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });

/** "Today", "Yesterday", or the date. */
export function dayLabel(at: string) {
  const d = new Date(at);
  const today = new Date();
  const days = Math.round((new Date(today.toDateString()).getTime() - new Date(d.toDateString()).getTime()) / 86_400_000);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  return d.toLocaleDateString([], { weekday: "short", day: "numeric", month: "short", year: d.getFullYear() === today.getFullYear() ? undefined : "numeric" });
}

export type LogEntry = { at: string; key: string; icon: ReactNode; tone?: string; boxed?: boolean; body: ReactNode };

/** Entries by day, each with its time and icon: the activity of a merge request,
 *  an issue or a person, oldest or newest first as given. */
export function Timeline({ entries, empty = "Nothing yet." }: { entries: LogEntry[]; empty?: ReactNode }) {
  let lastDay = "";
  return (
    <div className="timeline">
      {entries.length === 0 && <p className="muted small">{empty}</p>}
      {entries.map((e) => {
        const day = dayLabel(e.at);
        const sep = day !== lastDay;
        lastDay = day;
        return (
          <div key={e.key}>
            {sep && <div className="tl-day">{day}</div>}
            <div className={`tl-row ${e.boxed ? "boxed" : ""}`}>
              <time dateTime={e.at} title={new Date(e.at).toLocaleString()}>
                {hhmm(e.at)}
              </time>
              <span className={`tl-icon ${e.tone ?? ""}`}>{e.icon}</span>
              <div className="tl-body">{e.body}</div>
            </div>
          </div>
        );
      })}
    </div>
  );
}
