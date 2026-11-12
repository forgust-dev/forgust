import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { GitBranch, GitMerge, GitPullRequestClosed, Layers, X } from "lucide-react";
import { api, qs, timeAgo, type S } from "../../api/client";
import { usePref } from "../../shell/theme";
import { useBranches } from "./Code";
import { useAct } from "../../shell/toast";
import type { Ctx } from "./Project";

/** Open merge requests of the project (shared with the list's cache key). */
export function useOpenMrs(ctx: Ctx, enabled = true) {
  return useQuery({
    queryKey: ["mrs", ctx.p.id, "opened", ""],
    queryFn: () => api.get<S["MrListItem"][]>(`/projects/${ctx.p.id}/merge_requests${qs({ state: "opened", per_page: 100 })}`),
    enabled,
  });
}

/** "2/3" chip for list rows. */
export function StackChip({ stack }: { stack?: S["StackBadge"] | null }) {
  if (!stack) return null;
  return (
    <span className="badge stack-chip" title={`Stacked: layer ${stack.position} of ${stack.size}`}>
      <Layers size={11} /> {stack.position}/{stack.size}
    </span>
  );
}

function useActions(ctx: Ctx) {
  const qc = useQueryClient();
  return useAct(
    () => {
      qc.invalidateQueries({ queryKey: ["mr", ctx.p.id] });
      qc.invalidateQueries({ queryKey: ["mrs", ctx.p.id] });
    },
    { always: true },
  );
}

/**
 * The stack an MR belongs to, top layer first (as it builds up), with the one
 * being viewed marked, what merges next, and layers needing a rebase.
 */
export function StackMap({ ctx, m }: { ctx: Ctx; m: S["MrDetail"] }) {
  const act = useActions(ctx);
  const s = m.stack;
  if (!s) return null;
  const path = `/projects/${ctx.p.id}/merge_requests/${m.iid}`;
  const stale = s.entries.some((e) => e.needs_rebase);
  const layers = s.entries.map((e, i) => ({ ...e, n: i + 1 })).reverse();
  return (
    <section className="mr-card stack-map" aria-label="Stack">
      <div className="stack-map-head">
        <Layers size={15} />
        <strong>Stack</strong>
        <span className="muted small">
          layer {s.position} of {s.size} · into <code>{s.trunk}</code>
        </span>
        <span className="grow" />
        {stale && ctx.p.can_push && (
          <button className="btn small" onClick={() => act(() => api.post(`${path}/stack/rebase`), "Stack rebased")}>
            Rebase stack
          </button>
        )}
        {m.can_edit && m.state === "opened" && (
          <button className="link-btn small" onClick={() => act(() => api.del(`${path}/stack`), "Removed from the stack")}>
            Remove from stack
          </button>
        )}
      </div>
      <ol className="stack-map-list">
        {layers.map((e) => (
          <li key={e.iid} className={e.iid === m.iid ? "current" : undefined} aria-current={e.iid === m.iid ? "true" : undefined}>
            <span className="stack-n faint">{e.n}</span>
            <span className={`row-icon ${e.state === "merged" ? "ic-accent" : e.state === "closed" ? "ic-danger" : e.draft ? "faint" : "ic-ok"}`} aria-label={e.state} role="img">
              {e.state === "closed" ? <GitPullRequestClosed size={14} /> : <GitMerge size={14} />}
            </span>
            <Link to={`${ctx.web}/-/merge_requests/${e.iid}`} className="grow ellipsis">
              <span className="faint">!{e.iid}</span> {e.title}
            </Link>
            {e.needs_rebase && <span className="badge warn">needs rebase</span>}
            {e.iid === s.next_to_merge && <span className="badge ok">merges next</span>}
            {e.state === "merged" && <span className="badge accent">merged</span>}
          </li>
        ))}
      </ol>
    </section>
  );
}

/** For an open MR outside a stack: stack it on the MR whose branch it targets. */
export function StackOnFact({ ctx, m }: { ctx: Ctx; m: S["MrDetail"] }) {
  const show = !m.stack && m.state === "opened" && m.can_edit && ctx.p.can_push;
  const open = useOpenMrs(ctx, show);
  const act = useActions(ctx);
  if (!show) return null;
  const below = (open.data ?? []).filter((x) => x.source_branch === m.target_branch && x.iid !== m.iid);
  if (below.length === 0) return null;
  return (
    <div className="fact">
      <span className="fact-label">Stack</span>
      <div className="fact-value column">
        <span className="small muted">
          <code>{m.target_branch}</code> is the branch of {below.map((b) => `!${b.iid}`).join(", ")}.
        </span>
        {below.map((b) => (
          <button key={b.iid} className="btn small" onClick={() => act(() => api.put(`/projects/${ctx.p.id}/merge_requests/${m.iid}/stack`, { on_iid: b.iid }), `Stacked on !${b.iid}`)}>
            <Layers size={13} /> Stack on !{b.iid}
          </button>
        ))}
      </div>
    </div>
  );
}

/** Choices for "Stack on" in the new merge request form. */
export function useStackTargets(ctx: Ctx) {
  const open = useOpenMrs(ctx, ctx.p.can_push);
  return (open.data ?? []).map((x) => ({ iid: x.iid, title: x.title, branch: x.source_branch }));
}

const RECENT_SECS = 4 * 3600;

/**
 * "You pushed to feature/x — create a merge request?" for branches with a
 * recent tip commit and no open MR. (Commit time stands in for push time.)
 */
export function RecentBranchBanner({ ctx }: { ctx: Ctx }) {
  const enabled = ctx.p.can_push && !ctx.p.empty_repo;
  const branches = useBranches(ctx, enabled);
  const open = useOpenMrs(ctx, enabled);
  const [hidden, setHidden] = usePref<string[]>(`dismissed-branches.${ctx.p.id}`, []);
  if (!branches.data || !open.data) return null;
  const now = Date.now() / 1000;
  const withMr = new Set(open.data.map((m) => m.source_branch));
  const recent = branches.data
    .filter((b) => !b.default && b.commit && now - b.commit.committed_at < RECENT_SECS && !withMr.has(b.name) && !hidden.includes(`${b.name}@${b.commit.id}`))
    .sort((a, b) => b.commit!.committed_at - a.commit!.committed_at)
    .slice(0, 2);
  return (
    <>
      {recent.map((b) => (
        <div className="banner" key={b.name}>
          <GitBranch size={16} />
          <span className="grow">
            <strong>{b.name}</strong> <span className="muted">had recent changes {timeAgo(b.commit!.committed_at)}</span>
          </span>
          <Link className="btn small primary" to={`${ctx.web}/-/merge_requests/new?source=${encodeURIComponent(b.name)}`}>
            Create merge request
          </Link>
          <button className="icon-btn" aria-label="Dismiss" onClick={() => setHidden([...hidden.slice(-20), `${b.name}@${b.commit!.id}`])}>
            <X size={14} />
          </button>
        </div>
      ))}
    </>
  );
}
