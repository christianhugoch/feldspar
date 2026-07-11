// SPA entry point. Mounts the React admin app into the bootstrap document's
// `#root` (served either by `sc-server`'s `BOOTSTRAP_HTML` fallback or the built
// `index.html`). Bootstrap's stylesheet is imported here so the bundler emits it
// as a single same-origin `main.css` (loaded via `<link>`, satisfying the strict
// `style-src 'self'` CSP — no inline styles).
import "bootstrap/dist/css/bootstrap.min.css";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";

const container = document.getElementById("root");
if (!container) throw new Error("missing #root mount point");

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
