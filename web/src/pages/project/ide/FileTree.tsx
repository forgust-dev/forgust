import { memo, useMemo, type KeyboardEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { Box, ChevronDown, ChevronRight, FileCode, FilePlus2, FileText, Link2, Pencil, Trash2, Undo2 } from "lucide-react";
import { fetchers, keys } from "../../../prefetch";
import { listing, touches, type Drafts, type Row, type Status } from "./changes";

/** What the tree shows and what its rows can do: the workbench's state, handed down. */
export type TreeOps = {
  projectId: number;
  /** the commit the tree is read from */
  base: string;
  /** the changes, without their texts (`outline`): typing does not draw the tree again */
  drafts: Drafts;
  /** the file in the editor */
  active: string;
  expanded: ReadonlySet<string>;
  /** the row the keyboard is on (the one Tab reaches) */
  cursor: string | null;
  setCursor: (path: string) => void;
  toggle: (dir: string) => void;
  open: (path: string) => void;
  create: (dir: string) => void;
  rename: (path: string) => void;
  remove: (path: string) => void;
  discard: (path: string) => void;
};

const STATUS: Record<Status, { letter: string; label: string }> = {
  added: { letter: "A", label: "new" },
  modified: { letter: "M", label: "modified" },
  deleted: { letter: "D", label: "deleted" },
  renamed: { letter: "R", label: "renamed" },
};

/** The letter that says how a file differs from the base commit. */
export function StatusMark({ status }: { status: Status }) {
  return (
    <span className={`ide-status ide-${status}`} title={STATUS[status].label} aria-label={STATUS[status].label}>
      {STATUS[status].letter}
    </span>
  );
}

const FILE_ICONS: Record<string, React.ReactElement> = {
  executable: <FileCode size={15} />,
  symlink: <Link2 size={15} />,
  submodule: <Box size={15} />,
};

/** Only regular files open in the editor: a link or a submodule is not text to edit. */
const opens = (row: Row) => !row.dir && row.kind !== "symlink" && row.kind !== "submodule";

const indent = (depth: number) => ({ paddingLeft: 8 + (depth - 1) * 14 });

const items = (el: Element) => Array.from(el.closest("[role=tree]")?.querySelectorAll<HTMLElement>("[role=treeitem]") ?? []);

/** The repository's files, with the changes over them. Arrows move, Enter opens, F2 renames, Delete removes. */
export const FileTree = memo(function FileTree({ ops }: { ops: TreeOps }) {
  return (
    <div className="ide-tree" role="tree" aria-label="Files">
      <Dir ops={ops} dir="" depth={1} tracked />
    </div>
  );
});

function Dir({ ops, dir, depth, tracked }: { ops: TreeOps; dir: string; depth: number; tracked: boolean }) {
  const q = useQuery({
    queryKey: keys.tree(ops.projectId, ops.base, dir),
    queryFn: () => fetchers.tree(ops.projectId, ops.base, dir),
    enabled: tracked,
    // a commit's tree never changes
    staleTime: Infinity,
  });
  const rows = useMemo(() => listing(q.data?.entries ?? [], ops.drafts, dir), [q.data, ops.drafts, dir]);
  // nothing has the keyboard yet: Tab reaches the tree's first row
  const first = dir === "" && ops.cursor === null ? rows[0]?.path : undefined;
  return (
    <>
      {rows.map((row) => (
        <TreeRow key={row.path} ops={ops} row={row} depth={depth} tabbable={row.path === ops.cursor || row.path === first} />
      ))}
      {tracked && q.isLoading && (
        <div className="ide-row muted" style={indent(depth)}>
          Loading…
        </div>
      )}
      {tracked && q.isError && (
        <div className="ide-row ide-deleted" style={indent(depth)}>
          Could not list this folder
        </div>
      )}
      {dir === "" && q.isSuccess && rows.length === 0 && <div className="ide-row muted">No files yet</div>}
    </>
  );
}

function TreeRow({ ops, row, depth, tabbable }: { ops: TreeOps; row: Row; depth: number; tabbable: boolean }) {
  const open = row.dir && ops.expanded.has(row.path);
  const changed = row.dir && touches(ops.drafts, row.path);
  // what opens can be renamed and deleted: not a folder, a link or a submodule, nor what is already deleted
  const editable = opens(row) && row.status !== "deleted";
  const activate = () => {
    if (row.dir) ops.toggle(row.path);
    else if (opens(row)) ops.open(row.path);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.target !== e.currentTarget || e.ctrlKey || e.metaKey || e.altKey) return;
    const all = items(e.currentTarget);
    const at = all.indexOf(e.currentTarget);
    const focus = (i: number) => all[Math.max(0, Math.min(all.length - 1, i))]?.focus();
    const handled: Record<string, () => void> = {
      ArrowDown: () => focus(at + 1),
      ArrowUp: () => focus(at - 1),
      Home: () => focus(0),
      End: () => focus(all.length - 1),
      ArrowRight: () => (row.dir && !open ? ops.toggle(row.path) : row.dir && focus(at + 1)),
      // closes an open folder; from anything else, goes to the folder it is in
      ArrowLeft: () => (open ? ops.toggle(row.path) : all.slice(0, at).reverse().find((el) => Number(el.getAttribute("aria-level")) < depth)?.focus()),
      Enter: activate,
      " ": activate,
      F2: () => editable && ops.rename(row.path),
      Delete: () => editable && ops.remove(row.path),
    };
    if (!Object.hasOwn(handled, e.key)) return;
    e.preventDefault();
    handled[e.key]();
  };
  return (
    <>
      <div
        role="treeitem"
        aria-level={depth}
        aria-expanded={row.dir ? open : undefined}
        aria-selected={row.path === ops.active}
        aria-label={row.status ? `${row.name}, ${STATUS[row.status].label}` : row.name}
        tabIndex={tabbable ? 0 : -1}
        className={`ide-row ${row.status ? `ide-${row.status}` : ""}`}
        data-active={row.path === ops.active || undefined}
        data-inert={(!row.dir && !opens(row)) || undefined}
        style={indent(depth)}
        title={row.path}
        onClick={activate}
        onFocus={(e) => e.target === e.currentTarget && ops.setCursor(row.path)}
        onKeyDown={onKeyDown}
      >
        <span className="ide-row-icon">{row.dir ? open ? <ChevronDown size={15} /> : <ChevronRight size={15} /> : (FILE_ICONS[row.kind] ?? <FileText size={15} />)}</span>
        <span className="ide-row-name ellipsis">{row.name}</span>
        {changed && <span className="ide-dot" title="Has changes" />}
        {/* the keyboard has F2 and Delete: the buttons are the mouse's */}
        <span className="ide-row-actions" onClick={(e) => e.stopPropagation()}>
          {row.dir && (
            <button type="button" tabIndex={-1} title="New file here" aria-label={`New file in ${row.path}`} onClick={() => ops.create(row.path)}>
              <FilePlus2 size={14} />
            </button>
          )}
          {editable && (
            <>
              <button type="button" tabIndex={-1} title="Rename or move (F2)" aria-label={`Rename ${row.path}`} onClick={() => ops.rename(row.path)}>
                <Pencil size={14} />
              </button>
              <button type="button" tabIndex={-1} title="Delete (Del)" aria-label={`Delete ${row.path}`} onClick={() => ops.remove(row.path)}>
                <Trash2 size={14} />
              </button>
            </>
          )}
          {row.status && (
            <button type="button" tabIndex={-1} title={row.status === "deleted" ? "Restore" : "Discard the changes"} aria-label={`${row.status === "deleted" ? "Restore" : "Discard the changes of"} ${row.path}`} onClick={() => ops.discard(row.path)}>
              <Undo2 size={14} />
            </button>
          )}
        </span>
        {row.status && <StatusMark status={row.status} />}
      </div>
      {open && <Dir ops={ops} dir={row.path} depth={depth + 1} tracked={row.tracked} />}
    </>
  );
}
