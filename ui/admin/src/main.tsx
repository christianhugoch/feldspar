// SPA entry point. Mounts the React admin app into the bootstrap document's
// `#root` (served either by `sc-server`'s `BOOTSTRAP_HTML` fallback or the built
// `index.html`).
//
// The stylesheet is **Tabler** (`src/vendor/tabler`), the admin theme: it is a
// superset of Bootstrap 5's CSS, so the react-bootstrap components the screens
// are built from keep working and gain Tabler's styling, and the layout classes
// (`.page`, `.navbar-vertical`, `.card`, `.page-header`) come from the same
// file. It is imported here so the bundler emits it as a single same-origin
// `main.css` (loaded via `<link>`, satisfying the strict `style-src 'self'`
// CSP — no inline styles), and it is self-contained: every image in it is a
// `data:` URI and its font stack falls back to the system UI font, so nothing
// is fetched from another origin.
import "./vendor/tabler/tabler.min.css";
import "./admin.css";

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
