// The rules every settings page shares: how a form saves, and leaving a page with changes not
// saved yet. web/CLAUDE.md, "Settings", says when to use which.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useNavigate } from "react-router-dom";
import { Check, Plus } from "lucide-react";
import { useConfirm } from "../shell/dialog";
import { Switch } from "./controls";
import { ErrorBox, Section } from "./ui";
import { FormDialog } from "./FormDialog";

/** The forms of this page that have changes not saved yet. */
const unsaved = new Set<symbol>();

/** Counts this form among the page's unsaved ones while `dirty`. */
export function useUnsavedGuard(dirty: boolean) {
  const id = useRef(Symbol("form")).current;
  useEffect(() => {
    if (dirty) unsaved.add(id);
    else unsaved.delete(id);
    return () => {
      unsaved.delete(id);
    };
  }, [dirty, id]);
}

/**
 * Asks before a page with unsaved changes is left: closing or reloading the tab (the browser's
 * own question), or following a link of the app. Back and forward, and the Ctrl+K palette,
 * are not asked about. Mounted once, inside the router and the dialogs.
 */
export function UnsavedGuard() {
  const nav = useNavigate();
  const confirm = useConfirm();
  useEffect(() => {
    const unload = (e: BeforeUnloadEvent) => {
      if (unsaved.size === 0) return;
      e.preventDefault();
      e.returnValue = "";
    };
    // before the app's own handlers (capture, on the document): a plain click on a link of the app
    const click = (e: MouseEvent) => {
      if (unsaved.size === 0 || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
      const a = (e.target as Element | null)?.closest?.("a[href]") as HTMLAnchorElement | null;
      if (!a || (a.target && a.target !== "_self") || a.hasAttribute("download")) return;
      const to = new URL(a.href, window.location.href);
      if (to.origin !== window.location.origin) return;
      // the same page, at another place
      if (to.pathname === window.location.pathname && to.search === window.location.search) return;
      e.preventDefault();
      e.stopPropagation();
      void confirm({ title: "Leave without saving?", body: "Changes on this page are not saved yet.", action: "Leave the page" }).then((ok) => {
        if (!ok) return;
        unsaved.clear();
        nav(to.pathname + to.search + to.hash);
      });
    };
    window.addEventListener("beforeunload", unload);
    document.addEventListener("click", click, true);
    return () => {
      window.removeEventListener("beforeunload", unload);
      document.removeEventListener("click", click, true);
    };
  }, [nav, confirm]);
  return null;
}

/**
 * A settings form's values against what the server has: `dirty` while they differ (the page
 * then asks before it is left), `saved` from a save until the next change.
 */
export function useSaved<T>(initial: T) {
  const [value, setValue] = useState(initial);
  const [base, setBase] = useState(initial);
  const [saved, setSaved] = useState(false);
  const dirty = JSON.stringify(value) !== JSON.stringify(base);
  useUnsavedGuard(dirty);
  return {
    value,
    dirty,
    saved: saved && !dirty,
    set: (v: T) => {
      setValue(v);
      setSaved(false);
    },
    /** the server has `value` now (or what it answered, when it changes it) */
    done: (now: T = value) => {
      setValue(now);
      setBase(now);
      setSaved(true);
    },
  };
}

/** A settings form's foot: Save, grey until something changed, and "Saved" beside it after a save. */
export function SaveRow({ dirty, busy, saved, label = "Save", children }: { dirty: boolean; busy?: boolean; saved?: boolean; label?: string; children?: ReactNode }) {
  return (
    <div className="save-row">
      <button className="btn primary" aria-busy={busy} disabled={busy || !dirty}>
        {label}
      </button>
      {saved && <SavedMark />}
      {children}
    </div>
  );
}

/**
 * A setting that is one switch: saved as soon as it is flipped, with "Saved" beside it; back
 * where it was if the save fails.
 */
export function SettingSwitch({
  checked,
  onSave,
  children,
  disabled,
  confirmOff,
}: {
  checked: boolean;
  onSave: (on: boolean) => Promise<unknown>;
  children: ReactNode;
  disabled?: boolean;
  /** asked first when turning it off undoes something (maps removed…) */
  confirmOff?: { title: string; body: string; action: string };
}) {
  const confirm = useConfirm();
  const [on, setOn] = useState(checked);
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => setOn(checked), [checked]);
  const flip = async (v: boolean) => {
    if (!v && confirmOff && !(await confirm({ ...confirmOff, danger: true }))) return;
    setOn(v);
    setBusy(true);
    setSaved(false);
    setError(null);
    try {
      await onSave(v);
      setSaved(true);
    } catch (e) {
      setOn(!v);
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="stack" style={{ gap: 6 }}>
      <ErrorBox error={error} />
      <div className="save-row">
        <Switch checked={on} onChange={flip} disabled={disabled || busy}>
          {children}
        </Switch>
        {saved && <SavedMark />}
      </div>
    </div>
  );
}

/**
 * A settings section that holds a list: what is there first (or why nothing is), and its button
 * under the section's description, where every section keeps its actions. The button opens the
 * form in a window (FormDialog) titled with its words; `result`, once set, replaces the form
 * there (a token to copy). Cancel, Esc, Done and a click outside all go through `onClose`.
 */
export function ListSection({
  id,
  title,
  description,
  add,
  adding,
  onAdd,
  onClose,
  form,
  result,
  resultTitle,
  children,
}: {
  id?: string;
  title: ReactNode;
  description?: ReactNode;
  /** the button's words ("Protect a branch"), the window's title; none when nothing can be added here */
  add?: string;
  adding: boolean;
  onAdd: () => void;
  onClose: () => void;
  form: ReactNode;
  result?: ReactNode;
  /** the window's title while it shows the result ("Token created") */
  resultTitle?: string;
  children: ReactNode;
}) {
  return (
    <Section
      id={id}
      title={title}
      description={description}
      actions={
        add ? (
          <button type="button" className="btn small" onClick={onAdd}>
            <Plus size={14} /> {add}
          </button>
        ) : undefined
      }
    >
      <div className="stack">{children}</div>
      {add && (
        <FormDialog open={adding} onClose={onClose} title={result ? (resultTitle ?? add) : add} result={result}>
          {form}
        </FormDialog>
      )}
    </Section>
  );
}

/** Who a protection lets do something, in words. */
export function whoCan(level: string) {
  return ({ no_one: "Nobody", developer: "Developers and Maintainers", maintainer: "Maintainers", owner: "Owners" } as Record<string, string>)[level] ?? level;
}

/** A list with nothing in it yet: one line, in the list's own box. */
export function EmptyRow({ children }: { children: ReactNode }) {
  return (
    <div className="list">
      <div className="item muted">{children}</div>
    </div>
  );
}

/** "Saved", beside what was just saved. */
function SavedMark() {
  return (
    <span className="saved-mark" role="status">
      <Check size={14} /> Saved
    </span>
  );
}
