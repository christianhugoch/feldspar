// The **module host**: the JavaScript half of the Deno worker a Saltcorn module
// runs on.
//
// Written into the modules root from the server binary at every worker start
// (the binary is the authority; a stale copy from an older version would be a
// bug nobody would look for), and evaluated as that worker's main module with
// the modules root as its directory, so `require` resolves out of the modules
// root's own `node_modules`.
//
// ## The entry point
//
// There is no protocol. The Rust side calls `globalThis.__scModuleHost(id,
// request)` — one V8 function call, with the request as a real object — and the
// answer comes back through two functions the host installed on the global
// before this script was evaluated:
//
//   __scDone(id, jsonText)             a call answered
//   __scFail(id, message, stack)       a call threw
//
// The `id` is the host's, not this script's: many calls are in flight at once
// and a slow module's action does not hold anybody else's, which is what the id
// was always for. What crosses back is `JSON.stringify`'s text rather than the
// value itself, because a module's result is a module's own object — a function
// property, a stream, a cycle — and `JSON.stringify` is the rule v1 itself
// applies to one. A value it will not encode is a **failure naming why**, never
// a mangled result.
//
// (Until the "Modules in-process" milestone's phase 2 this was newline-JSON over
// a pipe to a `node` child process. Nothing above the transport changed: the
// stubs, `Workflow`, `Form`, `interpolate` and the manifest are the same text
// phase 0 proved portable.)
//
// ## Logging
//
// `console.log` and its five siblings go to the server's own log through
// `__scLog(level, module, message)`, tagged with the module that was running —
// tracked through an `AsyncLocalStorage`, so a line written from a callback the
// module registered during a call is still that module's line. Nothing is
// written to this process's stdout by this script, and nothing needs to be: the
// server's log is one place with one format, and a module's `console.log` is
// part of it rather than beside it.
//
// ## The `@saltcorn` stubs
//
// A v1 plugin's first lines are `require("@saltcorn/data/models/table")` and
// friends. Those packages are v1's server — the thing being replaced — and are
// not installed, so `Module._load` is patched to answer every `@saltcorn/*`
// specifier from the table below.
//
// Three tiers. `Workflow`, `Form` and `interpolate` are **real**, because the
// first two are what a `configuration_workflow` is written in and the third is
// called on every `proxmox_snapshot` run. Everything else is a stub whose
// properties are reachable and whose **calls throw**, naming the API. A silent
// no-op was the alternative and is refused on the same grounds the rest of this
// system refuses silent failures: a `Table.findOne` that returns `undefined`
// does not fail, it computes the wrong answer, inside somebody's trigger.

import { createRequire } from "node:module";
import Module from "node:module";
import * as path from "node:path";
import { AsyncLocalStorage } from "node:async_hooks";
import { format } from "node:util";

const require = createRequire(import.meta.url);

// ---------------------------------------------------------------------------
// What the host installed before this script ran
// ---------------------------------------------------------------------------

/** A call answered: `(id, jsonText)`. */
const done = globalThis.__scDone;
/** A call threw: `(id, message, stack)`. */
const fail = globalThis.__scFail;
/** One log line: `(level, moduleName | null, message)`. */
const log = globalThis.__scLog;

// ---------------------------------------------------------------------------
// Which module is speaking
// ---------------------------------------------------------------------------

/** The module whose call is running, for the log tag.
 *
 * An `AsyncLocalStorage` rather than a variable, because the interesting lines
 * are not the ones written on the way in: `@saltcorn/mqtt` logs from a `connect`
 * callback it registered while it was being loaded, long after `load` answered,
 * and a plain variable would have moved on by then. The store is captured when
 * the callback's async resource is created, so that line still says which module
 * wrote it. */
const running = new AsyncLocalStorage();

/** The module's own logging, in the server's log rather than beside it.
 *
 * `format` is node's own, so `console.log("%s rows", n)` and an object argument
 * both read the way their author expected. */
const speak = (level) =>
  (...args) => {
    try {
      log(level, running.getStore() ?? null, format(...args));
    } catch (_) {
      // A module that logs an object whose inspection throws must not have that
      // become the failure of whatever it was doing.
    }
  };

console.log = speak("info");
console.info = speak("info");
console.debug = speak("verbose");
console.trace = speak("verbose");
console.warn = speak("warning");
console.error = speak("error");

// ---------------------------------------------------------------------------
// The `@saltcorn` API: real, stubbed, and named
// ---------------------------------------------------------------------------

/** The message a stubbed API answers a *call* with. */
const notAvailable = (what) =>
  `the Saltcorn v1 API ${what} is not available to modules in this version of ` +
  `Saltcorn. This module needs an API that has not been implemented yet; the ` +
  `actions that do not use it still work.`;

/** Properties that must answer `undefined` rather than a stub.
 *
 * `then` is the dangerous one: a stub that answers a callable `then` turns
 * `await stub` into a call, and therefore into a throw from a line that never
 * meant to use the API. The rest are the runtime's own probes — `util.inspect`,
 * JSON serialisation, iteration — which must not report a stub as a function. */
const passThrough = new Set(["then", "catch", "finally", "toJSON", "inspect", "nodeType"]);

/** A stub reachable by property and fatal on call, naming the path that was
 * used. Reached lazily so `a.b.c()` names `a.b.c`, not `a`. */
function namedStub(pathName) {
  const target = function () {};
  return new Proxy(target, {
    get(_t, prop) {
      if (typeof prop === "symbol") return undefined;
      if (passThrough.has(prop)) return undefined;
      if (prop === "name") return pathName;
      return namedStub(`${pathName}.${prop}`);
    },
    apply() {
      throw new Error(notAvailable(pathName));
    },
    construct() {
      throw new Error(notAvailable(`new ${pathName}`));
    },
  });
}

/** v1's `Form`: the fields are the whole of what this host reads back. */
class Form {
  constructor(opts = {}) {
    Object.assign(this, opts);
    this.fields = opts.fields || [];
  }
}

/** v1's `Workflow`: a list of steps, each with a `form`. */
class Workflow {
  constructor(opts = {}) {
    Object.assign(this, opts);
    this.steps = opts.steps || [];
  }
}

/** v1's `interpolate(template, row, user)`: `{{ expression }}` substitution with
 * the row's columns and `user` in scope.
 *
 * Real rather than stubbed because `proxmox_snapshot` names every snapshot with
 * one, and a snapshot called `{{ name }}-{{ id }}` literally is not a snapshot.
 * The expression is JavaScript, as it is in v1. */
function interpolate(template, row = {}, user = undefined) {
  if (typeof template !== "string") return template;
  return template.replace(/\{\{([^}]*)\}\}/g, (_all, expr) => {
    const source = String(expr).trim();
    if (source === "") return "";
    const names = Object.keys(row || {}).filter((k) => /^[A-Za-z_$][\w$]*$/.test(k));
    // eslint-disable-next-line no-new-func
    const f = new Function(...names, "user", `return (${source});`);
    const value = f(...names.map((n) => row[n]), user);
    return value === null || value === undefined ? "" : String(value);
  });
}

/** The `@saltcorn/*` specifiers this host answers itself, and with what. */
function saltcornModule(specifier) {
  switch (specifier) {
    case "@saltcorn/data/models/form":
    case "@saltcorn/data/models/form.js":
      return Form;
    case "@saltcorn/data/models/workflow":
    case "@saltcorn/data/models/workflow.js":
      return Workflow;
    case "@saltcorn/data/utils":
    case "@saltcorn/data/utils.js":
      return { ...namedNamespace(specifier), interpolate };
    default:
      return namedNamespace(specifier);
  }
}

/** A stub *namespace*: a plain object whose every property is a named stub, so
 * `const { getState } = require("@saltcorn/data/db/state")` destructures
 * without complaint and `getState()` throws naming `getState`. */
function namedNamespace(specifier) {
  const short = specifier.replace(/^@saltcorn\//, "").replace(/\.js$/, "");
  return new Proxy(
    {},
    {
      get(_t, prop) {
        if (typeof prop === "symbol") return undefined;
        if (passThrough.has(prop)) return undefined;
        if (prop === "__esModule") return false;
        if (prop === "default") return namedStub(`${short}.default`);
        return namedStub(`${short}.${prop}`);
      },
      // A `require(…)` used as a constructor or a function — v1's models are
      // classes, and `new Table(...)` has to say the same thing.
      has() {
        return true;
      },
    },
  );
}

const originalLoad = Module._load;
Module._load = function (request, parent, isMain) {
  if (request === "@saltcorn" || request.startsWith("@saltcorn/")) {
    return saltcornModule(request);
  }
  return originalLoad.apply(this, arguments);
};

// ---------------------------------------------------------------------------
// Loading a module
// ---------------------------------------------------------------------------

/** The loaded modules, by package name: what `run` dispatches through. */
const loaded = new Map();

/** In-flight (and settled) loads, by package name.
 *
 * A `run` waits on its module's load before dispatching. Requests are handled
 * concurrently — that is the point of the id in the protocol — so without this
 * a `run` written straight after a `load` (which is exactly what the server
 * does when it restarts this process and replays its module set) could reach
 * `loaded` before the load had put anything in it. */
const loading = new Map();

/** The plugin keys this version reads. Everything else is counted and reported
 * (the Modules tab says "also supplies: 1 table provider"), never loaded, so an
 * admin knows what they are not getting. */
const supportedKeys = new Set([
  "actions",
  "configuration_workflow",
  "functions",
  "table_providers",
]);

/** Keys that are metadata rather than an entity type — including `onLoad`,
 * which is not an entity but a hook, and is called by `loadModule`. */
const metadataKeys = new Set([
  "sc_plugin_api_version",
  "plugin_name",
  "dependencies",
  "ready_for_mobile",
  "onLoad",
]);

/** How much of an exported entity there is, for the census. */
function entityCount(value) {
  if (Array.isArray(value)) return value.length;
  if (value && typeof value === "object") return Object.keys(value).length;
  return null;
}

/** Drop a package and everything under its directory from require's cache, so a
 * reload of a symlinked local checkout picks up the edits. */
function purgeCache(dir) {
  const prefix = path.resolve(dir);
  for (const key of Object.keys(require.cache)) {
    if (path.resolve(key).startsWith(prefix)) delete require.cache[key];
  }
}

/** v1's `configFields`: an array, or a function of a context, possibly async. */
async function evalConfigFields(fields, context) {
  const value = typeof fields === "function" ? await fields(context) : fields;
  return Array.isArray(value) ? value : [];
}

/** The fields of a `configuration_workflow`'s steps, concatenated.
 *
 * v1 configures a plugin with a wizard and v2 has no wizard vocabulary, so the
 * forms are flattened into one settings form. A step whose form cannot be built
 * without context is skipped rather than fatal: the module still loads, and its
 * actions still run.
 *
 * `subject` names whose workflow it is ("its", "the table provider \"RSS
 * feed\"") so an issue reads as a sentence: a module's own settings and each of
 * its table providers' settings go through this same function. */
async function workflowFields(makeWorkflow, subject) {
  if (typeof makeWorkflow !== "function") return { fields: [], issues: [] };
  const issues = [];
  let workflow;
  try {
    workflow = await makeWorkflow({});
  } catch (e) {
    return { fields: [], issues: [`${subject} configuration form could not be built: ${e.message}`] };
  }
  const fields = [];
  for (const step of (workflow && workflow.steps) || []) {
    try {
      const form = typeof step.form === "function" ? await step.form({}) : step.form;
      for (const field of (form && form.fields) || []) fields.push(field);
    } catch (e) {
      issues.push(
        `${subject} configuration step "${step.name || "?"}" could not be built: ${e.message}`,
      );
    }
  }
  return { fields, issues };
}

/** v1's `table_providers`: a virtual table whose rows the module supplies.
 *
 * ```js
 * table_providers: {
 *   "RSS feed": {
 *     configuration_workflow,                      // this provider's settings
 *     fields: [{ name: "title", type: "String" }], // or a function of the config
 *     get_table: (cfg) => ({ getRows: async (where, opts) => [...] }),
 *   },
 * }
 * ```
 *
 * Two shapes, both v1's: a plain object, and a function of the module's own
 * configuration — which is v1's `withCfg`, the rule that every facility key of a
 * plugin *with* a `configuration_workflow` is called with that configuration.
 * `actions` and `functions` are read the same way, so this is not new
 * vocabulary.
 *
 * A provider with no `get_table` is reported and skipped: it is the one method
 * that produces rows, and a table that cannot produce rows is not a table.
 */
async function evalTableProviders(plugin, configuration) {
  const exported = plugin.table_providers;
  let raw = {};
  const issues = [];
  if (typeof exported === "function") {
    try {
      raw = (await exported(configuration || {})) || {};
    } catch (e) {
      issues.push(`its table providers could not be built: ${e.message}`);
      raw = {};
    }
  } else if (exported && typeof exported === "object") {
    raw = exported;
  }

  const providers = [];
  const set = {};
  for (const [providerName, value] of Object.entries(raw)) {
    const impl = value || {};
    if (typeof impl.get_table !== "function") {
      issues.push(
        `the table provider "${providerName}" has no get_table function, so no table can be ` +
          `served by it`,
      );
      continue;
    }
    const { fields, issues: workflowIssues } = await workflowFields(
      impl.configuration_workflow,
      `the table provider "${providerName}"'s`,
    );
    issues.push(...workflowIssues);
    set[providerName] = impl;
    providers.push({ name: providerName, config_fields: fields });
  }
  return { providers, set, issues };
}

/** The fields one provider presents for one configuration.
 *
 * v1 writes `fields` two ways — an array, and a function of the configuration
 * (possibly async), which is how `@saltcorn/postgres-tables` reports the columns
 * an admin picked in its second workflow step. Both are read. */
async function providerFields({ module: name, provider: providerName, configuration }) {
  const impl = requireProvider(name, providerName);
  const declared =
    typeof impl.fields === "function" ? await impl.fields(configuration || {}) : impl.fields;
  return Array.isArray(declared) ? declared : [];
}

/** One provider's rows, for one v1 `where`/`options` pair.
 *
 * `get_table(cfg, table)` is called per request rather than once, which is v1's
 * own arrangement (`Table.to_provided_table` does the same): a provider that
 * wants to cache caches in its own module scope, as `@saltcorn/rss` does, and
 * one that holds a connection pool holds it there too. Caching the returned
 * object here would instead pin whatever it closed over to a configuration that
 * may since have been edited.
 *
 * The second argument is v1's table row. What a v1 provider reads off it is its
 * name, so its name is what it gets — this host has no `Table` to hand over, and
 * a stub would throw on the first property. */
async function providerRows({ module: name, provider: providerName, configuration, table, where, options }) {
  const impl = requireProvider(name, providerName);
  const provided = await impl.get_table(configuration || {}, { name: table || "" });
  if (!provided || typeof provided.getRows !== "function")
    throw new Error(
      `the table provider ${providerName} of module ${name} supplies no getRows, so it cannot ` +
        `be read`,
    );
  const rows = await provided.getRows(where || {}, options || {});
  return Array.isArray(rows) ? rows : [];
}

/** The loaded provider, or a sentence naming what is missing. */
function requireProvider(name, providerName) {
  const entry = loaded.get(name);
  if (!entry) throw new Error(`the module ${name} is not loaded in this host`);
  const impl = entry.providers && entry.providers[providerName];
  if (!impl) throw new Error(`the module ${name} has no table provider ${providerName}`);
  return impl;
}

/** v1's `functions`: what a plugin supplies to formulas and code bodies.
 *
 * Three shapes exist in real plugins and all three are v1's, so all three are
 * read here rather than one being declared canonical:
 *
 * ```js
 * functions: { geocode_lat: { run: async (q) => …, isAsync: true,
 *                             arguments: [{ name: "query", type: "Object" }] } }
 * functions: { md_to_html: (m) => md.render(m || "") }      // bare, synchronous
 * functions: (config) => ({ llm_generate: { run: async (p) => …, isAsync: true } })
 * ```
 *
 * The third is why the module's configuration is passed: the function closes
 * over it, exactly as `actions(cfg)` does, and it is *the module's* one
 * configuration because a module is loaded once.
 *
 * A function that will not declare itself is reported and skipped rather than
 * fatal, on `configFields`' grounds: a module with one odd function is a module
 * with one odd function.
 */
async function evalFunctions(plugin, configuration) {
  const exported = plugin.functions;
  let raw = {};
  const issues = [];
  if (typeof exported === "function") {
    // A *function of the configuration*, not a bare function: the top-level key
    // is the module's own, and v1 reads it exactly as it reads `actions`.
    try {
      raw = (await exported(configuration || {})) || {};
    } catch (e) {
      issues.push(`its functions could not be built: ${e.message}`);
      raw = {};
    }
  } else if (exported && typeof exported === "object") {
    raw = exported;
  }

  const functions = [];
  const set = {};
  for (const [fnName, value] of Object.entries(raw)) {
    const impl = typeof value === "function" ? { run: value } : value || {};
    if (typeof impl.run !== "function") {
      issues.push(
        `the function "${fnName}" has no run function, so it is not available to ` +
          `formulas or code bodies`,
      );
      continue;
    }
    set[fnName] = impl;
    functions.push({
      name: fnName,
      description: impl.description || "",
      // v1's own word for "this one is awaitable". A bare function is judged by
      // what it is: an `async function` is one whether or not anybody said so.
      isAsync: impl.isAsync === undefined
        ? impl.run.constructor && impl.run.constructor.name === "AsyncFunction"
        : !!impl.isAsync,
      // `arguments: [{ name, type }]` is v1's own field vocabulary. Normalised
      // to exactly that pair on the way out: what reads it is a code editor's
      // signature line, and an argument with no name is nothing it can show.
      arguments: (Array.isArray(impl.arguments) ? impl.arguments : [])
        .filter((a) => a && typeof a.name === "string" && a.name !== "")
        .map((a) => ({ name: a.name, type: typeof a.type === "string" ? a.type : null })),
    });
  }
  return { functions, set, issues };
}

/** Load (or reload) one module, and report what it supplies. */
async function loadModule({ module: name, dir, configuration }) {
  purgeCache(dir);
  let plugin;
  try {
    plugin = require(dir);
  } catch (e) {
    // A module whose *own* dependency is missing is the common failure for a
    // package installed from a checkout, and node's message names the package
    // but not what to do about it. Say both: the admin can install it, and
    // nobody else can.
    if (e && e.code === "MODULE_NOT_FOUND") {
      const missing = /Cannot find module '([^']+)'/.exec(e.message);
      throw new Error(
        `${name} needs the package ${missing ? missing[1] : "(unknown)"}, which is not ` +
          `installed. Run \`npm install ${missing ? missing[1] : "<package>"}\` in the ` +
          `modules directory, or add it to the module's own dependencies.`,
      );
    }
    throw e;
  }
  const issues = [];

  // v1's `onLoad(configuration)`, which is where a plugin builds the state its
  // actions close over. `@saltcorn/mqtt` is the whole argument for calling it:
  // its `mqtt_publish` publishes through a module-level `client` that **only**
  // `onLoad` ever assigns, so a host that skips this has a module whose one
  // action always throws. Awaited, so a module that connects at load has
  // started connecting before the first action runs.
  //
  // A failure here is an **issue**, not a refusal: the rest of the module is
  // already readable, and the Modules tab saying "its onLoad failed, and here
  // is what it said" is more use to an admin than a module that will not
  // install. This is also the one place a module's own network is reached
  // without a call behind it, so a permission denial is what an admin most
  // often sees here — and it arrives with the module's name on it either way.
  if (typeof plugin.onLoad === "function") {
    try {
      await plugin.onLoad(configuration || {});
    } catch (e) {
      issues.push(`the module's onLoad() failed: ${e.message}`);
    }
  }

  const actionsExport = plugin.actions;
  let actionSet = {};
  if (typeof actionsExport === "function") {
    actionSet = (await actionsExport(configuration || {})) || {};
  } else if (actionsExport && typeof actionsExport === "object") {
    actionSet = actionsExport;
  }

  const actions = [];
  for (const [actionName, action] of Object.entries(actionSet)) {
    const impl = typeof action === "function" ? { run: action } : action || {};
    let configFields = [];
    try {
      configFields = await evalConfigFields(impl.configFields, { mode: "trigger" });
    } catch (e) {
      issues.push(`the action "${actionName}" could not declare its settings: ${e.message}`);
    }
    actions.push({
      name: actionName,
      description: impl.description || "",
      requireRow: !!impl.requireRow,
      configFields,
    });
  }

  const { functions, set: functionSet, issues: functionIssues } = await evalFunctions(
    plugin,
    configuration,
  );
  issues.push(...functionIssues);

  const { fields: configFields, issues: configIssues } = await workflowFields(
    plugin.configuration_workflow,
    "its",
  );
  issues.push(...configIssues);

  const {
    providers,
    set: providerSet,
    issues: providerIssues,
  } = await evalTableProviders(plugin, configuration);
  issues.push(...providerIssues);

  const unsupported = [];
  for (const [key, value] of Object.entries(plugin)) {
    if (supportedKeys.has(key) || metadataKeys.has(key)) continue;
    unsupported.push({ key, count: entityCount(value) });
  }

  loaded.set(name, {
    plugin,
    actions: actionSet,
    functions: functionSet,
    providers: providerSet,
    configuration: configuration || {},
  });

  return {
    name,
    api_version: plugin.sc_plugin_api_version ?? null,
    plugin_name: plugin.plugin_name || null,
    actions,
    functions,
    table_providers: providers,
    config_fields: configFields,
    unsupported,
    issues,
  };
}

/** Run one action of one module, with v1's argument object. */
async function runAction({ module: name, action: actionName, args }) {
  const entry = loaded.get(name);
  if (!entry) throw new Error(`the module ${name} is not loaded in this host`);
  const action = entry.actions[actionName];
  if (!action) throw new Error(`the module ${name} has no action ${actionName}`);
  const impl = typeof action === "function" ? { run: action } : action;
  if (typeof impl.run !== "function")
    throw new Error(`the action ${actionName} of module ${name} has no run function`);
  const result = await impl.run(args || {});
  return result === undefined ? null : result;
}

/** Call one function of one module, with v1's positional arguments.
 *
 * The arguments are **positional** because v1's functions are: `geocode_lat(q)`
 * is called with what the formula or the body passed, in order. They arrived as
 * JSON, which is the whole of what crosses this seam — a callback or a stream
 * is not an argument a module function can be given from here, and the caller
 * is told so before the call rather than being handed a mangled value.
 */
async function callFunction({ module: name, function: fnName, args }) {
  const entry = loaded.get(name);
  if (!entry) throw new Error(`the module ${name} is not loaded in this host`);
  const impl = entry.functions && entry.functions[fnName];
  if (!impl) throw new Error(`the module ${name} has no function ${fnName}`);
  // `await` regardless of `isAsync`: a synchronous function's value is its own
  // value, and awaiting one costs a microtask. What `isAsync` decides is what
  // the *manifest* says, which is what a code body's author reads.
  const result = await impl.run(...(Array.isArray(args) ? args : []));
  return result === undefined ? null : result;
}

// ---------------------------------------------------------------------------
// The entry point
// ---------------------------------------------------------------------------

async function handle(request) {
  switch (request.op) {
    case "ping":
      return { pong: true, node: process.version };
    case "load": {
      const pending = loadModule(request);
      // Recorded before it is awaited, and with its rejection absorbed: this
      // copy exists to be waited *on*, and the caller of the load is the one
      // who is told it failed.
      loading.set(request.module, pending.catch(() => {}));
      return await pending;
    }
    case "unload":
      loaded.delete(request.module);
      loading.delete(request.module);
      return { unloaded: true };
    case "run": {
      const pending = loading.get(request.module);
      if (pending) await pending;
      return await runAction(request);
    }
    case "call": {
      const pending = loading.get(request.module);
      if (pending) await pending;
      return await callFunction(request);
    }
    case "provider_fields": {
      const pending = loading.get(request.module);
      if (pending) await pending;
      return await providerFields(request);
    }
    case "provider_rows": {
      const pending = loading.get(request.module);
      if (pending) await pending;
      return await providerRows(request);
    }
    default:
      throw new Error(`unknown module-host operation ${request.op}`);
  }
}

/** Answer one call, with the module's result encoded as v1 would encode it.
 *
 * `JSON.stringify` answers `undefined` for a function, a symbol, or nothing at
 * all, and **throws** on a cycle or on a `toJSON` that does. The first is the
 * `null` an action that returns nothing has always answered; the second is a
 * failure that names the value rather than a reply nobody can read. */
function answer(id, value) {
  let text;
  try {
    text = JSON.stringify(value === undefined ? null : value);
  } catch (e) {
    fail(id, `the module answered with a value that is not JSON: ${(e && e.message) || e}`, null);
    return;
  }
  done(id, text === undefined ? "null" : text);
}

/** What the Rust side calls. One request in, one answer out through `done` or
 * `fail`, and never a throw: a synchronous failure here would unwind into V8
 * from a host call that has no way to report it, so everything is settled
 * through the two functions instead.
 *
 * Deliberately not awaited by the caller — each request is its own task, which
 * is what puts many calls in flight at once. */
globalThis.__scModuleHost = (id, request) => {
  running.run(request && request.module ? request.module : null, () => {
    let pending;
    try {
      pending = handle(request);
    } catch (e) {
      fail(id, (e && e.message) || String(e), (e && e.stack) || null);
      return;
    }
    pending.then(
      (value) => answer(id, value),
      (e) => fail(id, (e && e.message) || String(e), (e && e.stack) || null),
    );
  });
};

// A module's own unhandled rejection must not take the worker down with it: the
// call it belongs to has already been answered (or is about to time out), and
// every other module on this worker is innocent.
process.on("unhandledRejection", (e) => {
  console.error(`unhandled rejection from a module: ${(e && e.stack) || e}`);
});
