import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The bundle is served by `sc-server` from a static directory at the site root.
//
// Output filenames keep Vite's default **content hash** (`assets/index-a1b2c3.js`)
// rather than being pinned to stable ones. A pinned name is a name a browser has
// already cached: rebuild the admin UI, restart the server, reload, and the page
// is still running the old bundle until someone thinks to empty the cache. The
// hash makes a changed file a changed URL, which is the only version of this that
// works without asking anyone to remember anything — and it lets the server mark
// those URLs immutable (see `router.rs`). The document that names them is served
// from this build's own `index.html`, including on the deep-link fallback, so it
// always links the assets that were built beside it.
//
// A single CSS file (`cssCodeSplit: false`) keeps the whole admin theme in one
// `<link>`. Everything is same-origin, so the strict CSP (`script-src 'self';
// style-src 'self'`) is satisfied.
export default defineConfig({
  plugins: [react()],
  base: "/",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    cssCodeSplit: false,
  },
});
