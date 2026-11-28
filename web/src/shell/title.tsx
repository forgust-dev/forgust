import { createContext, useCallback, useContext, useEffect, useId, useState, type ReactNode } from "react";
import { useNav } from "./nav";

type Own = { key: string; title: string };

const TitleCtx = createContext<(key: string, title: string) => () => void>(() => () => {});

/**
 * The browser's tab names the page: what it shows first (the name a page gives itself,
 * else its breadcrumbs, the last first), then its project or group, then the site. Tabs,
 * history and bookmarks are told apart by their start. While a page's context loads, the
 * last title stays.
 */
export function TitleProvider({ children }: { children: ReactNode }) {
  const nav = useNav();
  // the names given by the pages on screen (usePageTitle): the last one given is shown
  const [owns, setOwns] = useState<Own[]>([]);
  const register = useCallback((key: string, title: string) => {
    setOwns((o) => [...o.filter((x) => x.key !== key), { key, title }]);
    return () => setOwns((o) => o.filter((x) => x.key !== key));
  }, []);
  const own = owns.at(-1)?.title;
  useEffect(() => {
    if (nav.kind === "pending") return;
    const at = nav.kind === "project" ? { path: nav.project.full_path, crumbs: nav.crumbs } : nav.kind === "group" ? { path: nav.group.full_path, crumbs: nav.crumbs } : null;
    const parts = own ? [own] : (at?.crumbs ?? []).map((c) => c.label).reverse();
    if (at) parts.push(at.path);
    parts.push("Forgust");
    document.title = parts.join(" · ");
  }, [nav, own]);
  return <TitleCtx.Provider value={register}>{children}</TitleCtx.Provider>;
}

/**
 * The page's own name in the tab ("Fix the login · Issue #12"); its project or group and
 * the site follow. Nothing yet (its data still loading): the breadcrumbs stand in.
 */
export function usePageTitle(title: string | null | undefined) {
  const register = useContext(TitleCtx);
  const key = useId();
  useEffect(() => (title ? register(key, title) : undefined), [register, key, title]);
}
