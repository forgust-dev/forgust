import "@fontsource-variable/inter";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/500.css";
import "@fontsource/jetbrains-mono/600.css";
import { StrictMode, Suspense, lazy } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter, Navigate, Route, Routes, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ApiError } from "./api/client";
import { SessionProvider, useSession } from "./session";
import { Spinner } from "./components/ui";
import { ForgotPassword, Login, ResetPassword, Setup, VerifyEmail } from "./pages/Auth";
import { Dashboard, Explore } from "./pages/Dashboard";
import { SearchPage } from "./pages/Search";
import { UnsubscribePage } from "./pages/Unsubscribe";
import { Work } from "./pages/Work";
import { NewGroup, NewProject } from "./pages/New";
import { Shell } from "./shell/Shell";
import { NavProvider } from "./shell/nav";
import { PaletteHost } from "./shell/palette";
import { ThemeProvider } from "./shell/theme";
import { ToastProvider } from "./shell/toast";
// code split per route: each page loads only what it needs
const PathRoute = lazy(() => import("./pages/PathRoute").then((m) => ({ default: m.PathRoute })));
const Profile = lazy(() => import("./pages/Profile").then((m) => ({ default: m.Profile })));
const OAuthAuthorize = lazy(() => import("./pages/OAuthAuthorize").then((m) => ({ default: m.OAuthAuthorize })));
const ImportPage = lazy(() => import("./pages/Import").then((m) => ({ default: m.ImportPage })));
const ImportDetailPage = lazy(() => import("./pages/Import").then((m) => ({ default: m.ImportDetailPage })));
const Admin = lazy(() => import("./pages/Admin").then((m) => ({ default: m.Admin })));
import "./styles.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      retry: (n, e) => !(e instanceof ApiError && e.status < 500) && n < 2,
      refetchOnWindowFocus: false,
    },
  },
});

function RequireUser({ children }: { children: JSX.Element }) {
  const { user, loading } = useSession();
  const loc = useLocation();
  if (loading) return <Spinner />;
  if (!user) return <Navigate to={`/login?next=${encodeURIComponent(loc.pathname + loc.search)}`} replace />;
  return children;
}

function App() {
  const { loading } = useSession();
  if (loading) return <Spinner />;
  return (
    <Shell>
      <PaletteHost />
      <Suspense fallback={<Spinner />}>
      <Routes>
        <Route path="/login" element={<Login />} />
        <Route path="/setup" element={<Setup />} />
        <Route path="/forgot-password" element={<ForgotPassword />} />
        <Route path="/reset-password" element={<ResetPassword />} />
        <Route path="/verify-email" element={<VerifyEmail />} />
        <Route path="/" element={<RequireUser><Work /></RequireUser>} />
        <Route path="/dashboard/projects" element={<RequireUser><Dashboard /></RequireUser>} />
        <Route path="/explore" element={<Explore />} />
        <Route path="/search" element={<SearchPage />} />
        <Route path="/unsubscribe" element={<UnsubscribePage />} />
        <Route path="/new/group" element={<RequireUser><NewGroup /></RequireUser>} />
        <Route path="/new/project" element={<RequireUser><NewProject /></RequireUser>} />
        <Route path="/import" element={<RequireUser><ImportPage /></RequireUser>} />
        <Route path="/import/:id" element={<RequireUser><ImportDetailPage /></RequireUser>} />
        <Route path="/profile/*" element={<RequireUser><Profile /></RequireUser>} />
        <Route path="/admin/*" element={<RequireUser><Admin /></RequireUser>} />
        <Route path="/oauth/authorize" element={<RequireUser><OAuthAuthorize /></RequireUser>} />
        <Route path="/*" element={<PathRoute />} />
      </Routes>
      </Suspense>
    </Shell>
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <SessionProvider>
          <ThemeProvider>
            <ToastProvider>
              <NavProvider>
                <App />
              </NavProvider>
            </ToastProvider>
          </ThemeProvider>
        </SessionProvider>
      </BrowserRouter>
    </QueryClientProvider>
  </StrictMode>,
);
