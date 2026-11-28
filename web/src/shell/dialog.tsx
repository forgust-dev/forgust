import { createContext, useCallback, useContext, useRef, useState, type FormEvent, type ReactNode } from "react";
import { AlertDialog } from "@base-ui/react/alert-dialog";

/**
 * A question in the app's own dialog: what the browser's confirm() and prompt()
 * did, in the site's look, with the consequence spelled out and a button that
 * says what it does.
 */
export type Ask = {
  title: string;
  /** what will happen, in a sentence or two */
  body?: ReactNode;
  /** the button's words: what it does ("Delete branch"), never "OK" */
  action: string;
  /** cannot be undone: the button is red, and the focus starts on Cancel */
  danger?: boolean;
  /** asks for a value: the field's label and first value; `match` must be typed exactly to go on */
  input?: { label: string; value?: string; placeholder?: string; type?: string; match?: string; mono?: boolean };
};

const AskCtx = createContext<(a: Ask) => Promise<string | null>>(() => Promise.resolve(null));

export function DialogProvider({ children }: { children: ReactNode }) {
  const [q, setQ] = useState<Ask | null>(null);
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState("");
  const settle = useRef<((v: string | null) => void) | null>(null);
  // open now, as the fade-out's end sees it (the state may be a render behind)
  const openRef = useRef(false);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const goRef = useRef<HTMLButtonElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const ask = useCallback(
    (a: Ask) =>
      new Promise<string | null>((resolve) => {
        // one question at a time: a new one answers the open one "no"
        settle.current?.(null);
        settle.current = resolve;
        setQ(a);
        setValue(a.input?.value ?? "");
        openRef.current = true;
        setOpen(true);
      }),
    [],
  );
  const close = (v: string | null) => {
    settle.current?.(v);
    settle.current = null;
    openRef.current = false;
    setOpen(false);
  };
  const ready = !q?.input || (q.input.match != null ? value === q.input.match : value.trim() !== "");
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (ready) close(q?.input ? value.trim() : "");
  };

  return (
    <AskCtx.Provider value={ask}>
      {children}
      <AlertDialog.Root
        open={open}
        // Escape: no
        onOpenChange={(o) => !o && close(null)}
        // kept until it has faded out, then forgotten; unless the next one
        // was asked meanwhile (two questions in a row)
        onOpenChangeComplete={(o) => !o && !openRef.current && setQ(null)}
      >
        <AlertDialog.Portal>
          <AlertDialog.Backdrop className="modal-backdrop" />
          <AlertDialog.Popup className="modal" initialFocus={q?.input ? inputRef : q?.danger ? cancelRef : goRef}>
            {q && (
              <form onSubmit={submit}>
                <AlertDialog.Title className="modal-title">{q.title}</AlertDialog.Title>
                {q.body && (
                  <AlertDialog.Description className="modal-body" render={<div />}>
                    {q.body}
                  </AlertDialog.Description>
                )}
                {q.input && (
                  <label className="field modal-field">
                    {q.input.label}
                    <input
                      ref={inputRef}
                      type={q.input.type ?? "text"}
                      value={value}
                      placeholder={q.input.placeholder}
                      className={q.input.mono ? "mono" : undefined}
                      autoCapitalize="none"
                      autoCorrect="off"
                      spellCheck={false}
                      autoComplete="off"
                      onChange={(e) => setValue(e.target.value)}
                    />
                  </label>
                )}
                <div className="modal-actions">
                  <button type="button" ref={cancelRef} className="btn" onClick={() => close(null)}>
                    Cancel
                  </button>
                  <button type="submit" ref={goRef} className={`btn ${q.danger ? "danger solid" : "primary"}`} disabled={!ready}>
                    {q.action}
                  </button>
                </div>
              </form>
            )}
          </AlertDialog.Popup>
        </AlertDialog.Portal>
      </AlertDialog.Root>
    </AskCtx.Provider>
  );
}

/** Ask yes or no in the app's dialog (instead of the browser's confirm()): true for yes. */
export function useConfirm() {
  const ask = useContext(AskCtx);
  return useCallback(async (a: Omit<Ask, "input">) => (await ask(a)) !== null, [ask]);
}

/** Ask for a value in the app's dialog (instead of the browser's prompt()): null when canceled. */
export function usePrompt() {
  const ask = useContext(AskCtx);
  return useCallback((a: Ask & { input: NonNullable<Ask["input"]> }) => ask(a), [ask]);
}
