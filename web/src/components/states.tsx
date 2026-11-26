import { CircleCheck, CircleDot, GitMerge, GitPullRequest, GitPullRequestClosed, GitPullRequestDraft, type LucideIcon } from "lucide-react";
import { StateBadge, type Tone } from "./ui";

/** How a state shows (the UI convention): its word, its tone, its icon, and the class styles and
 *  tests key on (`mr-state opened`). */
export type StateLook = { cls: string; label: string; tone: Tone; icon: LucideIcon };

/** A merge request's state: open in green, a draft or a closed one in grey, merged in violet. */
export function mrLook(state: string, draft = false): StateLook {
  if (state === "merged") return { cls: "merged", label: "Merged", tone: "merged", icon: GitMerge };
  if (state === "closed" || state === "locked") return { cls: "closed", label: "Closed", tone: "neutral", icon: GitPullRequestClosed };
  if (draft) return { cls: "draft", label: "Draft", tone: "neutral", icon: GitPullRequestDraft };
  return { cls: "opened", label: "Open", tone: "ok", icon: GitPullRequest };
}

/** An issue's state: open in green, closed in grey. */
export function issueLook(state: string): StateLook {
  return state === "closed"
    ? { cls: "closed", label: "Closed", tone: "neutral", icon: CircleCheck }
    : { cls: "opened", label: "Open", tone: "ok", icon: CircleDot };
}

/** The class of a state's icon alone, in a dense list: the color of its pill. */
export const TONE_ICON: Record<Tone, string> = { ok: "ic-ok", danger: "ic-danger", warn: "ic-warn", info: "ic-info", merged: "ic-merged", neutral: "faint" };

/** A state as a pill: before a page's title. */
export function StateOf({ look }: { look: StateLook }) {
  return (
    <StateBadge tone={look.tone} icon={look.icon} className={`mr-state ${look.cls}`}>
      {look.label}
    </StateBadge>
  );
}

/** A state's icon alone, in a dense list: its color, and its word on hover and for screen readers. */
export function StateMark({ look, size = 16, className }: { look: StateLook; size?: number; className?: string }) {
  return (
    <span className={`state-mark ${TONE_ICON[look.tone]}${className ? ` ${className}` : ""}`} title={look.label} aria-label={look.label} role="img">
      <look.icon size={size} aria-hidden />
    </span>
  );
}
