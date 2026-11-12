import { useState, type FormEvent } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, CircleDot, GitMerge, GitPullRequestClosed, GitPullRequestArrow, MessageSquare, Pencil, Plus, RotateCcw, Trash2 } from "lucide-react";
import { api, qs, timeAgo, type S } from "../../api/client";
import { AuthorLink, Avatar, Empty, ErrorBox, Field, FieldGroup, FilterTabs, ImportedMark, ListHead, Q, SearchField, authorOf, Pager, useDebounced } from "../../components/ui";
import { Select } from "../../components/controls";
import { TemplatePicker } from "../../components/templates";
import { MarkdownEditor } from "../../components/MarkdownEditor";
import { EditLabelsButton, LabelChip, LabelList, LabelPicker, MilestoneChip, SystemText, useLabels } from "../../components/labels";
import { EditPeopleButton, PeopleAvatars, PeopleList, PeoplePicker } from "../../components/people";
import { useSession } from "../../session";
import { useToast } from "../../shell/toast";
import { Timeline, systemEvent, type LogEntry } from "./MergeRequests";
import type { Ctx } from "./Project";
import { DueDateFact, MilestoneFact, day, overdue, useMilestones } from "./Milestones";
import { ReactionBar } from "../../components/Reactions";
import { Markdown } from "../../components/Markdown";

type State = "opened" | "closed" | "all";

function IssueIcon({ state }: { state: string }) {
  return state === "opened" ? (
    <span className="row-icon ic-ok" title="Open" aria-label="Open" role="img">
      <CircleDot size={16} />
    </span>
  ) : (
    <span className="row-icon ic-accent" title="Closed" aria-label="Closed" role="img">
      <CheckCircle2 size={16} />
    </span>
  );
}

const PER_PAGE = 50;

export function IssueList({ ctx }: { ctx: Ctx }) {
  const { user } = useSession();
  const [params, setParams] = useSearchParams();
  const state = (params.get("state") as State) || "opened";
  const label = params.get("label") ?? "";
  const milestone = params.get("milestone") ?? "";
  const milestones = useMilestones(ctx.p.id, "all");
  const [search, setSearch] = useState("");
  const term = useDebounced(search, 250);
  const [page, setPage] = useState(1);
  const labels = useLabels(ctx.p.id);
  const set = (k: string, v: string) => {
    const next = new URLSearchParams(params);
    if (v) next.set(k, v);
    else next.delete(k);
    setParams(next, { replace: true });
    setPage(1);
  };
  const q = useQuery({
    queryKey: ["issues", ctx.p.id, state, label, milestone, term, page],
    queryFn: () => api.get<S["IssueList"]>(`/projects/${ctx.p.id}/issues${qs({ state, label, milestone, search: term, page, per_page: PER_PAGE })}`),
    placeholderData: (prev) => prev,
  });
  const labelTo = (l: S["Label"]) => `${ctx.web}/-/issues?label=${encodeURIComponent(l.name)}`;
  return (
    <div className="stack list-page">
      <ListHead title="Issues">
        {user && (
          <Link className="btn primary" to={`${ctx.web}/-/issues/new`}>
            New issue
          </Link>
        )}
      </ListHead>
      <div className="list-toolbar">
        <FilterTabs
          label="State"
          value={state}
          onChange={(v) => set("state", v === "opened" ? "" : v)}
          options={[
            { value: "opened", label: "Open", count: q.data?.opened },
            { value: "closed", label: "Closed", count: q.data?.closed },
            { value: "all", label: "All" },
          ]}
        />
        <div className="toolbar-end">
          {(labels.data?.length ?? 0) > 0 && (
            <Select
              label="Label"
              value={label}
              onChange={(v) => set("label", v)}
              options={[{ value: "", label: "Any label" }, ...(labels.data ?? []).map((l) => ({ value: l.name, label: l.name }))]}
              style={{ width: 170 }}
            />
          )}
          {(milestones.data?.length ?? 0) > 0 && (
            <Select
              label="Milestone"
              value={milestone}
              onChange={(v) => set("milestone", v)}
              options={[{ value: "", label: "Any milestone" }, { value: "None", label: "No milestone" }, ...(milestones.data ?? []).map((m) => ({ value: m.title, label: m.title }))]}
              style={{ width: 170 }}
            />
          )}
          <SearchField value={search} onChange={(v) => (setSearch(v), setPage(1))} placeholder="Filter by title or #number" />
        </div>
      </div>
      <Q q={q}>
        {(d) =>
          d.issues.length === 0 ? (
            <Empty>
              {search || label || milestone ? (
                "No issue matches."
              ) : state === "opened" ? (
                <>
                  No open issues.{" "}
                  {user && (
                    <Link to={`${ctx.web}/-/issues/new`} className="strong-link">
                      Open one
                    </Link>
                  )}
                </>
              ) : (
                `No ${state === "all" ? "" : `${state} `}issues.`
              )}
            </Empty>
          ) : (
            <div className="list rows">
              {d.issues.map((i) => (
                <div className="item issue-row" key={i.id}>
                  <IssueIcon state={i.state} />
                  <div className="grow" style={{ minWidth: 0 }}>
                    <div className="row-title wrap">
                      <Link to={`${ctx.web}/-/issues/${i.iid}`} className="ellipsis">
                        {i.title}
                      </Link>
                      <LabelList labels={i.labels} to={labelTo} />
                    </div>
                    <div className="row-meta">
                      <span>#{i.iid}</span>
                      <span>
                        {i.state === "closed" && i.closed_at ? `closed ${timeAgo(i.closed_at)}` : `opened ${timeAgo(i.created_at)}`} by <AuthorLink x={i} />
                      </span>
                      {i.milestone_title && (
                        <span>
                          <MilestoneChip web={ctx.web} id={i.milestone_id} title={i.milestone_title} />
                        </span>
                      )}
                      {i.due_date && (
                        <span className={i.state === "opened" && overdue(i.due_date) ? "error-text nowrap" : "nowrap"} title="Due date">
                          due {day(i.due_date)}
                        </span>
                      )}
                    </div>
                  </div>
                  <PeopleAvatars people={i.assignees} label="Assignee" />
                  {i.notes_count > 0 && (
                    <span className="row-comments" title={`${i.notes_count} comment${i.notes_count === 1 ? "" : "s"}`}>
                      <MessageSquare size={14} /> {i.notes_count}
                    </span>
                  )}
                  <span className="row-time" title={new Date(i.updated_at).toLocaleString()}>
                    {timeAgo(i.updated_at)}
                  </span>
                </div>
              ))}
            </div>
          )
        }
      </Q>
      <Pager page={page} count={q.data?.issues.length ?? 0} perPage={PER_PAGE} onPage={setPage} labels={["Newer", "Older"]} />
    </div>
  );
}

export function NewIssue({ ctx }: { ctx: Ctx }) {
  const nav = useNavigate();
  const qc = useQueryClient();
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [labels, setLabels] = useState<string[]>([]);
  const [error, setError] = useState<unknown>(null);
  const all = useLabels(ctx.p.id);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    try {
      const i = await api.post<S["IssueDto"]>(`/projects/${ctx.p.id}/issues`, { title, description, labels });
      qc.invalidateQueries({ queryKey: ["issues", ctx.p.id] });
      nav(`${ctx.web}/-/issues/${i.iid}`);
    } catch (err) {
      setError(err);
    }
  };
  return (
    <div className="stack list-page">
      <ListHead title="New issue" sub="Report a bug, propose a change, or ask a question." />
      <form className="stack" style={{ maxWidth: 760 }} onSubmit={submit}>
        <ErrorBox error={error} />
        <Field label="Title">
          <input autoFocus value={title} onChange={(e) => setTitle(e.target.value)} />
        </Field>
        <Field label="Description">
          <div className="stack" style={{ gap: 6 }}>
            <TemplatePicker projectId={ctx.p.id} kind="issue" description={description} setDescription={setDescription} />
            <MarkdownEditor projectId={ctx.p.id} rows={8} value={description} onChange={setDescription} label="Description" />
          </div>
        </Field>
        {ctx.p.can_push && (
          <FieldGroup label="Labels">
            <div className="row wrap" style={{ gap: 6 }}>
              <LabelList labels={(all.data ?? []).filter((l) => labels.includes(l.name))} />
              <LabelPicker
                projectId={ctx.p.id}
                manage={`${ctx.web}/-/labels`}
                value={labels}
                onChange={setLabels}
                trigger={
                  <button type="button" className="btn small">
                    <Plus size={13} /> Labels
                  </button>
                }
              />
            </div>
          </FieldGroup>
        )}
        <div className="form-actions">
          <Link className="btn" to={`${ctx.web}/-/issues`}>
            Cancel
          </Link>
          <button className="btn primary" disabled={!title.trim()}>
            Create issue
          </button>
        </div>
      </form>
    </div>
  );
}

export function IssuePage({ ctx, iid }: { ctx: Ctx; iid: number }) {
  const qc = useQueryClient();
  const toast = useToast();
  const path = `/projects/${ctx.p.id}/issues/${iid}`;
  const q = useQuery({ queryKey: ["issue", ctx.p.id, iid], queryFn: () => api.get<S["IssueDetail"]>(path) });
  const notes = useQuery({ queryKey: ["issue", ctx.p.id, iid, "notes"], queryFn: () => api.get<S["IssueNoteDto"][]>(`${path}/notes`) });
  const labels = useLabels(ctx.p.id);
  const [editing, setEditing] = useState(false);
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["issue", ctx.p.id, iid] });
    qc.invalidateQueries({ queryKey: ["issues", ctx.p.id] });
    qc.invalidateQueries({ queryKey: ["labels", ctx.p.id] });
  };
  const update = async (body: object, done?: string) => {
    try {
      await api.put(path, body);
      if (done) toast.ok(done);
      refresh();
    } catch (e) {
      toast.error(e);
    }
  };
  return (
    <Q q={q}>
      {(i) => (
        <div className="stack">
          <header className="mr-head">
            <div className="mr-status">
              <span className={`mr-state ${i.state}`}>{i.state === "opened" ? "Open" : "Closed"}</span>
              <span>#{i.iid}</span>
              <span aria-hidden="true">·</span>
              <span>
                <AuthorLink x={i} /> opened {timeAgo(i.created_at)}
              </span>
              {i.updated_at !== i.created_at && (
                <>
                  <span aria-hidden="true">·</span>
                  <span>updated {timeAgo(i.updated_at)}</span>
                </>
              )}
            </div>
            <div className="row" style={{ alignItems: "flex-start" }}>
              <h1 className="mr-title grow">{i.title}</h1>
              {i.can_edit && !editing && (
                <button className="icon-btn bordered" aria-label="Edit title and description" title="Edit title and description" onClick={() => setEditing(true)}>
                  <Pencil size={15} />
                </button>
              )}
            </div>
          </header>
          <div className="mr-layout with-panel issue-layout">
            <div className="mr-main">
              <div className="stack" style={{ gap: 28 }}>
                {editing ? (
                  <EditIssue projectId={ctx.p.id} i={i} path={path} done={() => (setEditing(false), refresh())} />
                ) : (
                  <section className="mr-description markdown" aria-label="Description">
                    {i.description ? <Markdown html={i.description_html} /> : <p className="muted">No description.</p>}
                  </section>
                )}
                <ReactionBar base={path} />
                <Q q={notes}>{(list) => <IssueActivity ctx={ctx} i={i} notes={list} labels={labels.data} refresh={refresh} />}</Q>
              </div>
            </div>
            <aside className="mr-aside" aria-label="Issue details">
              <section className="mr-facts">
                <div className="fact">
                  <span className="fact-label">Assignees</span>
                  <div className="fact-value column">
                    <PeopleList people={i.assignees} />
                    {i.can_assign && (
                      <PeoplePicker projectId={ctx.p.id} label="Assignees" selected={i.assignees} onChange={(ids) => update({ assignee_ids: ids })} trigger={<EditPeopleButton />} />
                    )}
                  </div>
                </div>
                <div className="fact">
                  <span className="fact-label">Labels</span>
                  <div className="fact-value column">
                    {i.labels.length === 0 ? <span className="muted">None</span> : <LabelList labels={i.labels} to={(l) => `${ctx.web}/-/issues?label=${encodeURIComponent(l.name)}`} />}
                    {i.can_label && (
                      <LabelPicker
                        projectId={ctx.p.id}
                        manage={`${ctx.web}/-/labels`}
                        value={i.labels.map((l) => l.name)}
                        onChange={(names) => update({ labels: names })}
                        trigger={<EditLabelsButton />}
                      />
                    )}
                  </div>
                </div>
                <MilestoneFact ctx={ctx} current={i.milestone_id} title={i.milestone_title} canEdit={i.can_label} onChange={(id) => update({ milestone_id: id })} />
                {i.merge_requests.length > 0 && (
                  <div className="fact">
                    <span className="fact-label">Merge requests</span>
                    <div className="fact-value column related-mrs">
                      {i.merge_requests.map((m) => (
                        <Link key={m.iid} to={`${ctx.web}/-/merge_requests/${m.iid}`} className="related-mr" title={m.closes ? `Closes this issue when it merges` : `Mentions this issue`}>
                          <MrStateMark state={m.state} />
                          <span className="ellipsis">{m.title}</span>
                          <span className="faint nowrap">!{m.iid}</span>
                          {m.closes && <span className="related-closes">closes</span>}
                        </Link>
                      ))}
                    </div>
                  </div>
                )}
                <DueDateFact value={i.due_date} open={i.state === "opened"} canEdit={i.can_edit} onChange={(d) => update({ due_date: d })} />
                <div className="fact">
                  <span className="fact-label">Author</span>
                  <div className="fact-value">
                    <Avatar name={authorOf(i)} username={i.author_username} />
                    <AuthorLink x={i} />
                    <ImportedMark x={i} />
                  </div>
                </div>
                <div className="fact">
                  <span className="fact-label">Created</span>
                  <div className="fact-value" title={new Date(i.created_at).toLocaleString()}>
                    {new Date(i.created_at).toLocaleDateString([], { day: "numeric", month: "short", year: "numeric" })}
                  </div>
                </div>
                {i.can_edit && (
                  <div className="fact">
                    {i.state === "opened" ? (
                      <button className="btn small" onClick={() => update({ state_event: "close" }, "Issue closed")}>
                        <CheckCircle2 size={14} /> Close issue
                      </button>
                    ) : (
                      <button className="btn small" onClick={() => update({ state_event: "reopen" }, "Issue reopened")}>
                        <RotateCcw size={14} /> Reopen issue
                      </button>
                    )}
                  </div>
                )}
              </section>
            </aside>
          </div>
        </div>
      )}
    </Q>
  );
}

function EditIssue({ projectId, i, path, done }: { projectId: number; i: S["IssueDetail"]; path: string; done: () => void }) {
  const [title, setTitle] = useState(i.title);
  const [description, setDescription] = useState(i.description);
  const [error, setError] = useState<unknown>(null);
  return (
    <form
      className="mr-card stack"
      onSubmit={async (e) => {
        e.preventDefault();
        try {
          await api.put(path, { title, description });
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
      <Field label="Description">
        <MarkdownEditor projectId={projectId} rows={8} value={description} onChange={setDescription} label="Description" />
      </Field>
      <div className="row">
        <button className="btn primary" disabled={!title.trim()}>
          Save
        </button>
        <button type="button" className="btn" onClick={done}>
          Cancel
        </button>
      </div>
    </form>
  );
}

function IssueActivity({ ctx, i, notes, labels, refresh }: { ctx: Ctx; i: S["IssueDetail"]; notes: S["IssueNoteDto"][]; labels?: S["LabelWithCounts"][]; refresh: () => void }) {
  const { user } = useSession();
  const milestones = useMilestones(ctx.p.id, "all");
  const toast = useToast();
  const [body, setBody] = useState("");
  const [error, setError] = useState<unknown>(null);
  const path = `/projects/${ctx.p.id}/issues/${i.iid}`;
  const send = async (close?: boolean) => {
    try {
      if (body.trim()) await api.post(`${path}/notes`, { body });
      if (close != null) await api.put(path, { state_event: close ? "close" : "reopen" });
      setBody("");
      setError(null);
      refresh();
    } catch (err) {
      setError(err);
    }
  };
  const entries: LogEntry[] = notes.map((n) => {
    if (n.system) {
      const ev = systemEvent(n.body);
      return {
        at: n.created_at,
        key: `n${n.id}`,
        icon: ev.icon,
        tone: ev.tone,
        body: (
          <span>
            <strong>
              <AuthorLink x={n} />
            </strong>{" "}
            <SystemText body={n.body} web={ctx.web} labels={labels} milestones={milestones.data} />
          </span>
        ),
      };
    }
    return { at: n.created_at, key: `n${n.id}`, icon: <Avatar name={authorOf(n)} username={n.author_username} />, tone: "avatar", body: <IssueComment ctx={ctx} i={i} n={n} refresh={refresh} toast={toast} /> };
  });
  return (
    <section className="activity" aria-label="Activity">
      <div className="activity-head">
        <h2>Activity</h2>
        <span className="rule" />
      </div>
      <Timeline entries={entries} />
      {user ? (
        <form
          className="composer"
          onSubmit={(e) => {
            e.preventDefault();
            send();
          }}
        >
          <ErrorBox error={error} />
          <MarkdownEditor projectId={ctx.p.id} rows={3} placeholder="Leave a comment" value={body} onChange={setBody} label="Comment" />
          <div className="row" style={{ justifyContent: "flex-end" }}>
            {i.can_edit && (
              <button type="button" className="btn" onClick={() => send(i.state === "opened")}>
                {i.state === "opened" ? (body.trim() ? "Close with comment" : "Close issue") : body.trim() ? "Reopen with comment" : "Reopen issue"}
              </button>
            )}
            <button className="btn dark" disabled={!body.trim()}>
              Comment
            </button>
          </div>
        </form>
      ) : (
        <p className="muted small">
          <Link to={`/login?next=${encodeURIComponent(location.pathname)}`}>Sign in</Link> to comment.
        </p>
      )}
    </section>
  );
}

function IssueComment({ ctx, i, n, refresh, toast }: { ctx: Ctx; i: S["IssueDetail"]; n: S["IssueNoteDto"]; refresh: () => void; toast: ReturnType<typeof useToast> }) {
  const { user } = useSession();
  const [editing, setEditing] = useState(false);
  const [body, setBody] = useState(n.body);
  const path = `/projects/${ctx.p.id}/issues/${i.iid}/notes/${n.id}`;
  const own = user?.id === n.author_id;
  return (
    <div className="thread plain">
      <div className="note-meta">
        <strong>
          <AuthorLink x={n} />
        </strong>{" "}
        <span className="faint">{timeAgo(n.created_at)}</span>
        <ImportedMark x={n} />
        {own && !editing && (
          <button className="link-btn small" onClick={() => setEditing(true)}>
            Edit
          </button>
        )}
        {(own || ctx.p.can_admin) && (
          <button
            className="link-btn small"
            aria-label="Delete comment"
            onClick={async () => {
              if (!confirm("Delete this comment?")) return;
              await api.del(path).catch(toast.error);
              refresh();
            }}
          >
            <Trash2 size={12} />
          </button>
        )}
      </div>
      {editing ? (
        <form
          className="stack"
          style={{ gap: 6 }}
          onSubmit={async (e) => {
            e.preventDefault();
            try {
              await api.put(path, { body });
              setEditing(false);
              refresh();
            } catch (err) {
              toast.error(err);
            }
          }}
        >
          <MarkdownEditor projectId={ctx.p.id} autoFocus rows={3} value={body} onChange={setBody} label="Edit comment" />
          <div className="row">
            <button className="btn primary small" disabled={!body.trim()}>
              Save
            </button>
            <button type="button" className="btn small" onClick={() => setEditing(false)}>
              Cancel
            </button>
          </div>
        </form>
      ) : (
        <>
          <Markdown className="markdown" html={n.body_html} />
          <ReactionBar base={`/projects/${ctx.p.id}/issues/${i.iid}`} noteId={n.id} />
        </>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ labels page

export function LabelsPage({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const toast = useToast();
  const labels = useLabels(ctx.p.id);
  const [search, setSearch] = useState("");
  const [editing, setEditing] = useState<number | "new" | null>(null);
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["labels", ctx.p.id] });
    qc.invalidateQueries({ queryKey: ["issues", ctx.p.id] });
    qc.invalidateQueries({ queryKey: ["issue", ctx.p.id] });
  };
  return (
    <div className="stack list-page">
      <ListHead title="Labels" sub="Sort issues and merge requests by topic, priority or team.">
        {ctx.p.can_push && editing !== "new" && (
          <button className="btn primary" onClick={() => setEditing("new")}>
            <Plus size={15} /> New label
          </button>
        )}
      </ListHead>
      {editing === "new" && <LabelForm ctx={ctx} done={() => (setEditing(null), refresh())} />}
      <div className="list-toolbar plain">
        <SearchField value={search} onChange={setSearch} placeholder="Filter labels" />
      </div>
      <Q q={labels}>
        {(all) => {
          const list = all.filter((l) => l.name.toLowerCase().includes(search.toLowerCase()) || l.description.toLowerCase().includes(search.toLowerCase()));
          if (list.length === 0) return <Empty>{search ? "No label matches." : ctx.p.can_push ? "No labels yet. Create one to sort issues and merge requests." : "No labels yet."}</Empty>;
          return (
            <div className="list rows">
              {list.map((l) =>
                editing === l.id ? (
                  <div className="item" key={l.id}>
                    <LabelForm ctx={ctx} label={l} done={() => (setEditing(null), refresh())} />
                  </div>
                ) : (
                  <div className="item label-row" key={l.id}>
                    <div className="label-row-name">
                      <LabelChip label={l} />
                    </div>
                    <span className="grow muted small ellipsis">{l.description}</span>
                    <span className="label-row-counts">
                      <Link to={`${ctx.web}/-/issues?label=${encodeURIComponent(l.name)}`}>
                        {l.open_issues} open issue{l.open_issues === 1 ? "" : "s"}
                      </Link>
                      <span className="faint">·</span>
                      <span className="muted">
                        {l.open_merge_requests} open MR{l.open_merge_requests === 1 ? "" : "s"}
                      </span>
                    </span>
                    {ctx.p.can_push && (
                      <>
                        <button className="icon-btn" aria-label={`Edit ${l.name}`} title="Edit" onClick={() => setEditing(l.id)}>
                          <Pencil size={15} />
                        </button>
                        <button
                          className="icon-btn"
                          aria-label={`Delete ${l.name}`}
                          title="Delete"
                          onClick={async () => {
                            if (!confirm(`Delete the label “${l.name}”? It is removed from every issue and merge request.`)) return;
                            try {
                              await api.del(`/projects/${ctx.p.id}/labels/${l.id}`);
                              refresh();
                            } catch (e) {
                              toast.error(e);
                            }
                          }}
                        >
                          <Trash2 size={15} />
                        </button>
                      </>
                    )}
                  </div>
                ),
              )}
            </div>
          );
        }}
      </Q>
    </div>
  );
}

const SWATCHES = ["#6366f1", "#0ea5e9", "#14b8a6", "#22c55e", "#eab308", "#f97316", "#ef4444", "#ec4899", "#a855f7", "#64748b"];

function LabelForm({ ctx, label, done }: { ctx: Ctx; label?: S["LabelWithCounts"]; done: () => void }) {
  const [name, setName] = useState(label?.name ?? "");
  const [color, setColor] = useState(label?.color ?? SWATCHES[Math.floor(Math.random() * SWATCHES.length)]);
  const [description, setDescription] = useState(label?.description ?? "");
  const [error, setError] = useState<unknown>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    try {
      if (label) await api.put(`/projects/${ctx.p.id}/labels/${label.id}`, { name, color, description });
      else await api.post(`/projects/${ctx.p.id}/labels`, { name, color, description });
      done();
    } catch (err) {
      setError(err);
    }
  };
  return (
    <form className="label-form" onSubmit={submit}>
      <ErrorBox error={error} />
      <div className="label-form-fields">
        <Field label="Name">
          <input autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="bug" />
        </Field>
        <Field label="Description">
          <input value={description} onChange={(e) => setDescription(e.target.value)} placeholder="Something does not work" />
        </Field>
        <FieldGroup label="Color">
          <div className="swatches">
            {SWATCHES.map((s) => (
              <button key={s} type="button" className={`swatch ${s === color ? "on" : ""}`} style={{ background: s }} aria-label={s} aria-pressed={s === color} onClick={() => setColor(s)} />
            ))}
            <input className="swatch-hex" value={color} onChange={(e) => setColor(e.target.value)} aria-label="Hex color" />
          </div>
        </FieldGroup>
      </div>
      <div className="row">
        <span className="grow">{name.trim() && <LabelChip label={{ name: name.trim(), color: /^#[0-9a-f]{6}$/i.test(color) ? color : "var(--text-faint)", description }} />}</span>
        <button type="button" className="btn" onClick={done}>
          Cancel
        </button>
        <button className="btn primary" disabled={!name.trim()}>
          {label ? "Save" : "Create label"}
        </button>
      </div>
    </form>
  );
}

/** A merge request's state as a small colored icon. */
function MrStateMark({ state }: { state: string }) {
  if (state === "merged") return <GitMerge size={14} className="mr-mark merged" aria-label="Merged" />;
  if (state === "closed") return <GitPullRequestClosed size={14} className="mr-mark closed" aria-label="Closed" />;
  return <GitPullRequestArrow size={14} className="mr-mark opened" aria-label="Open" />;
}
