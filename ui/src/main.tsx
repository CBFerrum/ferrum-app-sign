// Sign's own interface (Ferrum docs/plan/apps-ui.md): a signer's page at /sign/s/… that anyone with
// their link opens, and the app for people who are signed in: the requests, sending one, following
// one, and the records behind them in the records frame. Which of the two loads is decided here, so
// a signer on a phone downloads only the page they need and none of the app.

import { addCatalogs, isPublicPage } from "@ferrum/client";
import "./styles.css";

// Its own words in the page's language (src/locales), fetched as the page starts; it is drawn once they are in.
void addCatalogs({ ar: () => import("./locales/ar.json"), hi: () => import("./locales/hi.json") });

void (isPublicPage() ? import("./public") : import("./app"));
