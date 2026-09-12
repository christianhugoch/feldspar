// Evaluate Saltcorn UI's built view runtime and hold it to its shape (TODO 2.7).
//
//   node bundle_shape.mjs <dist/view-runtime.js> <vendor/v1-exports.json>
//
// Run by `bundle_shape.rs`. The bundle's one import is `node:module`, for its
// `require`; this script answers it the way the module worker will — Node's
// built-ins are real, and every `@saltcorn/*` host module is a stub that is
// readable and **fatal on call**, naming what was called. So the bundle
// evaluating at all asserts that loading it calls nothing on the host, which is
// what lets the worker load it before any view is asked for.
//
// Prints one line per failed check and exits 1, or prints `ok <n> checks`.
import { register } from "node:module";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [bundlePath, exportsPath] = process.argv.slice(2).map((p) => resolve(p));

register(
  "data:text/javascript," +
    encodeURIComponent(`
      export async function resolve(specifier, context, next) {
        if (specifier === "node:module" && context.parentURL?.endsWith("view-runtime.js")) {
          return { url: "sc-host:module", shortCircuit: true };
        }
        return next(specifier, context);
      }
      export async function load(url, context, next) {
        if (url !== "sc-host:module") return next(url, context);
        return { format: "module", shortCircuit: true, source: ${JSON.stringify(`
          import { createRequire as realCreateRequire } from "node:module";
          const stub = (path) => new Proxy(function () {}, {
            get(_t, p) {
              if (typeof p === "symbol" || p === "then" || p === "__esModule") return undefined;
              return stub(path + "." + String(p));
            },
            apply() { throw new Error("the bundle called host " + path + " while loading"); },
            construct() { throw new Error("the bundle constructed host " + path + " while loading"); },
          });
          export const createRequire = (from) => {
            const real = realCreateRequire(from);
            return (specifier) =>
              specifier.startsWith("@saltcorn/") ? stub(specifier) : real(specifier);
          };
        `)} };
      }
    `),
);

const failures = [];
let checks = 0;
const check = (ok, message) => {
  checks++;
  if (!ok) failures.push(message);
};

const { readFileSync } = await import("node:fs");
const v1 = JSON.parse(readFileSync(exportsPath, "utf8")).modules;
const rt = await import(pathToFileURL(bundlePath).href);

// --- the six patterns, under the names v1 gives them ----------------------
const patterns = Object.keys(rt.viewtemplates).sort();
check(
  JSON.stringify(patterns) === JSON.stringify(["Edit", "Feed", "Filter", "List", "ListShowList", "Show"]),
  `the patterns are ${patterns.join(", ")}`,
);
for (const name of patterns) {
  const vt = rt.viewtemplates[name];
  check(typeof vt.run === "function", `pattern ${name} has no run`);
  check(
    rt.library[`@saltcorn/data/base-plugin/viewtemplates/${name.toLowerCase()}`]?.name === name,
    `pattern ${name} is not the library's viewtemplates/${name.toLowerCase()}`,
  );
}
check(
  rt.viewPatterns().map((p) => p.name).sort().join() === patterns.join(),
  "viewPatterns() does not describe the registry",
);

// --- the registries --------------------------------------------------------
for (const t of ["String", "Integer", "Bool", "Date", "Float", "Color"]) {
  check(rt.types[t]?.name === t, `type ${t} is not registered`);
}
check(typeof rt.keyFieldviews.select?.run === "function", "the select key fieldview is not registered");
check(Object.keys(rt.fileviews).length > 0, "no fileviews are registered");

// --- the library answers what a v1 require answers -------------------------
check(
  Object.keys(rt.library).sort().join() === Object.keys(v1).sort().join(),
  "the library and v1-exports.json name different specifiers",
);
for (const [specifier, want] of Object.entries(v1)) {
  const got = rt.library[specifier];
  check(typeof got === want.type, `${specifier} is a ${typeof got}; under v1 it is a ${want.type}`);
  if (got == null) continue;
  const missing = want.exports.filter((name) => !(name in got));
  check(missing.length === 0, `${specifier} lacks v1's exports ${missing.join(", ")}`);
}

// --- the plugin-helper partition (TODO 2.2) ---------------------------------
const { upstream, kept, refused, absent } = rt.pluginHelperPartition;
const sides = [...kept, ...refused, ...absent];
for (const name of upstream) {
  const on = sides.filter((s) => s === name).length;
  check(on === 1, `plugin-helper's ${name} is on ${on} sides of the partition; it must be on exactly one`);
}
for (const name of sides) {
  check(upstream.includes(name), `the partition names ${name}, which plugin-helper does not export`);
}
const helper = rt.library["@saltcorn/data/plugin-helper"];
for (const name of refused) {
  let message = "";
  try {
    helper[name]();
  } catch (e) {
    message = e.message;
  }
  check(message.includes(name), `calling refused ${name} did not refuse by name (${message || "no error"})`);
}
for (const name of absent) {
  check(name in helper && helper[name] === undefined, `absent ${name} is not present-and-undefined`);
}
for (const name of kept) {
  check(typeof helper[name] === "function", `kept ${name} is not v1's function`);
}

// --- and the library is the real thing, not a stub --------------------------
const { div } = rt.library["@saltcorn/markup/tags"];
check(div({ class: "x" }, "hi") === '<div class="x">hi</div>', "@saltcorn/markup/tags' div does not render");

for (const f of failures) console.log(f);
if (failures.length) process.exit(1);
console.log(`ok ${checks} checks`);
