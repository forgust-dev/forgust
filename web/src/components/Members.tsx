import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { fetchers } from "../prefetch";
import { api, timeAgo, type S } from "../api/client";
import { Avatar, ErrorBox, LevelSelect, ListHead, Q, Section } from "./ui";

/** Members and sharing for a group (`/groups/:id`) or project (`/projects/:id`). */
export function Members({ base, canManage, kind }: { base: string; canManage: boolean; kind: "group" | "project" }) {
  const qc = useQueryClient();
  const members = useQuery({ queryKey: ["members", base], queryFn: () => api.get<S["MemberDto"][]>(`${base}/members`) });
  const shares = useQuery({ queryKey: ["shares", base], queryFn: () => api.get<S["ShareDto"][]>(`${base}/shares`) });
  const [username, setUsername] = useState("");
  const [level, setLevel] = useState("developer");
  const [shareGroup, setShareGroup] = useState("");
  const [shareLevel, setShareLevel] = useState("developer");
  const [error, setError] = useState<unknown>(null);
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["members", base] });
    qc.invalidateQueries({ queryKey: ["shares", base] });
  };
  const add = useMutation({
    mutationFn: () => api.post(`${base}/members`, { username, access_level: level }),
    onSuccess: () => {
      setUsername("");
      setError(null);
      refresh();
    },
    onError: setError,
  });
  const remove = useMutation({ mutationFn: (uid: number) => api.del(`${base}/members/${uid}`), onSuccess: refresh, onError: setError });
  const share = useMutation({
    mutationFn: async () => {
      const g = await fetchers.pathResolve(shareGroup);
      if (g.kind !== "group") throw new Error("not a group");
      return api.post(`${base}/shares`, { group_id: g.id, max_access_level: shareLevel });
    },
    onSuccess: () => {
      setShareGroup("");
      refresh();
    },
    onError: setError,
  });
  const unshare = useMutation({ mutationFn: (gid: number) => api.del(`${base}/shares/${gid}`), onSuccess: refresh, onError: setError });

  return (
    <div className="stack settings-page">
      {kind !== "group" && <ListHead title="Members" sub="Who can reach this project, and what they can do." />}
      <ErrorBox error={error} />
      <Section
        title="Members"
        description={kind === "group" ? "Members of a group are members of every subgroup and project inside it." : "Project members are added on top of the members inherited from its groups."}
      >
        <div className="stack">
          {canManage && (
            <form
              className="inline-form"
              onSubmit={(e: FormEvent) => {
                e.preventDefault();
                add.mutate();
              }}
            >
              <input className="grow" style={{ minWidth: 200 }} placeholder="username" aria-label="Username" value={username} onChange={(e) => setUsername(e.target.value)} />
              <div style={{ width: 160 }}>
                <LevelSelect value={level} onChange={setLevel} />
              </div>
              <button className="btn primary" disabled={!username}>
                Add member
              </button>
            </form>
          )}
          <Q q={members}>
            {(list) =>
              list.length === 0 ? (
                <p className="muted small" style={{ margin: 0 }}>
                  No direct members: access may be inherited from parent groups.
                </p>
              ) : (
                <div className="list rows">
                  {list.map((m) => (
                    <div className="item" key={m.user_id}>
                      <Avatar name={m.name || m.username} username={m.username} />
                      <div className="grow" style={{ minWidth: 0 }}>
                        <div className="row-title">
                          <span className="ellipsis">{m.name}</span>
                          {m.user_type !== "human" && <span className="pill">bot</span>}
                        </div>
                        <div className="row-meta">
                          <span>@{m.username}</span>
                          <span>since {timeAgo(m.created_at)}</span>
                        </div>
                      </div>
                      <span className="pill accent">{m.access_level}</span>
                      {canManage && (
                        <button className="btn small danger" onClick={() => remove.mutate(m.user_id)}>
                          Remove
                        </button>
                      )}
                    </div>
                  ))}
                </div>
              )
            }
          </Q>
        </div>
      </Section>
      <Section title="Shared with groups" description="Every member of these groups gets access, up to the level set here.">
        <div className="stack">
          {canManage && (
            <form
              className="inline-form"
              onSubmit={(e) => {
                e.preventDefault();
                share.mutate();
              }}
            >
              <input className="grow" style={{ minWidth: 200 }} placeholder="group path, e.g. acme/platform" aria-label="Group path" value={shareGroup} onChange={(e) => setShareGroup(e.target.value)} />
              <div style={{ width: 160 }}>
                <LevelSelect value={shareLevel} onChange={setShareLevel} />
              </div>
              <button className="btn" disabled={!shareGroup}>
                Share
              </button>
            </form>
          )}
          <Q q={shares}>
            {(list) =>
              list.length === 0 ? (
                <p className="muted small" style={{ margin: 0 }}>
                  Not shared with any group.
                </p>
              ) : (
                <div className="list rows">
                  {list.map((s) => (
                    <div className="item" key={s.group_id}>
                      <span className="tile group">{s.group_full_path.split("/").pop()?.[0]?.toUpperCase()}</span>
                      <span className="grow ref-name">{s.group_full_path}</span>
                      <span className="pill accent">up to {s.max_access_level}</span>
                      {canManage && (
                        <button className="btn small danger" onClick={() => unshare.mutate(s.group_id)}>
                          Unshare
                        </button>
                      )}
                    </div>
                  ))}
                </div>
              )
            }
          </Q>
        </div>
      </Section>
    </div>
  );
}
