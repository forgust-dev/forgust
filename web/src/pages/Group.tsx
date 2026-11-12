import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { fetchers } from "../prefetch";
import { api, bytes, type S } from "../api/client";
import { Badge, Section, Empty, ErrorBox, Field, LevelSelect, NotFound, Q, VISIBILITY_OPTIONS, VisibilitySelect, ListHead, SearchField, Tile, QuotaBar, useFormRun } from "../components/ui";
import { Select } from "../components/controls";
import { useSetNav } from "../shell/nav";
import { VariablesPanel } from "./project/ci/Settings";
import { RunnersPanel } from "./project/ci/Runners";
import { Members } from "../components/Members";
import { AccessTokens, DeployTokens } from "../components/Tokens";
import { Webhooks } from "../components/Webhooks";
import { GroupRow, ProjectRow, Visibility } from "./Dashboard";
import { AvatarUpload } from "../components/AvatarUpload";

export function GroupPage({ id, rest }: { id: number; fullPath: string; rest: string }) {
  const g = useQuery({ queryKey: ["group", id], queryFn: () => api.get<S["GroupDetail"]>(`/groups/${id}`) });
  return (
    <div className="page">
      <Q q={g}>
        {(group) => {
          const tab = rest.split("/")[0];
          return (
            <>
              <GroupNav group={group} tab={tab} />
              {tab === "" && (
                <div className="page-head">
                  <div className="row wrap">
                    <Tile name={group.name} src={group.avatar_url} group big />
                    <h1 style={{ margin: 0 }}>{group.name}</h1>
                    <span className="pill">
                      <Visibility v={group.visibility} />
                    </span>
                    {group.is_org && <span className="pill">Organization</span>}
                    <span className="grow" />
                    {group.access_level !== "none" && <Badge tone="accent">{group.access_level}</Badge>}
                  </div>
                  {group.description && <p className="muted" style={{ margin: "6px 0 0" }}>{group.description}</p>}
                </div>
              )}
              {tab === "members" && <ListHead title="Members" sub={`Who can reach ${group.name} and its projects, and what they can do.`} />}
              {tab === "" && <Overview group={group} />}
              {tab === "members" && <Members base={`/groups/${id}`} canManage={group.can_admin || group.access_level === "maintainer"} kind="group" />}
              {tab === "settings" &&
                (group.can_admin ? (
                  <div className="stack settings-page">
                    <GroupSettings group={group} />
                    {group.kind === "group" && (
                      <>
                        <RunnersPanel
                          title="Group runners"
                          list={`/groups/${group.id}/runners`}
                          create={`/groups/${group.id}/runners`}
                          note="Runners shared by every project in this group and its subgroups."
                        />
                        <VariablesPanel base={`/groups/${group.id}`} />
                      </>
                    )}
                  </div>
                ) : (
                  <NotFound />
                ))}
            </>
          );
        }}
      </Q>
    </div>
  );
}

function GroupNav({ group, tab }: { group: S["GroupDetail"]; tab: string }) {
  const crumbs = tab === "members" ? [{ label: "Members" }] : tab === "settings" ? [{ label: "Settings" }] : [];
  useSetNav({ kind: "group", group, crumbs }, [group, tab]);
  return null;
}

function Overview({ group }: { group: S["GroupDetail"] }) {
  const [search, setSearch] = useState("");
  const subgroups = useQuery({ queryKey: ["subgroups", group.id], queryFn: () => api.get<S["GroupDto"][]>(`/groups/${group.id}/subgroups`), enabled: group.kind === "group" });
  const projects = useQuery({ queryKey: ["group-projects", group.id], queryFn: () => api.get<S["ProjectDto"][]>(`/groups/${group.id}/projects`) });
  const match = (s: string) => s.toLowerCase().includes(search.toLowerCase());
  const subs = (subgroups.data ?? []).filter((g) => match(g.name) || match(g.full_path));
  return (
    <div className="stack list-page">
      <div className="list-toolbar plain">
        <SearchField value={search} onChange={setSearch} placeholder="Filter subgroups and projects" />
        <div className="spacer" />
        {group.can_create_subgroup && (
          <Link className="btn" to={`/new/group?parent=${group.id}`}>
            New subgroup
          </Link>
        )}
        {group.can_create_project && (
          <Link className="btn primary" to={`/new/project?namespace=${encodeURIComponent(group.full_path)}`}>
            New project
          </Link>
        )}
      </div>
      <Q q={projects}>
        {(all) => {
          const list = all.filter((p) => match(p.name) || match(p.full_path) || match(p.description));
          if (list.length === 0 && subs.length === 0)
            return <Empty>{search ? <>Nothing matches “{search}”.</> : <>This {group.kind === "group" ? "group" : "namespace"} is empty.</>}</Empty>;
          return (
            <div className="list rows">
              {subs.map((g) => (
                <GroupRow key={`g${g.id}`} g={g} />
              ))}
              {list.map((p) => (
                <ProjectRow key={p.id} p={p} />
              ))}
            </div>
          );
        }}
      </Q>
    </div>
  );
}

function GroupSettings({ group }: { group: S["GroupDetail"] }) {
  const qc = useQueryClient();
  const nav = useNavigate();
  const [name, setName] = useState(group.name);
  const [description, setDescription] = useState(group.description);
  const [visibility, setVisibility] = useState(group.visibility);
  const [path, setPath] = useState(group.path);
  const [parent, setParent] = useState("");
  const [policy, setPolicy] = useState({
    max_visibility: group.policy?.max_visibility ?? "",
    require_two_factor: group.policy?.require_two_factor ?? false,
    project_creation_level: group.policy?.project_creation_level ?? "maintainer",
    subgroup_creation_level: group.policy?.subgroup_creation_level ?? "owner",
  });
  const { run, msg, error } = useFormRun(() => qc.invalidateQueries());

  return (
    <div className="stack settings-page">
      <ListHead title="Settings" sub={`Settings of the ${group.name} group and what it applies to its projects.`} />
      <ErrorBox error={error} />
      {msg && <div className="alert ok">{msg}</div>}
      <Section title="Picture" description="The group's picture in lists and on its page.">
        <AvatarUpload
          preview={<Tile name={group.name} src={group.avatar_url} group big />}
          path={`/groups/${group.id}/avatar`}
          hasOne={!!group.avatar_url}
          onChanged={() => qc.invalidateQueries({ queryKey: ["group", group.id] })}
          what="in lists and on the group's page"
        />
      </Section>
      <Section title="General" description="Name, description and who can see the group.">
        <form className="stack" onSubmit={run(() => api.put(`/groups/${group.id}`, { name, description, visibility }), "Saved.")}>
          <Field label="Name">
            <input value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label="Description">
            <input value={description} onChange={(e) => setDescription(e.target.value)} />
          </Field>
          <Field label="Visibility" hint="Cannot be wider than the parent group, nor narrower than what is inside.">
            <VisibilitySelect value={visibility} onChange={setVisibility} />
          </Field>
          <div>
            <button className="btn primary">Save</button>
          </div>
        </form>
      </Section>
      {group.is_org && (
        <Section title="Organization policy" description="Rules for every subgroup and project of the organization.">
          <form className="stack" onSubmit={run(() => api.put(`/groups/${group.id}/policy`, { ...policy, max_visibility: policy.max_visibility || null }), "Policy saved.")}>
            <Field label="Maximum visibility" hint="Applies to every subgroup and project in the organization.">
              <Select
                value={policy.max_visibility}
                onChange={(v) => setPolicy({ ...policy, max_visibility: v })}
                options={[{ value: "", label: "No limit" }, ...VISIBILITY_OPTIONS.map(({ value, label }) => ({ value, label }))]}
              />
            </Field>
            <label className="check">
              <input type="checkbox" checked={policy.require_two_factor} onChange={(e) => setPolicy({ ...policy, require_two_factor: e.target.checked })} />
              Require two-factor authentication for all members
            </label>
            <div className="grid2">
              <Field label="Who can create projects">
                <LevelSelect value={policy.project_creation_level} onChange={(v) => setPolicy({ ...policy, project_creation_level: v })} levels={["developer", "maintainer", "owner"]} />
              </Field>
              <Field label="Who can create subgroups">
                <LevelSelect value={policy.subgroup_creation_level} onChange={(v) => setPolicy({ ...policy, subgroup_creation_level: v })} levels={["maintainer", "owner"]} />
              </Field>
            </div>
            <div>
              <button className="btn primary">Save policy</button>
            </div>
          </form>
        </Section>
      )}
      {group.is_org && (
        <Section title="Storage" description="What the organization's projects keep (repositories, LFS, CI artifacts and logs, images, uploads), against its storage quota.">
          <GroupStorage groupId={group.id} />
        </Section>
      )}
      <Section
        title="Dependency updates"
        description="Turn on Renovate for the group's projects: every project that never chose, and every new one. Each project can still turn it off."
      >
        <GroupDependencyUpdates groupId={group.id} />
      </Section>
      <Section
        title="Security scanning"
        description="Turn on the built-in scanners for the group's projects: every project that never chose, and every new one. Each project can still turn them off."
      >
        <GroupSecurityScanning groupId={group.id} />
      </Section>
      <Section title="Webhooks" description="Call a URL when something happens in any project of the group.">
        <Webhooks base={`/groups/${group.id}`} />
      </Section>
      <Section title="Access tokens" description="Tokens for scripts and integrations, acting as a bot member of this group.">
        <AccessTokens base={`/groups/${group.id}`} />
      </Section>
      <Section title="Deploy tokens" description="Credentials to clone the group's repositories without a user account.">
        <DeployTokens base={`/groups/${group.id}`} />
      </Section>
      <Section title="Advanced" description="Change the path, move the group, or delete it." danger>
        <div className="stack">
          <form
            className="row wrap"
            onSubmit={run(async () => {
              const g = await api.post<S["GroupDto"]>(`/groups/${group.id}/path`, { path });
              nav(`/${g.full_path}/-/settings`, { replace: true });
            }, "Path changed. Old URLs keep redirecting.")}
          >
            <Field label="Change path">
              <input value={path} onChange={(e) => setPath(e.target.value)} />
            </Field>
            <button className="btn" style={{ alignSelf: "flex-end" }}>
              Rename
            </button>
          </form>
          <form
            className="row wrap"
            onSubmit={run(async () => {
              let parentId: number | null = null;
              if (parent) parentId = (await fetchers.pathResolve(parent)).id;
              const g = await api.post<S["GroupDto"]>(`/groups/${group.id}/transfer`, { parent_id: parentId });
              nav(`/${g.full_path}/-/settings`, { replace: true });
            }, "Group moved.")}
          >
            <Field label="Move to another group" hint="Leave empty to make it a top-level organization.">
              <input placeholder="parent group path" value={parent} onChange={(e) => setParent(e.target.value)} />
            </Field>
            <button className="btn" style={{ alignSelf: "flex-end" }}>
              Move
            </button>
          </form>
          <div>
            <button
              className="btn danger"
              onClick={run(async () => {
                if (!confirm(`Delete ${group.full_path}? It must be empty.`)) return;
                await api.del(`/groups/${group.id}`);
                nav("/");
              }, "Deleted.")}
            >
              Delete group
            </button>
          </div>
        </div>
      </Section>
    </div>
  );
}

function GroupDependencyUpdates({ groupId }: { groupId: number }) {
  return (
    <GroupToggle<S["GroupRenovateStatus"]>
      url={`/groups/${groupId}/renovate`}
      queryKey={["group-renovate", groupId]}
      label="Dependency updates for the group's projects"
      said={(on, r) => (on ? `On for new projects; turned on for ${r.turned_on} project(s).` : "New projects no longer get dependency updates.")}
      unavailable="This instance does not offer dependency updates."
    />
  );
}

/** A group-wide default for a built-in feature: new projects and those that never chose. */
function GroupToggle<R>({
  url,
  queryKey,
  label,
  said,
  unavailable,
}: {
  url: string;
  queryKey: unknown[];
  label: string;
  said: (on: boolean, r: R) => string;
  unavailable?: string;
}) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey, queryFn: () => api.get<{ enabled: boolean; available?: boolean }>(url) });
  const [error, setError] = useState<unknown>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const set = async (enabled: boolean) => {
    setError(null);
    try {
      setMsg(said(enabled, await api.put<R>(url, { enabled })));
      qc.invalidateQueries({ queryKey });
    } catch (e) {
      setError(e);
    }
  };
  return (
    <Q q={q}>
      {(s) =>
        s.available === false ? (
          <div className="muted small">{unavailable}</div>
        ) : (
          <div className="stack">
            <ErrorBox error={error} />
            {msg && <div className="alert ok">{msg}</div>}
            <label className="check">
              <input type="checkbox" checked={s.enabled} onChange={(e) => set(e.target.checked)} /> {label}
            </label>
          </div>
        )
      }
    </Q>
  );
}

function GroupStorage({ groupId }: { groupId: number }) {
  const q = useQuery({ queryKey: ["group-storage", groupId], queryFn: () => api.get<S["GroupStorageUse"]>(`/groups/${groupId}/storage`) });
  return (
    <Q q={q}>
      {(u) =>
        u.quota ? (
          <div className="stack">
            <QuotaBar label={`Quota of ${u.namespace}, shared by all its projects`} used={u.used} quota={u.quota} />
            <p className="small muted" style={{ margin: 0 }}>
              Once it is full, new pushes, LFS files, CI artifacts, images and uploads are refused in every project of {u.namespace}. Administrators set quotas.
            </p>
          </div>
        ) : (
          <p className="small" style={{ margin: 0 }}>
            {u.namespace} keeps <strong>{bytes(u.used)}</strong>. No storage quota.
          </p>
        )
      }
    </Q>
  );
}

function GroupSecurityScanning({ groupId }: { groupId: number }) {
  return (
    <GroupToggle<{ projects_turned_on: number }>
      url={`/groups/${groupId}/security/scanning`}
      queryKey={["group-scanning", groupId]}
      label="Security scanning for the group's projects"
      said={(on, r) => (on ? `On for new projects; turned on for ${r.projects_turned_on} project(s).` : "New projects no longer get security scanning.")}
    />
  );
}

