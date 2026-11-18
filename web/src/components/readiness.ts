import type { S } from "../api/client";
import { isActive } from "../pages/project/ci/Status";
import type { Tone } from "./decide";

/** One check between a merge request and its merge: what its page's readiness card
 *  lists, and what the small rings of "Your work" count. */
export type ReadyCheck = { key: string; kind: "pipeline" | "approvals" | "rule" | "threads" | "conflicts"; tone: Tone; rule?: S["RuleStatus"] };

/** The checks of a merge request, from its merge status; `pipelineStatus` stands in
 *  when the status has none (the merge request's latest pipeline). */
export function readinessChecks(s: S["MergeStatus"], pipelineStatus?: string | null): ReadyCheck[] {
  const checks: ReadyCheck[] = [];
  const pstatus = s.pipeline_status ?? pipelineStatus;
  if (pstatus || s.pipeline_required) {
    const running = pstatus != null && isActive(pstatus);
    checks.push({ key: "pipeline", kind: "pipeline", tone: pstatus === "success" ? "ok" : running ? "warn" : s.pipeline_required ? "bad" : "warn" });
  }
  // without rules, approvals are shown but not counted
  if (s.approval_rules.length === 0) checks.push({ key: "approvals", kind: "approvals", tone: "none" });
  for (const r of s.approval_rules) checks.push({ key: `rule:${r.name}`, kind: "rule", tone: r.satisfied ? "ok" : "bad", rule: r });
  checks.push({ key: "threads", kind: "threads", tone: s.unresolved_threads > 0 ? "warn" : "ok" });
  checks.push({ key: "conflicts", kind: "conflicts", tone: s.conflicts.length > 0 ? "bad" : "ok" });
  return checks;
}

/** How many of the checks that count pass. */
export function readinessScore(checks: { tone: Tone }[]) {
  const scored = checks.filter((c) => c.tone !== "none");
  return { passed: scored.filter((c) => c.tone === "ok").length, total: scored.length };
}
