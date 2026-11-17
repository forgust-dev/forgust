import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowRight, CalendarDays, ChevronDown, ChevronRight, Clock3, ExternalLink, Folder, GitBranch, Layers, Play, Rocket, RotateCcw, Server, ShieldCheck, Square, Tag, Timer, Trash2, XCircle } from "lucide-react";
import { api, refLink, timeAgo, timeUntil, type S } from "../../../api/client";
import { Badge, Empty, ListHead, Q, Tabs } from "../../../components/ui";
import { DecideCard } from "../../../components/decide";
import { useConfirm } from "../../../shell/dialog";
import { useAct, useToast } from "../../../shell/toast";
import { useCiLive } from "../../../shell/live";
import type { Ctx } from "../Project";
import { StatusIcon, duration, statusLabel } from "./Status";
import { useConfirmRun } from "./Pipelines";

type Env = S["Environment"];
type Deployment = S["Deployment"];

/** Production first, as GitLab orders them. */
const TIERS = ["production", "staging", "testing", "development", "other"];
const tierRank = (t: string) => (TIERS.indexOf(t) + TIERS.length + 1) % (TIERS.length + 1);

const STATE: Record<string, { label: string; tone: string }> = {
  available: { label: "Available", tone: "opened" },
  stopping: { label: "Stopping", tone: "draft" },
  stopped: { label: "Stopped", tone: "closed" },
};

/** A link to an environment's site, when it is a web address. */
const siteOf = (e: Env) => (e.external_url && /^https?:\/\//i.test(e.external_url) ? e.external_url : null);

/** The folder of `review/feature-x`: review apps fold together. */
const folderOf = (name: string) => {
  const i = name.indexOf("/");
  return i > 0 ? name.slice(0, i) : null;
};

/** Stopping an environment, asked first in the app's dialog, with what it does. */
function useStop(ctx: Ctx) {
  const qc = useQueryClient();
  const toast = useToast();
  const confirm = useConfirm();
  return async (e: Env) => {
    const ok = await confirm({
      title: `Stop ${e.name}?`,
      body: e.stop_build_id
        ? "Its stop job runs, as you: the environment is stopped once it is done. Its deployments stay in its history."
        : "It has no stop job: it is marked stopped at once. Its deployments stay in its history.",
      action: "Stop environment",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.post(`/projects/${ctx.p.id}/environments/${e.id}/stop`);
      toast.ok(e.stop_build_id ? `Stopping ${e.name}` : `${e.name} stopped`);
      qc.invalidateQueries({ queryKey: ["environments", ctx.p.id] });
      qc.invalidateQueries({ queryKey: ["environment", ctx.p.id, e.id] });
    } catch (err) {
      toast.error(err);
    }
  };
}

/** Environments: where jobs deploy, what runs in each, stopping them. */
export function Environments({ ctx, stopped }: { ctx: Ctx; stopped: boolean }) {
  useCiLive(ctx.p.id);
  const stop = useStop(ctx);
  const q = useQuery({
    queryKey: ["environments", ctx.p.id, stopped],
    queryFn: () => api.get<S["EnvironmentRow"][]>(`/projects/${ctx.p.id}/environments?state=${stopped ? "stopped" : "available"}`),
  });
  return (
    <div className="stack list-page">
      <ListHead title="Environments" sub="Where jobs deploy: what runs in each, and since when." />
      <Tabs
        items={[
          { to: `${ctx.web}/-/environments`, label: "Available", end: true },
          { to: `${ctx.web}/-/environments/stopped`, label: "Stopped" },
        ]}
      />
      <Q q={q}>
        {(list) => {
          if (list.length === 0)
            return (
              <Empty>
                <Server size={28} />
                <p>{stopped ? "No stopped environments." : "No environments yet. A job with environment: creates one when it runs."}</p>
              </Empty>
            );
          // the ones of their own first, by tier; then each folder (review/…) as a group
          const top = list.filter((e) => !folderOf(e.name)).sort((a, b) => tierRank(a.tier) - tierRank(b.tier) || a.name.localeCompare(b.name));
          const folders = new Map<string, S["EnvironmentRow"][]>();
          for (const e of list) {
            const f = folderOf(e.name);
            if (f) folders.set(f, [...(folders.get(f) ?? []), e]);
          }
          return (
            <div className="list rows env-list">
              {top.map((e) => (
                <EnvRow key={e.id} ctx={ctx} e={e} onStop={stop} />
              ))}
              {[...folders].map(([name, envs]) => (
                <EnvFolder key={name} ctx={ctx} name={name} envs={envs} onStop={stop} />
              ))}
            </div>
          );
        }}
      </Q>
    </div>
  );
}

/** A folder of environments (review apps): open when it holds a few. */
function EnvFolder({ ctx, name, envs, onStop }: { ctx: Ctx; name: string; envs: S["EnvironmentRow"][]; onStop: (e: Env) => void }) {
  const [open, setOpen] = useState(envs.length <= 3);
  const newest = envs.map((e) => e.last_deployment?.created_at ?? "").sort().at(-1);
  const sorted = [...envs].sort((a, b) => (b.last_deployment?.created_at ?? "").localeCompare(a.last_deployment?.created_at ?? ""));
  return (
    <div className="list-group">
      <button type="button" className="item env-folder" aria-expanded={open} onClick={() => setOpen(!open)}>
        {open ? <ChevronDown size={15} /> : <ChevronRight size={15} />}
        <Folder size={16} className="faint" />
        <span className="row-title">{name}</span>
        <span className="section-count">{envs.length}</span>
        <span className="grow" />
        {newest && <span className="row-time">last deployed {timeAgo(newest)}</span>}
      </button>
      {open &&
        sorted.map((e) => (
          <EnvRow key={e.id} ctx={ctx} e={e} short={e.name.slice(name.length + 1)} onStop={onStop} />
        ))}
    </div>
  );
}

function EnvRow({ ctx, e, short, onStop }: { ctx: Ctx; e: S["EnvironmentRow"]; short?: string; onStop: (e: Env) => void }) {
  const d = e.last_deployment;
  const site = siteOf(e);
  return (
    <section className={`item env-row${short ? " in-folder" : ""}`} aria-label={`Environment ${e.name}`}>
      <span className={`row-icon ${d ? `st-${d.status}` : "faint"}`}>{d ? <StatusIcon status={d.status} size={16} /> : <Server size={16} />}</span>
      <div className="grow" style={{ minWidth: 0 }}>
        <div className="row-title">
          <Link to={`${ctx.web}/-/environments/${e.id}`} className="ellipsis">
            {short ?? e.name}
          </Link>
          <span className={`pill${e.tier === "production" ? " warn" : ""}`}>{e.tier}</span>
          {e.state !== "available" && <span className="pill">{STATE[e.state]?.label ?? e.state}</span>}
        </div>
        <div className="row-meta">
          {d ? (
            <>
              <span>
                #{d.iid} {statusLabel(d.status).toLowerCase()}
              </span>
              <span>
                {d.ref} @{d.sha.slice(0, 8)}
              </span>
              {d.username && <span>by {d.username}</span>}
              <span title={new Date(d.created_at).toLocaleString()}>{timeAgo(d.created_at)}</span>
            </>
          ) : (
            <span>never deployed</span>
          )}
          {e.auto_stop_at && e.state === "available" && <span>stops {timeUntil(e.auto_stop_at)}</span>}
        </div>
      </div>
      {site && (
        <a className="btn small" href={site} target="_blank" rel="noreferrer noopener">
          <ExternalLink size={13} /> Open
        </a>
      )}
      {e.state === "available" && e.can_stop && (
        <button className="btn small" onClick={() => onStop(e)}>
          Stop
        </button>
      )}
    </section>
  );
}

/** An environment's page: what runs there, its deployments, and what to do about it. */
export function EnvironmentPage({ ctx, id }: { ctx: Ctx; id: number }) {
  useCiLive(ctx.p.id);
  const q = useQuery({ queryKey: ["environment", ctx.p.id, id], queryFn: () => api.get<S["EnvironmentDetail"]>(`/projects/${ctx.p.id}/environments/${id}`) });
  return <Q q={q}>{(e) => <EnvironmentView ctx={ctx} e={e} />}</Q>;
}

function EnvironmentView({ ctx, e }: { ctx: Ctx; e: S["EnvironmentDetail"] }) {
  const nav = useNavigate();
  const qc = useQueryClient();
  const toast = useToast();
  const confirm = useConfirm();
  const stop = useStop(ctx);
  // what runs there: the latest deployment that went through
  const live = e.state === "available" ? e.deployments.find((d) => d.status === "success") : undefined;
  const last = e.deployments[0];
  const site = siteOf(e);
  const state = STATE[e.state] ?? { label: e.state, tone: "draft" };
  const remove = async () => {
    const ok = await confirm({
      title: `Delete ${e.name}?`,
      body: "Its deployment history goes with it. A job with this environment makes it again.",
      action: "Delete environment",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.del(`/projects/${ctx.p.id}/environments/${e.id}`);
      toast.ok(`${e.name} deleted`);
      qc.invalidateQueries({ queryKey: ["environments", ctx.p.id] });
      nav(`${ctx.web}/-/environments/stopped`);
    } catch (err) {
      toast.error(err);
    }
  };
  return (
    <div className="stack env-page">
      <header className="mr-head">
        <div className="mr-status">
          <span className={`mr-state ${state.tone}`}>{state.label}</span>
          <span>{e.tier}</span>
          <span aria-hidden="true">·</span>
          <span>created {timeAgo(e.created_at)}</span>
        </div>
        <div className="row wrap" style={{ alignItems: "flex-start", gap: 8 }}>
          <h1 className="mr-title grow">{e.name}</h1>
          {site && (
            <a className="btn" href={site} target="_blank" rel="noreferrer noopener">
              <ExternalLink size={14} /> Open
            </a>
          )}
          {e.state === "available" && e.can_stop && (
            <button className="btn" onClick={() => stop(e)}>
              <Square size={13} /> Stop
            </button>
          )}
          {e.state === "stopped" && e.can_stop && (
            <button className="btn danger" onClick={remove}>
              <Trash2 size={14} /> Delete
            </button>
          )}
        </div>
      </header>
      {last && e.state === "available" && <DeployDecision ctx={ctx} e={e} d={last} />}
      <div className="repo-layout env-layout">
        <section className="repo-main stack" style={{ gap: 8 }} aria-label="Deployments">
          <h2 className="section-title">
            Deployments<span className="section-count"> {e.deployments.length}</span>
          </h2>
          {e.deployments.length === 0 ? (
            <Empty>Nothing deployed here yet.</Empty>
          ) : (
            <div className="list rows">
              {e.deployments.map((d) => (
                <DeploymentRow key={d.id} ctx={ctx} d={d} live={d.id === live?.id} />
              ))}
            </div>
          )}
        </section>
        <aside className="repo-about" aria-label={`About ${e.name}`}>
          {live && (
            <section className="side-card ci-card" aria-label="Deployed now">
              <h2>Deployed now</h2>
              <p>
                #{live.iid} · {live.tag ? <Tag size={12} /> : <GitBranch size={12} />} <Link to={refLink(ctx.web, "tree", live.ref)}>{live.ref}</Link> @
                <Link className="mono" to={`${ctx.web}/-/commit/${live.sha}`}>
                  {live.sha.slice(0, 8)}
                </Link>
                {live.username && <> · by {live.username}</>} · {timeAgo(live.finished_at ?? live.created_at)}
              </p>
              {live.build_id && (
                <Link className="ci-link" to={`${ctx.web}/-/jobs/${live.build_id}`}>
                  Its deployment job <ArrowRight size={14} />
                </Link>
              )}
            </section>
          )}
          <section className="side-card">
            <h2>About</h2>
            <ul>
              {site && (
                <li>
                  <ExternalLink size={15} />
                  <a className="ellipsis" href={site} target="_blank" rel="noreferrer noopener">
                    {site.replace(/^https?:\/\//i, "")}
                  </a>
                </li>
              )}
              <li>
                <Layers size={15} />
                <span>Tier: {e.tier}</span>
              </li>
              {e.protected && (
                <li>
                  <ShieldCheck size={15} />
                  <span>Protected: only allowed roles deploy or stop it</span>
                </li>
              )}
              <li>
                <Rocket size={15} />
                <span>
                  {e.deployments.length} deployment{e.deployments.length === 1 ? "" : "s"}
                </span>
              </li>
              {e.auto_stop_at && e.state === "available" && (
                <li>
                  <Timer size={15} />
                  <span title={new Date(e.auto_stop_at).toLocaleString()}>Stops by itself {timeUntil(e.auto_stop_at)}</span>
                </li>
              )}
              {e.stop_build_id && (
                <li>
                  <Square size={15} />
                  <Link to={`${ctx.web}/-/jobs/${e.stop_build_id}`}>Its stop job</Link>
                </li>
              )}
              <li>
                <CalendarDays size={15} />
                <span>Created {timeAgo(e.created_at)}</span>
              </li>
            </ul>
          </section>
        </aside>
      </div>
    </div>
  );
}

/**
 * The page's emphasized card, when the latest deployment waits on someone: a
 * manual job to start, or a failed one to run again. None otherwise.
 */
function DeployDecision({ ctx, e, d }: { ctx: Ctx; e: S["EnvironmentDetail"]; d: Deployment }) {
  const qc = useQueryClient();
  const confirmRun = useConfirmRun();
  const job = useQuery({
    queryKey: ["job", ctx.p.id, d.build_id],
    queryFn: () => api.get<S["JobDetail"]>(`/projects/${ctx.p.id}/jobs/${d.build_id}`),
    enabled: d.build_id != null && d.status !== "success" && d.status !== "running",
  });
  const run = useAct(() => {
    qc.invalidateQueries({ queryKey: ["environment", ctx.p.id] });
    qc.invalidateQueries({ queryKey: ["environments", ctx.p.id] });
    qc.invalidateQueries({ queryKey: ["job", ctx.p.id] });
  });
  const j = job.data;
  if (!j || !j.can_run) return null;
  const where = (
    <>
      <span className="mono">{d.ref}</span> @<span className="mono">{d.sha.slice(0, 8)}</span>
    </>
  );
  if (j.status === "manual") {
    return (
      <DecideCard
        label="Waiting for you"
        eyebrow={
          <>
            <Clock3 size={14} aria-hidden /> Waiting for you
          </>
        }
        title={`Deploy #${d.iid} to ${e.name}`}
        sub={<>{where}: {j.name} is a manual job, it runs when someone starts it.</>}
        actions={
          <button
            type="button"
            className="btn run"
            onClick={async () => {
              if (await confirmRun(j)) run(() => api.post(`/projects/${ctx.p.id}/jobs/${j.id}/play`), `Started ${j.name}`);
            }}
          >
            <Play size={14} /> Start
          </button>
        }
      />
    );
  }
  if (j.status === "failed") {
    return (
      <DecideCard
        label="The deployment failed"
        late
        eyebrow={
          <>
            <XCircle size={14} aria-hidden /> Failed{j.finished_at ? ` ${timeAgo(j.finished_at)}` : ""}
          </>
        }
        title={`Deploy #${d.iid} to ${e.name} failed`}
        sub={
          <>
            {where}
            {j.failure_reason ? `: ${j.failure_reason.replaceAll("_", " ")}` : ""}. Look at its log, or run it again.
          </>
        }
        actions={
          <>
            <button type="button" className="btn run" onClick={() => run(() => api.post(`/projects/${ctx.p.id}/jobs/${j.id}/retry`), `Retrying ${j.name}`)}>
              <RotateCcw size={14} /> Run again
            </button>
            <Link to={`${ctx.web}/-/jobs/${j.id}`}>Open the log</Link>
          </>
        }
      />
    );
  }
  return null;
}

/** One deployment: where from, by whom, how it went; the live one is marked. */
function DeploymentRow({ ctx, d, live }: { ctx: Ctx; d: Deployment; live: boolean }) {
  const took = d.finished_at ? (new Date(d.finished_at).getTime() - new Date(d.created_at).getTime()) / 1000 : null;
  return (
    <div className={`item deploy-row${live ? " live" : ""}`}>
      <span className={`row-icon st-${d.status}`}>
        <StatusIcon status={d.status} size={16} />
      </span>
      <div className="grow" style={{ minWidth: 0 }}>
        <div className="row-title">
          <span>#{d.iid}</span>
          <span className="deploy-ref">
            {d.tag ? <Tag size={13} /> : <GitBranch size={13} />}
            <Link to={refLink(ctx.web, "tree", d.ref)}>{d.ref}</Link>
          </span>
          {live && <Badge tone="ok">Live</Badge>}
        </div>
        <div className="row-meta">
          <span>{statusLabel(d.status).toLowerCase()}</span>
          <Link to={`${ctx.web}/-/commit/${d.sha}`}>{d.sha.slice(0, 8)}</Link>
          {d.username && <span>by {d.username}</span>}
          {took != null && took > 0 && <span>took {duration(took)}</span>}
        </div>
      </div>
      {d.build_id && (
        <Link className="btn small" to={`${ctx.web}/-/jobs/${d.build_id}`}>
          Job
        </Link>
      )}
      <span className="row-time" title={new Date(d.created_at).toLocaleString()}>
        {timeAgo(d.created_at)}
      </span>
    </div>
  );
}
