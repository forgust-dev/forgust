import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api, setCsrf, type S } from "./api/client";

type Session = {
  user: S["UserDto"] | null;
  /** The administrator signed in as `user` (impersonation), if any. */
  impersonator: S["UserDto"] | null;
  /** Whether the instance sends email: without it no page promises (or waits for) an emailed link. */
  emailEnabled: boolean;
  loading: boolean;
  setSession: (s: S["SessionInfo"] | null) => void;
  /** Reload the signed-in user (after a profile change), keeping the caches. */
  refresh: () => Promise<void>;
  logout: () => Promise<void>;
};

const Ctx = createContext<Session>({ user: null, impersonator: null, emailEnabled: true, loading: true, setSession: () => {}, refresh: async () => {}, logout: async () => {} });

// asked as the app's code loads, while its fonts do (main.tsx), not after the first render
let firstAsk: Promise<S["SessionInfo"]> | null = api.get<S["SessionInfo"]>("/session");
firstAsk.catch(() => {});

const SIGNED_IN = "forgust.signed-in";

/** Whether someone was signed in here when the app last knew: the frame shown while the
 *  session loads takes its shape from it (ShellFrame), the sidebar's or not. */
export function wasSignedIn(): boolean {
  try {
    return localStorage.getItem(SIGNED_IN) === "1";
  } catch {
    return false;
  }
}

function rememberSignedIn(yes: boolean) {
  try {
    if (yes) localStorage.setItem(SIGNED_IN, "1");
    else localStorage.removeItem(SIGNED_IN);
  } catch {
    // storage blocked: the frame takes the signed-out shape
  }
}

export function SessionProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<S["UserDto"] | null>(null);
  const [impersonator, setImpersonator] = useState<S["UserDto"] | null>(null);
  const [emailEnabled, setEmailEnabled] = useState(true);
  const [loading, setLoading] = useState(true);
  const qc = useQueryClient();

  const setSession = (s: S["SessionInfo"] | null) => {
    setCsrf(s?.csrf_token ?? null);
    setUser(s?.user ?? null);
    setImpersonator(s?.impersonator ?? null);
    setEmailEnabled(s?.email_enabled ?? true);
    qc.clear();
  };

  useEffect(() => {
    const asked = firstAsk ?? api.get<S["SessionInfo"]>("/session");
    firstAsk = null;
    asked
      .then((s) => {
        setCsrf(s.csrf_token);
        setUser(s.user);
        setImpersonator(s.impersonator ?? null);
        setEmailEnabled(s.email_enabled);
      })
      .catch(() => setUser(null))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    if (!loading) rememberSignedIn(!!user);
  }, [user, loading]);

  const refresh = async () => {
    const s = await api.get<S["SessionInfo"]>("/session").catch(() => null);
    if (s) setUser(s.user);
  };

  const logout = async () => {
    await api.post("/auth/logout").catch(() => {});
    setSession(null);
  };

  return <Ctx.Provider value={{ user, impersonator, emailEnabled, loading, setSession, refresh, logout }}>{children}</Ctx.Provider>;
}

export const useSession = () => useContext(Ctx);
