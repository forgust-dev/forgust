import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";
import { flushSync } from "react-dom";

export type Theme = "light" | "dark" | "system";

const Ctx = createContext<{ theme: Theme; setTheme: (t: Theme) => void }>({ theme: "system", setTheme: () => {} });

function read(): Theme {
  try {
    const t = localStorage.getItem("forgust.theme");
    return t === "light" || t === "dark" ? t : "system";
  } catch {
    return "system";
  }
}

function apply(theme: Theme) {
  const el = document.documentElement;
  if (theme === "system") delete el.dataset.theme;
  else el.dataset.theme = theme;
}

/**
 * Every colour switches at once (`.theme-switching`, styles.css): one that eases
 * on its own (buttons, sidebar links) would trail behind the rest of the page.
 */
function switchTo(theme: Theme, render: () => void) {
  const root = document.documentElement;
  root.classList.add("theme-switching");
  render();
  apply(theme);
  void root.offsetWidth; // the new colours, computed while transitions are off
  root.classList.remove("theme-switching");
}

/** Light, dark, or follow the system; stored per browser. */
export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<Theme>(read);
  useEffect(() => {
    apply(theme);
    try {
      localStorage.setItem("forgust.theme", theme);
    } catch {
      /* private mode */
    }
  }, [theme]);
  // a change crossfades the whole page (styles.css) where the browser can: the
  // new theme is in place, rendered, before the browser takes its new picture
  const setTheme = useCallback((t: Theme) => {
    const change = () => switchTo(t, () => flushSync(() => setThemeState(t)));
    if ("startViewTransition" in document) document.startViewTransition(change);
    else change();
  }, []);
  return <Ctx.Provider value={{ theme, setTheme }}>{children}</Ctx.Provider>;
}

export const useTheme = () => useContext(Ctx);

/** A per-browser preference (sidebar collapsed, split diff…). */
export function usePref<T>(key: string, initial: T): [T, (v: T) => void] {
  const [v, setV] = useState<T>(() => {
    try {
      const raw = localStorage.getItem(`forgust.${key}`);
      return raw == null ? initial : (JSON.parse(raw) as T);
    } catch {
      return initial;
    }
  });
  const set = (nv: T) => {
    setV(nv);
    try {
      localStorage.setItem(`forgust.${key}`, JSON.stringify(nv));
    } catch {
      /* private mode */
    }
  };
  return [v, set];
}
