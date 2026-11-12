import { useState } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ExternalLink, Server } from "lucide-react";
import { api, timeAgo, type S } from "../../../api/client";
import { Badge, Empty, ListHead, Q, Tabs } from "../../../components/ui";
import { useToast } from "../../../shell/toast";
import { useCiLive } from "../../../shell/live";
import type { Ctx } from "../Project";
import { StatusIcon } from "./Status";

/** Environments: where each was last deployed from, and stopping them. */
export function Environments({ ctx, stopped }: { ctx: Ctx; stopped: boolean }) {
  useCiLive(ctx.p.id);
  const q = useQuery({
    queryKey: ["environments", ctx.p.id, stopped],
    queryFn: () => api.get<S["EnvironmentSummary"][]>(`/projects/${ctx.p.id}/environments?state=${stopped ? "stopped" : "available"}`),
  });
  return (
    <div className="stack">
      <ListHead title="Environments" sub="Where jobs with environment: deploy." />
      <Tabs
        items={[
          { to: `${ctx.web}/-/environments`, label: "Available", end: true },
          { to: `${ctx.web}/-/environments/stopped`, label: "Stopped" },
        ]}
      />
      <Q q={q}>
        {(list) =>
          list.length === 0 ? (
            <Empty>
              <Server size={28} />
              <p>{stopped ? "No stopped environments." : "No environments yet. A job with environment: creates one when it runs."}</p>
            </Empty>
          ) : (
            <div className="list">
              {list.map((e) => (
                <EnvironmentRow key={e.id} ctx={ctx} e={e} />
              ))}
            </div>
          )
        }
      </Q>
    </div>
  );
}

function EnvironmentRow({ ctx, e }: { ctx: Ctx; e: S["EnvironmentSummary"] }) {
  const [open, setOpen] = useState(false);
  const qc = useQueryClient();
  const toast = useToast();
  const detail = useQuery({
    queryKey: ["environment", ctx.p.id, e.id],
    queryFn: () => api.get<S["EnvironmentDetail"]>(`/projects/${ctx.p.id}/environments/${e.id}`),
    enabled: open,
  });
  const stop = async () => {
    if (!window.confirm(`Stop ${e.name}?`)) return;
    try {
      await api.post(`/projects/${ctx.p.id}/environments/${e.id}/stop`);
      toast.ok(`Stopping ${e.name}`);
      qc.invalidateQueries({ queryKey: ["environments", ctx.p.id] });
      qc.invalidateQueries({ queryKey: ["environment", ctx.p.id, e.id] });
    } catch (err) {
      toast.error(err);
    }
  };
  const d = e.last_deployment;
  return (
    <section className="item environment-row" aria-label={`Environment ${e.name}`}>
      <div className="row wrap grow" style={{ gap: 10 }}>
        <button className="link-btn" onClick={() => setOpen(!open)} aria-expanded={open}>
          <strong>{e.name}</strong>
        </button>
        <Badge tone={e.tier === "production" ? "warn" : undefined}>{e.tier}</Badge>
        {e.state !== "available" && <Badge>{e.state}</Badge>}
        {d && (
          <span className="small muted row" style={{ gap: 6 }}>
            <StatusIcon status={d.status} size={14} /> #{d.iid} {d.ref} @{d.sha.slice(0, 8)}
            {d.username && <> by {d.username}</>} · {timeAgo(d.created_at)}
          </span>
        )}
        {e.auto_stop_at && <span className="small faint">stops automatically {new Date(e.auto_stop_at).toLocaleDateString()}</span>}
        <span className="grow" />
        {e.external_url && /^https?:\/\//i.test(e.external_url) && (
          <a className="btn small" href={e.external_url} target="_blank" rel="noreferrer noopener">
            <ExternalLink size={13} /> Open
          </a>
        )}
        {e.state === "available" && ctx.p.can_push && (
          <button className="btn small" onClick={stop}>
            Stop
          </button>
        )}
      </div>
      {open && (
        <Q q={detail}>
          {(x) => (
            <div className="list" style={{ marginTop: 8 }}>
              {x.deployments.map((dep) => (
                <div className="item small" key={dep.id}>
                  <StatusIcon status={dep.status} size={14} />
                  <span>#{dep.iid}</span>
                  <span className="mono">
                    {dep.ref} @{dep.sha.slice(0, 8)}
                  </span>
                  {dep.username && <span>by {dep.username}</span>}
                  <span className="faint">{timeAgo(dep.created_at)}</span>
                  {dep.build_id && <Link to={`${ctx.web}/-/jobs/${dep.build_id}`}>job</Link>}
                </div>
              ))}
            </div>
          )}
        </Q>
      )}
    </section>
  );
}
