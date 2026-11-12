import { useEffect, useState, type FormEvent } from "react";
import { useNavigate } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { FilePlus2, GitBranch, Pencil, Trash2 } from "lucide-react";
import { api, ApiError, qs, type S } from "../../api/client";
import { useSession } from "../../session";
import { ErrorBox, Field, ListHead, Q } from "../../components/ui";
import { useResolved } from "./Code";
import type { Ctx } from "./Project";

type Mode = "edit" | "new" | "delete";

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

/** Edit, create or delete a file and commit it, here or on a new branch with an MR. */
export function EditFile({ ctx, refAndPath, mode }: { ctx: Ctx; refAndPath: string; mode: Mode }) {
  const r = useResolved(ctx, refAndPath);
  return <Q q={r}>{(res) => <Editor ctx={ctx} res={res} mode={mode} />}</Q>;
}

function Editor({ ctx, res, mode }: { ctx: Ctx; res: S["RefResolved"]; mode: Mode }) {
  const nav = useNavigate();
  const qc = useQueryClient();
  const { user } = useSession();
  const branch = useBranch(ctx, res.ref);
  const isBranch = !!branch.data;
  const name = res.path.split("/").pop() ?? "";
  const [path, setPath] = useState(mode === "new" ? (res.path ? `${res.path.replace(/\/$/, "")}/` : "") : res.path);
  const [content, setContent] = useState("");
  const [loaded, setLoaded] = useState(mode !== "edit");
  const [message, setMessage] = useState(mode === "edit" ? `Update ${name}` : mode === "delete" ? `Delete ${name}` : "");
  const [target, setTarget] = useState<"here" | "new">("here");
  const [newBranch, setNewBranch] = useState(() => suggestBranch(user?.username ?? "patch"));
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  // the commit the editor started from, pinned: a refetch of the ref must not
  // reload the text under the author nor move the head the commit is checked against
  const [base] = useState(res.commit);

  // the file as it is now (raw, so it is byte for byte what gets edited)
  useEffect(() => {
    if (mode !== "edit") return;
    fetch(`/api/v1/projects/${ctx.p.id}/repository/raw${qs({ ref: base, path: res.path })}`, { credentials: "same-origin" })
      .then((r) => (r.ok ? r.text() : Promise.reject(new Error(`could not load the file (${r.status})`))))
      .then((t) => {
        setContent(t);
        setLoaded(true);
      })
      .catch(setError);
  }, [mode, ctx.p.id, base, res.path]);
  // a tag or a commit cannot take commits: a new branch it is
  useEffect(() => {
    if (branch.isSuccess && !isBranch) setTarget("new");
  }, [branch.isSuccess, isBranch]);
  useEffect(() => {
    if (mode === "new") setMessage(path.split("/").pop() ? `Add ${path.split("/").pop()}` : "");
  }, [path, mode]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    setBusy(true);
    const onBranch = target === "here" ? res.ref : newBranch.trim();
    const action =
      mode === "delete"
        ? { action: "delete", file_path: res.path }
        : mode === "new"
          ? { action: "create", file_path: path.replace(/^\/+/, ""), content }
          : { action: "update", file_path: res.path, content };
    try {
      await api.post<S["Committed"]>(`${ctx.base}/commits`, {
        branch: onBranch,
        start_branch: target === "new" ? base : undefined,
        commit_message: message,
        actions: [action],
        // what was loaded: a push since then is not overwritten
        expected_head: target === "here" ? base : undefined,
      });
      qc.invalidateQueries({ queryKey: ["branches", ctx.p.id] });
      if (target === "new") {
        const t = isBranch ? res.ref : (ctx.p.default_branch ?? "");
        nav(`${ctx.web}/-/merge_requests/new${qs({ source: onBranch, target: t })}`);
      } else if (mode === "delete") {
        const dir = res.path.split("/").slice(0, -1).join("/");
        nav(`${ctx.web}/-/tree/${onBranch}${dir ? `/${dir}` : ""}`);
      } else {
        nav(`${ctx.web}/-/blob/${onBranch}/${action.file_path}`);
      }
    } catch (err) {
      // a protected branch: offer the other way at once
      if (err instanceof ApiError && err.status === 403 && target === "here") setTarget("new");
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  const title = mode === "edit" ? `Edit ${name}` : mode === "new" ? "New file" : `Delete ${name}`;
  const Icon = mode === "edit" ? Pencil : mode === "new" ? FilePlus2 : Trash2;
  return (
    <div className="stack list-page">
      <ListHead
        title={
          <span className="row" style={{ gap: 8 }}>
            <Icon size={18} /> {title}
          </span>
        }
        sub={
          <>
            on <code>{res.ref}</code>
            {mode !== "new" && (
              <>
                {" "}
                · <code>{res.path}</code>
              </>
            )}
          </>
        }
      />
      <form className="stack editor-form" onSubmit={submit}>
        <ErrorBox error={error} />
        {mode === "new" && (
          <Field label="File path" hint="Folders are created as needed">
            <input autoFocus value={path} onChange={(e) => setPath(e.target.value)} placeholder="docs/guide.md" spellCheck={false} />
          </Field>
        )}
        {mode !== "delete" &&
          (loaded ? (
            <textarea
              className="code-editor"
              aria-label="File content"
              value={content}
              spellCheck={false}
              autoFocus={mode === "edit"}
              onChange={(e) => setContent(e.target.value)}
              onKeyDown={(e) => {
                // Tab indents instead of leaving the editor (Escape then Tab leaves it)
                if (e.key === "Tab" && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
                  e.preventDefault();
                  const t = e.currentTarget;
                  const { selectionStart: a, selectionEnd: b } = t;
                  const next = `${content.slice(0, a)}\t${content.slice(b)}`;
                  setContent(next);
                  requestAnimationFrame(() => t.setSelectionRange(a + 1, a + 1));
                }
              }}
            />
          ) : (
            <p className="muted">Loading the file…</p>
          ))}
        {mode === "delete" && <p>The file is removed in a new commit; its history stays.</p>}
        <Field label="Commit message">
          <textarea rows={2} value={message} onChange={(e) => setMessage(e.target.value)} />
        </Field>
        <fieldset className="commit-target">
          <legend className="small muted">Where to commit</legend>
          <label className={`check ${!isBranch ? "disabled" : ""}`}>
            <input type="radio" name="target" checked={target === "here"} disabled={!isBranch} onChange={() => setTarget("here")} />
            <span>
              Commit directly to <code>{res.ref}</code>
              {!isBranch && branch.isSuccess && <span className="muted"> (not a branch)</span>}
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
        <div className="row">
          <button className={`btn ${mode === "delete" ? "danger" : "primary"}`} disabled={busy || !message.trim() || (mode === "new" && !path.trim()) || (target === "new" && !newBranch.trim()) || !loaded}>
            {busy ? "Committing…" : mode === "delete" ? "Delete the file" : "Commit changes"}
          </button>
          <button type="button" className="btn" onClick={() => nav(-1)}>
            Cancel
          </button>
        </div>
      </form>
    </div>
  );
}
