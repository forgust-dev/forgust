// The rules every settings page shares: how a form saves, and leaving a page with changes not
// saved yet. web/CLAUDE.md, "Settings", says when to use which.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useNavigate } from "react-router-dom";
import { Check } from "lucide-react";
import { useConfirm } from "../shell/dialog";

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
      {saved && (
        <span className="saved-mark" role="status">
          <Check size={14} /> Saved
        </span>
      )}
      {children}
    </div>
  );
}
