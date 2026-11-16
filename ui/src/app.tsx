// Sign's own interface (Ferrum docs/plan/apps-ui.md) at /sign/ for people who are signed in: their
// requests, sending one, following one, and templates, with Sign's records (events, settings,
// reports) in the records frame, Sign's modules in the sidebar under the platform's bar. A signer's
// page is another file (public.tsx): it has no sign-in and loads none of this.

import { useEffect } from "react";
import { Link, useRouteError } from "react-router";
import { shell, t } from "@ferrum/client";
import { StatePanel } from "@ferrum/kit";
import { mountApp, RequireAuth, setFrameMode } from "@ferrum/records/frame";
import { Requests } from "./Requests";

/** A page of the records frame, loaded when first opened. */
const page = (name: "FormView" | "ListView" | "ReportView") => () => import("./records").then((m) => ({ Component: m[name] }));

/** Sign's packages: the sidebar shows their modules. */
const SIGN_PACKAGES = ["sign"];

function RouteError() {
  return <StatePanel kind="failed" error={useRouteError()} onRetry={() => window.location.reload()} />;
}

function NotFound() {
  return (
    <StatePanel kind="empty" title={t("Page not found")} action={<Link to="/">{t("Go to your requests")}</Link>} />
  );
}

/** Every page tells the shell it started. */
function Started() {
  useEffect(() => shell().ready(), []);
  return null;
}

setFrameMode("app", SIGN_PACKAGES);

mountApp({
  route: "sign",
  routes: [
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
        { path: "*", element: <NotFound /> },
      ],
    },
  ],
});
