import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { GitBranch } from "lucide-react";
import { api, ApiError, qs, type S } from "../../api/client";
import { useSession } from "../../session";
import type { Ctx } from "./Project";

// Where a commit made in the browser goes: the file editor's and the web IDE's choice.

export type Target = "here" | "new";

/** The branch a ref names, with its head (null when the ref is a tag or a commit). */
function useBranch(ctx: Ctx, ref: string) {
  return useQuery({
    queryKey: ["branch-head", ctx.p.id, ref],
    queryFn: async () => {
      const list = await api.get<S["BranchDto"][]>(`${ctx.base}/branches${qs({ search: ref, per_page: 100 })}`);
      return list.find((b) => b.name === ref) ?? null;
    },
    staleTime: 0,
  });
}

function suggestBranch(username: string) {
  return `${username}-patch-${Math.floor(Date.now() / 1000) % 100000}`;
}

/** A commit that went through: where, and the branch a merge request of it would target. */
export type Landed = { target: Target; branch: string; mrTarget: string };

/**
 * A commit from the browser onto `gitRef`, whose files were read at the commit `base`:
 * the choice of where it goes (`<CommitTarget {...targetProps} />`) and the request.
 * `blocked` says why the ref itself cannot take it right now.
 */
export function useCommit(ctx: Ctx, gitRef: string, base: string, blocked?: string) {
  const qc = useQueryClient();
  const { user } = useSession();
  const branch = useBranch(ctx, gitRef);
  const isBranch = !!branch.data;
  const [target, setTarget] = useState<Target>("here");
  const [newBranch, setNewBranch] = useState(() => suggestBranch(user?.username ?? "patch"));
  // a tag or a commit cannot take commits, nor a ref that is blocked: a new branch it is
  useEffect(() => {
    if ((branch.isSuccess && !isBranch) || blocked) setTarget("new");
  }, [branch.isSuccess, isBranch, blocked]);
  const commit = async (message: string, actions: S["FileAction"][]): Promise<Landed> => {
    const onBranch = target === "here" ? gitRef : newBranch.trim();
    try {
      await api.post<S["Committed"]>(`${ctx.base}/commits`, {
        branch: onBranch,
        // a new branch starts from the commit the files were read at, wherever the ref went
        start_branch: target === "new" ? base : undefined,
        commit_message: message,
        actions,
        // a push since the files were read is not overwritten
        expected_head: target === "here" ? base : undefined,
      });
    } catch (err) {
      // a protected branch: offer the other way at once
      if (err instanceof ApiError && err.status === 403 && target === "here") setTarget("new");
      throw err;
    }
    qc.invalidateQueries({ queryKey: ["branches", ctx.p.id] });
    return { target, branch: onBranch, mrTarget: isBranch ? gitRef : (ctx.p.default_branch ?? "") };
  };
  return {
    commit,
    target,
    /** whether the choice made can take a commit */
    ready: target === "new" ? newBranch.trim() !== "" : isBranch && !blocked,
    targetProps: { gitRef, branch, target, setTarget, newBranch, setNewBranch, blocked },
  };
}

/**
 * Commit to the ref itself, or to a new branch with a merge request. Only a branch
 * takes commits; `blocked` says why this one cannot right now.
 */
export function CommitTarget({
  gitRef,
  branch,
  target,
  setTarget,
  newBranch,
  setNewBranch,
  blocked,
}: {
  gitRef: string;
  branch: { data?: S["BranchDto"] | null; isSuccess: boolean };
  target: Target;
  setTarget: (t: Target) => void;
  newBranch: string;
  setNewBranch: (name: string) => void;
  blocked?: string;
}) {
  const isBranch = !!branch.data;
  const closed = !isBranch || !!blocked;
  return (
    <fieldset className="commit-target">
      <legend className="small muted">Where to commit</legend>
      <label className={`check ${closed ? "disabled" : ""}`}>
        <input type="radio" name="target" checked={target === "here"} disabled={closed} onChange={() => setTarget("here")} />
        <span>
          Commit directly to <code>{gitRef}</code>
          {!isBranch && branch.isSuccess && <span className="muted"> (not a branch)</span>}
          {isBranch && blocked && <span className="muted"> ({blocked})</span>}
          {branch.data?.protected && <span className="muted"> (protected)</span>}
        </span>
      </label>
      <label className="check">
        <input type="radio" name="target" checked={target === "new"} onChange={() => setTarget("new")} />
        <span>
          <GitBranch size={13} /> Commit to a new branch and open a merge request
        </span>
      </label>
      {target === "new" && <input aria-label="New branch name" value={newBranch} onChange={(e) => setNewBranch(e.target.value)} spellCheck={false} />}
    </fieldset>
  );
}
