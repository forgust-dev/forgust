import { useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { GitBranch, Pause, Pencil, Play, Plus, RotateCw, Trash2 } from "lucide-react";
import { api, listAnd, qs, refLink, timeAgo, timeUntil, type S } from "../../../api/client";
import { useSession } from "../../../session";
import { Empty, ErrorBox, Field, FieldGroup, Q, ListHead, StateBadge } from "../../../components/ui";
import { Select } from "../../../components/controls";
import { useToast } from "../../../shell/toast";
import { useConfirm } from "../../../shell/dialog";
import { FormActions, FormDialog, useJustAdded } from "../../../components/FormDialog";
import type { Ctx } from "../Project";
import { VariableRows } from "./Pipelines";
import { StatusIcon, statusLabel } from "./Status";
import { useBranches } from "../Code";

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

/** How often, as the form asks it; a cron it cannot say is "custom" and kept as written. */
type Repeat = "hour" | "day" | "weekday" | "week" | "month" | "custom";
type When = { repeat: Exclude<Repeat, "custom">; minute: number; hour: number; weekday: number; monthday: number };

const REPEATS: { value: Repeat; label: string }[] = [
  { value: "hour", label: "Every hour" },
  { value: "day", label: "Every day" },
  { value: "weekday", label: "Every weekday (Monday to Friday)" },
  { value: "week", label: "Every week" },
  { value: "month", label: "Every month" },
  { value: "custom", label: "Custom (a cron line)" },
];
const DEFAULT_WHEN: When = { repeat: "day", minute: 0, hour: 3, weekday: 1, monthday: 1 };

/** The form's reading of a cron line, or null when the form cannot say it. */
function whenOf(cron: string): When | null {
  const f = cron.trim().split(/\s+/);
  if (f.length !== 5 || f[3] !== "*") return null;
  const [mi, ho, dom, , dow] = f;
  const num = (x: string, max: number) => (/^\d+$/.test(x) && Number(x) <= max ? Number(x) : null);
  const minute = num(mi, 59);
  if (minute == null) return null;
  if (ho === "*" && dom === "*" && dow === "*") return { ...DEFAULT_WHEN, repeat: "hour", minute };
  const hour = num(ho, 23);
  if (hour == null) return null;
  const at = { ...DEFAULT_WHEN, minute, hour };
  if (dom === "*" && dow === "*") return { ...at, repeat: "day" };
  if (dom === "*" && dow === "1-5") return { ...at, repeat: "weekday" };
  const wd = num(dow, 7);
  if (dom === "*" && wd != null) return { ...at, repeat: "week", weekday: wd % 7 };
  const md = num(dom, 31);
  if (dow === "*" && md != null && md >= 1) return { ...at, repeat: "month", monthday: md };
  return null;
}

function cronOf(w: When): string {
  if (w.repeat === "hour") return `${w.minute} * * * *`;
  const at = `${w.minute} ${w.hour}`;
  if (w.repeat === "day") return `${at} * * *`;
  if (w.repeat === "weekday") return `${at} * * 1-5`;
  if (w.repeat === "week") return `${at} * * ${w.weekday}`;
  return `${at} ${w.monthday} * *`;
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

const HOURS = Array.from({ length: 24 }, (_, h) => ({ value: String(h), label: pad(h) }));
/** Every 5 minutes, and the one a schedule already has if it falls between. */
const minutes = (current: number) =>
  [...new Set([...Array.from({ length: 12 }, (_, i) => i * 5), current])].sort((a, b) => a - b).map((m) => ({ value: String(m), label: pad(m) }));
const WEEKDAYS = [1, 2, 3, 4, 5, 6, 0].map((d) => ({ value: String(d), label: DAYS[d] }));
const MONTHDAYS = Array.from({ length: 31 }, (_, i) => ({ value: String(i + 1), label: ordinal(i + 1) }));

/** How many of a schedule's latest runs its row shows. */
const RUNS = 10;

type Form = {
  id?: number;
  /** who its pipelines run as today (an existing schedule) */
  owner?: string | null;
  description: string;
  ref: string;
  cron: string;
  cron_timezone: string;
  /** an existing schedule keeps its own: editing a paused one leaves it paused */
  active: boolean;
  variables: { key: string; value: string }[];
  custom: boolean;
};

/** When it runs, asked as a sentence: "every weekday, at 03:00"; a cron line only if wanted. */
function WhenFields({ form, setForm }: { form: Form; setForm: (f: Form) => void }) {
  const when = whenOf(form.cron) ?? DEFAULT_WHEN;
  const set = (patch: Partial<When>) => setForm({ ...form, cron: cronOf({ ...when, ...patch }), custom: false });
  const repeat: Repeat = form.custom ? "custom" : when.repeat;
  const words = cadence(form.cron);
  return (
    <div className="stack" style={{ gap: 8 }}>
      <div className="sched-when">
        <Field label="Repeat">
          <Select
            value={repeat}
            onChange={(v) => (v === "custom" ? setForm({ ...form, custom: true }) : set({ repeat: v as When["repeat"] }))}
            options={REPEATS}
          />
        </Field>
        {repeat === "week" && (
          <Field label="On">
            <Select value={String(when.weekday)} onChange={(v) => set({ weekday: Number(v) })} options={WEEKDAYS} />
          </Field>
        )}
        {repeat === "month" && (
          <Field label="On the">
            <Select value={String(when.monthday)} onChange={(v) => set({ monthday: Number(v) })} options={MONTHDAYS} />
          </Field>
        )}
        {repeat === "hour" && (
          <Field label="At minute">
            <Select value={String(when.minute)} onChange={(v) => set({ minute: Number(v) })} options={minutes(when.minute)} />
          </Field>
        )}
        {repeat !== "hour" && repeat !== "custom" && (
          <>
            <Field label="Hour">
              <Select value={String(when.hour)} onChange={(v) => set({ hour: Number(v) })} options={HOURS} />
            </Field>
            <Field label="Minute">
              <Select value={String(when.minute)} onChange={(v) => set({ minute: Number(v) })} options={minutes(when.minute)} />
            </Field>
          </>
        )}
        {repeat === "custom" && (
          <Field label="Cron" hint="minute hour day-of-month month day-of-week">
            <input className="mono" value={form.cron} onChange={(e) => setForm({ ...form, cron: e.target.value })} placeholder="0 3 * * 1-5" />
          </Field>
        )}
        <Field label="Time zone">
          <Select value={form.cron_timezone} onChange={(v) => setForm({ ...form, cron_timezone: v })} options={zones().map((z) => ({ value: z, label: z }))} />
        </Field>
      </div>
      <p className="sched-summary">
        {words ? (
          <>
            Runs <strong>{words.charAt(0).toLowerCase() + words.slice(1)}</strong>, {form.cron_timezone} time.
          </>
        ) : (
          <>Runs at the times of the cron line, {form.cron_timezone} time.</>
        )}
      </p>
    </div>
  );
}

/** A schedule, said in words: how often, on which branch, when next, and how it went lately. */
function ScheduleRow({
  ctx,
  s,
  runs,
  onPlay,
  onToggle,
  onEdit,
  onRemove,
  fresh = "",
}: {
  ctx: Ctx;
  s: S["ScheduleDto"];
  /** " just-added" when it was just added or saved */
  fresh?: string;
  runs: S["PipelineSummary"][];
  onPlay: () => void;
  onToggle: () => void;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const words = cadence(s.cron);
  // without a name, it is called by when it runs: the sentence then does not say it twice
  const name = s.description || words || "Custom schedule";
  const said = words != null && !!s.description;
  return (
    <article className={`sched-row${s.active ? "" : " paused"}${fresh}`} aria-label={`Schedule ${name}`}>
      <div className="sched-main">
        <div className="sched-name-line">
          <h2 className="sched-name ellipsis">{name}</h2>
          {!s.active && (
            <StateBadge tone="neutral" icon={Pause}>
              Paused
            </StateBadge>
          )}
        </div>
        <p className="sched-sentence" title={`cron: ${s.cron}`}>
          {words == null ? (
            <span>
              Custom: <code>{s.cron}</code>
            </span>
          ) : (
            said && <span>{words}</span>
          )}
          <span className="faint">{words == null || said ? `(${s.cron_timezone})` : `${s.cron_timezone} time`}</span>
          <span>on</span>
          <Link className="ref-chip small" to={refLink(ctx.web, "tree", s.ref)}>
            <GitBranch size={11} /> {s.ref}
          </Link>
        </p>
        <p className="sched-meta">
          {s.active ? (
            s.next_run_at ? (
              <span title={new Date(s.next_run_at).toLocaleString()}>Next run {timeUntil(s.next_run_at)}</span>
            ) : (
              <span>Next run not planned yet</span>
            )
          ) : (
            <span>Paused: it runs again once resumed</span>
          )}
          <span aria-hidden>·</span>
          <span>runs as {s.owner_username ?? "nobody"}</span>
          {runs.length > 0 && (
            <>
              <span aria-hidden>·</span>
              <span>last runs</span>
              <span className="sched-dots">
                {runs.map((p) => (
                  <Link key={p.id} to={`${ctx.web}/-/pipelines/${p.iid}`} className={`st-${p.status}`} title={`#${p.iid} ${statusLabel(p.status).toLowerCase()} · ${timeAgo(p.created_at)}`}>
                    <StatusIcon status={p.status} size={15} />
                  </Link>
                ))}
              </span>
            </>
          )}
        </p>
      </div>
      <div className="sched-actions">
        <button className="btn small" onClick={onPlay}>
          <Play size={13} /> Run now
        </button>
        <button className="btn small" onClick={onToggle}>
          {s.active ? <Pause size={13} /> : <RotateCw size={13} />} {s.active ? "Pause" : "Resume"}
        </button>
        <button className="icon-btn" aria-label="Edit schedule" title="Edit" onClick={onEdit}>
          <Pencil size={15} />
        </button>
        <button className="icon-btn" aria-label="Delete schedule" title="Delete" onClick={onRemove}>
          <Trash2 size={15} />
        </button>
      </div>
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
  const fresh = useJustAdded();
  const close = () => {
    setForm(null);
    setError(null);
  };
  // a browser without a configured zone reports one the server does not know
  // (Etc/Unknown in containers): offer UTC then
  const browserTz = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const tz = browserTz && zones().includes(browserTz) ? browserTz : "UTC";
  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (!form) return;
    setError(null);
    try {
      const body = { description: form.description, ref: form.ref, cron: form.cron.trim(), cron_timezone: form.cron_timezone, active: form.active, variables: form.variables.filter((v) => v.key) };
      if (form.id) await api.put(`${base}/${form.id}`, body);
      const id = form.id ?? (await api.post<S["ScheduleDto"]>(base, body)).id;
      fresh.mark(id);
      toast.ok("Schedule saved");
      close();
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
  const edit = (s: S["ScheduleDto"]) =>
    setForm({
      id: s.id,
      owner: s.owner_username,
      description: s.description,
      ref: s.ref,
      cron: s.cron,
      cron_timezone: s.cron_timezone,
      active: s.active,
      variables: s.variables.map((v) => ({ ...v })),
      custom: whenOf(s.cron) == null,
    });
  const theirs = form?.owner && form.owner !== user?.username;

  return (
    <div className="stack list-page">
      <ListHead title="Pipeline schedules" sub="Pipelines that run on their own, at the times you set.">
        <button
          className="btn primary"
          onClick={() => setForm({ description: "", ref: ctx.p.default_branch ?? "main", cron: cronOf(DEFAULT_WHEN), cron_timezone: tz, active: true, variables: [], custom: false })}
        >
          <Plus size={15} /> Add a schedule
        </button>
      </ListHead>
      <FormDialog open={form != null} onClose={close} title={form?.id ? "Edit the schedule" : "Add a schedule"}>
        {form && (
        <form className="stack sched-form" onSubmit={save}>
          <ErrorBox error={error} />
          <Field label="Name" hint="Optional: without one, the schedule is called by when it runs">
            <input value={form.description} onChange={(e) => setForm({ ...form, description: e.target.value })} placeholder="Nightly build" />
          </Field>
          <WhenFields form={form} setForm={setForm} />
          <Field label="Branch">
            <Select value={form.ref} onChange={(v) => setForm({ ...form, ref: v })} options={(branches.data ?? []).map((b) => ({ value: b.name, label: b.name }))} />
          </Field>
          <FieldGroup label="Variables" hint="Optional: passed to the pipelines it starts">
            <VariableRows rows={form.variables} setRows={(variables) => setForm({ ...form, variables })} />
          </FieldGroup>
          <p className="small muted" style={{ margin: 0 }}>
            {theirs ? `It runs as ${form.owner} today. Saving makes it yours: its pipelines will run as you.` : "Its pipelines run as you, with your access."}
          </p>
          <FormActions label="Save schedule" onCancel={close} />
        </form>
        )}
      </FormDialog>
      <Q q={q}>
        {(list) =>
          list.length === 0 ? (
            <Empty>No schedules yet. A schedule runs a pipeline on its own, for example every night to check what changed in your dependencies.</Empty>
          ) : (
            <div className="sched-list">
              {list.map((s) => (
                <ScheduleRow key={s.id} ctx={ctx} s={s} runs={runsOf(s)} onPlay={() => play(s.id)} onToggle={() => toggle(s)} onEdit={() => edit(s)} onRemove={() => remove(s.id)} fresh={fresh.fresh(s.id)} />
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}
