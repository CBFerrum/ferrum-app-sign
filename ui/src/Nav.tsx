// Sign's own screens, at the top of each.

import { NavLink } from "react-router";
import { t } from "@ferrum/client";

export function Nav() {
  return (
    <nav className="sg-tabs" aria-label={t("Sign")}>
      <NavLink to="/" end>
        {t("Requests")}
      </NavLink>
      <NavLink to="/templates">{t("Templates")}</NavLink>
    </nav>
  );
}
