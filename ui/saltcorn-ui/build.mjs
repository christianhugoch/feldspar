// Build Saltcorn UI's view runtime: `dist/view-runtime.js`, one ESM file, plus
// the browser assets staged into `dist/public/` (TODO Phase 2).
//
// The bundle is **a library, not a private bundle** (TODO §5). Everything in
// `vendor/` that renders is bundled; everything on the other side of the line —
// v1's models, `getState()`, `db`, and Node's built-ins — is **host-supplied**
// and reached through one `require`, the one `module-host.mjs` patches:
//
//   import { createRequire } from "node:module";
//   const require = createRequire(import.meta.url);
//
// is the bundle's only import. Every host specifier becomes a lazy
// `require("<specifier>")` inside the bundle, so the worker's `Module._load`
// answers the built-in patterns exactly as it answers an installed plugin.
import * as esbuild from "esbuild";
import { builtinModules } from "node:module";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const vendor = path.join(here, "vendor");
const dataDir = path.join(vendor, "saltcorn-data");
const dist = path.join(here, "dist");

/** `@saltcorn/data` modules the host answers (TODO §5's right-hand column),
 * by their path inside the package. A vendored file's relative import of one
 * of these becomes `require("@saltcorn/data/<path>")`. */
const HOST_DATA_MODULES = new Set([
  "db/index",
  "db/state",
  "models/config",
  "models/crash",
  "models/discovery",
  "models/field",
  "models/file",
  "models/library",
  "models/multi_node_mutex",
  "models/page",
  "models/page_group",
  "models/table",
  "models/tag",
  "models/trigger",
  "models/user",
  "models/view",
  "models/workflow",
]);

/** The other v1 packages the rendering half imports, vendored whole or in part. */
const VENDORED_PACKAGES = {
  "@saltcorn/markup": "saltcorn-markup",
  "@saltcorn/common-code": "common-code",
  "@saltcorn/db-common": "db-common",
  "@saltcorn/types": "saltcorn-types",
  "@saltcorn/plain-date": "plain-date",
};

/** Imports that are neither rendering nor host: server-only packages `utils.ts`
 * and `expression.ts` import at the top and call on paths a view never takes.
 * v1's mobile build aliases the same names to the same mocks. */
const MOCKS = {
  dockerode: path.join(dataDir, "mobile-mocks/npm/dockerode.ts"),
  xml2js: path.join(dataDir, "mobile-mocks/npm/xml2js.ts"),
  "fs-extra": path.join(dataDir, "mobile-mocks/node/fs-extra.ts"),
  "https-proxy-agent": path.join(here, "src/shims/https-proxy-agent.ts"),
  vm2: path.join(here, "src/shims/vm2.ts"),
};

const builtins = new Set(builtinModules);
const isBuiltin = (spec) => builtins.has(spec.replace(/^node:/, ""));
const stripExt = (p) => p.replace(/\.(js|ts|cjs|mjs)$/, "");

/** Resolve a path inside a vendored package, `.js` → `.ts` as tsc does. */
function vendoredFile(dir, sub) {
  const base = path.join(vendor, dir, stripExt(sub || "index"));
  for (const candidate of [`${base}.ts`, path.join(base, "index.ts")]) {
    if (fs.existsSync(candidate)) return candidate;
  }
  return null;
}

const hostPlugin = {
  name: "saltcorn-host",
  setup(build) {
    build.onResolve({ filter: /.*/ }, (args) => {
      // The inner `require` of a host stub is the real, runtime one.
      if (args.namespace === "sc-host") return { path: args.path, external: true };

      const spec = args.path;
      const inData = args.importer.startsWith(dataDir + path.sep);

      if (MOCKS[spec]) return { path: MOCKS[spec] };

      // `utils.ts` builds its own `require` to reach db/index and db/state
      // lazily; that `require` has to be the host's.
      if (spec === "module" && args.importer === path.join(dataDir, "utils.ts")) {
        return { path: path.join(here, "src/shims/module.ts") };
      }
      // …and it asks for the home directory at load, which the worker's empty
      // permission set would refuse.
      if (spec === "os" && args.importer === path.join(dataDir, "utils.ts")) {
        return { path: path.join(here, "src/shims/os.ts") };
      }
      if (isBuiltin(spec)) return { path: spec, namespace: "sc-host" };

      // A path inside @saltcorn/data: relative from a vendored file, or the one
      // baseUrl-style `models/field.js` in base-plugin/types.ts.
      let dataPath = null;
      if (inData && spec.startsWith(".")) {
        dataPath = stripExt(path.relative(dataDir, path.resolve(args.resolveDir, spec)));
      } else if (inData && /^(models|db)\//.test(spec)) {
        dataPath = stripExt(spec);
      } else if (spec.startsWith("@saltcorn/data/")) {
        dataPath = stripExt(spec.slice("@saltcorn/data/".length));
      }
      if (dataPath !== null) {
        if (HOST_DATA_MODULES.has(dataPath)) {
          return { path: `@saltcorn/data/${dataPath}`, namespace: "sc-host" };
        }
        if (dataPath === "plugin-testing") {
          return { path: path.join(dataDir, "mobile-mocks/saltcorn/plugin-testing.ts") };
        }
        // Every import of plugin-helper reaches the partition (TODO §2), so a
        // refused export is refused inside the bundle as well as outside it.
        const partition = path.join(here, "src/plugin-helper.ts");
        if (dataPath === "plugin-helper" && args.importer !== partition) {
          return { path: partition };
        }
        if (spec.startsWith(".")) return undefined; // an ordinary vendored file
        const file = vendoredFile("saltcorn-data", dataPath);
        if (file) return { path: file };
        return {
          errors: [{ text: `@saltcorn/data/${dataPath} is neither vendored nor host-supplied` }],
        };
      }

      for (const [pkg, dir] of Object.entries(VENDORED_PACKAGES)) {
        if (spec === pkg || spec.startsWith(pkg + "/")) {
          const file = vendoredFile(dir, spec.slice(pkg.length + 1));
          if (file) return { path: file };
          return {
            errors: [{ text: `${spec} is not vendored; add it to vendor/refresh.sh` }],
          };
        }
      }
      return undefined;
    });

    build.onLoad({ filter: /.*/, namespace: "sc-host" }, (args) => ({
      contents: `module.exports = require(${JSON.stringify(args.path)});`,
      loader: "js",
    }));
  },
};

fs.rmSync(dist, { recursive: true, force: true });

await esbuild.build({
  entryPoints: [path.join(here, "src/index.ts")],
  outfile: path.join(dist, "view-runtime.js"),
  bundle: true,
  format: "esm",
  platform: "node",
  target: "es2022",
  // Unminified, like v1's own webpack build of @saltcorn/data: a stack trace from
  // a view pattern should name v1's function.
  minify: false,
  legalComments: "none",
  banner: {
    js: [
      "// Saltcorn UI view runtime. Generated by ui/saltcorn-ui/build.mjs; do not edit.",
      'import { createRequire as __scCreateRequire } from "node:module";',
      "const require = __scCreateRequire(import.meta.url);",
    ].join("\n"),
  },
  plugins: [hostPlugin],
  logLevel: "warning",
});

fs.cpSync(path.join(here, "public"), path.join(dist, "public"), { recursive: true });
console.log(`built ${path.relative(here, path.join(dist, "view-runtime.js"))}`);
