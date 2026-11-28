import "@fontsource-variable/inter";
import "@fontsource-variable/bricolage-grotesque";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/500.css";
import "@fontsource/jetbrains-mono/600.css";
import { StrictMode, Suspense, lazy, type ReactElement } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter, Navigate, Route, Routes, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ApiError, setOnMineChanged } from "./api/client";
import { refreshMine } from "./prefetch";
import { SessionProvider, useSession } from "./session";
import { PageErrorBoundary, Spinner } from "./components/ui";
import { ForgotPassword, Login, ResetPassword, Setup, VerifyEmail } from "./pages/Auth";
import { Dashboard, ExploreRoute } from "./pages/Dashboard";
import { SearchPage } from "./pages/Search";
import { UnsubscribePage } from "./pages/Unsubscribe";
import { Work } from "./pages/Work";
import { NewGroup, NewProject } from "./pages/New";
import { Shell, ShellFrame } from "./shell/Shell";
import { KeepNav, NavProvider } from "./shell/nav";
import { TitleProvider } from "./shell/title";
import { PaletteHost } from "./shell/palette";
import { ThemeProvider } from "./shell/theme";
import { ToastProvider } from "./shell/toast";
import { DialogProvider } from "./shell/dialog";
import { UnsavedGuard } from "./components/settings";
// code split per route: each page loads only what it needs
const PathRoute = lazy(() => import("./pages/PathRoute").then((m) => ({ default: m.PathRoute })));
const Profile = lazy(() => import("./pages/Profile").then((m) => ({ default: m.Profile })));
const OAuthAuthorize = lazy(() => import("./pages/OAuthAuthorize").then((m) => ({ default: m.OAuthAuthorize })));
const ImportPage = lazy(() => import("./pages/Import").then((m) => ({ default: m.ImportPage })));
const ImportDetailPage = lazy(() => import("./pages/Import").then((m) => ({ default: m.ImportDetailPage })));
const Admin = lazy(() => import("./pages/Admin").then((m) => ({ default: m.Admin })));
const UiKit = lazy(() => import("./pages/UiKit").then((m) => ({ default: m.UiKit })));
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

setOnMineChanged(() => refreshMine(queryClient));

function RequireUser({ children }: { children: ReactElement }) {
  const { user, loading } = useSession();
  const loc = useLocation();
  if (loading) return <Spinner page />;
  if (!user) return <Navigate to={`/login?next=${encodeURIComponent(loc.pathname + loc.search)}`} replace />;
  return children;
}

function App() {
  const { loading } = useSession();
  const loc = useLocation();
  // the frame, where the page will be, while we learn who is signed in
  if (loading) return <ShellFrame />;
  return (
    <Shell>
      <PaletteHost />
      <PageErrorBoundary resetKey={loc.pathname}>
      {/* a page's code still loading: the sidebar keeps its menu meanwhile */}
      <Suspense
        fallback={
          <>
            <KeepNav />
            <Spinner page />
          </>
        }
      >
      <Routes>
        <Route path="/login" element={<Login />} />
        <Route path="/setup" element={<Setup />} />
        <Route path="/forgot-password" element={<ForgotPassword />} />
        <Route path="/reset-password" element={<ResetPassword />} />
        <Route path="/verify-email" element={<VerifyEmail />} />
        <Route path="/" element={<RequireUser><Work /></RequireUser>} />
        <Route path="/dashboard/projects" element={<RequireUser><Dashboard /></RequireUser>} />
        <Route path="/dashboard/groups" element={<RequireUser><Dashboard tab="groups" /></RequireUser>} />
        <Route path="/explore" element={<ExploreRoute />} />
        <Route path="/search" element={<SearchPage />} />
        <Route path="/unsubscribe" element={<UnsubscribePage />} />
        <Route path="/new/group" element={<RequireUser><NewGroup /></RequireUser>} />
        <Route path="/new/project" element={<RequireUser><NewProject /></RequireUser>} />
        <Route path="/import" element={<RequireUser><ImportPage /></RequireUser>} />
        <Route path="/import/:id" element={<RequireUser><ImportDetailPage /></RequireUser>} />
        <Route path="/profile/*" element={<RequireUser><Profile /></RequireUser>} />
        <Route path="/admin/*" element={<RequireUser><Admin /></RequireUser>} />
        <Route path="/oauth/authorize" element={<RequireUser><OAuthAuthorize /></RequireUser>} />
        <Route path="/-/ui" element={<RequireUser><UiKit /></RequireUser>} />
        <Route path="/*" element={<PathRoute />} />
      </Routes>
      </Suspense>
      </PageErrorBoundary>
    </Shell>
  );
}

// the first frame in the app's own fonts rather than swapping into them: they load
// meanwhile (a few ms from the cache), 100 ms at most, then the fallback shows as before
const fonts = Promise.race([
  Promise.all(['700 18px "Bricolage Grotesque Variable"', '400 14px "Inter Variable"'].map((f) => document.fonts.load(f))),
  new Promise((done) => setTimeout(done, 100)),
]).catch(() => {});

void fonts.then(() =>
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <BrowserRouter>
          <SessionProvider>
            <ThemeProvider>
              <ToastProvider>
                <DialogProvider>
                  <NavProvider>
                    <TitleProvider>
                      <UnsavedGuard />
                      <App />
                    </TitleProvider>
                  </NavProvider>
                </DialogProvider>
              </ToastProvider>
            </ThemeProvider>
          </SessionProvider>
        </BrowserRouter>
      </QueryClientProvider>
    </StrictMode>,
  ),
);
