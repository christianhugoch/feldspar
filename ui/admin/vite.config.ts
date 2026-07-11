import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The bundle is served by `sc-server` from a static directory at the site root,
// and its `BOOTSTRAP_HTML` fallback references stable `/main.js` + `/main.css`
// entry points (no hashed asset names). We therefore pin the output filenames
// and emit a single CSS file so a deep-link that falls back to the bootstrap
// document loads the same assets as `index.html`. Everything is same-origin so
// the strict CSP (`script-src 'self'; style-src 'self'`) is satisfied.
export default defineConfig({
  plugins: [react()],
  base: "/",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    cssCodeSplit: false,
    rollupOptions: {
      output: {
        entryFileNames: "main.js",
        chunkFileNames: "main-[name].js",
        assetFileNames: (asset) =>
          asset.names?.some((n) => n.endsWith(".css")) ? "main.css" : "assets/[name][extname]",
      },
    },
  },
});
