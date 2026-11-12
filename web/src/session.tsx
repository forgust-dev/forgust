import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api, setCsrf, type S } from "./api/client";

type Session = {
  user: S["UserDto"] | null;
  /** The administrator signed in as `user` (impersonation), if any. */
  impersonator: S["UserDto"] | null;
  loading: boolean;
  setSession: (s: S["SessionInfo"] | null) => void;
  /** Reload the signed-in user (after a profile change), keeping the caches. */
  refresh: () => Promise<void>;
  logout: () => Promise<void>;
};

const Ctx = createContext<Session>({ user: null, impersonator: null, loading: true, setSession: () => {}, refresh: async () => {}, logout: async () => {} });

export function SessionProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<S["UserDto"] | null>(null);
  const [impersonator, setImpersonator] = useState<S["UserDto"] | null>(null);
  const [loading, setLoading] = useState(true);
  const qc = useQueryClient();

  const setSession = (s: S["SessionInfo"] | null) => {
    setCsrf(s?.csrf_token ?? null);
    setUser(s?.user ?? null);
    setImpersonator(s?.impersonator ?? null);
    qc.clear();
  };

  useEffect(() => {
    api
      .get<S["SessionInfo"]>("/session")
      .then((s) => {
        setCsrf(s.csrf_token);
        setUser(s.user);
        setImpersonator(s.impersonator ?? null);
      })
      .catch(() => setUser(null))
      .finally(() => setLoading(false));
  }, []);

  const refresh = async () => {
    const s = await api.get<S["SessionInfo"]>("/session").catch(() => null);
    if (s) setUser(s.user);
  };

  const logout = async () => {
    await api.post("/auth/logout").catch(() => {});
    setSession(null);
  };

  return <Ctx.Provider value={{ user, impersonator, loading, setSession, refresh, logout }}>{children}</Ctx.Provider>;
}

export const useSession = () => useContext(Ctx);
