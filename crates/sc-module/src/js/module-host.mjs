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
// ## Asking the server something
//
// Everything above answers; this is the other direction. A module's action can
// now *ask* — one more native function on the global, and one this file
// installs back:
//
//   __scAsk(callId, askId, requestJson)   a host call, from inside a call
//   __scAnswer(askId, ok, text)           its answer, later
//
// The `callId` is the one the request arrived with, because that is what says
// **whose** authority the ask runs under: the caller of that call is where the
// host is borrowed, and the worker routes the ask to it. An ask is answered on
// the server's own task, so a module parked on a query is not holding this
// worker's JavaScript slice — awaiting a promise yields, which is the whole of
// why the bounds are unchanged.
//
// ## The `@saltcorn` API
//
// A v1 plugin's first lines are `require("@saltcorn/data/models/table")` and
// friends. Those packages are v1's server — the thing being replaced — and are
// not installed, so `Module._load` is patched to answer every `@saltcorn/*`
// specifier from the table below.
//
// Three tiers. `Table` and `Field` are the **real** v1 classes — the shared
// `v1_api.js` this file is concatenated after, over the ask channel above and
// the schema snapshot the call carried. `Workflow`, `Form` and `interpolate`
// are real too, because the first two are what a `configuration_workflow` is
// written in and the third is called on every `proxmox_snapshot` run.
// Everything else is a stub whose properties are reachable and whose **calls
// throw**, naming the API. A silent no-op was the alternative and is refused on
// the same grounds the rest of this system refuses silent failures: a
// `Table.findOne` that returns `undefined` does not fail, it computes the wrong
// answer, inside somebody's trigger.

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
/** One host call, out: `(callId, askId, requestJson)`. */
const askHost = globalThis.__scAsk;

// ---------------------------------------------------------------------------
// Which module is speaking
// ---------------------------------------------------------------------------

/** The **call** that is running: which module it is of, its id, whether it has
 * a caller to ask, the schema snapshot it carried, and the v1 API built over
 * those — built once, lazily, per call.
 *
 * An `AsyncLocalStorage` rather than a variable, because the interesting lines
 * are not the ones written on the way in: `@saltcorn/mqtt` logs from a `connect`
 * callback it registered while it was being loaded, long after `load` answered,
 * and a plain variable would have moved on by then. The store is captured when
 * the callback's async resource is created, so that line still says which module
 * wrote it — and, since this milestone, so a `Table.findOne` inside an action
 * still knows which call's authority it is reading under. */
const running = new AsyncLocalStorage();

/** The module's own logging, in the server's log rather than beside it.
 *
 * `format` is node's own, so `console.log("%s rows", n)` and an object argument
 * both read the way their author expected. */
const speak = (level) =>
  (...args) => {
    try {
      const store = running.getStore();
      log(level, (store && store.module) || null, format(...args));
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
// Asking this server something
// ---------------------------------------------------------------------------

/** The asks this worker is waiting on, by ask id, and the counter that names
 * them. Per **worker** rather than per call, because the ids are what
 * `__scAnswer` routes on and the two sides have to agree on one space. */
const asks = new Map();
let nextAsk = 1;

/** One ask, answered. `ok` says which of the two the third argument is: the
 * answer's JSON text, or the message it failed with.
 *
 * Installed from here rather than by the Rust side, because what it settles is
 * a promise this file made — and an answer for an ask nobody is waiting on is
 * dropped rather than reported: the call it belonged to was given up on, and
 * its caller has already been told why. */
globalThis.__scAnswer = (askId, ok, text) => {
  const pending = asks.get(askId);
  if (!pending) return;
  asks.delete(askId);
  if (!ok) {
    pending.reject(new Error(text || "the server refused without saying why"));
    return;
  }
  let value = null;
  try {
    value = text === undefined || text === null || text === "" ? null : JSON.parse(text);
  } catch (e) {
    pending.reject(
      new Error(`the server answered with something this host cannot read: ${(e && e.message) || e}`),
    );
    return;
  }
  pending.resolve(value);
};

/** What a module is told when it reaches for the database from somewhere that
 * has no caller to borrow one from (§3).
 *
 * A load, a module function and a table provider are all called with nobody's
 * authority: `onLoad` runs while the module is being installed, a function is
 * hoisted into a formula, and a provider is called from inside a query. None of
 * them has a `CodeHosts` borrowed on a caller's stack, so none of them can ask.
 * Said by name at the property, rather than answered with nothing. */
const noCaller = (what) =>
  `\`${what}\` is not available here: the Saltcorn v1 Table and Field read and ` +
  `write under the authority of the call they are used in, and this call has ` +
  `none to lend — a module load (onLoad, a configuration workflow), a module ` +
  `function and a table provider are each called with nobody's. A module reads ` +
  `and writes rows from an action.`;

/** One host call, from inside the call it belongs to.
 *
 * `surface` is which of this server's seams is meant — `db` for a plan, and
 * `trigger` for a run of another trigger — and the pair crosses as one JSON
 * request, because there is one native function rather than one per seam. */
function ask(surface, plan) {
  return new Promise((resolve, reject) => {
    const store = running.getStore();
    if (!store || !store.asks) {
      reject(new Error(noCaller(surface === "trigger" ? "run_trigger" : "this database call")));
      return;
    }
    let text;
    try {
      text = JSON.stringify({ surface, plan });
    } catch (e) {
      reject(new Error(`this request is not JSON: ${(e && e.message) || e}`));
      return;
    }
    const id = nextAsk;
    nextAsk += 1;
    asks.set(id, { resolve, reject });
    try {
      askHost(store.call, id, text);
    } catch (e) {
      asks.delete(id);
      reject(e);
    }
  });
}

// ---------------------------------------------------------------------------
// The schema snapshot
// ---------------------------------------------------------------------------

/** The snapshot this worker holds, by the catalog generation it was built at.
 *
 * One entry, exactly as the code isolates keep it: a generation is bumped by a
 * catalog reload, so the previous one is of no use to any call that has not
 * already started — and a call that *has* resolved its snapshot holds the
 * object itself, so clearing the map never pulls a schema out from under a
 * module's action. A call carries the generation; it carries the JSON only when
 * this worker does not have that generation yet. */
const schemas = new Map();

/** The snapshot for one generation — what this call's `Table` is built over.
 *
 * A generation this worker does not hold is a **named failure** and never an
 * empty schema, for `v1_api.js`'s own reason: a `Table.findOne` answering
 * undefined for every table would compute the wrong answer inside somebody's
 * action rather than fail. */
function schemaFor(generation) {
  if (generation === null || generation === undefined) return null;
  const held = schemas.get(generation);
  if (held === undefined) {
    throw new Error(
      `the schema snapshot for catalog generation ${generation} is not on this module worker`,
    );
  }
  return held;
}

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

/** v1's `Table` and `Field`, as the one object a plugin captures.
 *
 * The awkward part of the port, and it is v1's own doing: a plugin's **first
 * line** is `const Table = require("@saltcorn/data/models/table")`, evaluated
 * once at load time, and every action it ever runs uses that one binding. So
 * the object has to be stable for the module's life while what it answers has
 * to be the *running call's* — a different schema after a catalog reload, and a
 * different caller's authority on every firing.
 *
 * Hence a façade: a stable proxy whose every property is read off the v1 API of
 * the call in flight, built once per call and lazily, so a call that never
 * names a table never builds one. Outside a call there is nothing to read it
 * from, and the property says so ([`noCaller`]) rather than answering nothing.
 *
 * `default` and the class's own name answer the façade itself, because a plugin
 * transpiled from ESM writes `require("…/table").default` and a careful one
 * writes `.Table`; both mean this. */
function v1Facade(which) {
  const target = function () {};
  return new Proxy(target, {
    get(_t, prop) {
      if (typeof prop === "symbol") return undefined;
      if (passThrough.has(prop)) return undefined;
      if (prop === "name") return which;
      if (prop === "__esModule") return false;
      if (prop === "default" || prop === which) return v1Classes[which];
      // The two v1 statics with no authority in them: `Field.labelToName` and
      // `Field.nameToLabel` are string functions, and a plugin building form
      // labels in its `configuration_workflow` calls them where there is no call
      // in flight. Answered from an api over no snapshot and no sender, which is
      // all they need.
      if (which === "Field" && (prop === "labelToName" || prop === "nameToLabel")) {
        return pureApi().Field[prop];
      }
      return callApi(`${which}.${String(prop)}`)[which][prop];
    },
    // v1's models are classes, so `Table(…)` is a mistake and `new Table(…)` is
    // schema editing — which `v1_api.js` refuses by name from its own list. The
    // constructor is refused here because there is no instance to refuse from.
    apply() {
      throw new Error(notAvailable(which));
    },
    construct() {
      throw new Error(notAvailable(`new ${which}`));
    },
  });
}

/** The two façades, built once. */
const v1Classes = {};
v1Classes.Table = v1Facade("Table");
v1Classes.Field = v1Facade("Field");

/** The v1 API over nothing at all: no sender, no snapshot.
 *
 * What it is for is the two pure `Field` statics above. Everything else on it
 * refuses by name (`v1_api.js` builds it that way deliberately), which is why it
 * is not the answer to a `Table.findOne` outside a call — that one has a sharper
 * thing to say. */
let pure = null;
function pureApi() {
  if (!pure) pure = globalThis.__scMakeV1Api(null, null, null);
  return pure;
}

/** This call's v1 API — `{ Table, Field }` from the shared `v1_api.js` — built
 * over this call's own ask channel and the snapshot it carried.
 *
 * Built on the store rather than in a module-level variable because calls are
 * concurrent: two actions of two modules are in flight at once, and each has
 * its own caller, its own authority and its own budget. */
function callApi(what) {
  const store = running.getStore();
  if (!store || !store.asks) throw new Error(noCaller(what));
  if (!store.api) {
    store.api = globalThis.__scMakeV1Api(
      (plan) => ask("db", plan),
      store.schema,
      (request) => ask("trigger", request),
    );
  }
  return store.api;
}

/** The `@saltcorn/*` specifiers this host answers itself, and with what. */
function saltcornModule(specifier) {
  switch (specifier) {
    case "@saltcorn/data/models/table":
    case "@saltcorn/data/models/table.js":
      return v1Classes.Table;
    case "@saltcorn/data/models/field":
    case "@saltcorn/data/models/field.js":
      return v1Classes.Field;
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
  "modelproviders",
  "frameworks",
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

/** The outcome declarations a model provider may make, and what each needs.
 *
 * `sc_model::OutcomeSpec`'s JSON, checked here rather than on the Rust side so
 * that a module with one mis-declared provider is reported on its own card and
 * still supplies everything else it has. */
const OUTCOME_KINDS = {
  supervised: "label",
  regression: "label",
  classification: "label",
  cluster: null,
  embedding: "components",
  test: null,
};

/** One provider's `outcome`, or a sentence saying what is wrong with it. */
function readOutcome(declared) {
  if (!declared || typeof declared !== "object")
    return { error: `its outcome must be an object such as { kind: "regression", label: "label" }` };
  const kind = declared.kind;
  if (!Object.prototype.hasOwnProperty.call(OUTCOME_KINDS, kind))
    return {
      error: `its outcome kind ${JSON.stringify(kind)} is not one of ${Object.keys(
        OUTCOME_KINDS,
      ).join(", ")}`,
    };
  const key = OUTCOME_KINDS[kind];
  if (key && typeof declared[key] !== "string")
    return { error: `its outcome is "${kind}", which needs a string "${key}" naming the configuration key that holds it` };
  return { outcome: key ? { kind, [key]: declared[key] } : { kind } };
}

/** v1 has no `modelproviders`; this is **this** system's key (TODO §14).
 *
 * ```js
 * modelproviders: {
 *   ridge: {
 *     description: "Linear regression with an L2 penalty",
 *     config_fields: [{ name: "label", type: "String", required: true }],
 *     hyperparameters: [{ name: "alpha", type: "Float", default: 1 }],
 *     outcome: { kind: "regression", label: "label" },
 *     standardise: true,
 *     fit: async ({ frame, configuration, hyperparameters }) => ({ state, parameters }),
 *     predict: async ({ state, frame }) => [1.2, 3.4],
 *   },
 * }
 * ```
 *
 * Read the two ways every other facility key is read — a plain object, and a
 * function of the module's own configuration — because that is v1's `withCfg`
 * rule and a plugin author should not have to learn a third.
 *
 * A provider missing `fit`, missing `predict`, or declaring an outcome nothing
 * can read is **reported and skipped**: a provider that cannot be fitted is not
 * one to put on the model form, and an admin who can see why can fix it.
 *
 * Settings may be declared either as a v1 `configuration_workflow` (which is
 * what a plugin that already has one will reach for) or as a plain
 * `config_fields` array, which is what a provider whose settings are one label
 * picker actually wants to write.
 */
async function evalModelProviders(plugin, configuration) {
  const exported = plugin.modelproviders;
  let raw = {};
  const issues = [];
  if (typeof exported === "function") {
    try {
      raw = (await exported(configuration || {})) || {};
    } catch (e) {
      issues.push(`its model providers could not be built: ${e.message}`);
      raw = {};
    }
  } else if (exported && typeof exported === "object") {
    raw = exported;
  }

  const providers = [];
  const set = {};
  for (const [providerName, value] of Object.entries(raw)) {
    const impl = value || {};
    let broken = null;
    if (typeof impl.fit !== "function") broken = "it has no fit function";
    else if (typeof impl.predict !== "function") broken = "it has no predict function";
    const read = broken ? { error: broken } : readOutcome(impl.outcome);
    if (read.error) {
      issues.push(`the model provider "${providerName}" is not available: ${read.error}`);
      continue;
    }
    const { fields, issues: workflowIssues } = await workflowFields(
      impl.configuration_workflow,
      `the model provider "${providerName}"'s`,
    );
    issues.push(...workflowIssues);
    set[providerName] = impl;
    providers.push({
      name: providerName,
      description: impl.description || "",
      config_fields: [...fields, ...(Array.isArray(impl.config_fields) ? impl.config_fields : [])],
      hyperparameters: Array.isArray(impl.hyperparameters) ? impl.hyperparameters : [],
      outcome: read.outcome,
      standardise: !!impl.standardise,
    });
  }
  return { providers, set, issues };
}

/** The names this version will not let a module claim for a framework.
 *
 * The built-ins. Framework names share one namespace — an application stores
 * `vue`, not `@feldspar/vue:vue` — and `react` is the path an admin should be
 * offered, so a module must not be able to take that sentence over. The refusal
 * is per framework: the module still loads, its actions still run, and its card
 * says which name was refused and why. */
const reservedFrameworks = new Set(["react", "code"]);

/** v1 has no `frameworks`; this is **this** system's key (§13.3, §15.1).
 *
 * ```js
 * frameworks: {
 *   vue: {
 *     label: "Vue",
 *     description: "A Vue 3 + Vite project, scaffolded and built for you.",
 *     config_fields: [{ name: "store", type: "String", required: true }],
 *     build: { store: "{{ store }}", source: "{{ project }}",
 *              output: "{{ project }}/dist", command: "npm run build",
 *              install: { command: "npm install", marker: "node_modules" },
 *              runtime: "{{ project }}/src/feldspar", client: "client.ts" },
 *     csp: { "img-src": ["'self'", "data:"] },
 *     builder_prompt: "You maintain {{ app }} …",
 *     scaffold: async (ctx) => [{ path: "package.json", contents: "…" }],
 *     runtime: async (ctx) => [{ path: `${ctx.runtime}/composables.ts`, contents: "…" }],
 *   },
 * }
 * ```
 *
 * Read the two ways every other facility key is read — a plain object, and a
 * function of the module's own configuration — because that is v1's `withCfg`
 * rule and a plugin author should not have to learn a third.
 *
 * What crosses is the **declaration**, evaluated once: the settings, the path
 * templates, the CSP, the prompt. `scaffold` and `runtime` stay here and are
 * called again through the `framework_files` op — they are the only part that
 * depends on the application, which is a thing the plugin author did not know.
 *
 * A framework with no `build` is **reported and skipped**: this version serves a
 * framework's built bundle and nothing else, so one that cannot say how to build
 * one could never serve anything, and an admin who can see why can ask its author
 * for a version that does.
 */
async function evalFrameworks(plugin, configuration) {
  const exported = plugin.frameworks;
  let raw = {};
  const issues = [];
  if (typeof exported === "function") {
    try {
      raw = (await exported(configuration || {})) || {};
    } catch (e) {
      issues.push(`its frameworks could not be built: ${e.message}`);
      raw = {};
    }
  } else if (exported && typeof exported === "object") {
    raw = exported;
  }

  const frameworks = [];
  const set = {};
  for (const [frameworkName, value] of Object.entries(raw)) {
    const impl = value || {};
    if (reservedFrameworks.has(frameworkName)) {
      issues.push(
        `the framework "${frameworkName}" is not available: that name belongs to one of ` +
          `this server's own frameworks, and an application stores a framework by name`,
      );
      continue;
    }
    if (!impl.build || typeof impl.build !== "object") {
      issues.push(
        `the framework "${frameworkName}" is not available: it declares no build, and this ` +
          `version serves a framework's built bundle`,
      );
      continue;
    }
    const { fields, issues: workflowIssues } = await workflowFields(
      impl.configuration_workflow,
      `the framework "${frameworkName}"'s`,
    );
    issues.push(...workflowIssues);
    set[frameworkName] = impl;
    frameworks.push({
      name: frameworkName,
      label: impl.label || "",
      description: impl.description || "",
      config_fields: [...fields, ...(Array.isArray(impl.config_fields) ? impl.config_fields : [])],
      build: impl.build,
      csp: impl.csp && typeof impl.csp === "object" ? impl.csp : {},
      builder_prompt: typeof impl.builder_prompt === "string" ? impl.builder_prompt : "",
      scaffolds: typeof impl.scaffold === "function",
    });
  }
  return { frameworks, set, issues };
}

/** The files one framework generates for one application.
 *
 * `phase` is `scaffold` (the whole project, written once) or `runtime` (the
 * framework's own generated code, rewritten on every build). A phase the
 * framework does not implement answers **no files**, which is a legitimate
 * declaration rather than a failure: a framework that brings its own project
 * exports no `scaffold`, and one whose runtime is entirely Saltcorn's exports no
 * `runtime`.
 *
 * Every answer is checked here rather than trusted: a generator that returns
 * something other than a list of `{ path, contents }` has made a mistake whose
 * consequence would otherwise be a project directory full of `undefined`. */
async function frameworkFiles({ module: name, framework: frameworkName, phase, context }) {
  const entry = loaded.get(name);
  if (!entry) throw new Error(`the module ${name} is not loaded in this host`);
  const impl = entry.frameworks && entry.frameworks[frameworkName];
  if (!impl) throw new Error(`the module ${name} has no framework ${frameworkName}`);
  const generate = impl[phase];
  if (typeof generate !== "function") return [];
  const answer = await generate(context || {});
  if (!Array.isArray(answer))
    throw new Error(
      `the ${phase} of framework ${frameworkName} of module ${name} answered ` +
        `${JSON.stringify(answer)}, which is not a list of files`,
    );
  return answer.map((file, index) => {
    const path = file && typeof file.path === "string" ? file.path.trim() : "";
    if (!path)
      throw new Error(
        `the ${phase} of framework ${frameworkName} of module ${name} answered a file at ` +
          `position ${index} with no path`,
      );
    if (typeof file.contents !== "string")
      throw new Error(
        `the ${phase} of framework ${frameworkName} of module ${name} answered "${path}" ` +
          `with contents that are not text`,
      );
    return { path, contents: file.contents };
  });
}

/** The loaded model provider, or a sentence naming what is missing. */
function requireModelProvider(name, providerName) {
  const entry = loaded.get(name);
  if (!entry) throw new Error(`the module ${name} is not loaded in this host`);
  const impl = entry.modelProviders && entry.modelProviders[providerName];
  if (!impl) throw new Error(`the module ${name} has no model provider ${providerName}`);
  return impl;
}

/** Fit one model provider over a columnar frame.
 *
 * What comes back is `sc_model::FitResult` — `{ state, parameters }` — and a
 * provider that answers only its state is read as having no parameters rather
 * than as having failed: `state` is the half without which nothing can predict,
 * and `parameters` is the half a screen shows. */
async function modelFit({ module: name, provider: providerName, frame, configuration, hyperparameters }) {
  const impl = requireModelProvider(name, providerName);
  const result = await impl.fit({
    frame: frame || { rows: 0, columns: [] },
    configuration: configuration || {},
    hyperparameters: hyperparameters || {},
  });
  if (result === undefined || result === null)
    throw new Error(
      `the model provider ${providerName} of module ${name} fitted nothing: it must answer ` +
        `{ state, parameters }`,
    );
  // A provider that answers a bare state rather than the pair — which is what
  // one whose fit *is* its state will write — is read as meaning it.
  const pair =
    typeof result === "object" && !Array.isArray(result) && "state" in result
      ? result
      : { state: result };
  return { state: pair.state === undefined ? null : pair.state, parameters: pair.parameters || [] };
}

/** Predict with one, over a frame of any height. Always a list, one per row. */
async function modelPredict({ module: name, provider: providerName, state, frame }) {
  const impl = requireModelProvider(name, providerName);
  const answer = await impl.predict({
    state: state === undefined ? null : state,
    frame: frame || { rows: 0, columns: [] },
  });
  if (!Array.isArray(answer))
    throw new Error(
      `the model provider ${providerName} of module ${name} answered its predictions with ` +
        `${JSON.stringify(answer)}, which is not a list`,
    );
  return answer;
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

/** One provider's table object, built afresh for this request.
 *
 * `get_table(cfg, table)` per request rather than once, which is v1's own
 * arrangement (`Table.to_provided_table` does the same) and what
 * [`providerRows`] does: a provider that wants to cache caches in its own module
 * scope, and one that holds a connection pool holds it there too. */
async function providedTable({ module: name, provider: providerName, configuration, table }) {
  const impl = requireProvider(name, providerName);
  const provided = await impl.get_table(configuration || {}, { name: table || "" });
  if (!provided || typeof provided !== "object")
    throw new Error(
      `the table provider ${providerName} of module ${name} supplied no table for these settings`,
    );
  return provided;
}

/** Which of v1's three write methods this configuration answers.
 *
 * v1 has no declaration of writability: `get_table(cfg)` either puts the methods
 * on the object it returns or it does not, which is how
 * `@saltcorn/postgres-tables`'s `read_only` flag works. So the answer is read
 * off the object, and the object is built with the configuration in hand. */
async function providerWrites(request) {
  const provided = await providedTable(request);
  return {
    insert: typeof provided.insertRow === "function",
    update: typeof provided.updateRow === "function",
    delete: typeof provided.deleteRows === "function",
  };
}

/** v1's `insertRow(record, user)`: the new row's primary key, or `null`.
 *
 * v1 lets a provider answer nothing — a key generated remotely may not come back
 * — so nothing is `null` here rather than an error, and the caller reads the row
 * back through what it wrote instead. */
async function providerInsert(request) {
  const provided = await providedTable(request);
  requireMethod(provided, "insertRow", request);
  const key = await provided.insertRow(request.record || {}, request.user);
  return { key: key === undefined ? null : key };
}

/** v1's `updateRow(record, id, user)`, which answers nothing. */
async function providerUpdate(request) {
  const provided = await providedTable(request);
  requireMethod(provided, "updateRow", request);
  await provided.updateRow(request.record || {}, request.id, request.user);
  return { updated: true };
}

/** v1's `deleteRows(where, user)`.
 *
 * The `where` is not optional the way `getRows`' is: a provider handed `{}` here
 * deletes the table, so an absent one is refused rather than defaulted. The
 * caller (`sc_catalog::provider`) always sends `{ pk: { in: [...] } }`. */
async function providerDelete(request) {
  const provided = await providedTable(request);
  requireMethod(provided, "deleteRows", request);
  const where = request.where;
  if (!where || typeof where !== "object" || Array.isArray(where) || !Object.keys(where).length)
    throw new Error(
      `a delete on the table provider ${request.provider} of module ${request.module} arrived ` +
        `with no rows named, and a provider handed an empty where deletes everything`,
    );
  await provided.deleteRows(where, request.user);
  return { deleted: true };
}

/** Refuse a write this configuration does not answer, naming the method a
 * module author would have to add. */
function requireMethod(provided, method, { module: name, provider: providerName }) {
  if (typeof provided[method] !== "function")
    throw new Error(
      `the table provider ${providerName} of module ${name} supplies no ${method} for these ` +
        `settings, so it cannot be written`,
    );
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

  const {
    providers: modelProviders,
    set: modelProviderSet,
    issues: modelProviderIssues,
  } = await evalModelProviders(plugin, configuration);
  issues.push(...modelProviderIssues);

  const {
    frameworks,
    set: frameworkSet,
    issues: frameworkIssues,
  } = await evalFrameworks(plugin, configuration);
  issues.push(...frameworkIssues);

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
    modelProviders: modelProviderSet,
    frameworks: frameworkSet,
    configuration: configuration || {},
  });

  return {
    name,
    api_version: plugin.sc_plugin_api_version ?? null,
    plugin_name: plugin.plugin_name || null,
    actions,
    functions,
    table_providers: providers,
    model_providers: modelProviders,
    frameworks,
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
    case "provider_writes": {
      const pending = loading.get(request.module);
      if (pending) await pending;
      return await providerWrites(request);
    }
    case "provider_insert": {
      const pending = loading.get(request.module);
      if (pending) await pending;
      return await providerInsert(request);
    }
    case "provider_update": {
      const pending = loading.get(request.module);
      if (pending) await pending;
      return await providerUpdate(request);
    }
    case "provider_delete": {
      const pending = loading.get(request.module);
      if (pending) await pending;
      return await providerDelete(request);
    }
    case "model_fit": {
      const pending = loading.get(request.module);
      if (pending) await pending;
      return await modelFit(request);
    }
    case "model_predict": {
      const pending = loading.get(request.module);
      if (pending) await pending;
      return await modelPredict(request);
    }
    case "framework_files": {
      const pending = loading.get(request.module);
      if (pending) await pending;
      return await frameworkFiles(request);
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
  // The call's own context: which module it is of (the log tag), its id (what
  // an ask is routed by), whether it has a caller to ask at all, and the schema
  // its `Table` answers from. Everything a v1 `Table` needs reaches it through
  // this and nothing else, because a module holds one `Table` for its whole
  // life and only the store knows which call is using it.
  const context = {
    module: request && request.module ? request.module : null,
    call: id,
    asks: !!(request && request.asks),
    schema: null,
    api: null,
  };
  running.run(context, () => {
    let pending;
    try {
      // The snapshot the call carried, when this worker did not already have
      // that generation. Recorded before anything is dispatched, and
      // synchronously — the entry point runs to its first `await` before the
      // Rust side hands over the next call, so two calls at one generation
      // cannot race here.
      if (request && typeof request.schema === "string") {
        schemas.clear();
        schemas.set(request.schemaGeneration, JSON.parse(request.schema));
      }
      context.schema = schemaFor(request && request.schemaGeneration);
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
