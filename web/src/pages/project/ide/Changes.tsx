import { useState, type FormEvent } from "react";
import { RefreshCw, TriangleAlert, Undo2 } from "lucide-react";
import { ApiError, plural, type S } from "../../../api/client";
import { ErrorBox, Field } from "../../../components/ui";
import { CommitTarget, useCommit, type Landed } from "../commitTarget";
import type { Ctx } from "../Project";
import { describe, MAX_ACTIONS, nameOf, parentOf, statusOf, toActions, type Drafts } from "./changes";
import { StatusMark } from "./FileTree";

/**
 * The changed files, each a click from its differences, and the commit that takes
 * them all: to the branch, or to a new one with a merge request.
 */
export function Changes({
  ctx,
  res,
  base,
  drafts,
  active,
  show,
  discard,
  update,
  refresh,
  committed,
}: {
  ctx: Ctx;
  res: S["RefResolved"];
  /** the commit the changes were made on */
  base: string;
  drafts: Drafts;
  active: string;
  /** open a file on what changed in it */
  show: (path: string) => void;
  discard: (path: string) => void;
  /** bring the changes onto the ref's latest commit */
  update: () => Promise<void>;
  /** read the ref's head again */
  refresh: () => Promise<unknown>;
  committed: (landed: Landed) => Promise<void>;
}) {
  // the ref took commits since the changes were started: they cannot land on it as they are
  const moved = base !== res.commit;
  const { commit, target, ready, targetProps } = useCommit(ctx, res.ref, base, moved ? "has new commits" : undefined);
  // the message follows the changes until it is typed
  const [typed, setTyped] = useState<string | null>(null);
  const message = typed ?? describe(drafts);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState<"commit" | "update" | null>(null);
  const paths = Object.keys(drafts).sort();

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const actions = toActions(drafts);
    if (actions.length > MAX_ACTIONS) return setError(new Error(`A commit takes ${MAX_ACTIONS} files at most: these are ${actions.length}. Commit them in parts from a clone.`));
    setError(null);
    setBusy("commit");
    try {
      const landed = await commit(message, actions);
      setTyped(null);
      await committed(landed);
    } catch (err) {
      // the branch moved under the changes: read its head, and the way out shows above
      if (err instanceof ApiError && err.status === 409 && target === "here") await refresh().catch(() => {});
      setError(err);
    } finally {
      setBusy(null);
    }
  };
  const bring = () => {
    setError(null);
    setBusy("update");
    update()
      .catch(setError)
      .finally(() => setBusy(null));
  };

  if (paths.length === 0) return <p className="ide-changes-empty muted small">No changes yet. Edit, add, rename or delete files: they gather here to be committed together.</p>;
  return (
    <form className="ide-changes" onSubmit={submit}>
      <ul className="ide-change-list" aria-label="Changed files">
        {paths.map((p) => (
          <li key={p} className="ide-row" data-active={p === active || undefined}>
            <StatusMark status={statusOf(p, drafts[p])} />
            <button type="button" className="ide-change-open ellipsis" title={drafts[p].origin && drafts[p].origin !== p ? `${drafts[p].origin} → ${p}` : p} onClick={() => show(p)}>
              <span className={drafts[p].deleted ? "ide-deleted" : undefined}>{nameOf(p)}</span>
              <span className="faint"> {parentOf(p)}</span>
            </button>
            <span className="ide-row-actions">
              <button type="button" title={drafts[p].deleted ? "Restore" : "Discard the changes"} aria-label={`${drafts[p].deleted ? "Restore" : "Discard the changes of"} ${p}`} onClick={() => discard(p)}>
                <Undo2 size={14} />
              </button>
            </span>
          </li>
        ))}
      </ul>
      <div className="ide-commit stack">
        {moved && (
          <div className="alert warn small" role="status">
            <TriangleAlert size={14} /> <code>{res.ref}</code> has new commits since you started. Bring your changes onto them, or commit to a new branch.
            <button type="button" className="btn small" disabled={busy !== null} onClick={bring}>
              <RefreshCw size={13} /> {busy === "update" ? "Updating…" : `Update to ${res.commit.slice(0, 8)}`}
            </button>
          </div>
        )}
        <ErrorBox error={error} />
        <Field label="Commit message">
          <textarea rows={2} value={message} onChange={(e) => setTyped(e.target.value)} />
        </Field>
        <CommitTarget {...targetProps} />
        <button className="btn primary" disabled={busy !== null || !message.trim() || !ready}>
          {busy === "commit" ? "Committing…" : `Commit ${plural(paths.length, "file")}`}
        </button>
      </div>
    </form>
  );
}
