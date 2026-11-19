import { createContext, useContext, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import type { S } from "../api/client";

/** What the current page is about: drives the sidebar and the breadcrumbs. */
export type Nav =
  | { kind: "global" }
  | { kind: "project"; project: S["ProjectDetail"]; ref?: string; crumbs?: { label: string; to?: string }[] }
  | { kind: "group"; group: S["GroupDetail"]; crumbs?: { label: string; to?: string }[] };

const GLOBAL: Nav = { kind: "global" };

const NavCtx = createContext<{ nav: Nav; setNav: (n: Nav) => void; shown: { current: Nav } }>({ nav: GLOBAL, setNav: () => {}, shown: { current: GLOBAL } });

export function NavProvider({ children }: { children: ReactNode }) {
  const [nav, setNav] = useState<Nav>(GLOBAL);
  // the context on screen: what a page still loading keeps (KeepNav)
  const shown = useRef<Nav>(nav);
  useLayoutEffect(() => {
    shown.current = nav;
  }, [nav]);
  return <NavCtx.Provider value={{ nav, setNav, shown }}>{children}</NavCtx.Provider>;
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
 * Rendered while a project's or a group's page loads: the sidebar keeps the context
 * it shows (the page left unmounted in the same commit, its reset to global is
 * overridden) rather than flashing the main menu until the next context arrives.
 */
export function KeepNav() {
  const { setNav, shown } = useContext(NavCtx);
  useEffect(() => {
    setNav(shown.current);
    return () => setNav(GLOBAL);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return null;
}
