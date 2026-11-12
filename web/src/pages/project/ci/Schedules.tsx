import { useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarClock, Pencil, Play, Plus, Trash2 } from "lucide-react";
import { api, type S } from "../../../api/client";
import { Empty, ErrorBox, Field, FieldGroup, Q, ListHead } from "../../../components/ui";
import { Select } from "../../../components/controls";
import { useToast } from "../../../shell/toast";
import type { Ctx } from "../Project";
import { VariableRows } from "./Pipelines";
import { StatusIcon } from "./Status";
import { useBranches } from "../Code";

type Form = { id?: number; description: string; ref: string; cron: string; cron_timezone: string; active: boolean; variables: { key: string; value: string }[] };

const PRESETS: [string, string][] = [
  ["Every day (03:00)", "0 3 * * *"],
  ["Every weekday (03:00)", "0 3 * * 1-5"],
  ["Every week (Sunday 03:00)", "0 3 * * 0"],
  ["Every hour", "0 * * * *"],
];

function zones(): string[] {
  try {
    const list = (Intl as unknown as { supportedValuesOf(k: string): string[] }).supportedValuesOf("timeZone");
    // Chrome's list leaves out UTC itself
    return list.includes("UTC") ? list : ["UTC", ...list];
  } catch {
    return ["UTC"];
  }
}

export function Schedules({ ctx }: { ctx: Ctx }) {
  const qc = useQueryClient();
  const toast = useToast();
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
  const remove = async (id: number) => {
    if (!confirm("Delete this schedule?")) return;
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
            <div className="list rows">
              {list.map((s) => (
                <div className="item" key={s.id}>
                  <span className={`row-icon ${s.active ? "ic-accent" : "faint"}`}>
                    <CalendarClock size={16} />
                  </span>
                  <div className="grow" style={{ minWidth: 0 }}>
                    <div className="row-title">
                      <span className="ellipsis">{s.description || s.cron}</span>
                      {!s.active && <span className="pill">inactive</span>}
                    </div>
                    <div className="row-meta">
                      <span>
                        {s.cron} ({s.cron_timezone})
                      </span>
                      <span className="ref-chip small">{s.ref}</span>
                      {s.next_run_at && s.active && <span>next {new Date(s.next_run_at).toLocaleString()}</span>}
                      {s.owner_username && <span>runs as {s.owner_username}</span>}
                    </div>
                  </div>
                  {s.last_pipeline && (
                    <Link to={`${ctx.web}/-/pipelines/${s.last_pipeline[0]}`} className="row small" style={{ gap: 4 }} title="Last pipeline">
                      <StatusIcon status={s.last_pipeline[1]} /> #{s.last_pipeline[0]}
                    </Link>
                  )}
                  <button className="icon-btn" aria-label="Run now" title="Run now" onClick={() => play(s.id)}>
                    <Play size={15} />
                  </button>
                  <button
                    className="icon-btn"
                    aria-label="Edit schedule"
                    onClick={() =>
                      setForm({ id: s.id, description: s.description, ref: s.ref, cron: s.cron, cron_timezone: s.cron_timezone, active: s.active, variables: s.variables.map((v) => ({ ...v })) })
                    }
                  >
                    <Pencil size={15} />
                  </button>
                  <button className="icon-btn" aria-label="Delete schedule" onClick={() => remove(s.id)}>
                    <Trash2 size={15} />
                  </button>
                </div>
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}
