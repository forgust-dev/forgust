import { lazy, Suspense, useEffect, useState, type FormEvent } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { FilePlus2, Map as MapIcon, Pencil, Trash2 } from "lucide-react";
import { qs, refLink, type S } from "../../api/client";
import { usePref } from "../../shell/theme";
import { AS_TYPED, ErrorBox, Field, ListHead, Q } from "../../components/ui";
import { applyAssayEdit, assayMessage, type AssayEdit } from "./assayToml";
import { useResolved } from "./Code";
import { CommitTarget, useCommit } from "./commitTarget";
import type { Ctx } from "./Project";
import { usePageTitle } from "../../shell/title";

// fetched while the file loads (see `Editor`); deleting a file never needs it
const loadEditor = () => import("../../components/CodeEditor");
const CodeEditor = lazy(() => loadEditor().then((m) => ({ default: m.CodeEditor })));

type Mode = "edit" | "new" | "delete";

/** Edit, create or delete a file and commit it, here or on a new branch with an MR. */
export function EditFile({ ctx, refAndPath, mode }: { ctx: Ctx; refAndPath: string; mode: Mode }) {
  const r = useResolved(ctx, refAndPath);
  usePageTitle(mode === "new" ? "New file" : r.data && `${mode === "edit" ? "Edit" : "Delete"} ${r.data.path}`);
  return <Q q={r}>{(res) => <Editor ctx={ctx} res={res} mode={mode} />}</Q>;
}

function Editor({ ctx, res, mode }: { ctx: Ctx; res: S["RefResolved"]; mode: Mode }) {
  const nav = useNavigate();
  const name = res.path.split("/").pop() ?? "";
  // a correction from the Smart View: written into assay.toml as it is loaded (or made)
  const assay = (useLocation().state as { assay?: AssayEdit } | null)?.assay;
  const [path, setPath] = useState(mode === "new" ? (assay ? "assay.toml" : res.path ? `${res.path.replace(/\/$/, "")}/` : "") : res.path);
  const [content, setContent] = useState(mode === "new" && assay ? applyAssayEdit("", assay) : "");
  const [loaded, setLoaded] = useState(mode !== "edit");
  const [message, setMessage] = useState(assay ? assayMessage(assay) : mode === "edit" ? `Update ${name}` : mode === "delete" ? `Delete ${name}` : "");
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  // the commit the editor started from, pinned: a refetch of the ref must not
  // reload the text under the author nor move the head the commit is checked against
  const [base] = useState(res.commit);
  const { commit, ready, targetProps } = useCommit(ctx, res.ref, base);
  // chosen in the web IDE
  const [vim] = usePref("editor.vim", false);

  useEffect(() => {
    if (mode !== "delete") void loadEditor();
  }, [mode]);
  // the file as it is now (raw, so it is byte for byte what gets edited)
  useEffect(() => {
    if (mode !== "edit") return;
    fetch(`/api/v1/projects/${ctx.p.id}/repository/raw${qs({ ref: base, path: res.path })}`, { credentials: "same-origin" })
      .then((r) => (r.ok ? r.text() : Promise.reject(new Error(`could not load the file (${r.status})`))))
      .then((t) => {
        setContent(assay ? applyAssayEdit(t, assay) : t);
        setLoaded(true);
      })
      .catch(setError);
    // the correction is the one the page was opened with
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mode, ctx.p.id, base, res.path]);
  useEffect(() => {
    if (mode === "new" && !assay) setMessage(path.split("/").pop() ? `Add ${path.split("/").pop()}` : "");
  }, [path, mode, assay]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    setBusy(true);
    const action: S["FileAction"] =
      mode === "delete"
        ? { action: "delete", file_path: res.path }
        : mode === "new"
          ? { action: "create", file_path: path.replace(/^\/+/, ""), content }
          : { action: "update", file_path: res.path, content };
    try {
      const landed = await commit(message, [action]);
      if (landed.target === "new") {
        nav(`${ctx.web}/-/merge_requests/new${qs({ source: landed.branch, target: landed.mrTarget })}`);
      } else if (mode === "delete") {
        const dir = res.path.split("/").slice(0, -1).join("/");
        nav(refLink(ctx.web, "tree", landed.branch, dir));
      } else {
        nav(refLink(ctx.web, "blob", landed.branch, action.file_path));
      }
    } catch (err) {
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
        {assay && (
          <div className="banner info" role="note">
            <MapIcon size={16} aria-hidden />
            <span className="grow small">
              The Smart View&apos;s correction is written in below. Check it, then commit it here or on a new branch with a merge request.
            </span>
          </div>
        )}
        {mode === "new" && (
          <Field label="File path" hint="Folders are created as needed">
            <input autoFocus value={path} onChange={(e) => setPath(e.target.value)} placeholder="docs/guide.md" {...AS_TYPED} />
          </Field>
        )}
        {mode !== "delete" &&
          (loaded ? (
            <Suspense fallback={<p className="muted">Loading the editor…</p>}>
              <CodeEditor value={content} onChange={setContent} path={mode === "new" ? path : res.path} label="File content" autoFocus={mode === "edit"} vim={vim} />
            </Suspense>
          ) : (
            <p className="muted">Loading the file…</p>
          ))}
        {mode === "delete" && <p>The file is removed in a new commit; its history stays.</p>}
        <Field label="Commit message">
          <textarea rows={2} value={message} onChange={(e) => setMessage(e.target.value)} />
        </Field>
        <CommitTarget {...targetProps} />
        <div className="row">
          <button className={`btn large ${mode === "delete" ? "danger" : "primary"}`} aria-busy={busy} disabled={busy || !message.trim() || (mode === "new" && !path.trim()) || !ready || !loaded}>
            {busy ? "Committing…" : mode === "delete" ? "Delete the file" : "Commit changes"}
          </button>
          <button type="button" className="btn large" onClick={() => nav(-1)}>
            Cancel
          </button>
        </div>
      </form>
    </div>
  );
}
