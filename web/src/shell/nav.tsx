import { createContext, useContext, useEffect, useLayoutEffect, useRef, useState, type Dispatch, type ReactNode, type SetStateAction } from "react";
import type { S } from "../api/client";

/**
 * What the current page is about: drives the sidebar and the breadcrumbs. `pending`:
 * the app opened on a page whose context is still loading (a project's address), so
 * the sidebar shows no menu until it comes, rather than the main menu first.
 */
export type Nav =
  | { kind: "global" }
  // `keep`: what the sidebar went on showing while this page loads (KeepNav); the rest
  // (breadcrumbs, the New menu, the palette) has no context meanwhile
  | { kind: "pending"; keep?: SideView }
  // `ide`: the page is the web IDE (a file found opens in it)
  | { kind: "project"; project: S["ProjectDetail"]; ref?: string; ide?: boolean; crumbs?: { label: string; to?: string }[] }
  | { kind: "group"; group: S["GroupDetail"]; crumbs?: { label: string; to?: string }[] };

/** The areas that are not a project or a group but open their own menu, as one does:
 *  the instance's administration and your settings. */
export type Area = "admin" | "settings";

/** What the sidebar shows: a context and, at the top level, an area's menu. */
export type SideView = { nav: Nav; area: Area | null };

const GLOBAL: Nav = { kind: "global" };
const PENDING: Nav = { kind: "pending" };

type NavState = {
  nav: Nav;
  setNav: Dispatch<SetStateAction<Nav>>;
  shown: { current: Nav };
  shownArea: { current: Area | null };
  keeping: { current: number };
};

const NavCtx = createContext<NavState>({ nav: PENDING, setNav: () => {}, shown: { current: PENDING }, shownArea: { current: null }, keeping: { current: 0 } });

export function NavProvider({ children }: { children: ReactNode }) {
  // pending until the first page is on screen (useSettleNav)
  const [nav, setNav] = useState<Nav>(PENDING);
  // the context on screen: what a page still loading keeps (KeepNav)
  const shown = useRef<Nav>(nav);
  useLayoutEffect(() => {
    shown.current = nav.kind === "pending" && nav.keep ? nav.keep.nav : nav;
  }, [nav]);
  // the area menu the sidebar shows (useSideView), kept with the context
  const shownArea = useRef<Area | null>(null);
  // how many pages are loading their context (KeepNav), for useSettleNav
  const keeping = useRef(0);
  return <NavCtx.Provider value={{ nav, setNav, shown, shownArea, keeping }}>{children}</NavCtx.Provider>;
}

/** The page's context: for what acts on it (breadcrumbs, menus, the palette). While a
 *  page loads it is pending, even as the sidebar keeps the last one (useSideView). */
export const useNav = () => useContext(NavCtx).nav;

/**
 * What the sidebar shows: the page's context, or while a page loads the one it showed
 * before, with its area. `area` is the one worked out from the address (`fromUrl`),
 * unless a loading page keeps the previous one; the sidebar's is recorded for that.
 */
export function useSideView(fromUrl: (nav: Nav) => Area | null): SideView {
  const { nav, shownArea } = useContext(NavCtx);
  const kept = nav.kind === "pending" ? nav.keep : undefined;
  const view = kept ?? { nav, area: fromUrl(nav) };
  useEffect(() => {
    shownArea.current = view.area;
  });
  return view;
}

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
    if (keeping.current === 0) setNav((n) => (n.kind === "pending" && !n.keep ? GLOBAL : n));
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
  const { setNav, shown, shownArea, keeping } = useContext(NavCtx);
  // what the sidebar showed, taken once before this frame is painted (its own effects,
  // run later, already see the new address)
  const kept = useRef<Nav | null>(null);
  useLayoutEffect(() => {
    keeping.current++;
    kept.current = shown.current.kind === "pending" ? PENDING : { kind: "pending", keep: { nav: shown.current, area: shownArea.current } };
    setNav(kept.current);
    return () => {
      keeping.current--;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useEffect(() => {
    // again after the page that left reset it to global (its cleanup runs now)
    if (kept.current) setNav(kept.current);
    return () => setNav(GLOBAL);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return null;
}
