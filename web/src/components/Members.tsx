import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { fetchers } from "../prefetch";
import { api, timeAgo, type S } from "../api/client";
import { AS_TYPED, Avatar, ErrorBox, Field, LevelSelect, ListHead, Q } from "./ui";
import { EmptyRow, ListSection } from "./settings";
import { FormActions, useJustAdded } from "./FormDialog";

/** Members and sharing for a group (`/groups/:id`) or project (`/projects/:id`). */
export function Members({ base, canManage, kind }: { base: string; canManage: boolean; kind: "group" | "project" }) {
  const qc = useQueryClient();
  const members = useQuery({ queryKey: ["members", base], queryFn: () => api.get<S["MemberDto"][]>(`${base}/members`) });
  const shares = useQuery({ queryKey: ["shares", base], queryFn: () => api.get<S["ShareDto"][]>(`${base}/shares`) });
  const [username, setUsername] = useState("");
  const [level, setLevel] = useState("developer");
  const [shareGroup, setShareGroup] = useState("");
  const [shareLevel, setShareLevel] = useState("developer");
  // the list's own errors (removing someone); a window shows its own
  const [error, setError] = useState<unknown>(null);
  const [formError, setFormError] = useState<unknown>(null);
  const [adding, setAdding] = useState(false);
  const [sharing, setSharing] = useState(false);
  const fresh = useJustAdded();
  const closeAdd = () => {
    setAdding(false);
    setUsername("");
    setFormError(null);
  };
  const closeShare = () => {
    setSharing(false);
    setShareGroup("");
    setFormError(null);
  };
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["members", base] });
    qc.invalidateQueries({ queryKey: ["shares", base] });
  };
  const add = useMutation({
    mutationFn: () => api.post(`${base}/members`, { username, access_level: level }),
    onSuccess: () => {
      fresh.mark(username.trim());
      closeAdd();
      refresh();
    },
    onError: setFormError,
  });
  const remove = useMutation({ mutationFn: (uid: number) => api.del(`${base}/members/${uid}`), onSuccess: refresh, onError: setError });
  const share = useMutation({
    mutationFn: async () => {
      const g = await fetchers.pathResolve(shareGroup);
      if (g.kind !== "group") throw new Error("not a group");
      return api.post(`${base}/shares`, { group_id: g.id, max_access_level: shareLevel });
    },
    onSuccess: () => {
      fresh.mark(shareGroup.trim());
      closeShare();
      refresh();
    },
    onError: setFormError,
  });
  const unshare = useMutation({ mutationFn: (gid: number) => api.del(`${base}/shares/${gid}`), onSuccess: refresh, onError: setError });

  return (
    <div className="stack settings-page">
      {kind !== "group" && <ListHead title="Members" sub="Who can reach this project, and what they can do." />}
      <ErrorBox error={error} />
      <ListSection
        title="Members"
        description={kind === "group" ? "Members of a group are members of every subgroup and project inside it." : "Project members are added on top of the members inherited from its groups."}
        add={canManage ? "Add a member" : undefined}
        adding={adding}
        onAdd={() => setAdding(true)}
        onClose={closeAdd}
        form={
          <form
            className="stack"
            onSubmit={(e) => {
              e.preventDefault();
              add.mutate();
            }}
          >
            <ErrorBox error={formError} />
            <Field label="Username">
              <input value={username} onChange={(e) => setUsername(e.target.value)} placeholder="username" {...AS_TYPED} autoComplete="off" />
            </Field>
            <Field label="Role">
              <LevelSelect value={level} onChange={setLevel} />
            </Field>
            <FormActions label="Add member" disabled={!username.trim()} busy={add.isPending} onCancel={closeAdd} />
          </form>
        }
      >
        <Q q={members}>
          {(list) =>
            list.length === 0 ? (
              <EmptyRow>No direct members: access may be inherited from parent groups.</EmptyRow>
            ) : (
              <div className="list rows">
                {list.map((m) => (
                  <div className={"item" + fresh.fresh(m.username)} key={m.user_id}>
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
                    <span className="pill">{m.access_level}</span>
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
      </ListSection>
      <ListSection
        title="Shared with groups"
        description="Every member of these groups gets access, up to the level set here."
        add={canManage ? "Share with a group" : undefined}
        adding={sharing}
        onAdd={() => setSharing(true)}
        onClose={closeShare}
        form={
          <form
            className="stack"
            onSubmit={(e) => {
              e.preventDefault();
              share.mutate();
            }}
          >
            <ErrorBox error={formError} />
            <Field label="Group path">
              <input value={shareGroup} onChange={(e) => setShareGroup(e.target.value)} placeholder="acme/platform" {...AS_TYPED} autoComplete="off" />
            </Field>
            <Field label="Up to">
              <LevelSelect value={shareLevel} onChange={setShareLevel} />
            </Field>
            <FormActions label="Share" disabled={!shareGroup.trim()} busy={share.isPending} onCancel={closeShare} />
          </form>
        }
      >
        <Q q={shares}>
          {(list) =>
            list.length === 0 ? (
              <EmptyRow>Not shared with any group.</EmptyRow>
            ) : (
              <div className="list rows">
                {list.map((s) => (
                  <div className={"item" + fresh.fresh(s.group_full_path)} key={s.group_id}>
                    <span className="tile group">{s.group_full_path.split("/").pop()?.[0]?.toUpperCase()}</span>
                    <span className="grow ref-name">{s.group_full_path}</span>
                    <span className="pill">up to {s.max_access_level}</span>
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
      </ListSection>
    </div>
  );
}
