import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link } from "react-router-dom";
import { Bell, CheckCircle2, CircleAlert, Info, X } from "lucide-react";
import { ApiError } from "../api/client";

/** Where a notice leads, and what opening it does (marks it read). */
type Open = { to: string; onOpen?: () => void };
/** What a done toast says besides its line: what the change means, and how to take it back. */
export type Done = { detail?: string; undo?: () => void };
type Toast = { id: number; kind: "ok" | "error" | "notice"; text: string; open?: Open; leaving?: boolean } & Done;

/** Something to read and act on, which stays until dismissed or withdrawn (one per `key`). */
export type Announcement = { key: string; title: string; text: string; action?: { label: string; href: string }; onDismiss?: () => void };

const Ctx = createContext<{
  ok: (t: string, more?: Done) => void;
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
/** How long one takes to leave (`.toast.leaving`, styles.css): it stays that long on its way out. */
const LEAVE_MS = 200;
/** How long one stays: a done line; one with more to read, or to undo; an error or a notice; and
 *  once the pointer that held it left. */
const STAY_MS = { done: 3500, read: 6000, other: 7000, after: 2500 };

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  // each toast's going, put off while the pointer is on it (to read it, to undo)
  const timers = useRef(new Map<number, number>());
  // apart from the toasts, which go by themselves: these stay until dismissed,
  // or withdrawn by what announced them once it no longer holds
  const [announcements, setAnnouncements] = useState<(Announcement & { leaving?: boolean })[]>([]);
  // out through the edge it came in by, then out of the list
  const dismiss = useCallback((id: number) => {
    window.clearTimeout(timers.current.get(id));
    timers.current.delete(id);
    setToasts((t) => t.map((x) => (x.id === id ? { ...x, leaving: true } : x)));
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), LEAVE_MS);
  }, []);
  const leaveIn = useCallback(
    (id: number, ms: number) => {
      window.clearTimeout(timers.current.get(id));
      timers.current.set(id, window.setTimeout(() => dismiss(id), ms));
    },
    [dismiss],
  );
  const push = useCallback(
    (kind: Toast["kind"], text: string, open?: Open, more: Done = {}) => {
      const id = ++seq;
      setToasts((t) => [...t.slice(-3), { id, kind, text, open, ...more }]);
      leaveIn(id, kind !== "ok" ? STAY_MS.other : more.detail || more.undo ? STAY_MS.read : STAY_MS.done);
    },
    [leaveIn],
  );
  // something that reached the person (the inbox): it leads where it happened
  const notice = useCallback((t: string, open?: Open) => push("notice", t, open), [push]);
  const ok = useCallback((t: string, more?: Done) => push("ok", t, undefined, more), [push]);
  const error = useCallback((e: unknown) => push("error", e instanceof ApiError ? e.message : String(e)), [push]);
  const announce = useCallback(
    (a: Announcement) =>
      setAnnouncements((all) => {
        const here = all.find((x) => x.key === a.key);
        if (!here) return [...all, a];
        // one per key; announced again on its way out, it stays
        return here.leaving ? all.map((x) => (x === here ? { ...x, leaving: false } : x)) : all;
      }),
    [],
  );
  const withdraw = useCallback((key: string) => {
    setAnnouncements((all) => all.map((x) => (x.key === key ? { ...x, leaving: true } : x)));
    setTimeout(() => setAnnouncements((all) => all.filter((x) => !(x.key === key && x.leaving))), LEAVE_MS);
  }, []);
  // the same for as long as the page lives: a toast shown does not render again what can show one
  const value = useMemo(() => ({ ok, error, notice, announce, withdraw }), [ok, error, notice, announce, withdraw]);
  return (
    <Ctx.Provider value={value}>
      {children}
      {createPortal(
        <div className="toasts" role="status" aria-live="polite">
          {announcements.map((a) => (
            <div key={a.key} className={`toast info${a.leaving ? " leaving" : ""}`}>
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
                title="Dismiss"
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
            <div
              key={t.id}
              className={`toast ${t.kind}${t.detail ? " detailed" : ""}${t.leaving ? " leaving" : ""}`}
              onPointerEnter={() => window.clearTimeout(timers.current.get(t.id))}
              onPointerLeave={() => !t.leaving && leaveIn(t.id, STAY_MS.after)}
            >
              {t.kind === "ok" ? <CheckCircle2 size={16} /> : t.kind === "notice" ? <Bell size={16} /> : <CircleAlert size={16} />}
              {t.detail ? (
                <div className="grow toast-body">
                  <strong>{t.text}</strong>
                  <span>{t.detail}</span>
                </div>
              ) : t.open ? (
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
              {/* Undo and the cross on one line, that of the title */}
              <div className="toast-actions">
                {t.undo && (
                  <button
                    className="btn small"
                    onClick={() => {
                      dismiss(t.id);
                      t.undo?.();
                    }}
                  >
                    Undo
                  </button>
                )}
                <button className="icon-btn" title="Dismiss" aria-label="Dismiss" onClick={() => dismiss(t.id)}>
                  <X size={14} />
                </button>
              </div>
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
