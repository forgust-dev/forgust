import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import type { S } from "../api/client";

/** What the current page is about: drives the sidebar and the breadcrumbs. */
export type Nav =
  | { kind: "global" }
  | { kind: "project"; project: S["ProjectDetail"]; ref?: string; crumbs?: { label: string; to?: string }[] }
  | { kind: "group"; group: S["GroupDetail"]; crumbs?: { label: string; to?: string }[] };

const NavCtx = createContext<{ nav: Nav; setNav: (n: Nav) => void }>({ nav: { kind: "global" }, setNav: () => {} });

export function NavProvider({ children }: { children: ReactNode }) {
  const [nav, setNav] = useState<Nav>({ kind: "global" });
  return <NavCtx.Provider value={{ nav, setNav }}>{children}</NavCtx.Provider>;
}

export const useNav = () => useContext(NavCtx).nav;

/** Pages declare their context; it resets to global when they unmount. */
export function useSetNav(nav: Nav, deps: unknown[]) {
  const { setNav } = useContext(NavCtx);
  useEffect(() => {
    setNav(nav);
    return () => setNav({ kind: "global" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}

