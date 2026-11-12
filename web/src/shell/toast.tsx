import { createContext, useCallback, useContext, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { CheckCircle2, CircleAlert, X } from "lucide-react";
import { ApiError } from "../api/client";

type Toast = { id: number; kind: "ok" | "error"; text: string };

const Ctx = createContext<{ ok: (t: string) => void; error: (e: unknown) => void }>({ ok: () => {}, error: () => {} });

let seq = 0;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const push = useCallback((kind: Toast["kind"], text: string) => {
    const id = ++seq;
    setToasts((t) => [...t.slice(-3), { id, kind, text }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), kind === "ok" ? 3500 : 7000);
  }, []);
  const ok = useCallback((t: string) => push("ok", t), [push]);
  const error = useCallback((e: unknown) => push("error", e instanceof ApiError ? e.message : String(e)), [push]);
  return (
    <Ctx.Provider value={{ ok, error }}>
      {children}
      {createPortal(
        <div className="toasts" role="status" aria-live="polite">
          {toasts.map((t) => (
            <div key={t.id} className={`toast ${t.kind}`}>
              {t.kind === "ok" ? <CheckCircle2 size={16} /> : <CircleAlert size={16} />}
              <span className="grow">{t.text}</span>
              <button className="icon-btn" aria-label="Dismiss" onClick={() => setToasts((all) => all.filter((x) => x.id !== t.id))}>
                <X size={14} />
              </button>
            </div>
          ))}
        </div>,
        document.body,
      )}
    </Ctx.Provider>
  );
}

export const useToast = () => useContext(Ctx);

/**
 * Run an action with its outcome in a toast, then `refresh` (after a success;
 * with `always`, after a failure too: what failed may have changed things).
 * Resolves to whether it worked.
 */
export function useAct(refresh: () => void, opts: { always?: boolean } = {}) {
  const toast = useToast();
  return async (f: () => Promise<unknown>, done: string): Promise<boolean> => {
    let worked = false;
    try {
      await f();
      toast.ok(done);
      worked = true;
    } catch (e) {
      toast.error(e);
    }
    if (worked || opts.always) refresh();
    return worked;
  };
}
