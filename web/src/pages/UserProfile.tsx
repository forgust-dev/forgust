import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Activity, CalendarDays, Clock3, FolderGit2, Pencil } from "lucide-react";
import { api, plural, timeAgo, type S } from "../api/client";
import { Avatar, Empty, Q } from "../components/ui";
import { UserContributions, useContributions } from "../components/Contributions";
import { ProjectRow } from "./Dashboard";


/** A person's facts, as a project's right column: how much, how lately, and where. */
function ProfileAbout({ p }: { p: S["Profile"] }) {
  // the calendar's request
  const c = useContributions(p.username).data;
  const total = c?.days.reduce((t, d) => t + d.count, 0);
  const last = c?.activity[0];
  // where they were busy lately: pushes count their commits
  const places = new Map<string, number>();
  for (const a of c?.activity ?? []) places.set(a.project_path, (places.get(a.project_path) ?? 0) + (a.action === "pushed" ? a.count : 1));
  const busiest = [...places].sort((a, b) => b[1] - a[1]).slice(0, 5);
  return (
    <aside className="repo-about" aria-label={`About ${p.name || p.username}`}>
      <section className="side-card">
        <h2>About</h2>
        <ul>
          {total != null && (
            <li>
              <Activity size={15} />
              <span>{plural(total, "contribution")} in the last year</span>
            </li>
          )}
          {last && (
            <li>
              <Clock3 size={15} />
              <span title={new Date(last.at).toLocaleString()}>Last active {timeAgo(last.at)}</span>
            </li>
          )}
          <li>
            <FolderGit2 size={15} />
            <span>{plural(p.projects.length, "personal project")}</span>
          </li>
        </ul>
      </section>
      {busiest.length > 0 && (
        <section className="side-card">
          <h2>Recently active in</h2>
          <ul className="side-places">
            {busiest.map(([path, count]) => (
              <li key={path}>
                <Link className="grow ellipsis" to={`/${path}`}>
                  {path}
                </Link>
                <span className="faint mono small" title={`${count} recent contribution${count === 1 ? "" : "s"}`}>
                  {count}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}
    </aside>
  );
}

/** A person's page at `/<username>`: who they are, what they did, their projects. */
export function UserProfilePage({ username }: { username: string }) {
  const q = useQuery({ queryKey: ["profile", username], queryFn: () => api.get<S["Profile"]>(`/users/${encodeURIComponent(username)}`) });
  return (
    <div className="page stack profile-page">
      <Q q={q}>
        {(p) => (
          <>
            <header className="profile-head">
              <Avatar name={p.name || p.username} src={p.avatar_url} className="profile-avatar" />
              <div className="grow" style={{ minWidth: 0 }}>
                <h1>{p.name || p.username}</h1>
                <div className="muted profile-sub">
                  <span>@{p.username}</span>
                  <span className="profile-since">
                    <CalendarDays size={13} /> Member since {new Date(p.member_since).toLocaleDateString(undefined, { month: "long", year: "numeric" })}
                  </span>
                </div>
              </div>
              {p.is_you && (
                <Link className="btn" to="/profile">
                  <Pencil size={14} /> Edit profile
                </Link>
              )}
            </header>
            <div className="repo-layout">
              <div className="repo-main stack" style={{ gap: 28 }}>
                <UserContributions username={p.username} />
                <section className="stack" style={{ gap: 8 }} aria-label="Personal projects">
                  <h2 className="section-title">
                    Personal projects<span className="section-count"> {p.projects.length}</span>
                  </h2>
                  {p.projects.length === 0 ? (
                    <Empty>{p.is_you ? "You have no personal project yet." : `No personal project of ${p.name || p.username} that you can see.`}</Empty>
                  ) : (
                    <div className="list rows">
                      {p.projects.map((x) => (
                        <ProjectRow key={x.id} p={x} />
                      ))}
                    </div>
                  )}
                </section>
              </div>
              <ProfileAbout p={p} />
            </div>
          </>
        )}
      </Q>
    </div>
  );
}
