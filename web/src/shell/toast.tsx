import { createContext, useCallback, useContext, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link } from "react-router-dom";
import { Bell, CheckCircle2, CircleAlert, Info, X } from "lucide-react";
import { ApiError } from "../api/client";

/** Where a notice leads, and what opening it does (marks it read). */
type Open = { to: string; onOpen?: () => void };
type Toast = { id: number; kind: "ok" | "error" | "notice"; text: string; open?: Open };

/** Something to read and act on, which stays until dismissed or withdrawn (one per `key`). */
export type Announcement = { key: string; title: string; text: string; action?: { label: string; href: string }; onDismiss?: () => void };

const Ctx = createContext<{
  ok: (t: string) => void;
  error: (e: unknown) => void;
  notice: (t: string, open?: Open) => void;
  announce: (a: Announcement) => void;
  withdraw: (key: string) => void;
}>({
  ok: () => {},
  error: () => {},
  notice: () => {},
  announce: () => {},
  withdraw: () => {},
});

let seq = 0;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  // apart from the toasts, which go by themselves: these stay until dismissed,
  // or withdrawn by what announced them once it no longer holds
  const [announcements, setAnnouncements] = useState<Announcement[]>([]);
  const dismiss = useCallback((id: number) => setToasts((t) => t.filter((x) => x.id !== id)), []);
  const push = useCallback(
    (kind: Toast["kind"], text: string, open?: Open) => {
      const id = ++seq;
      setToasts((t) => [...t.slice(-3), { id, kind, text, open }]);
      setTimeout(() => dismiss(id), kind === "ok" ? 3500 : 7000);
    },
    [dismiss],
  );
  // something that reached the person (the inbox): it leads where it happened
  const notice = useCallback((t: string, open?: Open) => push("notice", t, open), [push]);
  const ok = useCallback((t: string) => push("ok", t), [push]);
  const error = useCallback((e: unknown) => push("error", e instanceof ApiError ? e.message : String(e)), [push]);
  const announce = useCallback((a: Announcement) => setAnnouncements((all) => (all.some((x) => x.key === a.key) ? all : [...all, a])), []);
  const withdraw = useCallback((key: string) => setAnnouncements((all) => all.filter((x) => x.key !== key)), []);
  return (
    <Ctx.Provider value={{ ok, error, notice, announce, withdraw }}>
      {children}
      {createPortal(
        <div className="toasts" role="status" aria-live="polite">
          {announcements.map((a) => (
            <div key={a.key} className="toast info">
              <Info size={16} />
              <div className="grow toast-body">
                <strong>{a.title}</strong>
                <span>{a.text}</span>
                {a.action && (
                  <a className="btn small" href={a.action.href} target="_blank" rel="noreferrer">
                    {a.action.label}
                  </a>
                )}
              </div>
              <button
                className="icon-btn"
                aria-label="Dismiss"
                onClick={() => {
                  withdraw(a.key);
                  a.onDismiss?.();
                }}
              >
                <X size={14} />
              </button>
            </div>
          ))}
          {toasts.map((t) => (
            <div key={t.id} className={`toast ${t.kind}`}>
              {t.kind === "ok" ? <CheckCircle2 size={16} /> : t.kind === "notice" ? <Bell size={16} /> : <CircleAlert size={16} />}
              {t.open ? (
                <Link
                  className="grow toast-link"
                  to={t.open.to}
                  onClick={() => {
                    t.open?.onOpen?.();
                    dismiss(t.id);
                  }}
                >
                  {t.text}
                </Link>
              ) : (
                <span className="grow">{t.text}</span>
              )}
              <button className="icon-btn" aria-label="Dismiss" onClick={() => dismiss(t.id)}>
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
