import { Suspense, lazy, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { Archive, BookOpen, Building2, CalendarDays, GitBranch, GitCommitHorizontal, Globe, Handshake, HardDrive, History as HistoryIcon, Lock, Scale, ShieldCheck, UserCheck, Users as UsersIcon } from "lucide-react";
import { api, bytes, qs, timeAgo, type S, encPath } from "../../api/client";
import { StatusIcon, statusLabel } from "./ci/Status";
import { fetchers, keys } from "../../prefetch";
import { Badge, CopyField, NotFound, Q, Spinner, Tile } from "../../components/ui";
import { useSetNav } from "../../shell/nav";
import { Blame, Blob, Tree } from "./Code";
import { Commit, Commits } from "./History";
import { Branches, Compare, Tags } from "./Refs";
import { MilestoneForm, MilestonePage, MilestonesPage } from "./Milestones";
import { NotificationBell } from "../../components/Notifications";
import { useSession } from "../../session";
import { useToast } from "../../shell/toast";
import { Members } from "../../components/Members";
import { RecentBranchBanner } from "./MrStack";

// each section loads with its first visit (the repository's home needs none of these)
const mrs = () => import("./MergeRequests");
const issues = () => import("./Issues");
const pipelines = () => import("./ci/Pipelines");
const releases = () => import("./Releases");
const MergeRequestList = lazy(() => mrs().then((m) => ({ default: m.MergeRequestList })));
const MergeRequestPage = lazy(() => mrs().then((m) => ({ default: m.MergeRequestPage })));
const NewMergeRequest = lazy(() => mrs().then((m) => ({ default: m.NewMergeRequest })));
const IssueList = lazy(() => issues().then((m) => ({ default: m.IssueList })));
const IssuePage = lazy(() => issues().then((m) => ({ default: m.IssuePage })));
const LabelsPage = lazy(() => issues().then((m) => ({ default: m.LabelsPage })));
const NewIssue = lazy(() => issues().then((m) => ({ default: m.NewIssue })));
const PipelineList = lazy(() => pipelines().then((m) => ({ default: m.PipelineList })));
const PipelinePage = lazy(() => pipelines().then((m) => ({ default: m.PipelinePage })));
const RunPipeline = lazy(() => pipelines().then((m) => ({ default: m.RunPipeline })));
const ReleaseForm = lazy(() => releases().then((m) => ({ default: m.ReleaseForm })));
const ReleasePage = lazy(() => releases().then((m) => ({ default: m.ReleasePage })));
const Releases = lazy(() => releases().then((m) => ({ default: m.Releases })));
const JobPage = lazy(() => import("./ci/Job").then((m) => ({ default: m.JobPage })));
const ArtifactBrowser = lazy(() => import("./ci/Artifacts").then((m) => ({ default: m.ArtifactBrowser })));
const Schedules = lazy(() => import("./ci/Schedules").then((m) => ({ default: m.Schedules })));
const Environments = lazy(() => import("./ci/Environments").then((m) => ({ default: m.Environments })));
const CiSettings = lazy(() => import("./ci/Settings").then((m) => ({ default: m.CiSettings })));
const ProjectSettings = lazy(() => import("./Settings").then((m) => ({ default: m.ProjectSettings })));
const SecurityPage = lazy(() => import("./Security").then((m) => ({ default: m.SecurityPage })));
const Registry = lazy(() => import("./Registry").then((m) => ({ default: m.Registry })));
const EditFile = lazy(() => import("./Edit").then((m) => ({ default: m.EditFile })));

export type Ctx = { p: S["ProjectDetail"]; base: string; web: string; setRef?: (r: string) => void };

const SECTIONS: Record<string, string> = {
  tree: "Repository",
  blob: "Repository",
  blame: "Blame",
  commits: "Commits",
  commit: "Commits",
  branches: "Branches",
  tags: "Tags",
  releases: "Releases",
  container_registry: "Container registry",
  security: "Security",
  compare: "Compare",
  members: "Members",
  merge_requests: "Merge requests",
  issues: "Issues",
  labels: "Labels",
  milestones: "Milestones",
  settings: "Settings",
  pipelines: "Pipelines",
  jobs: "Jobs",
  pipeline_schedules: "Schedules",
  environments: "Environments",
};

/** Sidebar + breadcrumbs for a project page. */
function ProjectNav({ p, rest, gitRef }: { p: S["ProjectDetail"]; rest: string; gitRef?: string }) {
  const [section, ...more] = rest.split("/");
  const crumbs: { label: string; to?: string }[] = [];
  const web = `/${p.full_path}`;
  if (SECTIONS[section]) crumbs.push({ label: SECTIONS[section], to: section === "commit" ? `${web}/-/commits/${gitRef ?? p.default_branch ?? "main"}` : section === "tree" || section === "blob" ? web : section === "jobs" ? `${web}/-/pipelines` : `${web}/-/${section}` });
  if (section === "merge_requests" && more[0]) crumbs.push({ label: more[0] === "new" ? "New" : `!${more[0]}` });
  if (section === "issues" && more[0]) crumbs.push({ label: more[0] === "new" ? "New" : `#${more[0]}` });
  if (section === "commit" && more[0]) crumbs.push({ label: more[0].slice(0, 8) });
  if (section === "pipelines" && more[0]) crumbs.push({ label: more[0] === "new" ? "Run" : `#${more[0]}` });
  if (section === "jobs" && more[0]) crumbs.push({ label: `#${more[0]}` });
  if (section === "settings" && more[0] === "ci_cd") crumbs.push({ label: "CI/CD" });
  if (section === "milestones" && more[0]) crumbs.push({ label: more[0] === "new" ? "New" : more[1] === "edit" ? "Edit" : `Milestone` });
  if (section === "releases" && more[0]) crumbs.push({ label: more.length === 1 && more[0] === "new" ? "New" : more.at(-1) === "edit" ? `Edit ${more.slice(0, -1).join("/")}` : more.join("/") });
  useSetNav({ kind: "project", project: p, ref: gitRef, crumbs }, [p, rest, gitRef]);
  return null;
}

/**
 * `releases`, `releases/new`, `releases/<tag>` and `releases/<tag>/edit` (the path
 * arrives decoded, so a tag's `/` splits it: the segments are joined back).
 */
function ReleaseRoute({ ctx, more }: { ctx: Ctx; more: string[] }) {
  if (more.length === 0) return <Releases ctx={ctx} />;
  const edit = more.length > 1 && more[more.length - 1] === "edit";
  if (edit || (more.length === 1 && more[0] === "new")) {
    if (!ctx.p.can_push) return <NotFound />;
    return edit ? <ReleaseForm ctx={ctx} tag={more.slice(0, -1).join("/")} /> : <ReleaseForm ctx={ctx} />;
  }
  return <ReleasePage ctx={ctx} tag={more.join("/")} />;
}

export function ProjectPage({ id, fullPath, rest }: { id: number; fullPath: string; rest: string }) {
  const q = useQuery({ queryKey: keys.project(id), queryFn: () => fetchers.project(id) });
  const [gitRef, setRef] = useState<string | undefined>();
  return (
    <div className="page">
      <Q q={q}>
        {(p) => {
          const ctx: Ctx = { p, base: `/projects/${id}/repository`, web: `/${fullPath}`, setRef };
          const [section, ...more] = rest.split("/");
          const tail = more.join("/");
          const ref = p.default_branch ?? "main";
          return (
            <>
              <ProjectNav p={p} rest={rest} gitRef={gitRef} />
              {section === "" && <ProjectHeader p={p} />}
              <ArchivedBanner p={p} />
              <StorageBanner p={p} />
              {!p.can_read_code && !["members", "issues", "labels", "milestones"].includes(section) ? (
                <p className="muted">You can see this project but not its code. Ask a maintainer for the Reporter role.</p>
              ) : p.empty_repo && ["", "tree", "blob", "commits", "merge_requests", "pipelines"].includes(section) ? (
                <EmptyRepo p={p} />
              ) : (
                // a section still loading: a spinner here, the header and sidebar stay
                <Suspense fallback={<Spinner />}>
                  {section === "" && (
                    <div className="repo-layout">
                      <div className="repo-main">
                        <RecentBranchBanner ctx={ctx} />
                        <Tree ctx={ctx} refAndPath={ref} />
                      </div>
                      <RepoAbout ctx={ctx} />
                    </div>
                  )}
                  {section === "tree" && <Tree ctx={ctx} refAndPath={tail} />}
                  {section === "blob" && <Blob ctx={ctx} refAndPath={tail} />}
                  {section === "blame" && <Blame ctx={ctx} refAndPath={tail} />}
                  {(section === "edit" || section === "new" || section === "delete") && (p.can_push ? <EditFile ctx={ctx} refAndPath={tail} mode={section} /> : <NotFound />)}
                  {section === "commits" && <Commits ctx={ctx} refAndPath={tail || ref} />}
                  {section === "commit" && <Commit ctx={ctx} sha={tail} />}
                  {section === "branches" && <Branches ctx={ctx} />}
                  {section === "tags" && <Tags ctx={ctx} />}
                  {section === "releases" && <ReleaseRoute ctx={ctx} more={more} />}
                  {section === "container_registry" && <Registry ctx={ctx} />}
                  {section === "security" && <SecurityPage ctx={ctx} />}
                  {section === "compare" && <Compare ctx={ctx} spec={tail} />}
                  {section === "members" && <Members base={`/projects/${id}`} canManage={p.can_admin} kind="project" />}
                  {section === "issues" && more.length === 0 && <IssueList ctx={ctx} />}
                  {section === "issues" && more[0] === "new" && <NewIssue ctx={ctx} />}
                  {section === "issues" && /^\d+$/.test(more[0] ?? "") && <IssuePage ctx={ctx} iid={Number(more[0])} />}
                  {section === "labels" && <LabelsPage ctx={ctx} />}
                  {section === "milestones" && more.length === 0 && <MilestonesPage ctx={ctx} />}
                  {section === "milestones" && more[0] === "new" && (p.can_push ? <MilestoneForm ctx={ctx} /> : <NotFound />)}
                  {section === "milestones" && /^\d+$/.test(more[0] ?? "") && more[1] === "edit" && (p.can_push ? <MilestoneForm ctx={ctx} id={Number(more[0])} /> : <NotFound />)}
                  {section === "milestones" && /^\d+$/.test(more[0] ?? "") && !more[1] && <MilestonePage ctx={ctx} id={Number(more[0])} />}
                  {section === "merge_requests" && more.length === 0 && <MergeRequestList ctx={ctx} />}
                  {section === "merge_requests" && more[0] === "new" && <NewMergeRequest ctx={ctx} />}
                  {section === "merge_requests" && /^\d+$/.test(more[0] ?? "") && (
                    <MergeRequestPage ctx={ctx} iid={Number(more[0])} tab={more[1] === "commits" ? "commits" : more[1] === "diffs" ? "diffs" : "overview"} />
                  )}
                  {section === "settings" && more[0] !== "ci_cd" && (p.can_admin ? <ProjectSettings ctx={ctx} /> : <NotFound />)}
                  {section === "settings" && more[0] === "ci_cd" && (p.can_admin ? <CiSettings ctx={ctx} /> : <NotFound />)}
                  {section === "pipelines" && more.length === 0 && <PipelineList ctx={ctx} />}
                  {section === "pipelines" && more[0] === "new" && <RunPipeline ctx={ctx} />}
                  {section === "pipelines" && /^\d+$/.test(more[0] ?? "") && <PipelinePage ctx={ctx} iid={Number(more[0])} />}
                  {section === "jobs" && /^\d+$/.test(more[0] ?? "") && !more[1] && <JobPage ctx={ctx} jobId={Number(more[0])} />}
                  {section === "jobs" && /^\d+$/.test(more[0] ?? "") && more[1] === "artifacts" && <ArtifactBrowser ctx={ctx} jobId={Number(more[0])} />}
                  {section === "pipeline_schedules" && (p.can_push ? <Schedules ctx={ctx} /> : <NotFound />)}
                  {section === "environments" && <Environments ctx={ctx} stopped={more[0] === "stopped"} />}
                  {!["", "tree", "blob", "blame", "edit", "new", "delete", "commits", "commit", "branches", "tags", "releases", "container_registry", "security", "compare", "members", "settings", "merge_requests", "issues", "labels", "milestones", "pipelines", "jobs", "pipeline_schedules", "environments"].includes(section) && <NotFound />}
                </Suspense>
              )}
            </>
          );
        }}
      </Q>
    </div>
  );
}

const VISIBILITY_ICON: Record<string, ReactNode> = { private: <Lock size={12} />, internal: <Building2 size={12} />, public: <Globe size={12} /> };

/** On every page of an archived project: it is read-only; its Owner unarchives it here. */
function ArchivedBanner({ p }: { p: S["ProjectDetail"] }) {
  const qc = useQueryClient();
  const toast = useToast();
  if (!p.archived) return null;
  return (
    <div className="banner" role="status">
      <Archive size={16} />
      <span className="grow small">
        This project is <strong>archived</strong>: everything stays readable, nothing can change (code, issues, merge requests, CI, settings).
      </span>
      {p.can_own && (
        <button
          className="btn small"
          onClick={async () => {
            try {
              await api.post(`/projects/${p.id}/unarchive`);
              toast.ok("Unarchived");
              qc.invalidateQueries();
            } catch (e) {
              toast.error(e);
            }
          }}
        >
          Unarchive
        </button>
      )}
    </div>
  );
}

/** For who can push: the storage quota (the project's, or its namespace's) is 80 % or 100 % full. */
function StorageBanner({ p }: { p: S["ProjectDetail"] }) {
  const w = p.storage_warning;
  if (!w || p.archived) return null;
  const what = w.scope === "namespace" ? <>The storage of <strong>{w.namespace}</strong></> : <>This project's storage</>;
  return (
    <div className={`banner${w.level >= 100 ? " danger" : ""}`} role="status">
      <HardDrive size={16} />
      <span className="grow small">
        {what} is {w.level >= 100 ? <strong>full</strong> : <>{w.level} % full</>}
        {w.level >= 100
          ? ": new pushes, LFS files, CI artifacts, images and uploads are refused until something is deleted or the quota is raised."
          : ": once it is full, new pushes, LFS files, CI artifacts, images and uploads are refused."}
        {p.can_admin && (
          <>
            {" "}
            <Link to={`/${p.full_path}/-/settings#storage`}>See what it keeps</Link>
          </>
        )}
      </span>
    </div>
  );
}

function ProjectHeader({ p }: { p: S["ProjectDetail"] }) {
  const { user } = useSession();
  return (
    <div className="page-head repo-head">
      <div className="row wrap" style={{ gap: 12 }}>
        <Tile name={p.name} src={p.avatar_url} big />
        <h1 style={{ margin: 0 }}>{p.name}</h1>
        <span className="pill">
          {VISIBILITY_ICON[p.visibility]}
          {p.visibility[0].toUpperCase() + p.visibility.slice(1)}
        </span>
        {p.ref_format === "reftable" && <span className="pill">reftable</span>}
        {p.archived && (
          <span className="pill warn" title={p.archived_at ? `Archived ${new Date(p.archived_at).toLocaleString()}` : undefined}>
            <Archive size={13} /> Archived
          </span>
        )}
        <span className="grow" />
        {p.access_level !== "none" && <Badge tone="accent">{p.access_level}</Badge>}
        {user && <NotificationBell projectId={p.id} />}
      </div>
    </div>
  );
}

/** Share of each language, as a bar and a legend (small ones grouped as "Other"). */
function Languages({ langs }: { langs: S["LanguageShare"][] }) {
  const total = langs.reduce((t, l) => t + l.bytes, 0) || 1;
  const shown = langs.filter((l) => l.bytes / total >= 0.005).slice(0, 8);
  const rest = total - shown.reduce((t, l) => t + l.bytes, 0);
  const rows = [...shown.map((l) => ({ name: l.name, color: l.color, bytes: l.bytes })), ...(rest > 0 ? [{ name: "Other", color: "var(--text-faint)", bytes: rest }] : [])];
  const pct = (b: number) => {
    const v = (100 * b) / total;
    return v >= 10 ? v.toFixed(0) : v.toFixed(1);
  };
  return (
    <section className="languages">
      <h2>Languages</h2>
      <div className="lang-bar" role="img" aria-label={rows.map((r) => `${r.name} ${pct(r.bytes)}%`).join(", ")}>
        {rows.map((r) => (
          <span key={r.name} style={{ flexGrow: r.bytes, background: r.color }} title={`${r.name} ${pct(r.bytes)}%`} />
        ))}
      </div>
      <ul>
        {rows.map((r) => (
          <li key={r.name}>
            <span className="lang-dot" style={{ background: r.color }} />
            <strong>{r.name}</strong> <span className="muted">{pct(r.bytes)}%</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

/** Right column of the overview: what the project is and how it is doing. */
type Community = { kind: string; label: string; icon: ReactNode; path: string };

/** The files a project shows about itself (GitHub's community files), at the root. */
const COMMUNITY: { kind: string; label: string; icon: ReactNode; re: RegExp }[] = [
  { kind: "license", label: "License", icon: <Scale size={15} />, re: /^(licen[cs]e|copying)(\.(md|txt|rst))?$/i },
  { kind: "security", label: "Security policy", icon: <ShieldCheck size={15} />, re: /^security(\.(md|txt|rst))?$/i },
  { kind: "contributing", label: "Contributing", icon: <Handshake size={15} />, re: /^contributing(\.(md|txt|rst))?$/i },
  { kind: "conduct", label: "Code of conduct", icon: <UsersIcon size={15} />, re: /^code[_-]of[_-]conduct(\.(md|txt|rst))?$/i },
  { kind: "changelog", label: "Changelog", icon: <HistoryIcon size={15} />, re: /^(changelog|changes|history)(\.(md|txt|rst))?$/i },
  { kind: "codeowners", label: "Code owners", icon: <UserCheck size={15} />, re: /^codeowners$/i },
];

function communityFiles(entries: S["TreeEntry"][]): Community[] {
  const out: Community[] = [];
  for (const c of COMMUNITY) {
    const e = entries.find((x) => x.kind === "blob" && c.re.test(x.name));
    if (e) out.push({ kind: c.kind, label: c.label, icon: c.icon, path: e.path });
  }
  return out;
}

/** The common licenses, recognized from the first lines of the file. */
function licenseName(text: string): string | null {
  const t = text.replace(/\s+/g, " ");
  const known: [RegExp, string][] = [
    [/MIT License|Permission is hereby granted, free of charge/i, "MIT license"],
    [/Apache License,? Version 2\.0/i, "Apache-2.0 license"],
    [/GNU AFFERO GENERAL PUBLIC LICENSE Version 3/i, "AGPL-3.0 license"],
    [/GNU LESSER GENERAL PUBLIC LICENSE Version 3/i, "LGPL-3.0 license"],
    [/GNU GENERAL PUBLIC LICENSE Version 3/i, "GPL-3.0 license"],
    [/GNU GENERAL PUBLIC LICENSE Version 2/i, "GPL-2.0 license"],
    [/Mozilla Public License,? (Version|v\.) 2\.0/i, "MPL-2.0 license"],
    [/BSD 3-Clause|Neither the name of/i, "BSD-3-Clause license"],
    [/BSD 2-Clause|Redistribution and use in source and binary forms/i, "BSD-2-Clause license"],
    [/ISC License/i, "ISC license"],
    [/This is free and unencumbered software released into the public domain/i, "Unlicense"],
  ];
  return known.find(([re]) => re.test(t))?.[1] ?? null;
}

function RepoAbout({ ctx }: { ctx: Ctx }) {
  const { p } = ctx;
  const ref = p.default_branch ?? "main";
  const pipeline = useQuery({
    queryKey: ["pipelines", p.id, "latest", ref],
    queryFn: () => api.get<S["PipelineSummary"][]>(`/projects/${p.id}/pipelines${qs({ ref, per_page: 1 })}`),
  });
  const last = pipeline.data?.[0];
  const stats = useQuery({
    queryKey: ["repo-stats", p.id, ref],
    queryFn: () => api.get<S["RepoStats"]>(`/projects/${p.id}/repository/stats${qs({ ref })}`),
    staleTime: 60_000,
  });
  // the entries the file list beside it reads (same keys): one request
  const resolved = useQuery({ queryKey: keys.resolve(p.id, ref), queryFn: () => fetchers.resolve(p.id, ref), staleTime: 5_000 });
  const root = useQuery({
    queryKey: keys.tree(p.id, resolved.data?.commit ?? "", resolved.data?.path ?? ""),
    queryFn: () => fetchers.tree(p.id, resolved.data!.commit, resolved.data!.path),
    enabled: !!resolved.data,
    staleTime: 60_000,
  });
  const files = communityFiles(root.data?.entries ?? []);
  const licenseFile = files.find((f) => f.kind === "license")?.path;
  const license = useQuery({
    queryKey: ["license", p.id, root.data?.commit, licenseFile],
    queryFn: async () => {
      const r = await fetch(`${ctx.web}/-/raw/${encodeURIComponent(root.data!.commit)}/${encPath(licenseFile!)}`, { credentials: "same-origin" });
      return r.ok ? licenseName((await r.text()).slice(0, 4000)) : null;
    },
    enabled: !!licenseFile && !!root.data,
    staleTime: Infinity,
  });
  return (
    <aside className="repo-about" aria-label="About this project">
      <h2>About</h2>
      <p className={p.description ? "" : "muted"}>{p.description || "No description."}</p>
      <ul>
        <li>
          <BookOpen size={15} />
          <a href="#readme">Readme</a>
        </li>
        {files.map((f) => (
          <li key={f.path}>
            {f.icon}
            <Link to={`${ctx.web}/-/blob/${ref}/${f.path}`}>{f.kind === "license" && license.data ? license.data : f.label}</Link>
          </li>
        ))}
        {stats.data && (
          <li>
            <GitCommitHorizontal size={15} />
            <Link to={`${ctx.web}/-/commits/${ref}`}>
              {stats.data.commit_count.toLocaleString()} commit{stats.data.commit_count === 1 ? "" : "s"}
            </Link>
          </li>
        )}
        {last && (
          <li>
            <StatusIcon status={last.status} size={15} />
            <Link to={`${ctx.web}/-/pipelines/${last.iid}`}>
              Pipeline {statusLabel(last.status)} on <code>{ref}</code>
            </Link>
          </li>
        )}
        <li>
          <GitBranch size={15} />
          <span>
            Default branch <code>{ref}</code>
          </span>
        </li>
        <li>
          <CalendarDays size={15} />
          <span>Created {timeAgo(p.created_at)}</span>
        </li>
        {stats.data && (
          <li>
            <HardDrive size={15} />
            <span>
              {bytes(stats.data.repository_bytes)}
              {stats.data.lfs_objects > 0 && ` · LFS ${bytes(stats.data.lfs_bytes)}`}
            </span>
          </li>
        )}
      </ul>
      {stats.data && stats.data.languages.length > 0 && <Languages langs={stats.data.languages} />}
    </aside>
  );
}

export function CloneBox({ p }: { p: S["ProjectDetail"] }) {
  return (
    <div className="stack" style={{ gap: 6, minWidth: 320 }}>
      <CopyField value={p.http_url} />
      <CopyField value={p.ssh_url} />
    </div>
  );
}

function EmptyRepo({ p }: { p: S["ProjectDetail"] }) {
  const branch = p.default_branch ?? "main";
  return (
    <div className="stack">
      <div className="card">
        <div className="card-head">
          <strong className="grow">This repository is empty</strong>
        </div>
        <div className="card-body stack">
          <CloneBox p={p} />
          {p.can_push ? (
            <>
              <p style={{ margin: 0 }}>Push an existing repository:</p>
              <pre className="markdown" style={{ margin: 0 }}>
                <code>{`git remote add origin ${p.ssh_url}\ngit push -u origin ${branch}`}</code>
              </pre>
              <p className="muted small" style={{ margin: 0 }}>
                The default branch <code>{branch}</code> is protected: Maintainers push to it, Developers push other branches.
              </p>
            </>
          ) : (
            <p className="muted">Nothing has been pushed yet.</p>
          )}
        </div>
      </div>
    </div>
  );
}
