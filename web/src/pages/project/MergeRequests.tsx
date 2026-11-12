import { useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, ApiError, qs, timeAgo, type S } from "../../api/client";
import { useSession } from "../../session";
import { AuthorLink, Avatar, Badge, Empty, ErrorBox, Field, FilterTabs, ImportedMark, ListHead, Q, SearchField, UserLink, authorOf, noteAuthor, useDebounced } from "../../components/ui";
import { MarkdownEditor } from "../../components/MarkdownEditor";
import { TemplatePicker } from "../../components/templates";
import { EditLabelsButton, LabelList, LabelPicker, MilestoneChip, SystemText, useLabels } from "../../components/labels";
import { EditPeopleButton, PeopleAvatars, PeopleList, PeoplePicker, ReviewerList } from "../../components/people";
import { Popover, Segmented, Select, Switch } from "../../components/controls";
import { ArrowRight, Check, CheckCircle2, ChevronDown, ChevronRight, Circle, CircleAlert, Clock3, FileDiff, Dot, GitBranch, GitCommitHorizontal, GitMerge, GitPullRequestClosed, MessageSquare, Milestone as MilestoneIcon, FileWarning, Tag, PanelRightClose, PanelRightOpen, Pencil, RotateCcw, X, XCircle } from "lucide-react";
import { fetchers, keys, usePrefetch } from "../../prefetch";
import { StatusIcon, duration, isActive, statusLabel } from "./ci/Status";
import { usePipeline } from "./ci/Pipelines";
import { usePref } from "../../shell/theme";
import { useAct, useToast } from "../../shell/toast";
import { useCiLive, useProjectEvents } from "../../shell/live";
import { PagedDiff, lineKey, type LineKey, type NewComment, type Review } from "./Diff";
import { CommitList, CommitRow, useSignatures } from "./History";
import type { Ctx } from "./Project";
import { MilestoneFact, useMilestones } from "./Milestones";
import { ReactionBar } from "../../components/Reactions";
import { MrTestSummary } from "./ci/Tests";
import { MrSecurity } from "./Security";
import { StackChip, StackMap, RecentBranchBanner, StackOnFact, useStackTargets } from "./MrStack";
import { useBranches } from "./Code";
import { Markdown } from "../../components/Markdown";

/** Invalidate MR queries when the server reports a change (shared SSE stream). */
export function useLiveUpdates(projectId: number) {
  const qc = useQueryClient();
  useProjectEvents(projectId, (ev) => {
    if (ev.type === "merge_request") {
      qc.invalidateQueries({ queryKey: ["mr", projectId, ev.iid] });
      qc.invalidateQueries({ queryKey: ["mrs", projectId] });
    }
    // a push to a source branch arrives as a merge_request event; one to a target
    // branch only changes whether MRs can merge: refresh just their status
    if (ev.type === "push") qc.invalidateQueries({ predicate: (q) => q.queryKey[0] === "mr" && q.queryKey[1] === projectId && q.queryKey[3] === "status" });
  });
}


export function MergeRequestList({ ctx }: { ctx: Ctx }) {
  const [state, setState] = useState<"opened" | "merged" | "closed" | "all">("opened");
  const [search, setSearch] = useState("");
  // a request once typing pauses, not per key
  const term = useDebounced(search, 250);
  const q = useQuery({
    queryKey: ["mrs", ctx.p.id, state, term],
    queryFn: () => api.get<S["MrListItem"][]>(`/projects/${ctx.p.id}/merge_requests${qs({ state, search: term, per_page: 100 })}`),
  });
  const counts = useQuery({
    queryKey: ["mrs", ctx.p.id, "count", term],
    queryFn: () => api.get<S["MrCount"]>(`/projects/${ctx.p.id}/merge_requests_count${qs({ search: term })}`),
  });
  const c = counts.data;
  return (
    <div className="stack list-page">
      <ListHead title="Merge requests">
        {ctx.p.can_push && (
          <Link className="btn primary" to={`${ctx.web}/-/merge_requests/new`}>
            New merge request
          </Link>
        )}
      </ListHead>
      <div className="list-toolbar">
        <FilterTabs
          label="State"
          value={state}
          onChange={setState}
          options={[
            { value: "opened", label: "Open", count: c?.opened },
            { value: "merged", label: "Merged", count: c?.merged },
            { value: "closed", label: "Closed", count: c?.closed },
            { value: "all", label: "All", count: c && c.opened + c.merged + c.closed },
          ]}
        />
        <SearchField value={search} onChange={setSearch} placeholder="Filter by title or branch" />
      </div>
      {state === "opened" && <RecentBranchBanner ctx={ctx} />}
      <Q q={q}>
        {(list) =>
          list.length === 0 ? (
            <Empty>
              {search ? <>No merge request matches “{search}”.</> : state === "opened" ? <>No open merge requests. Push a branch and open one to start a review.</> : <>No {state === "all" ? "" : `${state} `}merge requests.</>}
            </Empty>
          ) : (
            <div className="list rows">
              {list.map((m) => (
                <div className="item mr-row" key={m.id}>
                  <MrStateIcon state={m.state} draft={m.draft} />
                  <div className="grow" style={{ minWidth: 0 }}>
                    <div className="row-title wrap">
                      <MrLink ctx={ctx} m={m} />
                      <StackChip stack={m.stack} />
                      <LabelList labels={m.labels} />
                    </div>
                    <div className="row-meta">
                      <span>!{m.iid}</span>
                      <span>
                        {m.state === "merged" && m.merged_at ? `merged ${timeAgo(m.merged_at)}` : m.state === "closed" && m.closed_at ? `closed ${timeAgo(m.closed_at)}` : `opened ${timeAgo(m.created_at)}`} by <AuthorLink x={m} />
                      </span>
                      {m.milestone_title && (
                        <span>
                          <MilestoneChip web={ctx.web} id={m.milestone_id} title={m.milestone_title} />
                        </span>
                      )}
                    </div>
                  </div>
                  <PeopleAvatars people={m.assignees} label="Assignee" />
                  <PeopleAvatars people={m.reviewers} label="Reviewer" />
                  <span className="mr-row-route hide-narrow" title={`${m.source_branch} → ${m.target_branch}`}>
                    <span className="ref-chip small">{m.source_branch}</span>
                    <ArrowRight size={13} className="faint" />
                    <span className="ref-chip small">{m.target_branch}</span>
                  </span>
                  <span className="row-time" title={new Date(m.updated_at).toLocaleString()}>
                    {timeAgo(m.updated_at)}
                  </span>
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}

function MrStateIcon({ state, draft }: { state: string; draft: boolean }) {
  const label = draft && state === "opened" ? "Draft" : state === "opened" ? "Open" : state === "merged" ? "Merged" : "Closed";
  const cls = state === "opened" ? (draft ? "faint" : "ic-ok") : state === "merged" ? "ic-accent" : "ic-danger";
  return (
    <span className={`row-icon ${cls}`} title={label} aria-label={label} role="img">
      {state === "closed" ? <GitPullRequestClosed size={16} /> : <GitMerge size={16} />}
    </span>
  );
}

function MrLink({ ctx, m }: { ctx: Ctx; m: S["MergeRequest"] }) {
  const pf = usePrefetch({ kind: "mr", pid: ctx.p.id, iid: m.iid });
  return (
    <Link to={`${ctx.web}/-/merge_requests/${m.iid}`} className="row-title" {...pf}>
      {m.draft && <span className="draft-tag">Draft</span>}
      <span className="ellipsis">{m.title}</span>
    </Link>
  );
}

export function NewMergeRequest({ ctx }: { ctx: Ctx }) {
  const nav = useNavigate();
  const [params] = useSearchParams();
  const branches = useBranches(ctx);
  const [source, setSource] = useState(params.get("source") ?? "");
  const [target, setTarget] = useState(params.get("target") ?? ctx.p.default_branch ?? "");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [draft, setDraft] = useState(false);
  const [squash, setSquash] = useState(false);
  const [removeSource, setRemoveSource] = useState(true);
  const [stackOn, setStackOn] = useState(params.get("stack_on") ?? "");
  const [error, setError] = useState<unknown>(null);
  const names = (branches.data ?? []).map((b) => b.name);
  const stackTargets = useStackTargets(ctx).filter((t) => t.branch !== source);
  const below = stackTargets.find((t) => String(t.iid) === stackOn);
  useEffect(() => {
    if (below) setTarget(below.branch);
  }, [below?.branch]);
  useEffect(() => {
    if (!source && names.length) setSource(names.find((n) => n !== target) ?? "");
  }, [names.length]);
  useEffect(() => {
    if (!title && source) setTitle(source.replace(/^.*\//, "").replace(/[-_]/g, " ").replace(/^./, (c) => c.toUpperCase()));
  }, [source]);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    try {
      const m = await api.post<S["MergeRequest"]>(`/projects/${ctx.p.id}/merge_requests`, {
        source_branch: source,
        target_branch: target,
        title,
        description,
        draft,
        squash,
        remove_source_branch: removeSource,
        stack_on_iid: below ? below.iid : undefined,
      });
      nav(`${ctx.web}/-/merge_requests/${m.iid}`);
    } catch (err) {
      setError(err);
    }
  };
  return (
    <div className="stack list-page">
      <ListHead title="New merge request" sub="Propose merging the changes of one branch into another." />
      <form className="stack" style={{ maxWidth: 760 }} onSubmit={submit}>
        <ErrorBox error={error} />
        <div className="grid2">
          <Field label="Source branch">
            <Select value={source} onChange={setSource} placeholder="Choose a branch" options={names.map((n) => ({ value: n, label: n }))} />
          </Field>
          <Field label="Target branch">
            <Select value={target} onChange={setTarget} disabled={!!below} options={names.map((n) => ({ value: n, label: n }))} />
          </Field>
        </div>
        {stackTargets.length > 0 && (
          <Field label="Stack on" hint="Build on another open merge request: this one targets its branch and merges after it.">
            <Select
              value={stackOn}
              onChange={setStackOn}
              options={[{ value: "", label: "Nothing (a standalone merge request)" }, ...stackTargets.map((t) => ({ value: String(t.iid), label: `!${t.iid} ${t.title} (${t.branch})` }))]}
            />
          </Field>
        )}
        <Field label="Title">
          <input value={title} onChange={(e) => setTitle(e.target.value)} />
        </Field>
        <Field label="Description" hint="Markdown">
          <div className="stack" style={{ gap: 6 }}>
            <TemplatePicker projectId={ctx.p.id} kind="merge_request" description={description} setDescription={setDescription} />
            <MarkdownEditor projectId={ctx.p.id} rows={8} value={description} onChange={setDescription} label="Description" />
          </div>
        </Field>
        <div className="row wrap" style={{ gap: 20 }}>
          <label className="check">
            <input type="checkbox" checked={draft} onChange={(e) => setDraft(e.target.checked)} />
            Draft (cannot be merged yet)
          </label>
          <label className="check">
            <input type="checkbox" checked={squash} onChange={(e) => setSquash(e.target.checked)} />
            Squash commits when merging
          </label>
          <label className="check">
            <input type="checkbox" checked={removeSource} onChange={(e) => setRemoveSource(e.target.checked)} />
            Delete the source branch when merged
          </label>
        </div>
        <div>
          <button className="btn primary" disabled={!source || !target || source === target || !title}>
            Create merge request
          </button>
        </div>
      </form>
      {source && target && source !== target && <ChangesPreview ctx={ctx} from={target} to={source} />}
    </div>
  );
}

/** What a new merge request would bring: its commits and its diff. */
function ChangesPreview({ ctx, from, to }: { ctx: Ctx; from: string; to: string }) {
  const q = useQuery({
    queryKey: ["compare", ctx.p.id, from, to],
    queryFn: () => api.get<S["CompareResponse"]>(`${ctx.base}/compare${qs({ from, to, limit: 1 })}`),
  });
  return (
    <section className="stack new-mr-preview">
      <h2 className="section-title">Changes</h2>
      <Q q={q}>
        {(c) =>
          c.commits.length === 0 ? (
            <Empty>
              <code>{to}</code> has nothing that is not already in <code>{from}</code>.
            </Empty>
          ) : (
            <div className="stack">
              <CommitList ctx={ctx} list={c.commits} />
              <PagedDiff url={(offset) => `${ctx.base}/compare${qs({ from, to, offset })}`} pick={(v) => (v as S["CompareResponse"]).diff} />
            </div>
          )
        }
      </Q>
    </section>
  );
}

type Tab = "overview" | "commits" | "diffs";

const STATE_LABEL: Record<string, string> = { opened: "Open", merged: "Merged", closed: "Closed" };

export function MergeRequestPage({ ctx, iid, tab }: { ctx: Ctx; iid: number; tab: Tab }) {
  useLiveUpdates(ctx.p.id);
  const q = useQuery({ queryKey: keys.mr(ctx.p.id, iid), queryFn: () => fetchers.mr(ctx.p.id, iid) });
  const base = `${ctx.web}/-/merge_requests/${iid}`;
  const [panel, setPanel] = usePref("mr.panel", true);
  const [editing, setEditing] = useState(false);
  return (
    <Q q={q}>
      {(m) => {
        const v = m.versions[0];
        const canEdit = m.can_edit && m.state !== "merged";
        const aside = panel && tab === "overview";
        return (
          <div className="stack" style={{ gap: 0 }}>
            <header className="mr-head">
              <div className="mr-status">
                <span className={`mr-state ${m.draft && m.state === "opened" ? "draft" : m.state}`}>{m.draft && m.state === "opened" ? "Draft" : (STATE_LABEL[m.state] ?? m.state)}</span>
                <span>!{m.iid}</span>
                <span aria-hidden="true">·</span>
                <span>
                  <AuthorLink x={m} /> opened {timeAgo(m.created_at)}
                </span>
                {m.updated_at !== m.created_at && (
                  <>
                    <span aria-hidden="true">·</span>
                    <span>updated {timeAgo(m.updated_at)}</span>
                  </>
                )}
              </div>
              <div className="row" style={{ alignItems: "flex-start" }}>
                <h1 className="mr-title grow">{m.title}</h1>
                {canEdit && !editing && (
                  <button
                    className="icon-btn bordered"
                    aria-label="Edit title and description"
                    title="Edit title and description"
                    onClick={() => {
                      setEditing(true);
                      if (tab !== "overview") window.history.pushState(null, "", base);
                    }}
                  >
                    <Pencil size={15} />
                  </button>
                )}
              </div>
              <div className="mr-route">
                <Link className="ref-chip" to={`${ctx.web}/-/tree/${m.source_branch}`}>
                  <GitBranch size={13} />
                  {m.source_branch}
                </Link>
                <span className="mr-route-arrow">
                  <span className="line" />
                  {v ? `${v.commits_count} commit${v.commits_count === 1 ? "" : "s"}` : ""}
                  <ArrowRight size={15} />
                </span>
                <Link className="ref-chip target" to={`${ctx.web}/-/tree/${m.target_branch}`}>
                  <GitBranch size={13} />
                  {m.target_branch}
                </Link>
              </div>
            </header>
            <nav className="tabs mr-tabs">
              <Link to={base} className={tab === "overview" ? "active" : ""}>
                Overview
              </Link>
              <Link to={`${base}/commits`} className={tab === "commits" ? "active" : ""}>
                Commits <span className="tab-count">{v?.commits_count ?? 0}</span>
              </Link>
              <Link to={`${base}/diffs`} className={tab === "diffs" ? "active" : ""}>
                Changes <span className="tab-count">{v?.files_count ?? 0}</span>
                {v && <DiffBar add={v.additions} del={v.deletions} />}
              </Link>
              <span className="grow" />
              {tab === "overview" && (
                <button className="icon-btn" onClick={() => setPanel(!panel)} aria-pressed={panel} title={panel ? "Hide details panel" : "Show details panel"} aria-label="Toggle details panel">
                  {panel ? <PanelRightClose size={16} /> : <PanelRightOpen size={16} />}
                </button>
              )}
            </nav>
            {/* readiness and details belong to the overview; commits and changes get the full width */}
            <div className={`mr-layout ${aside ? "with-panel" : ""} ${tab === "diffs" ? "wide" : ""}`}>
              <div className="mr-main">
                {tab === "overview" && <Overview ctx={ctx} m={m} editing={editing && canEdit} setEditing={setEditing} />}
                {tab === "commits" && <MrCommits ctx={ctx} iid={iid} />}
                {tab === "diffs" && <Changes ctx={ctx} m={m} />}
              </div>
              {aside && <MrAside ctx={ctx} m={m} />}
            </div>
          </div>
        );
      }}
    </Q>
  );
}

/** Five squares, green for additions and red for deletions, in proportion. */
function DiffBar({ add, del }: { add: number; del: number }) {
  const total = add + del;
  const greens = total === 0 ? 0 : Math.max(add > 0 ? 1 : 0, Math.min(del > 0 ? 4 : 5, Math.round((5 * add) / total)));
  const reds = total === 0 ? 0 : 5 - greens;
  return (
    <span className="diff-bar" role="img" aria-label={`${add} additions, ${del} deletions`} title={`+${add} −${del}`}>
      {Array.from({ length: 5 }, (_, i) => (
        <span key={i} className={i < greens ? "add" : i < greens + reds ? "del" : ""} />
      ))}
    </span>
  );
}

function useNotes(ctx: Ctx, iid: number, version?: number) {
  return useQuery({
    queryKey: ["mr", ctx.p.id, iid, "notes", version ?? "latest"],
    queryFn: () => api.get<S["NoteDto"][]>(`/projects/${ctx.p.id}/merge_requests/${iid}/notes${qs({ version })}`),
  });
}

/** Pipelines of this MR (MR and branch pipelines), newest first. */
function useMrPipelines(ctx: Ctx, m: S["MrDetail"]) {
  useCiLive(ctx.p.id);
  return useQuery({
    queryKey: ["mr-pipelines", ctx.p.id, m.iid],
    queryFn: () => api.get<S["PipelineSummary"][]>(`/projects/${ctx.p.id}/merge_requests/${m.iid}/pipelines`),
  });
}

function Overview({ ctx, m, editing, setEditing }: { ctx: Ctx; m: S["MrDetail"]; editing: boolean; setEditing: (e: boolean) => void }) {
  const notes = useNotes(ctx, m.iid);
  const qc = useQueryClient();
  const refresh = () => qc.invalidateQueries({ queryKey: ["mr", ctx.p.id, m.iid] });
  const path = `/projects/${ctx.p.id}/merge_requests/${m.iid}`;
  return (
    <div className="stack" style={{ gap: 28 }}>
      <StackMap ctx={ctx} m={m} />
      {editing ? (
        <EditMr
          projectId={ctx.p.id}
          m={m}
          path={path}
          done={() => {
            setEditing(false);
            refresh();
          }}
        />
      ) : (
        <section className="mr-description markdown" aria-label="Description">
          {m.description ? <Markdown html={m.description_html} /> : <p className="muted">No description.</p>}
        </section>
      )}
      <ReactionBar base={path} />
      <MrTestSummary ctx={ctx} iid={m.iid} />
      <MrSecurity ctx={ctx} iid={m.iid} />
      <Q q={notes}>{(list) => <Activity ctx={ctx} m={m} notes={list} />}</Q>
    </div>
  );
}

/** Right column: merge readiness (open MRs) or outcome, then facts and actions. */
function MrAside({ ctx, m }: { ctx: Ctx; m: S["MrDetail"] }) {
  const qc = useQueryClient();
  const path = `/projects/${ctx.p.id}/merge_requests/${m.iid}`;
  const act = useAct(() => {
    qc.invalidateQueries({ queryKey: ["mr", ctx.p.id, m.iid] });
    qc.invalidateQueries({ queryKey: ["mrs", ctx.p.id] });
  });
  return (
    <aside className="mr-aside" aria-label="Merge request details">
      {m.state === "opened" ? (
        <Readiness ctx={ctx} m={m} />
      ) : (
        <section className="mr-card">
          <h2 className="mr-card-title">{m.state === "merged" ? "Merged" : "Closed"}</h2>
          <p className="small muted" style={{ margin: "6px 0 0" }}>
            {m.state === "merged" ? (
              <>
                into <code>{m.target_branch}</code>
                {m.merge_commit_sha && (
                  <>
                    {" "}
                    as <Link to={`${ctx.web}/-/commit/${m.merge_commit_sha}`}>{m.merge_commit_sha.slice(0, 8)}</Link>
                  </>
                )}
                {m.merged_at && ` · ${timeAgo(m.merged_at)}`}
              </>
            ) : (
              m.closed_at && `${timeAgo(m.closed_at)}`
            )}
          </p>
        </section>
      )}
      <section className="mr-facts">
        <div className="fact">
          <span className="fact-label">Approvals</span>
          <div className="fact-value">
            {m.approvals.length === 0 ? (
              <span className="muted">None yet</span>
            ) : (
              <span className="avatars">
                {m.approvals.map((a) => (
                  <Avatar key={a.user_id} name={a.name || a.username} username={a.username} ok title={`${a.name || a.username} approved ${timeAgo(a.created_at)}`} />
                ))}
              </span>
            )}
            {m.can_approve &&
              (m.approved_by_me ? (
                <button className="btn small" onClick={() => act(() => api.post(`${path}/unapprove`), "Approval revoked")}>
                  Revoke
                </button>
              ) : (
                <button className="btn small primary" onClick={() => act(() => api.post(`${path}/approve`), "Approved")}>
                  Approve
                </button>
              ))}
            {m.can_approve && <RequestChanges onSubmit={async (body) => void (await act(() => api.post(`${path}/request_changes`, { body }), "Changes requested"))} />}
          </div>
        </div>
        <div className="fact">
          <span className="fact-label">Assignees</span>
          <div className="fact-value column">
            <PeopleList people={m.assignees} />
            {m.can_edit && m.state !== "merged" && (
              <PeoplePicker
                projectId={ctx.p.id}
                label="Assignees"
                selected={m.assignees}
                onChange={(ids) => act(() => api.put(path, { assignee_ids: ids }), "Assignees updated")}
                trigger={<EditPeopleButton />}
              />
            )}
          </div>
        </div>
        <div className="fact">
          <span className="fact-label">Reviewers</span>
          <div className="fact-value column">
            <ReviewerList
              reviewers={m.reviewers}
              onRerequest={m.can_edit && m.state === "opened" ? (r) => act(() => api.post(`${path}/reviewers/${r.id}/rerequest`), `Asked ${r.username} to review again`) : undefined}
            />
            {m.can_edit && m.state !== "merged" && (
              <PeoplePicker
                projectId={ctx.p.id}
                label="Reviewers"
                selected={m.reviewers}
                onChange={(ids) => act(() => api.put(path, { reviewer_ids: ids }), "Reviewers updated")}
                trigger={<EditPeopleButton />}
              />
            )}
          </div>
        </div>
        <CodeOwnersFact
          ctx={ctx}
          iid={m.iid}
          canEdit={m.can_edit && m.state === "opened"}
          onAsk={(ids) => act(() => api.put(path, { reviewer_ids: [...m.reviewers.map((r) => r.id), ...ids] }), "Code owners asked to review")}
        />
        <div className="fact">
          <span className="fact-label">Versions</span>
          <div className="fact-value column">
            {m.versions.map((v) => (
              <span key={v.id} className="small">
                v{v.version} · <code>{v.head_sha.slice(0, 8)}</code> <span className="faint">{timeAgo(v.created_at)}</span>
              </span>
            ))}
          </div>
        </div>
        <div className="fact">
          <span className="fact-label">Labels</span>
          <div className="fact-value column">
            {m.labels.length === 0 ? <span className="muted">None</span> : <LabelList labels={m.labels} />}
            {ctx.p.can_push && m.state !== "merged" && (
              <LabelPicker
                projectId={ctx.p.id}
                manage={`${ctx.web}/-/labels`}
                value={m.labels.map((l) => l.name)}
                onChange={(names) => act(() => api.put(`${path}/labels`, { labels: names }), "Labels updated")}
                trigger={<EditLabelsButton />}
              />
            )}
          </div>
        </div>
        <MilestoneFact
          ctx={ctx}
          current={m.milestone_id}
          title={m.milestone_title}
          canEdit={ctx.p.can_push && m.state !== "merged"}
          onChange={(id) => act(() => api.put(path, { milestone_id: id }), "Milestone updated")}
        />
        <StackOnFact ctx={ctx} m={m} />
        <div className="fact">
          <span className="fact-label">Author</span>
          <div className="fact-value">
            <Avatar name={authorOf(m)} username={m.author_username} />
            <AuthorLink x={m} />
            <ImportedMark x={m} />
          </div>
        </div>
        {m.can_edit && m.state !== "merged" && (
          <div className="fact">
            {m.state === "opened" ? (
              <button className="btn small" onClick={() => act(() => api.post(`${path}/close`), "Merge request closed")}>
                Close merge request
              </button>
            ) : (
              <button className="btn small" onClick={() => act(() => api.post(`${path}/reopen`), "Merge request reopened")}>
                Reopen
              </button>
            )}
          </div>
        )}
      </section>
    </aside>
  );
}

/** "Request changes" asks what to change: the comment goes on the timeline
 *  with the verdict, and the reviewer's state links to it. */
function RequestChanges({ onSubmit }: { onSubmit: (body: string) => Promise<void> }) {
  const [open, setOpen] = useState(false);
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);
  return (
    <Popover
      open={open}
      onOpenChange={setOpen}
      label="Request changes"
      trigger={
        <button className="btn small" type="button">
          Request changes
        </button>
      }
    >
      <form
        className="request-changes"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          await onSubmit(body.trim());
          setBusy(false);
          setBody("");
          setOpen(false);
        }}
      >
        <strong>Request changes</strong>
        <p className="small muted">Say what to change. Merging waits until you approve, or are asked to review again.</p>
        <textarea autoFocus rows={5} value={body} onChange={(e) => setBody(e.target.value)} placeholder="What should change?" aria-label="What should change" />
        <div className="row" style={{ justifyContent: "flex-end" }}>
          <button type="button" className="btn small" onClick={() => setOpen(false)}>
            Cancel
          </button>
          <button className="btn small danger" disabled={!body.trim() || busy}>
            Request changes
          </button>
        </div>
      </form>
    </Popover>
  );
}

function EditMr({ projectId, m, path, done }: { projectId: number; m: S["MrDetail"]; path: string; done: () => void }) {
  const [title, setTitle] = useState(m.title);
  const [description, setDescription] = useState(m.description);
  const [draft, setDraft] = useState(m.draft);
  const [removeSource, setRemoveSource] = useState(m.remove_source_branch);
  const [error, setError] = useState<unknown>(null);
  return (
    <form
      className="mr-card stack"
      onSubmit={async (e) => {
        e.preventDefault();
        try {
          await api.put(path, { title, description, draft, remove_source_branch: removeSource });
          done();
        } catch (err) {
          setError(err);
        }
      }}
    >
      <ErrorBox error={error} />
      <Field label="Title">
        <input value={title} onChange={(e) => setTitle(e.target.value)} />
      </Field>
      <Field label="Description" hint="Markdown">
        <MarkdownEditor projectId={projectId} rows={8} value={description} onChange={setDescription} label="Description" />
      </Field>
      <Switch checked={draft} onChange={setDraft}>
        Draft (cannot be merged)
      </Switch>
      <Switch checked={removeSource} onChange={setRemoveSource}>
        Delete the source branch when merged
      </Switch>
      <div className="row">
        <button className="btn primary">Save</button>
        <button type="button" className="btn" onClick={done}>
          Cancel
        </button>
      </div>
    </form>
  );
}

type Check = { key: string; tone: "ok" | "warn" | "bad" | "none"; text: ReactNode; action?: ReactNode };

const METHODS: { value: S["MergeMethod"]; label: string }[] = [
  { value: "merge", label: "Merge" },
  { value: "squash", label: "Squash" },
  { value: "rebase", label: "Rebase" },
  { value: "fast_forward", label: "FF" },
];

/** What stands between this MR and a merge, and the merge itself. */
function Readiness({ ctx, m }: { ctx: Ctx; m: S["MrDetail"] }) {
  const qc = useQueryClient();
  const toast = useToast();
  const path = `/projects/${ctx.p.id}/merge_requests/${m.iid}`;
  const st = useQuery({ queryKey: ["mr", ctx.p.id, m.iid, "status"], queryFn: () => api.get<S["MergeStatus"]>(`${path}/merge_status`) });
  const pipelines = useMrPipelines(ctx, m);
  const [method, setMethod] = useState<S["MergeMethod"]>(m.squash ? "squash" : "merge");
  const [removeSource, setRemoveSource] = useState(m.remove_source_branch);
  const [busy, setBusy] = useState(false);
  const act = useAct(
    () => {
      qc.invalidateQueries({ queryKey: ["mr", ctx.p.id, m.iid] });
      qc.invalidateQueries({ queryKey: ["mrs", ctx.p.id] });
    },
    { always: true },
  );
  const run = async (f: () => Promise<unknown>, done: string) => {
    setBusy(true);
    await act(f, done);
    setBusy(false);
  };
  return (
    <section className="mr-card readiness">
      <Q q={st}>
        {(s) => {
          const p = pipelines.data?.[0];
          const checks: Check[] = [];
          const pstatus = s.pipeline_status ?? p?.status;
          if (pstatus || s.pipeline_required) {
            const ok = pstatus === "success";
            const running = pstatus != null && isActive(pstatus);
            checks.push({
              key: "pipeline",
              tone: ok ? "ok" : running ? "warn" : s.pipeline_required ? "bad" : "warn",
              text: pstatus ? `Pipeline${p ? ` #${p.iid}` : ""} ${statusLabel(pstatus)}` : "No pipeline for the latest commit",
              action: p && (
                <Link className="small" to={`${ctx.web}/-/pipelines/${p.iid}`}>
                  Checks
                </Link>
              ),
            });
          }
          if (s.approval_rules.length === 0) {
            checks.push({ key: "approvals", tone: "none", text: m.approvals.length ? `Approved by ${m.approvals.map((a) => a.username).join(", ")}` : "No approval required" });
          }
          for (const r of s.approval_rules) {
            checks.push({
              key: `rule:${r.name}`,
              tone: r.satisfied ? "ok" : "bad",
              text: r.satisfied ? `${r.name} approved` : `${r.name}: ${r.required - r.given} more approval${r.required - r.given === 1 ? "" : "s"}`,
              action: (
                <span className="mono small faint">
                  {r.given}/{r.required}
                </span>
              ),
            });
          }
          checks.push(
            s.unresolved_threads > 0
              ? { key: "threads", tone: "warn", text: `${s.unresolved_threads} open thread${s.unresolved_threads === 1 ? "" : "s"}` }
              : { key: "threads", tone: "ok", text: "All threads resolved" },
          );
          const canRebase = s.user_can_merge && !s.fast_forward && s.conflicts.length === 0;
          checks.push(
            s.conflicts.length > 0
              ? { key: "conflicts", tone: "bad", text: <>Conflicts in {s.conflicts.map((c) => <code key={c}>{c} </code>)}</> }
              : {
                  key: "conflicts",
                  tone: "ok",
                  text: `No conflicts with ${m.target_branch}`,
                  action: canRebase && (
                    <button className="link-btn small" disabled={busy} onClick={() => run(() => api.post(`${path}/rebase`), `Rebased onto ${m.target_branch}`)}>
                      Rebase
                    </button>
                  ),
                },
          );
          // blockers the checklist does not already show
          const other = s.blockers.filter((b) => !/^(there are merge conflicts|approval rule|the pipeline must succeed)/.test(b));
          const scored = checks.filter((c) => c.tone !== "none");
          const passed = scored.filter((c) => c.tone === "ok").length;
          return (
            <>
              <div className="readiness-head">
                <h2 className="mr-card-title">{s.can_merge ? "Ready to merge" : "Not ready yet"}</h2>
                <span className="mono small muted">
                  {passed} / {scored.length}
                </span>
              </div>
              <div className="meter" role="img" aria-label={`${passed} of ${scored.length} checks pass`}>
                {scored.map((c) => (
                  <span key={c.key} className={c.tone} />
                ))}
              </div>
              <ul className="checks">
                {checks.map((c) => (
                  <li key={c.key} className={c.tone}>
                    <CheckIcon tone={c.tone} />
                    <span className="grow">{c.text}</span>
                    {c.action}
                  </li>
                ))}
              </ul>
              {p && <MrJobs ctx={ctx} iid={p.iid} />}
              {other.length > 0 && (
                <ul className="blockers">
                  {other.map((b) => (
                    <li key={b}>{b[0].toUpperCase() + b.slice(1)}</li>
                  ))}
                </ul>
              )}
              {m.auto_merge_user ? (
                <div className="auto-merge-on">
                  <Clock3 size={15} />
                  <span className="grow">
                    Set to auto-merge by <strong>{m.auto_merge_user.name || m.auto_merge_user.username}</strong>: merges into <code>{m.target_branch}</code> when every check passes
                    {m.auto_merge_method && m.auto_merge_method !== "merge" ? ` (${m.auto_merge_method.replace("_", "-")})` : ""}.
                  </span>
                  {(s.user_can_merge || m.can_edit) && (
                    <button className="btn small" disabled={busy} onClick={() => run(() => api.del(`${path}/auto_merge`), "Auto-merge canceled")}>
                      Cancel
                    </button>
                  )}
                </div>
              ) : s.train_position != null ? (
                <div className="auto-merge-on">
                  <Clock3 size={15} />
                  <span className="grow">
                    {s.train_position === 1 ? (
                      <>Merges next: first on the <code>{m.target_branch}</code> merge train.</>
                    ) : (
                      <>
                        Number {s.train_position} on the <code>{m.target_branch}</code> merge train: tested on top of the {s.train_position - 1} ahead of it.
                      </>
                    )}
                  </span>
                  {(s.user_can_merge || m.can_edit) && (
                    <button className="btn small" disabled={busy} onClick={() => run(() => api.del(`${path}/merge_train`), "Removed from the merge train")}>
                      Remove
                    </button>
                  )}
                </div>
              ) : s.user_can_merge && s.merge_trains_enabled ? (
                <div className="merge-box">
                  {/* the train tests the very commit that lands: a merge commit, one squashed commit,
                      or the MR's commits rebased onto what is ahead (rebase and fast-forward alike) */}
                  <span className="fact-label">Method</span>
                  <Segmented
                    label="Merge method"
                    value={method === "fast_forward" ? "rebase" : method}
                    onChange={setMethod}
                    options={METHODS.filter((o) => o.value !== "fast_forward")}
                  />
                  <Switch checked={removeSource} onChange={setRemoveSource}>
                    Delete <code>{m.source_branch}</code> after
                  </Switch>
                  {(() => {
                    // as the server: the train runs the pipeline, everything else must be ready
                    const blockers = s.blockers.filter((b) => !b.startsWith("the pipeline must succeed"));
                    const broken = pstatus === "failed" || pstatus === "canceled";
                    const why = broken
                      ? `The pipeline ${pstatus}: push a fix first.`
                      : blockers.length > 0
                        ? `Not ready: ${blockers.join("; ")}.`
                        : null;
                    return (
                      <>
                        <button
                          className="btn primary block"
                          disabled={busy || why != null}
                          onClick={() => run(
                              () =>
                                api.post(`${path}/merge_train`, {
                                  squash: method === "squash",
                                  rebase: method === "rebase" || method === "fast_forward",
                                  remove_source_branch: removeSource,
                                }),
                              "Added to the merge train",
                            )}
                          title="Tests this merge request on top of the ones queued ahead of it, then merges it in turn"
                        >
                          <Clock3 size={15} /> {busy ? "Working…" : "Add to merge train"}
                        </button>
                        {why && <p className="small muted train-why">{why}</p>}
                      </>
                    );
                  })()}
                </div>
              ) : s.user_can_merge && (
                <div className="merge-box">
                  <span className="fact-label">Method</span>
                  <Segmented
                    label="Merge method"
                    value={method}
                    onChange={setMethod}
                    options={METHODS.map((o) => (o.value === "fast_forward" && !s.fast_forward ? { ...o, disabled: true, hint: `Rebase first: ${m.target_branch} has moved` } : o))}
                  />
                  <Switch checked={removeSource} onChange={setRemoveSource}>
                    Delete <code>{m.source_branch}</code> after
                  </Switch>
                  {(() => {
                    const ffBlocked = method === "fast_forward" && !s.fast_forward;
                    const waiting = pstatus != null && isActive(pstatus);
                    const merge = () => run(() => api.post(`${path}/merge`, { method, sha: s.head_sha, remove_source_branch: removeSource }), `Merged into ${m.target_branch}`);
                    // waiting for a pipeline, approvals or threads: merge once they pass
                    const auto = () =>
                      run(async () => {
                        const r = await api.post<S["AutoMergeResult"]>(`${path}/auto_merge`, { method, sha: s.head_sha, remove_source_branch: removeSource });
                        if (r.outcome === "merged") toast.ok(`Merged into ${m.target_branch}`);
                      }, "Set to auto-merge");
                    if (s.can_merge && !waiting) {
                      return (
                        <button className="btn primary block" disabled={busy || ffBlocked} onClick={merge}>
                          {busy ? "Working…" : `Merge into ${m.target_branch}`}
                        </button>
                      );
                    }
                    return (
                      <>
                        <button className="btn primary block" disabled={busy || ffBlocked || s.conflicts.length > 0 || m.draft} onClick={auto} title="Merges as you once every check passes; new commits or a failed pipeline cancel it">
                          <Clock3 size={15} /> {busy ? "Working…" : "Set to auto-merge"}
                        </button>
                        {s.can_merge && (
                          <button className="link-btn small" disabled={busy || ffBlocked} onClick={merge}>
                            Merge now, without waiting for the pipeline
                          </button>
                        )}
                      </>
                    );
                  })()}
                </div>
              )}
            </>
          );
        }}
      </Q>
    </section>
  );
}

/** The latest pipeline's jobs at a glance (name, status, duration), like
 *  GitHub's checks: open when something failed or still runs. */
function MrJobs({ ctx, iid }: { ctx: Ctx; iid: number }) {
  const q = usePipeline(ctx, iid);
  const [open, setOpen] = useState<boolean | null>(null);
  const jobs = (q.data?.jobs ?? []).filter((j) => !j.retried).sort((a, b) => a.stage_idx - b.stage_idx || a.id - b.id);
  if (jobs.length === 0) return null;
  const count = (f: (s: string) => boolean) => jobs.filter((j) => f(j.status)).length;
  const failed = count((s) => s === "failed");
  const running = count((s) => isActive(s));
  const parts = [
    [count((s) => s === "success"), "passed"],
    [failed, "failed"],
    [running, "running"],
    [count((s) => s === "manual"), "manual"],
    [count((s) => s === "skipped" || s === "canceled"), "skipped"],
  ].filter(([n]) => n) as [number, string][];
  const shown = open ?? (failed > 0 || running > 0);
  return (
    <div className="mr-jobs">
      <button type="button" className="mr-jobs-toggle" aria-expanded={shown} onClick={() => setOpen(!shown)}>
        {shown ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        <span className="grow">
          {jobs.length} job{jobs.length === 1 ? "" : "s"}
          <span className="muted"> · {parts.map(([n, w]) => `${n} ${w}`).join(", ")}</span>
        </span>
      </button>
      {shown && (
        <ul className="mr-jobs-list">
          {jobs.map((j) => (
            <li key={j.id}>
              <StatusIcon status={j.status} size={14} />
              <Link className="grow ellipsis" to={j.bridge && j.downstream ? `/${j.downstream.project_path}/-/pipelines/${j.downstream.iid}` : `${ctx.web}/-/jobs/${j.id}`} title={`${j.name}: ${statusLabel(j.status)}`}>
                {j.name}
              </Link>
              <span className="faint small nowrap hide-narrow">{j.stage}</span>
              <span className="mono small muted nowrap">{j.duration != null ? duration(j.duration) : isActive(j.status) ? "…" : ""}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function CheckIcon({ tone }: { tone: Check["tone"] }) {
  if (tone === "ok") return <CheckCircle2 size={16} className="check-icon ok" aria-label="passes" />;
  if (tone === "bad") return <XCircle size={16} className="check-icon bad" aria-label="blocks the merge" />;
  if (tone === "warn") return <CircleAlert size={16} className="check-icon warn" aria-label="needs attention" />;
  return <Circle size={16} className="check-icon none" aria-hidden="true" />;
}

/**
 * A comment with its replies. Code threads (`boxed`) are framed, with their
 * file and line, open/resolved state and a resolve control.
 */
function Thread({ ctx, m, root, replies, boxed }: { ctx: Ctx; m: S["MrDetail"]; root: S["NoteDto"]; replies: S["NoteDto"][]; boxed?: boolean }) {
  const qc = useQueryClient();
  const toast = useToast();
  const { user } = useSession();
  const [reply, setReply] = useState("");
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const path = `/projects/${ctx.p.id}/merge_requests/${m.iid}`;
  const refresh = () => qc.invalidateQueries({ queryKey: ["mr", ctx.p.id, m.iid] });
  const send = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api.post(`${path}/notes`, { body: reply, parent_id: root.id });
      setReply("");
      setOpen(false);
      refresh();
    } catch (err) {
      setError(err);
    }
  };
  const resolvable = root.new_line != null || root.old_line != null;
  const file = root.position?.new_path ?? root.new_path;
  const line = root.position?.new_line ?? root.new_line ?? root.old_line;
  // a comment on several lines (numbers as written, on the version it was made on)
  const start = root.start_new_line ?? root.start_old_line;
  const end = root.start_new_line != null ? root.new_line : root.old_line;
  const span = start != null && end != null && start !== end ? `lines ${start}–${end}` : null;
  return (
    <div id={`note_${root.id}`} className={`thread ${boxed ? "boxed" : "plain"} ${root.resolved_at ? "resolved" : ""} ${root.review ? "review" : ""}`}>
      {root.review === "requested_changes" && (
        <div className="thread-review">
          <FileWarning size={13} /> Requested changes
        </div>
      )}
      {!boxed && span && <div className="thread-span small muted">On {span}</div>}
      {boxed && (
        <div className="thread-head">
          <span className={`thread-state ${root.resolved_at ? "resolved" : "open"}`}>{root.resolved_at ? "Resolved" : "Open thread"}</span>
          {file && (
            <Link className="mono small" to={`${ctx.web}/-/merge_requests/${m.iid}/diffs`}>
              {file}:{span ? span.replace("lines ", "") : line}
            </Link>
          )}
          {root.outdated && <Badge>outdated</Badge>}
          <span className="grow" />
          {resolvable && (
            <button
              className="btn small"
              onClick={async () => {
                await api.post(`${path}/notes/${root.id}/resolve`, { resolved: !root.resolved_at }).catch(setError);
                refresh();
              }}
            >
              {root.resolved_at ? "Unresolve" : "Resolve thread"}
            </button>
          )}
        </div>
      )}
      <div className="thread-notes">
        {[root, ...replies].map((n) => (
          <div key={n.id} className="note">
            {(boxed || n.id !== root.id) && <Avatar name={noteAuthor(n)} username={n.author_username} />}
            <div className="grow" style={{ minWidth: 0 }}>
              <div className="note-meta">
                <strong>
                  <AuthorLink x={n} name />
                </strong>{" "}
                <span className="faint">{timeAgo(n.created_at)}</span>
                <ImportedMark x={n} />
                {user?.id === n.author_id && (
                  <button
                    className="link-btn small"
                    onClick={async () => {
                      if (!confirm("Delete this comment?")) return;
                      await api.del(`${path}/notes/${n.id}`).catch(setError);
                      refresh();
                    }}
                  >
                    Delete
                  </button>
                )}
              </div>
              <Markdown className="markdown" html={n.body_html} />
              <ReactionBar base={path} noteId={n.id} />
              {hasSuggestion(n.body) &&
                (n.suggestion_applied_sha ? (
                  <div className="suggestion-state small">
                    <Check size={13} /> Applied in{" "}
                    <Link to={`${ctx.web}/-/commit/${n.suggestion_applied_sha}`} className="mono">
                      {n.suggestion_applied_sha.slice(0, 8)}
                    </Link>
                  </div>
                ) : (
                  ctx.p.can_push &&
                  m.state === "opened" &&
                  !root.outdated &&
                  root.new_line != null && (
                    <button
                      className="btn small primary suggestion-apply"
                      onClick={async () => {
                        try {
                          await api.post(`${path}/notes/${n.id}/apply_suggestion`);
                          toast.ok(`Suggestion committed to ${m.source_branch}`);
                        } catch (e) {
                          toast.error(e);
                        }
                        refresh();
                      }}
                    >
                      <FileDiff size={14} /> Apply suggestion
                    </button>
                  )
                ))}
            </div>
          </div>
        ))}
        <ErrorBox error={error} />
        {open ? (
          <form className="stack" style={{ gap: 6 }} onSubmit={send}>
            <MarkdownEditor projectId={ctx.p.id} autoFocus rows={3} value={reply} onChange={setReply} placeholder="Reply…" label="Reply" />
            <div className="row">
              <button className="btn primary small" disabled={!reply.trim()}>
                Reply
              </button>
              <button type="button" className="btn small" onClick={() => setOpen(false)}>
                Cancel
              </button>
            </div>
          </form>
        ) : (
          <div>
            <button className="link-btn small" onClick={() => setOpen(true)}>
              Reply
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

/** A comment carrying a ```suggestion block (what the server can apply). */
function hasSuggestion(body: string) {
  return /^\s*```suggestion(:-\d+\+\d+)?\s*$/m.test(body);
}

function threadsOf(notes: S["NoteDto"][]) {
  const roots = notes.filter((n) => !n.parent_id);
  const replies = new Map<number, S["NoteDto"][]>();
  for (const n of notes) if (n.parent_id) replies.set(n.parent_id, [...(replies.get(n.parent_id) ?? []), n]);
  return { roots, replies };
}

const hhmm = (at: string) => new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });

export function dayLabel(at: string) {
  const d = new Date(at);
  const today = new Date();
  const days = Math.round((new Date(today.toDateString()).getTime() - new Date(d.toDateString()).getTime()) / 86_400_000);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  return d.toLocaleDateString([], { weekday: "short", day: "numeric", month: "short", year: d.getFullYear() === today.getFullYear() ? undefined : "numeric" });
}

/** Icon and wording for a system note. */
export function systemEvent(body: string): { icon: ReactNode; tone: string } {
  if (body.startsWith("merged")) return { icon: <GitMerge size={12} />, tone: "accent" };
  if (body === "approved") return { icon: <Check size={12} />, tone: "ok-solid" };
  if (body.startsWith("added version")) return { icon: <GitCommitHorizontal size={12} />, tone: "" };
  if (body === "closed") return { icon: <X size={12} />, tone: "bad" };
  if (body === "reopened") return { icon: <RotateCcw size={12} />, tone: "" };
  if (body.startsWith("added ~") || body.startsWith("removed ~")) return { icon: <Tag size={12} />, tone: "" };
  if (body.startsWith("changed milestone") || body.startsWith("removed milestone")) return { icon: <MilestoneIcon size={12} />, tone: "" };
  if (body === "requested changes") return { icon: <FileWarning size={12} />, tone: "bad" };
  return { icon: <Dot size={12} />, tone: "" };
}

/** Entries by day, each with its time and icon. */
export function Timeline({ entries }: { entries: LogEntry[] }) {
  let lastDay = "";
  return (
    <div className="timeline">
      {entries.length === 0 && <p className="muted small">Nothing yet.</p>}
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

export type LogEntry = { at: string; key: string; icon: ReactNode; tone?: string; boxed?: boolean; body: ReactNode };

function Activity({ ctx, m, notes }: { ctx: Ctx; m: S["MrDetail"]; notes: S["NoteDto"][] }) {
  const qc = useQueryClient();
  const [body, setBody] = useState("");
  const [error, setError] = useState<unknown>(null);
  const [filter, setFilter] = usePref<"all" | "comments" | "history">("mr.activity", "all");
  const pipelines = useMrPipelines(ctx, m);
  const labels = useLabels(ctx.p.id);
  const milestones = useMilestones(ctx.p.id, "all");
  const { roots, replies } = threadsOf(notes);
  const send = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api.post(`/projects/${ctx.p.id}/merge_requests/${m.iid}/notes`, { body });
      setBody("");
      qc.invalidateQueries({ queryKey: ["mr", ctx.p.id, m.iid] });
    } catch (err) {
      setError(err);
    }
  };

  const entries: LogEntry[] = [];
  for (const n of roots) {
    const who = <strong>{n.author_username ? <UserLink username={n.author_username} /> : "Forgust"}</strong>;
    if (n.system) {
      if (filter === "comments") continue;
      const ev = systemEvent(n.body);
      const text = <SystemText body={n.body} web={ctx.web} labels={labels.data} milestones={milestones.data} />;
      entries.push({ at: n.created_at, key: `n${n.id}`, icon: ev.icon, tone: ev.tone, body: <span>{who} {text}</span> });
      continue;
    }
    if (filter === "history") continue;
    const code = n.new_line != null || n.old_line != null;
    entries.push({
      at: n.created_at,
      key: `n${n.id}`,
      icon: code ? <MessageSquare size={12} /> : n.review ? <FileWarning size={12} /> : <Avatar name={noteAuthor(n)} username={n.author_username} />,
      tone: code ? (n.resolved_at ? "" : "warn") : n.review ? "bad" : "avatar",
      boxed: code,
      body: <Thread ctx={ctx} m={m} root={n} replies={replies.get(n.id) ?? []} boxed={code} />,
    });
  }
  if (filter !== "comments")
    for (const p of pipelines.data ?? [])
      entries.push({
        at: p.created_at,
        key: `p${p.id}`,
        icon: <StatusIcon status={p.status} size={14} />,
        tone: "plain",
        body: (
          <span>
            <Link to={`${ctx.web}/-/pipelines/${p.iid}`} className="strong-link">
              Pipeline #{p.iid}
            </Link>{" "}
            {statusLabel(p.status)}
            <span className="muted">
              {" "}
              · <code>{p.sha.slice(0, 8)}</code>
              {p.duration != null && ` · ${duration(p.duration)}`}
            </span>
          </span>
        ),
      });
  entries.sort((a, b) => a.at.localeCompare(b.at));

  return (
    <section className="activity" aria-label="Activity">
      <div className="activity-head">
        <h2>Activity</h2>
        <span className="rule" />
        <Segmented
          label="Show"
          value={filter}
          onChange={setFilter}
          options={[
            { value: "all", label: "All" },
            { value: "comments", label: "Comments" },
            { value: "history", label: "History" },
          ]}
        />
      </div>
      <Timeline entries={entries} />
      <form className="composer" onSubmit={send}>
        <ErrorBox error={error} />
        <MarkdownEditor projectId={ctx.p.id} rows={3} placeholder="Leave a comment" value={body} onChange={setBody} label="Comment" />
        <div className="row" style={{ justifyContent: "flex-end" }}>
          <button className="btn dark" disabled={!body.trim()}>
            Comment
          </button>
        </div>
      </form>
    </section>
  );
}

/** Who owns the changed files (CODEOWNERS), whether they approved, and a
 *  way to ask the ones who are not reviewers yet. */
function CodeOwnersFact({ ctx, iid, canEdit, onAsk }: { ctx: Ctx; iid: number; canEdit: boolean; onAsk: (ids: number[]) => void }) {
  const q = useQuery({
    queryKey: ["mr", ctx.p.id, iid, "code-owners"],
    queryFn: () => api.get<S["CodeOwners"]>(`/projects/${ctx.p.id}/merge_requests/${iid}/code_owners`),
    enabled: ctx.p.can_read_code,
  });
  const co = q.data;
  if (!co?.file || co.sections.length === 0) return null;
  return (
    <div className="fact">
      <span className="fact-label">Code owners</span>
      <div className="fact-value column">
        {co.sections.map((s) => (
          <div key={s.name} className="small" title={s.files.join("\n")}>
            <strong>{s.name || "Default"}</strong>
            {s.optional ? <span className="faint"> (optional)</span> : co.approval_required && <span className="faint"> ({s.approved_by.length}/{Math.min(s.approvals_required, Math.max(s.owners.length, 1))} approved)</span>}
            {": "}
            {s.owners.length === 0 ? <span className="faint">no eligible owner</span> : s.owners.map((o) => (
              <span key={o} className={s.approved_by.includes(o) ? "ic-ok" : undefined}>
                @{o}{" "}
              </span>
            ))}
            <span className="faint">· {s.files.length} file{s.files.length === 1 ? "" : "s"}</span>
          </div>
        ))}
        {canEdit && co.suggested_reviewers.length > 0 && (
          <button className="btn small" onClick={() => onAsk(co.suggested_reviewers.map((u) => u.id))}>
            Ask {co.suggested_reviewers.map((u) => `@${u.username}`).join(", ")} to review
          </button>
        )}
      </div>
    </div>
  );
}

function MrCommits({ ctx, iid }: { ctx: Ctx; iid: number }) {
  const q = useQuery({ queryKey: ["mr", ctx.p.id, iid, "commits"], queryFn: () => api.get<S["CommitInfo"][]>(`/projects/${ctx.p.id}/merge_requests/${iid}/commits`) });
  const signatures = useSignatures(ctx, q.data?.map((c) => c.id) ?? []);
  return (
    <Q q={q}>
      {(list) => (
        <div className="list">
          {list.map((c) => (
            <CommitRow key={c.id} ctx={ctx} c={c} signature={signatures[c.id]} />
          ))}
        </div>
      )}
    </Q>
  );
}

function Changes({ ctx, m }: { ctx: Ctx; m: S["MrDetail"] }) {
  const qc = useQueryClient();
  const latest = m.versions[0]?.version ?? 1;
  const [version, setVersion] = useState(latest);
  const [from, setFrom] = useState<number | "">("");
  const notes = useNotes(ctx, m.iid, version);
  const path = `/projects/${ctx.p.id}/merge_requests/${m.iid}`;

  const { threads, outdated, unresolved, replies } = useMemo(() => {
    const t = new Map<LineKey, ReactNode>();
    const unresolved = new Set<LineKey>();
    const out: S["NoteDto"][] = [];
    const { roots, replies } = threadsOf(notes.data ?? []);
    for (const n of roots) {
      if (n.system || (n.new_line == null && n.old_line == null)) continue;
      const pos = n.position;
      if (!pos) {
        out.push(n);
        continue;
      }
      const key = pos.new_line != null ? lineKey(pos.new_path ?? "", "n", pos.new_line) : lineKey(pos.old_path ?? "", "o", pos.old_line ?? 0);
      const node = <Thread ctx={ctx} m={m} root={n} replies={replies.get(n.id) ?? []} boxed />;
      if (!n.resolved_at) unresolved.add(key);
      t.set(key, t.has(key) ? (
        <>
          {t.get(key)}
          {node}
        </>
      ) : node);
    }
    return { threads: t, outdated: out, unresolved, replies };
  }, [notes.data, m]);

  // stable between renders: the diff view re-flattens its rows when it changes
  const review: Review | undefined = useMemo(
    () =>
      from === ""
        ? {
            threads,
            unresolved,
            composer: (_key, pos: NewComment, close) => (
              <Composer projectId={ctx.p.id} path={path} version={version} pos={pos} close={close} onDone={() => qc.invalidateQueries({ queryKey: ["mr", ctx.p.id, m.iid] })} />
            ),
          }
        : undefined,
    [from, threads, unresolved, version, path, ctx.p.id, m.iid, qc],
  );

  return (
    <div className="stack">
      <div className="row wrap small">
        <span>Compare</span>
        <Select
          style={{ width: "auto" }}
          value={String(from)}
          onChange={(v) => setFrom(v === "" ? "" : Number(v))}
          label="Base version"
          options={[
            { value: "", label: `${m.target_branch} (base)` },
            ...m.versions.filter((v) => v.version < version).map((v) => ({ value: String(v.version), label: `version ${v.version}` })),
          ]}
        />
        <span>and</span>
        <Select
          style={{ width: "auto" }}
          value={String(version)}
          onChange={(v) => setVersion(Number(v))}
          label="Version"
          options={m.versions.map((v) => ({ value: String(v.version), label: `version ${v.version}${v.version === latest ? " (latest)" : ""}` }))}
        />
        {from !== "" && <span className="muted">Comments are shown when comparing against the base.</span>}
      </div>
      {outdated.length > 0 && (
        <details className="card card-body">
          <summary>{outdated.length} outdated thread(s): their lines changed in this version</summary>
          <div className="stack" style={{ marginTop: 10 }}>
            {outdated.map((n) => (
              <Thread key={n.id} ctx={ctx} m={m} root={n} replies={replies.get(n.id) ?? []} boxed />
            ))}
          </div>
        </details>
      )}
      <PagedDiff url={(offset) => `${path}/diff${qs({ version, from_version: from === "" ? undefined : from, offset })}`} review={review} />
    </div>
  );
}

function Composer({ projectId, path, version, pos, close, onDone }: { projectId: number; path: string; version: number; pos: NewComment; close: () => void; onDone: () => void }) {
  const [body, setBody] = useState("");
  const [error, setError] = useState<unknown>(null);
  return (
    <form
      className="thread stack"
      style={{ gap: 6 }}
      onSubmit={async (e) => {
        e.preventDefault();
        try {
          const { text: _text, above: _above, ...position } = pos;
          await api.post(`${path}/notes`, { body, position: { version, ...position } });
          close();
          onDone();
        } catch (err) {
          setError(err instanceof ApiError ? err : String(err));
        }
      }}
    >
      <ErrorBox error={error} />
      {(pos.start_new_line != null || pos.start_old_line != null) && (
        <span className="small muted">
          Commenting on lines {pos.start_new_line ?? pos.start_old_line}–{pos.new_line ?? pos.old_line}
        </span>
      )}
      <MarkdownEditor projectId={projectId} autoFocus rows={3} value={body} onChange={setBody} placeholder="Comment on this line" label="Line comment" />
      <div className="row">
        <button className="btn primary small" disabled={!body.trim()}>
          Comment
        </button>
        {pos.new_line != null && pos.text != null && (
          <button
            type="button"
            className="btn small"
            title="Propose new content for this line; the author applies it with one click"
            onClick={() => setBody((b) => `${b}${b && !b.endsWith("\n") ? "\n" : ""}\`\`\`suggestion${pos.above ? `:-${pos.above}+0` : ""}\n${pos.text}\n\`\`\`\n`)}
          >
            <FileDiff size={14} /> Suggest a change
          </button>
        )}
        <button type="button" className="btn small" onClick={close}>
          Cancel
        </button>
      </div>
    </form>
  );
}
