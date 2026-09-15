// Build Saltcorn UI's builder: `dist/builder.js`, `dist/builder.css`, Monaco's
// workers and CKEditor, for the builder's admin route (TODO "The builder" §1, §2).
//
// esbuild, not v1's webpack and babel: JSX for the vendored files, React 18, and
// the dependency versions v1's lock file resolved (`package.json`). Four things
// make v1's source work here without editing it, and all four are **keyed on
// the importer being in `vendor/`**, so `src/` and `node_modules` see the real
// modules and globals:
//
// - **`fetch`** (§3). Every vendored file is loaded with
//   `import { builderFetch as fetch } from "src/builder-fetch.ts"` in front of it,
//   so its free `fetch` calls go through `routes.ts`. §3 says esbuild's `inject`,
//   but `inject` rewrites the free `fetch` of *every* file in the bundle,
//   Monaco's and CKEditor's integration included, and those must not be.
// - **`Function`**. Every vendored file is also loaded with
//   `import { syntaxCheckedFunction as Function } from "src/formula-syntax.ts"`,
//   because v1 checks a formula's syntax by constructing a function from it,
//   which the builder's CSP refuses as `eval` (`formula-syntax.ts` says why a
//   parse answers the same question, and its test holds every vendored use of
//   `Function` to that one kind).
// - **Shims** (`VENDOR_SHIMS`, 7.3): packages that would load code from another
//   origin.
// - **Aliases** (`ALIASES`): v1 packages that are vendored rather than installed.
//
// **Output**:
// - `builder.js` is an ES module, loaded by the host document with
//   `<script type="module">`.
// - Monaco is a chunk of its own, beside it, imported the first time a code
//   editor mounts. §1's "one output file" would put megabytes of editor in front
//   of every canvas that has no code field.
// - `builder.importers.json` records who reached each shim, and `builder.test.ts`
//   holds that to `VENDOR_SHIMS`.
import * as esbuild from "esbuild";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const vendor = path.join(here, "vendor", "saltcorn-builder");
const dist = path.join(here, "dist");
const builderFetch = path.join(here, "src", "builder-fetch.ts");
const formulaSyntax = path.join(here, "src", "formula-syntax.ts");

/** A bare specifier a vendored file imports → the shim it gets instead, with
 * the reason in the shim's header. */
export const VENDOR_SHIMS = {
  // Loads Monaco from `paths.vs` (a CDN, or v1's `/monaco`) with an AMD loader.
  "@monaco-editor/react": "src/shims/monaco-editor-react.tsx",
  // Loads CKEditor from cdn.ckeditor.com unless given a URL.
  "ckeditor4-react": "src/shims/ckeditor4-react.tsx",
};

/** v1 packages the builder imports that are vendored, not installed. */
const ALIASES = {
  // `src/index.ts`'s one import of the vendored builder.
  "@saltcorn/builder": path.join(vendor, "index.js"),
  // The relation finder, already vendored for the view runtime at the same
  // commit (`refresh.sh` refuses any other).
  "@saltcorn/common-code": path.join(here, "../saltcorn-ui/vendor/common-code/index.ts"),
};

export const IMPORTERS_FILE = "builder.importers.json";

const inVendor = (file) => file.startsWith(vendor + path.sep);

const builderPlugin = {
  name: "saltcorn-builder",
  setup(build) {
    build.onResolve({ filter: /.*/ }, (args) => {
      if (ALIASES[args.path]) return { path: ALIASES[args.path] };
      if (inVendor(args.importer) && VENDOR_SHIMS[args.path]) {
        return { path: path.join(here, VENDOR_SHIMS[args.path]) };
      }
      return undefined;
    });
    build.onLoad({ filter: /\.js$/ }, async (args) => {
      if (!inVendor(args.path)) return undefined;
      const source = await fs.promises.readFile(args.path, "utf8");
      return {
        // One line in front, so a stack trace is one line off, not rewritten.
        contents:
          `import { builderFetch as fetch } from ${JSON.stringify(builderFetch)};` +
          `import { syntaxCheckedFunction as Function } from ${JSON.stringify(formulaSyntax)};${source}`,
        loader: "jsx",
      };
    });
  },
};

fs.rmSync(dist, { recursive: true, force: true });

const common = {
  absWorkingDir: here,
  bundle: true,
  minify: true,
  sourcemap: "linked",
  legalComments: "linked",
  logLevel: "warning",
};

const app = await esbuild.build({
  ...common,
  entryPoints: { builder: "src/index.ts" },
  outdir: dist,
  entryNames: "[name]",
  chunkNames: "[name]-[hash]",
  format: "esm",
  splitting: true,
  platform: "browser",
  target: "es2020",
  jsx: "automatic",
  // Monaco's modules import their CSS; it is in `builder.css` instead, whole.
  loader: { ".css": "empty" },
  define: { "process.env.NODE_ENV": '"production"' },
  metafile: true,
  plugins: [builderPlugin],
});

await esbuild.build({
  ...common,
  entryPoints: {
    "editor.worker": "monaco-editor/esm/vs/editor/editor.worker.js",
    "ts.worker": "monaco-editor/esm/vs/language/typescript/ts.worker.js",
  },
  outdir: dist,
  entryNames: "[name]",
  format: "iife",
  platform: "browser",
  target: "es2020",
});

await esbuild.build({
  ...common,
  entryPoints: { builder: "src/builder.css" },
  outdir: dist,
  entryNames: "[name]",
  assetNames: "assets/[name]-[hash]",
  loader: { ".ttf": "file", ".woff": "file", ".svg": "file" },
});

fs.cpSync(path.join(here, "public", "ckeditor"), path.join(dist, "ckeditor"), { recursive: true });

// Who reached each shim, as the bundle was actually built: the evidence the
// test holds to "vendor only".
const importers = {};
for (const shim of Object.values(VENDOR_SHIMS)) {
  importers[shim] = Object.entries(app.metafile.inputs)
    .filter(([, input]) => input.imports.some((i) => i.path === shim))
    .map(([from]) => from)
    .sort();
}
fs.writeFileSync(
  path.join(dist, IMPORTERS_FILE),
  JSON.stringify(
    {
      "//": "Generated by ui/builder/build.mjs: the files that import each shim, which must all be in vendor/.",
      importers,
    },
    null,
    2,
  ) + "\n",
);

console.log(`built ${path.relative(here, path.join(dist, "builder.js"))}`);
