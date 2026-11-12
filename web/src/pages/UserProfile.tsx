import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { CalendarDays, Pencil } from "lucide-react";
import { api, type S } from "../api/client";
import { Avatar, Empty, Q } from "../components/ui";
import { UserContributions } from "../components/Contributions";
import { ProjectRow } from "./Dashboard";

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
            <UserContributions username={p.username} />
            <section className="stack" style={{ gap: 8 }}>
              <h2 className="section-title">Personal projects</h2>
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
          </>
        )}
      </Q>
    </div>
  );
}
