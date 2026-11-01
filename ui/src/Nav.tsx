// Sign's own screens, at the top of each.

import { NavLink } from "react-router";

export function Nav() {
  return (
    <nav className="sg-tabs" aria-label="Sign">
      <NavLink to="/" end>
        Requests
      </NavLink>
      <NavLink to="/templates">Templates</NavLink>
    </nav>
  );
}
