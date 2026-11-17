import { useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarClock, GitBranch, Pause, Pencil, Play, Plus, RotateCw, Trash2 } from "lucide-react";
import { api, listAnd, qs, refLink, timeAgo, timeUntil, type S } from "../../../api/client";
import { useSession } from "../../../session";
import { Empty, ErrorBox, Field, FieldGroup, Q, ListHead } from "../../../components/ui";
import { Select } from "../../../components/controls";
import { useToast } from "../../../shell/toast";
import { useConfirm } from "../../../shell/dialog";
import type { Ctx } from "../Project";
import { VariableRows } from "./Pipelines";
import { StatusIcon, statusLabel } from "./Status";
import { useBranches } from "../Code";

type Form = { id?: number; description: string; ref: string; cron: string; cron_timezone: string; active: boolean; variables: { key: string; value: string }[] };

const PRESETS: [string, string][] = [
  ["Every day (03:00)", "0 3 * * *"],
  ["Every weekday (03:00)", "0 3 * * 1-5"],
  ["Every week (Sunday 03:00)", "0 3 * * 0"],
  ["Every hour", "0 * * * *"],
];

const DAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const pad = (n: number) => String(n).padStart(2, "0");
const ordinal = (n: number) => `${n}${n % 10 === 1 && n !== 11 ? "st" : n % 10 === 2 && n !== 12 ? "nd" : n % 10 === 3 && n !== 13 ? "rd" : "th"}`;

/** A cron line in words ("Every weekday at 03:00"), for the usual ones; null for the rest. */
export function cadence(cron: string): string | null {
  const f = cron.trim().split(/\s+/);
  if (f.length !== 5 || f[3] !== "*") return null;
  const [mi, ho, dom, , dow] = f;
  const num = (x: string) => (/^\d+$/.test(x) ? Number(x) : null);
  const m = num(mi);
  const h = num(ho);
  const anyDay = dom === "*" && dow === "*";
  if (anyDay && ho === "*" && /^\*\/\d+$/.test(mi)) return `Every ${mi.slice(2)} minutes`;
  if (anyDay && ho === "*" && m != null) return m === 0 ? "Every hour" : `Every hour at :${pad(m)}`;
  if (anyDay && m != null && /^\*\/\d+$/.test(ho)) return `Every ${ho.slice(2)} hours${m ? ` at :${pad(m)}` : ""}`;
  if (m == null || h == null || h > 23 || m > 59) return null;
  const at = `at ${pad(h)}:${pad(m)}`;
  if (anyDay) return `Every day ${at}`;
  if (dom === "*" && dow === "1-5") return `Every weekday ${at}`;
  if (dom === "*" && /^[0-7](,[0-7])*$/.test(dow)) {
    const days = [...new Set(dow.split(",").map((d) => Number(d) % 7))].sort();
    return days.length === 1 ? `Every ${DAYS[days[0]]} ${at}` : `${listAnd(days.map((d) => `${DAYS[d]}s`))} ${at}`;
  }
  const d = num(dom);
  if (dow === "*" && d != null && d >= 1 && d <= 31) return `Every month on the ${ordinal(d)} ${at}`;
  return null;
}

function zones(): string[] {
  try {
    const list = (Intl as unknown as { supportedValuesOf(k: string): string[] }).supportedValuesOf("timeZone");
    // Chrome's list leaves out UTC itself
    return list.includes("UTC") ? list : ["UTC", ...list];
  } catch {
    return ["UTC"];
  }
}

/** How many of a schedule's latest runs its card shows. */
const RUNS = 10;

/**
 * A schedule, in time rather than as a list row: how often in words, when next,
 * and how its latest runs went (oldest to newest, left to right).
 */
function ScheduleCard({
  ctx,
  s,
  runs,
  onPlay,
  onToggle,
  onEdit,
  onRemove,
}: {
  ctx: Ctx;
  s: S["ScheduleDto"];
  runs: S["PipelineSummary"][];
  onPlay: () => void;
  onToggle: () => void;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const words = cadence(s.cron);
  const name = s.description || words || s.cron;
  return (
    <article className={`card sched-card${s.active ? "" : " paused"}`} aria-label={`Schedule ${name}`}>
      <header className="sched-head">
        <span className="sched-icon" aria-hidden>
          <CalendarClock size={18} />
        </span>
        <div className="grow" style={{ minWidth: 0 }}>
          <h2 className="sched-title ellipsis">{name}</h2>
          <p className="sched-when">
            {words ?? "Custom interval"} <span className="faint">· {s.cron_timezone}</span>
          </p>
        </div>
        {!s.active && <span className="pill">Paused</span>}
      </header>
      <dl className="sched-facts">
        <div>
          <dt>Next run</dt>
          <dd>{s.active && s.next_run_at ? <span title={new Date(s.next_run_at).toLocaleString()}>{timeUntil(s.next_run_at)}</span> : <span className="faint">paused</span>}</dd>
        </div>
        <div>
          <dt>Branch</dt>
          <dd>
            <Link className="ref-chip small" to={refLink(ctx.web, "tree", s.ref)}>
              <GitBranch size={11} /> {s.ref}
            </Link>
          </dd>
        </div>
        <div>
          <dt>Runs as</dt>
          <dd>{s.owner_username ?? <span className="faint">nobody</span>}</dd>
        </div>
        <div>
          <dt>Cron</dt>
          <dd className="mono">{s.cron}</dd>
        </div>
      </dl>
      <div className="sched-runs">
        <span className="sched-runs-label">Latest runs</span>
        {runs.length === 0 ? (
          <span className="faint small">none yet</span>
        ) : (
          <span className="sched-dots">
            {runs.map((p) => (
              <Link key={p.id} to={`${ctx.web}/-/pipelines/${p.iid}`} className={`st-${p.status}`} title={`#${p.iid} ${statusLabel(p.status).toLowerCase()} · ${timeAgo(p.created_at)}`}>
                <StatusIcon status={p.status} size={16} />
              </Link>
            ))}
          </span>
        )}
      </div>
      <footer className="sched-actions">
        <button className="btn small" onClick={onPlay}>
          <Play size={13} /> Run now
        </button>
        <button className="btn small" onClick={onToggle}>
          {s.active ? <Pause size={13} /> : <RotateCw size={13} />} {s.active ? "Pause" : "Resume"}
        </button>
        <span className="grow" />
        <button className="icon-btn" aria-label="Edit schedule" title="Edit" onClick={onEdit}>
          <Pencil size={15} />
        </button>
        <button className="icon-btn" aria-label="Delete schedule" title="Delete" onClick={onRemove}>
          <Trash2 size={15} />
        </button>
      </footer>
    </article>
  );
}

export function Schedules({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const toast = useToast();
  const confirm = useConfirm();
  const { user } = useSession();
  const base = `/projects/${ctx.p.id}/pipeline_schedules`;
  const q = useQuery({ queryKey: ["schedules", ctx.p.id], queryFn: () => api.get<S["ScheduleDto"][]>(base) });
  const branches = useBranches(ctx);
  const [form, setForm] = useState<Form | null>(null);
  const [error, setError] = useState<unknown>(null);
  // a browser without a configured zone reports one the server does not know
  // (Etc/Unknown in containers): offer UTC then
  const browserTz = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const tz = browserTz && zones().includes(browserTz) ? browserTz : "UTC";

  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (!form) return;
    setError(null);
    try {
      const body = { ...form, variables: form.variables.filter((v) => v.key) };
      if (form.id) await api.put(`${base}/${form.id}`, body);
      else await api.post(base, body);
      toast.ok("Schedule saved");
      setForm(null);
      qc.invalidateQueries({ queryKey: ["schedules", ctx.p.id] });
    } catch (err) {
      setError(err);
    }
  };
  const play = async (id: number) => {
    try {
      const r = await api.post<{ pipeline_iid?: number }>(`${base}/${id}/play`);
      toast.ok(r.pipeline_iid ? `Started pipeline #${r.pipeline_iid}` : "Nothing to run for this ref");
      qc.invalidateQueries({ queryKey: ["schedules", ctx.p.id] });
    } catch (e) {
      toast.error(e);
    }
  };
  // saving a schedule makes you its owner (its pipelines run as you): said first when it is someone else's
  const toggle = async (s: S["ScheduleDto"]) => {
    const theirs = s.owner_username && s.owner_username !== user?.username;
    if (
      theirs &&
      !(await confirm({
        title: `${s.active ? "Pause" : "Resume"} ${s.description || "this schedule"}?`,
        body: `It runs as ${s.owner_username} today. Saving it makes it yours: once it runs, it runs as you.`,
        action: s.active ? "Pause schedule" : "Resume schedule",
      }))
    )
      return;
    try {
      await api.put(`${base}/${s.id}`, { description: s.description, ref: s.ref, cron: s.cron, cron_timezone: s.cron_timezone, active: !s.active, variables: s.variables });
      toast.ok(s.active ? "Schedule paused" : "Schedule resumed");
      qc.invalidateQueries({ queryKey: ["schedules", ctx.p.id] });
    } catch (e) {
      toast.error(e);
    }
  };
  // each schedule's latest runs: the pipelines it started, however busy its branch
  const ids = (q.data ?? []).map((s) => s.id);
  const recent = useQueries({
    queries: ids.map((id) => ({
      queryKey: ["pipelines", ctx.p.id, "schedule-runs", id],
      queryFn: () => api.get<S["PipelineSummary"][]>(`/projects/${ctx.p.id}/pipelines${qs({ schedule_id: id, per_page: RUNS })}`),
      staleTime: 30_000,
    })),
  });
  const runsOf = (s: S["ScheduleDto"]) => [...(recent[ids.indexOf(s.id)]?.data ?? [])].reverse();
  const remove = async (id: number) => {
    if (!(await confirm({ title: "Delete this schedule?", body: "Its pipelines stop being started; those it started stay.", action: "Delete schedule", danger: true }))) return;
    try {
      await api.del(`${base}/${id}`);
      qc.invalidateQueries({ queryKey: ["schedules", ctx.p.id] });
    } catch (e) {
      toast.error(e);
    }
  };

  return (
    <div className="stack list-page">
      <ListHead title="Pipeline schedules" sub="Pipelines that run on their own, at the times you set.">
        <button className="btn primary" onClick={() => setForm({ description: "", ref: ctx.p.default_branch ?? "main", cron: "0 3 * * *", cron_timezone: tz, active: true, variables: [] })}>
          <Plus size={15} /> New schedule
        </button>
      </ListHead>
      {form && (
        <form className="card card-body stack" onSubmit={save} style={{ maxWidth: 760 }}>
          <ErrorBox error={error} />
          <Field label="Description">
            <input value={form.description} onChange={(e) => setForm({ ...form, description: e.target.value })} placeholder="Nightly build" />
          </Field>
          <div className="grid2">
            <Field label="Interval" hint="Cron syntax: minute hour day month weekday">
              <input value={form.cron} onChange={(e) => setForm({ ...form, cron: e.target.value })} style={{ fontFamily: "var(--mono)" }} />
            </Field>
            <Field label="Time zone">
              <Select value={form.cron_timezone} onChange={(v) => setForm({ ...form, cron_timezone: v })} options={zones().map((z) => ({ value: z, label: z }))} />
            </Field>
          </div>
          <div className="row wrap">
            {PRESETS.map(([label, cron]) => (
              <button type="button" key={cron} className={`btn small ${form.cron === cron ? "primary" : ""}`} onClick={() => setForm({ ...form, cron })}>
                {label}
              </button>
            ))}
          </div>
          <Field label="Branch">
            <Select value={form.ref} onChange={(v) => setForm({ ...form, ref: v })} options={(branches.data ?? []).map((b) => ({ value: b.name, label: b.name }))} />
          </Field>
          <FieldGroup label="Variables">
            <VariableRows rows={form.variables} setRows={(variables) => setForm({ ...form, variables })} />
          </FieldGroup>
          <label className="check">
            <input type="checkbox" checked={form.active} onChange={(e) => setForm({ ...form, active: e.target.checked })} /> Active
          </label>
          <p className="small muted" style={{ margin: 0 }}>
            Pipelines run as the person who last saved the schedule.
          </p>
          <div className="row">
            <button className="btn primary">Save schedule</button>
            <button type="button" className="btn" onClick={() => setForm(null)}>
              Cancel
            </button>
          </div>
        </form>
      )}
      <Q q={q}>
        {(list) =>
          list.length === 0 ? (
            <Empty>No schedules. Run pipelines nightly or weekly, for example to rebuild with fresh dependencies.</Empty>
          ) : (
            <div className="sched-grid">
              {list.map((s) => (
                <ScheduleCard
                  key={s.id}
                  ctx={ctx}
                  s={s}
                  runs={runsOf(s)}
                  onPlay={() => play(s.id)}
                  onToggle={() => toggle(s)}
                  onEdit={() =>
                    setForm({ id: s.id, description: s.description, ref: s.ref, cron: s.cron, cron_timezone: s.cron_timezone, active: s.active, variables: s.variables.map((v) => ({ ...v })) })
                  }
                  onRemove={() => remove(s.id)}
                />
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}
