import { createContext, useContext, useEffect, useLayoutEffect, useRef, useState, type Dispatch, type ReactNode, type SetStateAction } from "react";
import type { S } from "../api/client";

/**
 * What the current page is about: drives the sidebar and the breadcrumbs. `pending`:
 * the app opened on a page whose context is still loading (a project's address), so
 * the sidebar shows no menu until it comes, rather than the main menu first.
 */
export type Nav =
  | { kind: "global" }
  | { kind: "pending" }
  | { kind: "project"; project: S["ProjectDetail"]; ref?: string; crumbs?: { label: string; to?: string }[] }
  | { kind: "group"; group: S["GroupDetail"]; crumbs?: { label: string; to?: string }[] };

const GLOBAL: Nav = { kind: "global" };
const PENDING: Nav = { kind: "pending" };

type NavState = {
  nav: Nav;
  setNav: Dispatch<SetStateAction<Nav>>;
  shown: { current: Nav };
  keeping: { current: number };
};

const NavCtx = createContext<NavState>({ nav: PENDING, setNav: () => {}, shown: { current: PENDING }, keeping: { current: 0 } });

export function NavProvider({ children }: { children: ReactNode }) {
  // pending until the first page is on screen (useSettleNav)
  const [nav, setNav] = useState<Nav>(PENDING);
  // the context on screen: what a page still loading keeps (KeepNav)
  const shown = useRef<Nav>(nav);
  useLayoutEffect(() => {
    shown.current = nav;
  }, [nav]);
  // how many pages are loading their context (KeepNav), for useSettleNav
  const keeping = useRef(0);
  return <NavCtx.Provider value={{ nav, setNav, shown, keeping }}>{children}</NavCtx.Provider>;
}

export const useNav = () => useContext(NavCtx).nav;

/** Pages declare their context; it resets to global when they unmount. */
export function useSetNav(nav: Nav, deps: unknown[]) {
  const { setNav } = useContext(NavCtx);
  useEffect(() => {
    setNav(nav);
    return () => setNav(GLOBAL);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}

/**
 * The shell calls it once the first page is mounted, before it is painted: the main
 * menu shows, unless that page is still loading its context (a project's or a group's
 * address opened as is), which then stays pending until it comes. (A layout effect runs
 * after those of the shell's descendants, KeepNav's count among them.)
 */
export function useSettleNav() {
  const { setNav, keeping } = useContext(NavCtx);
  useLayoutEffect(() => {
    if (keeping.current === 0) setNav((n) => (n.kind === "pending" ? GLOBAL : n));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}

/**
 * Rendered while a project's or a group's page loads: the sidebar keeps the context
 * it shows (the page left unmounted in the same commit, its reset to global is
 * overridden) rather than flashing the main menu until the next context arrives.
 * Opened on such a page, the app has none yet: the sidebar stays pending.
 */
export function KeepNav() {
  const { setNav, shown, keeping } = useContext(NavCtx);
  useLayoutEffect(() => {
    keeping.current++;
    return () => {
      keeping.current--;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useEffect(() => {
    setNav(shown.current);
    return () => setNav(GLOBAL);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return null;
}
