import { useEffect, useState, type FormEvent } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { api, enc, type S } from "../api/client";
import { useSession } from "../session";
import { ErrorBox, Field, VisibilitySelect } from "../components/ui";
import { Select } from "../components/controls";
import { useGroups } from "../prefetch";

const slug = (s: string) =>
  s
    .toLowerCase()
    .replace(/[^a-z0-9_.]+/g, "-")
    .replace(/^[-.]+|[-.]+$/g, "");

export function NewGroup() {
  const nav = useNavigate();
  const [params] = useSearchParams();
  const parentId = params.get("parent");
  const parent = useQuery({ queryKey: ["group", Number(parentId)], queryFn: () => api.get<S["GroupDetail"]>(`/groups/${parentId}`), enabled: !!parentId });
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [pathEdited, setPathEdited] = useState(false);
  const [visibility, setVisibility] = useState("private");
  const [description, setDescription] = useState("");
  const [error, setError] = useState<unknown>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    try {
      const g = await api.post<S["GroupDto"]>("/groups", { name, path, visibility, description, parent_id: parentId ? Number(parentId) : null });
      nav(`/${g.full_path}`);
    } catch (err) {
      setError(err);
    }
  };

  return (
    <div className="page narrow">
      <h1>{parentId ? "New subgroup" : "New organization"}</h1>
      <p className="muted">
        {parentId
          ? `Subgroups are folders inside ${parent.data?.full_path ?? "the group"}. Members, CI variables and runners are inherited.`
          : "An organization is a top-level group. It holds subgroups (folders) and projects, and its policy applies to everything inside."}
      </p>
      <form className="stack" onSubmit={submit}>
        <ErrorBox error={error} />
        <Field label="Name">
          <input
            autoFocus
            value={name}
            onChange={(e) => {
              setName(e.target.value);
              if (!pathEdited) setPath(slug(e.target.value));
            }}
          />
        </Field>
        <Field label="Path" hint={`URL: /${parent.data ? parent.data.full_path + "/" : ""}${path || "…"}`}>
          <input
            value={path}
            onChange={(e) => {
              setPath(e.target.value);
              setPathEdited(true);
            }}
          />
        </Field>
        <Field label="Visibility">
          <VisibilitySelect value={visibility} onChange={setVisibility} />
        </Field>
        <Field label="Description">
          <input value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        <div className="form-actions">
          <button className="btn primary">{parentId ? "Create subgroup" : "Create organization"}</button>
        </div>
      </form>
    </div>
  );
}

type NsOption = { id: number; label: string };

export function NewProject() {
  const nav = useNavigate();
  const { user } = useSession();
  const [params] = useSearchParams();
  const groups = useGroups();
  const preset = params.get("namespace");
  const presetNs = useQuery({ queryKey: ["group", preset], queryFn: () => api.get<S["GroupDetail"]>(`/groups/${enc(preset!)}`), enabled: !!preset });
  const [ns, setNs] = useState<string>("");
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [pathEdited, setPathEdited] = useState(false);
  const [visibility, setVisibility] = useState("private");
  const [description, setDescription] = useState("");
  const [refFormat, setRefFormat] = useState("files");
  const [error, setError] = useState<unknown>(null);

  const options: NsOption[] = [];
  if (user?.namespace_id) options.push({ id: user.namespace_id, label: `${user.username} (personal)` });
  if (presetNs.data && !groups.data?.some((g) => g.id === presetNs.data.id)) options.push({ id: presetNs.data.id, label: presetNs.data.full_path });
  for (const g of groups.data ?? []) options.push({ id: g.id, label: g.full_path });

  const [touched, setTouched] = useState(false);
  useEffect(() => {
    // the preset (from a group's "New project" button) wins once it has loaded
    if (touched) return;
    if (presetNs.data) setNs(String(presetNs.data.id));
    else if (!preset && !ns && options.length) setNs(String(options[0].id));
  }, [presetNs.data, options.length, touched]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    try {
      const p = await api.post<S["ProjectDto"]>("/projects", { namespace_id: Number(ns), name, path, visibility, description, ref_format: refFormat });
      nav(`/${p.full_path}`);
    } catch (err) {
      setError(err);
    }
  };

  return (
    <div className="page narrow">
      <h1>New project</h1>
      <p className="muted">
        A Git repository with its merge requests, pipelines and members. Moving from GitLab? <Link to="/import">Import projects</Link> instead.
      </p>
      <form className="stack" onSubmit={submit}>
        <ErrorBox error={error} />
        <Field label="Namespace" hint="An organization, a subgroup, or your personal namespace">
          <Select
            value={String(ns)}
            onChange={(v) => {
              setTouched(true);
              setNs(v);
            }}
            options={options.map((o) => ({ value: String(o.id), label: o.label }))}
          />
        </Field>
        <Field label="Project name">
          <input
            autoFocus
            value={name}
            onChange={(e) => {
              setName(e.target.value);
              if (!pathEdited) setPath(slug(e.target.value));
            }}
          />
        </Field>
        <Field label="Path">
          <input
            value={path}
            onChange={(e) => {
              setPath(e.target.value);
              setPathEdited(true);
            }}
          />
        </Field>
        <Field label="Visibility">
          <VisibilitySelect value={visibility} onChange={setVisibility} />
        </Field>
        <Field label="Description">
          <input value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        <Field label="Ref storage" hint="Reftable suits repos with very many refs; files gets the fastest browsing (ADR-0011).">
          <Select
            value={refFormat}
            onChange={setRefFormat}
            options={[
              { value: "files", label: "files", hint: "Default" },
              { value: "reftable", label: "reftable" },
            ]}
          />
        </Field>
        <div className="form-actions">
          <button className="btn primary">Create project</button>
        </div>
      </form>
    </div>
  );
}
