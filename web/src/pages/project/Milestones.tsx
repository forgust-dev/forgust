import { useState, type FormEvent } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarDays, CircleDot, GitMerge, Milestone as MilestoneIcon, Pencil, Plus, Trash2 } from "lucide-react";
import { api, qs, type S } from "../../api/client";
import { Empty, ErrorBox, Field, FilterTabs, ListHead, Q } from "../../components/ui";
import { Select } from "../../components/controls";
import { MarkdownEditor } from "../../components/MarkdownEditor";
import { useToast } from "../../shell/toast";
import type { Ctx } from "./Project";

type M = S["Milestone"];

export function useMilestones(projectId: number, state: "active" | "closed" | "all" = "active") {
  return useQuery({ queryKey: ["milestones", projectId, state], queryFn: () => api.get<M[]>(`/projects/${projectId}/milestones${qs({ state })}`) });
}

/** A date (`YYYY-MM-DD`) as people read it, without a time zone shift. */
export function day(d: string) {
  const [y, m, dd] = d.split("-").map(Number);
  return new Date(y, m - 1, dd).toLocaleDateString([], { day: "numeric", month: "short", year: "numeric" });
}

/** Past due (and still open). */
export function overdue(d?: string | null) {
  if (!d) return false;
  const today = new Date();
  const iso = `${today.getFullYear()}-${String(today.getMonth() + 1).padStart(2, "0")}-${String(today.getDate()).padStart(2, "0")}`;
  return d < iso;
}

function Progress({ m }: { m: M }) {
  const total = m.open_issues + m.closed_issues;
  const pct = total === 0 ? 0 : Math.round((m.closed_issues / total) * 100);
  return (
    <div className="ms-progress" title={`${m.closed_issues} of ${total} issues closed`}>
      <div className="ms-bar" role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} aria-label={`${m.title} progress`}>
        <span style={{ width: `${pct}%` }} />
      </div>
      <span className="small muted nowrap">{pct}% complete</span>
    </div>
  );
}

function Dates({ m }: { m: M }) {
  if (!m.start_date && !m.due_date) return <span className="faint">No dates</span>;
  return (
    <span className={m.state === "active" && overdue(m.due_date) ? "error-text" : undefined}>
      {m.start_date && day(m.start_date)}
      {m.start_date && m.due_date && " – "}
      {m.due_date && (m.start_date ? day(m.due_date) : `Due ${day(m.due_date)}`)}
      {m.state === "active" && overdue(m.due_date) && " (past due)"}
    </span>
  );
}

export function MilestonesPage({ ctx }: { ctx: Ctx }) {
  const [params, setParams] = useSearchParams();
  const state = (params.get("state") as "active" | "closed" | "all") || "active";
  const q = useMilestones(ctx.p.id, state);
  return (
    <div className="stack list-page">
      <ListHead title="Milestones" sub="Goals with dates that issues and merge requests work towards.">
        {ctx.p.can_push && (
          <Link className="btn primary" to={`${ctx.web}/-/milestones/new`}>
            <Plus size={14} /> New milestone
          </Link>
        )}
      </ListHead>
      <div className="list-toolbar plain">
        <FilterTabs
          label="State"
          value={state}
          onChange={(v) => setParams(v === "active" ? {} : { state: v }, { replace: true })}
          options={[
            { value: "active", label: "Open" },
            { value: "closed", label: "Closed" },
            { value: "all", label: "All" },
          ]}
        />
      </div>
      <Q q={q}>
        {(list) =>
          list.length === 0 ? (
            <Empty>{state === "active" ? "No open milestones." : "No milestones."}</Empty>
          ) : (
            <div className="list rows">
              {list.map((m) => (
                <div className="item ms-row" key={m.id}>
                  <span className="row-icon faint">
                    <MilestoneIcon size={16} />
                  </span>
                  <div className="grow" style={{ minWidth: 0 }}>
                    <div className="row-title">
                      <Link to={`${ctx.web}/-/milestones/${m.id}`} className="ellipsis">
                        {m.title}
                      </Link>
                      {m.state === "closed" && <span className="pill">closed</span>}
                    </div>
                    <div className="row-meta">
                      <Dates m={m} />
                      <span>
                        {m.open_issues + m.closed_issues} issues · {m.open_merge_requests + m.merged_merge_requests} merge requests
                      </span>
                    </div>
                  </div>
                  <Progress m={m} />
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}

export function MilestonePage({ ctx, id }: { ctx: Ctx; id: number }) {
  const nav = useNavigate();
  const qc = useQueryClient();
  const toast = useToast();
  const path = `/projects/${ctx.p.id}/milestones/${id}`;
  const q = useQuery({ queryKey: ["milestone", ctx.p.id, id], queryFn: () => api.get<M>(path) });
  const issues = useQuery({
    queryKey: ["issues", ctx.p.id, "milestone", id, q.data?.title],
    queryFn: () => api.get<S["IssueList"]>(`/projects/${ctx.p.id}/issues${qs({ milestone: q.data!.title, state: "all", per_page: 100 })}`),
    enabled: !!q.data,
  });
  const mrs = useQuery({
    queryKey: ["mrs", ctx.p.id, "milestone", id, q.data?.title],
    queryFn: () => api.get<S["MrListItem"][]>(`/projects/${ctx.p.id}/merge_requests${qs({ milestone: q.data!.title, state: "all", per_page: 100 })}`),
    enabled: !!q.data && ctx.p.can_read_code,
  });
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["milestone", ctx.p.id, id] });
    qc.invalidateQueries({ queryKey: ["milestones", ctx.p.id] });
  };
  const setState = async (state: string) => {
    try {
      await api.put(path, { state });
      refresh();
    } catch (e) {
      toast.error(e);
    }
  };
  const remove = async (m: M) => {
    if (!confirm(`Delete the milestone ${m.title}? Its issues and merge requests stay, without a milestone.`)) return;
    try {
      await api.del(path);
      qc.invalidateQueries({ queryKey: ["milestones", ctx.p.id] });
      toast.ok(`Milestone ${m.title} deleted`);
      nav(`${ctx.web}/-/milestones`);
    } catch (e) {
      toast.error(e);
    }
  };
  return (
    <Q q={q}>
      {(m) => (
        <div className="stack list-page">
          <ListHead
            title={
              <span className="row" style={{ gap: 8 }}>
                <MilestoneIcon size={18} /> {m.title} {m.state === "closed" && <span className="pill">closed</span>}
              </span>
            }
            sub={<Dates m={m} />}
          >
            {ctx.p.can_push && (
              <div className="row" style={{ gap: 6 }}>
                <button className="btn" onClick={() => setState(m.state === "active" ? "closed" : "active")}>
                  {m.state === "active" ? "Close milestone" : "Reopen milestone"}
                </button>
                <Link className="icon-btn bordered" to={`${ctx.web}/-/milestones/${m.id}/edit`} title="Edit" aria-label={`Edit milestone ${m.title}`}>
                  <Pencil size={15} />
                </Link>
                <button className="icon-btn bordered" title="Delete" aria-label={`Delete milestone ${m.title}`} onClick={() => remove(m)}>
                  <Trash2 size={15} />
                </button>
              </div>
            )}
          </ListHead>
          <Progress m={m} />
          {m.description && <p style={{ whiteSpace: "pre-wrap" }}>{m.description}</p>}
          <section className="stack" style={{ gap: 8 }} aria-label="Issues">
            <h2 className="section-title">Issues · {m.open_issues + m.closed_issues}</h2>
            <Q q={issues}>
              {(d) =>
                d.issues.length === 0 ? (
                  <Empty>No issues on this milestone.</Empty>
                ) : (
                  <div className="list rows">
                    {d.issues.map((i) => (
                      <div className="item" key={i.id}>
                        <CircleDot size={15} className={i.state === "closed" ? "faint" : "ic-ok"} />
                        <Link to={`${ctx.web}/-/issues/${i.iid}`} className="grow ellipsis">
                          {i.title}
                        </Link>
                        <span className="faint small">#{i.iid}</span>
                      </div>
                    ))}
                  </div>
                )
              }
            </Q>
          </section>
          {ctx.p.can_read_code && (
            <section className="stack" style={{ gap: 8 }} aria-label="Merge requests">
              <h2 className="section-title">Merge requests · {m.open_merge_requests + m.merged_merge_requests}</h2>
              <Q q={mrs}>
                {(list) =>
                  list.length === 0 ? (
                    <Empty>No merge requests on this milestone.</Empty>
                  ) : (
                    <div className="list rows">
                      {list.map((r) => (
                        <div className="item" key={r.id}>
                          <GitMerge size={15} className={r.state === "merged" ? "ic-accent" : r.state === "opened" ? "ic-ok" : "faint"} />
                          <Link to={`${ctx.web}/-/merge_requests/${r.iid}`} className="grow ellipsis">
                            {r.title}
                          </Link>
                          <span className="faint small">!{r.iid}</span>
                        </div>
                      ))}
                    </div>
                  )
                }
              </Q>
            </section>
          )}
        </div>
      )}
    </Q>
  );
}

export function MilestoneForm({ ctx, id }: { ctx: Ctx; id?: number }) {
  const q = useQuery({ queryKey: ["milestone", ctx.p.id, id], queryFn: () => api.get<M>(`/projects/${ctx.p.id}/milestones/${id}`), enabled: id != null });
  if (id != null) return <Q q={q}>{(m) => <Form ctx={ctx} m={m} />}</Q>;
  return <Form ctx={ctx} />;
}

function Form({ ctx, m }: { ctx: Ctx; m?: M }) {
  const nav = useNavigate();
  const qc = useQueryClient();
  const [title, setTitle] = useState(m?.title ?? "");
  const [description, setDescription] = useState(m?.description ?? "");
  const [start, setStart] = useState(m?.start_date ?? "");
  const [due, setDue] = useState(m?.due_date ?? "");
  const [error, setError] = useState<unknown>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      const body = { title: title.trim(), description, start_date: start, due_date: due };
      const saved = m ? await api.put<M>(`/projects/${ctx.p.id}/milestones/${m.id}`, body) : await api.post<M>(`/projects/${ctx.p.id}/milestones`, body);
      qc.invalidateQueries({ queryKey: ["milestones", ctx.p.id] });
      qc.invalidateQueries({ queryKey: ["milestone", ctx.p.id, saved.id] });
      nav(`${ctx.web}/-/milestones/${saved.id}`);
    } catch (err) {
      setError(err);
    }
  };
  return (
    <div className="stack list-page">
      <ListHead title={m ? `Edit ${m.title}` : "New milestone"} />
      <form className="stack" style={{ maxWidth: 640 }} onSubmit={submit}>
        <ErrorBox error={error} />
        <Field label="Title">
          <input autoFocus value={title} onChange={(e) => setTitle(e.target.value)} placeholder="v1.0" />
        </Field>
        <div className="row wrap" style={{ gap: 12 }}>
          <Field label="Start date" hint="Optional">
            <input type="date" value={start} onChange={(e) => setStart(e.target.value)} />
          </Field>
          <Field label="Due date" hint="Optional">
            <input type="date" value={due} onChange={(e) => setDue(e.target.value)} />
          </Field>
        </div>
        <Field label="Description">
          <MarkdownEditor projectId={ctx.p.id} rows={5} value={description} onChange={setDescription} label="Description" />
        </Field>
        <div className="row">
          <button className="btn primary" disabled={!title.trim()}>
            {m ? "Save changes" : "Create milestone"}
          </button>
          <button type="button" className="btn" onClick={() => nav(-1)}>
            Cancel
          </button>
        </div>
      </form>
    </div>
  );
}

/** Sidebar fact: the milestone, editable by those who may set labels. */
export function MilestoneFact({ ctx, current, title, canEdit, onChange }: { ctx: Ctx; current?: number | null; title?: string | null; canEdit: boolean; onChange: (id: number) => void }) {
  const list = useMilestones(ctx.p.id, "active");
  const options = [{ value: "0", label: "No milestone" }, ...(list.data ?? []).map((m) => ({ value: String(m.id), label: m.title }))];
  if (current && title && !options.some((o) => o.value === String(current))) options.push({ value: String(current), label: title });
  return (
    <div className="fact">
      <span className="fact-label">Milestone</span>
      <div className="fact-value column">
        {canEdit ? (
          <Select label="Milestone" value={String(current ?? 0)} onChange={(v) => onChange(Number(v))} options={options} />
        ) : current && title ? (
          <Link to={`${ctx.web}/-/milestones/${current}`}>
            <MilestoneIcon size={13} /> {title}
          </Link>
        ) : (
          <span className="muted">None</span>
        )}
      </div>
    </div>
  );
}

/** Sidebar fact: an issue's due date. */
export function DueDateFact({ value, open, canEdit, onChange }: { value?: string | null; open: boolean; canEdit: boolean; onChange: (d: string) => void }) {
  return (
    <div className="fact">
      <span className="fact-label">Due date</span>
      <div className="fact-value column">
        {canEdit ? (
          <input type="date" aria-label="Due date" value={value ?? ""} onChange={(e) => onChange(e.target.value)} />
        ) : value ? (
          <span className={open && overdue(value) ? "error-text" : undefined}>
            <CalendarDays size={13} /> {day(value)}
          </span>
        ) : (
          <span className="muted">None</span>
        )}
      </div>
    </div>
  );
}
