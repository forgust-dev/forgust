// Adding a row to a list, or changing one, in a window: web/CLAUDE.md, "Adding", says when.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Dialog } from "@base-ui/react/dialog";
import { InWindow } from "./controls";

/** The field a window starts on: its first one that can be typed in or chosen. */
const FIRST = 'input:not([type="hidden"]):not([disabled]), textarea:not([disabled]), select:not([disabled]), [role="combobox"]:not([disabled])';

/**
 * A window that adds a row to a list or changes one: its title, then its form (the fields, then
 * FormActions). Once the row is added, `result` replaces the form when there is something to
 * copy from it (a token shown once), with Done. Esc, Cancel, Done and a click outside all close
 * it through `onClose`; a click outside does nothing once something was typed, or while a
 * result is shown. The confirmation's look and entry, wider; its buttons stay in view while a
 * long form scrolls.
 */
export function FormDialog({ open, onClose, title, result, children }: { open: boolean; onClose: () => void; title: ReactNode; result?: ReactNode; children: ReactNode }) {
  const popup = useRef<HTMLDivElement>(null);
  const done = useRef<HTMLButtonElement>(null);
  const [typed, setTyped] = useState(false);
  useEffect(() => {
    if (open) setTyped(false);
  }, [open]);
  // what it showed stays while it fades out: closing does not flash the emptied form
  const shown = useRef({ title, result, children });
  if (open) shown.current = { title, result, children };
  const view = shown.current;
  // a result replaces the form, and its button with it: Done takes the focus
  useEffect(() => {
    if (open && result) done.current?.focus();
  }, [open, result]);
  return (
    <Dialog.Root open={open} onOpenChange={(o) => !o && onClose()} disablePointerDismissal={typed || !!view.result}>
      <Dialog.Portal>
        <Dialog.Backdrop className="modal-backdrop" />
        <Dialog.Popup ref={popup} className="modal form-modal" initialFocus={() => popup.current?.querySelector<HTMLElement>(FIRST) ?? true} onInput={() => setTyped(true)}>
          <InWindow.Provider value>
            <Dialog.Title className="modal-title">{view.title}</Dialog.Title>
            {view.result ? (
              <div className="stack">
                {view.result}
                <div className="modal-actions">
                  <button ref={done} type="button" className="btn primary" onClick={onClose}>
                    Done
                  </button>
                </div>
              </div>
            ) : (
              view.children
            )}
          </InWindow.Provider>
        </Dialog.Popup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

/** A window's buttons, at the bottom of its form: Cancel, then what it does. */
export function FormActions({ label, disabled, busy, onCancel }: { label: string; disabled?: boolean; busy?: boolean; onCancel: () => void }) {
  return (
    <div className="modal-actions">
      <button type="button" className="btn" onClick={onCancel}>
        Cancel
      </button>
      <button className="btn primary" aria-busy={busy} disabled={disabled || busy}>
        {label}
      </button>
    </div>
  );
}

/**
 * The row just added, highlighted as it comes into its list, then fading back: `mark` its key
 * once added, and give each row's class `fresh(itsKey)`.
 */
export function useJustAdded() {
  const [key, setKey] = useState<string | null>(null);
  useEffect(() => {
    if (key == null) return;
    // the list refetches first; the highlight's own fade is shorter than this
    const t = setTimeout(() => setKey(null), 4000);
    return () => clearTimeout(t);
  }, [key]);
  return {
    mark: (k: string | number) => setKey(String(k)),
    fresh: (k: string | number) => (key != null && String(k) === key ? " just-added" : ""),
  };
}
