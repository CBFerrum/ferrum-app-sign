// Sign's own interface (Ferrum docs/plan/apps-ui.md) at /sign/ for people who are signed in: their
// requests, sending one, following one, and templates, with Sign's records (events, settings,
// reports) in the records frame, Sign's modules in the sidebar under the platform's bar. A signer's
// page is another file (public.tsx): it has no sign-in and loads none of this.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode, useEffect } from "react";
import { createRoot } from "react-dom/client";
import { createBrowserRouter, Link, RouterProvider, useRouteError } from "react-router";
import { routeBase, shell, t, wordsReady } from "@ferrum/client";
import { ConfirmHost, EmptyState, ErrorState, Toaster } from "@ferrum/kit";
import { RequireAuth, setFrameMode } from "@ferrum/records/frame";
import { Requests } from "./Requests";

/** A page of the records frame, loaded when first opened. */
const page = (name: "FormView" | "ListView" | "ReportView" | "AppSettingsPage" | "ScreenPage") => () => import("./records").then((m) => ({ Component: m[name] }));

/** Sign's packages: the sidebar shows their modules. */
const SIGN_PACKAGES = ["sign"];

const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false, staleTime: 30_000 } },
});

function RouteError() {
  return <ErrorState error={useRouteError()} onRetry={() => window.location.reload()} />;
}

function NotFound() {
  return (
    <EmptyState title={t("Page not found")}>
      <Link to="/">{t("Go to your requests")}</Link>
    </EmptyState>
  );
}

/** Every page tells the shell it started. */
function Started() {
  useEffect(() => shell().ready(), []);
  return null;
}

setFrameMode("app", SIGN_PACKAGES);

const router = createBrowserRouter(
  [
    {
      path: "/",
      element: (
        <>
          <Started />
          <RequireAuth />
        </>
      ),
      errorElement: <RouteError />,
      children: [
        { index: true, element: <Requests /> },
        { path: "new", lazy: () => import("./New").then((m) => ({ Component: m.NewRequest })) },
        { path: "templates", lazy: () => import("./Templates").then((m) => ({ Component: m.Templates })) },
        { path: "template/:id/fields", lazy: () => import("./TemplateFields").then((m) => ({ Component: m.TemplateFieldsRoute })) },
        { path: "request/:id", lazy: () => import("./RequestPage").then((m) => ({ Component: m.RequestRoute })) },
        { path: "request/:id/prepare/:step?", lazy: () => import("./Prepare").then((m) => ({ Component: m.PrepareRoute })) },
        { path: "e/:entity", lazy: page("ListView") },
        { path: "e/:entity/:id", lazy: page("FormView") },
        { path: "r/:name", lazy: page("ReportView") },
        { path: "apps/:app/settings", lazy: page("AppSettingsPage") },
        { path: "x/:app/:page", lazy: page("ScreenPage") },
        { path: "*", element: <NotFound /> },
      ],
    },
  ],
  { basename: routeBase("sign") },
);

// Drawn once the page's words are in (its language's catalogs, fetched as the page starts).
void wordsReady().then(() =>
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
        <Toaster />
        <ConfirmHost />
      </QueryClientProvider>
    </StrictMode>,
  ),
);
