import { defineConfig } from "vite";

// The IDE is a **second** bundle, served by `sc-server` under `/ide/` rather than
// at the site root (design §12.1: VS Code initializes once per page and cannot be
// unloaded, so it cannot be a screen inside the admin SPA). Entry filenames are
// pinned to `main.js` + `main.css` for the same reason `ui/admin` pins them: the
// server's `IDE_BOOTSTRAP_HTML` references them by name, so a request that falls
// back to that document loads the same assets `index.html` links.
export default defineConfig({
  base: "/ide/",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // VS Code's own code is shipped as modern ESM and is not down-levelled.
    target: "esnext",
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
  // The workbench's workers (the editor worker, the extension host, textmate,
  // search) are ES modules.
  worker: {
    format: "es",
  },
  esbuild: {
    // VS Code relies on syntax esbuild's minifier is willing to rewrite in ways
    // that break it; upstream's own demo disables this and so do we.
    minifySyntax: false,
  },
  resolve: {
    // `vscode` is an alias for the extension API package. Two copies of it would
    // be two separate service registries.
    dedupe: ["vscode", "monaco-editor"],
  },
  plugins: [
    {
      // VS Code's stylesheets must arrive as strings the workbench injects itself
      // (it owns where its styles go), not as `<link>`s Vite adds to the page
      // head. Asset references inside them still have to be resolved, so `?inline`
      // rather than `?raw`.
      name: "load-vscode-css-as-string",
      enforce: "pre",
      async resolveId(source, importer, options) {
        const resolved = await this.resolve(source, importer, options);
        if (
          resolved != null &&
          /node_modules\/(@codingame\/monaco-vscode|vscode|monaco-editor).*\.css$/.test(resolved.id)
        ) {
          return { ...resolved, id: `${resolved.id}?inline` };
        }
        return undefined;
      },
    },
  ],
});
