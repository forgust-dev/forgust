import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, ArrowRight, ArrowUpCircle, CheckCircle2, XCircle } from "lucide-react";
import { api, bytes, type S } from "../api/client";

// Apart from AdminTools: home shows this card, and loads it with the first page, but
// not the admin area's code.

/** A fact of a right-column card: a label, its value, and what the value is made of. */
export function Fact({ label, value, hint, to }: { label: string; value: ReactNode; hint?: ReactNode; to?: string }) {
  return (
    <div>
      <dt>{label}</dt>
      <dd>{to ? <Link to={to}>{value}</Link> : value}</dd>
      {hint && <p className="side-fact-hint">{hint}</p>}
    </div>
  );
}

/**
 * The instance at a glance, for an administrator's home: the health checks, CI's
 * runners, storage and the release check, each linking to its admin page. The
 * overview's requests (the checks too: kept 5 minutes, as there).
 */
export function InstanceCard() {
  const o = useQuery({ queryKey: ["admin-overview"], queryFn: () => api.get<S["Overview"]>("/admin/overview"), staleTime: 60_000 });
  const health = useQuery({ queryKey: ["admin-health"], queryFn: () => api.get<S["Health"]>("/admin/health"), staleTime: 300_000 });
  const storage = useQuery({ queryKey: ["admin-storage"], queryFn: () => api.get<S["StorageOverview"]>("/admin/storage"), staleTime: 300_000 });
  const update = useQuery({ queryKey: ["admin-update"], queryFn: () => api.get<S["UpdateStatus"]>("/admin/update"), staleTime: 3_600_000 });
  const checks = health.data?.checks ?? [];
  const failing = checks.filter((c) => c.status === "fail");
  const warning = checks.filter((c) => c.status === "warn");
  const ci = o.data?.ci;
  return (
    <section className="side-card instance-card" aria-label="This instance">
      <h2>Instance</h2>
      <dl className="side-facts">
        <Fact
          label="Health"
          to="/admin/health"
          value={
            !health.data ? (
              <span className="faint">{health.isError ? "Unavailable" : "Checking…"}</span>
            ) : failing.length > 0 ? (
              <span className="instance-status bad">
                <XCircle size={14} aria-hidden /> {failing.length} failing
              </span>
            ) : warning.length > 0 ? (
              <span className="instance-status warn">
                <AlertTriangle size={14} aria-hidden /> {warning.length} warning{warning.length === 1 ? "" : "s"}
              </span>
            ) : (
              <span className="instance-status ok">
                <CheckCircle2 size={14} aria-hidden /> All {checks.length} checks pass
              </span>
            )
          }
          hint={[...failing, ...warning].map((c) => c.name).join(" · ") || undefined}
        />
        <Fact
          label="Runners online"
          to="/admin/runners"
          value={ci ? `${ci.runners_online} / ${ci.runners}` : o.isError ? "Unavailable" : "…"}
          hint={ci ? `${ci.jobs_queued} job${ci.jobs_queued === 1 ? "" : "s"} waiting · ${ci.jobs_running} running` : undefined}
        />
        <Fact label="Storage" to="/admin/storage" value={storage.data ? bytes(storage.data.total) : storage.isError ? "Unavailable" : "…"} hint={o.data ? `${o.data.counts.projects} projects · ${o.data.counts.users_active} users` : undefined} />
        {update.data && (
          <Fact
            label="Version"
            value={<span className="mono">{update.data.current}</span>}
            hint={
              update.data.available ? (
                <span className="warn-text">
                  <ArrowUpCircle size={12} aria-hidden /> Forgust {update.data.available} is out
                  {update.data.url && (
                    <>
                      {" · "}
                      <a href={update.data.url} target="_blank" rel="noreferrer">
                        Release notes
                      </a>
                    </>
                  )}
                </span>
              ) : undefined
            }
          />
        )}
      </dl>
      <Link className="side-more" to="/admin">
        Admin area <ArrowRight size={14} />
      </Link>
    </section>
  );
}
