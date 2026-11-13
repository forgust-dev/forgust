import { createContext, useCallback, useContext, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { CheckCircle2, CircleAlert, Info, X } from "lucide-react";
import { ApiError } from "../api/client";

/** Something to read and act on, which stays until dismissed (one per `key`). */
export type Notice = { key: string; title: string; text: string; action?: { label: string; href: string }; onDismiss?: () => void };

type Toast = { id: number; kind: "ok" | "error"; text: string } | ({ id: number; kind: "info" } & Notice);

const Ctx = createContext<{ ok: (t: string) => void; error: (e: unknown) => void; notice: (n: Notice) => void }>({ ok: () => {}, error: () => {}, notice: () => {} });

let seq = 0;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const push = useCallback((kind: "ok" | "error", text: string) => {
    const id = ++seq;
    // the last few outcomes, and every notice still open
    setToasts((t) => {
      const outcomes = t.filter((x) => x.kind !== "info");
      return [...t.filter((x) => x.kind === "info" || outcomes.slice(-3).includes(x)), { id, kind, text }];
    });
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), kind === "ok" ? 3500 : 7000);
  }, []);
  const ok = useCallback((t: string) => push("ok", t), [push]);
  const error = useCallback((e: unknown) => push("error", e instanceof ApiError ? e.message : String(e)), [push]);
  const notice = useCallback((n: Notice) => setToasts((t) => (t.some((x) => x.kind === "info" && x.key === n.key) ? t : [...t, { id: ++seq, kind: "info", ...n }])), []);
  const dismiss = (t: Toast) => {
    setToasts((all) => all.filter((x) => x.id !== t.id));
    if (t.kind === "info") t.onDismiss?.();
  };
  return (
    <Ctx.Provider value={{ ok, error, notice }}>
      {children}
      {createPortal(
        <div className="toasts" role="status" aria-live="polite">
          {toasts.map((t) => (
            <div key={t.id} className={`toast ${t.kind}`}>
              {t.kind === "ok" ? <CheckCircle2 size={16} /> : t.kind === "error" ? <CircleAlert size={16} /> : <Info size={16} />}
              {t.kind === "info" ? (
                <div className="grow toast-body">
                  <strong>{t.title}</strong>
                  <span>{t.text}</span>
                  {t.action && (
                    <a className="btn small" href={t.action.href} target="_blank" rel="noreferrer">
                      {t.action.label}
                    </a>
                  )}
                </div>
              ) : (
                <span className="grow">{t.text}</span>
              )}
              <button className="icon-btn" aria-label="Dismiss" onClick={() => dismiss(t)}>
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
