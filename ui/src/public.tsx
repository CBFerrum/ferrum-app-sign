// The signer's page and nothing else: no router, no records frame, no sign-in.

import { StrictMode, useEffect } from "react";
import { createRoot } from "react-dom/client";
import { shell } from "@ferrum/client";
import { SigningPage } from "./SignPage";

function Started() {
  useEffect(() => shell().ready(), []);
  return null;
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Started />
    <SigningPage />
  </StrictMode>,
);
