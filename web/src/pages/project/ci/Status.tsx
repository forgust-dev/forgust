import { Link } from "react-router-dom";
import { CheckCircle2, ChevronsRight, CircleDashed, CircleSlash, Clock, Loader2, PauseCircle, PlayCircle, TriangleAlert, XCircle } from "lucide-react";

// statuses shown under another word (the rest read as-is)
const LABEL: Record<string, string> = { success: "passed", waiting_for_resource: "waiting" };

export function statusLabel(status: string, allowFailure?: boolean) {
  if (status === "failed" && allowFailure) return "failed (allowed)";
  return LABEL[status] ?? status;
}

/** One icon per CI status; color comes from `.st-<status>`. */
export function StatusIcon({ status, size = 16, allowFailure }: { status: string; size?: number; allowFailure?: boolean }) {
  const p = { size, "aria-hidden": true } as const;
  const icon = (() => {
    switch (status) {
      case "success":
        return <CheckCircle2 {...p} />;
      case "failed":
        return allowFailure ? <TriangleAlert {...p} /> : <XCircle {...p} />;
      case "running":
      case "canceling":
        return <Loader2 {...p} className="spin" />;
      case "pending":
      case "preparing":
      case "waiting_for_resource":
        return <Clock {...p} />;
      case "canceled":
        return <CircleSlash {...p} />;
      case "skipped":
        return <ChevronsRight {...p} />;
      case "manual":
        return <PlayCircle {...p} />;
      case "scheduled":
        return <PauseCircle {...p} />;
      default:
        return <CircleDashed {...p} />;
    }
  })();
  const cls = status === "failed" && allowFailure ? "warn" : status;
  return (
    <span className={`st st-${cls}`} title={statusLabel(status, allowFailure)} role="img" aria-label={statusLabel(status, allowFailure)}>
      {icon}
    </span>
  );
}

export function StatusBadge({ status, to }: { status: string; to?: string }) {
  const inner = (
    <>
      <StatusIcon status={status} size={14} />
      {statusLabel(status)}
    </>
  );
  return to ? (
    <Link to={to} className={`status-badge st-${status}`}>
      {inner}
    </Link>
  ) : (
    <span className={`status-badge st-${status}`}>{inner}</span>
  );
}

/** Stage dots of a pipeline (`[stage, status]`). */
export function MiniPipeline({ stages, to }: { stages: [string, string][]; to?: string }) {
  return (
    <span className="mini-pipeline">
      {stages.map(([name, st], i) => (
        <span key={name} className="row" style={{ gap: 2 }}>
          {i > 0 && <span className="mini-sep" />}
          {to ? (
            <Link to={to} title={`${name}: ${statusLabel(st)}`}>
              <StatusIcon status={st} size={17} />
            </Link>
          ) : (
            <span title={`${name}: ${statusLabel(st)}`}>
              <StatusIcon status={st} size={17} />
            </span>
          )}
        </span>
      ))}
    </span>
  );
}

export function duration(secs?: number | null): string {
  if (secs == null) return "";
  const s = Math.max(0, Math.round(secs));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** Finished jobs that can run again. */
export const RETRYABLE = ["success", "failed", "canceled"];
export const isActive = (s: string) => ["pending", "running", "canceling", "preparing", "waiting_for_resource"].includes(s);
