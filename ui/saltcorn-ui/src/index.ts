// Saltcorn UI's view runtime: the entry of `dist/view-runtime.js` (TODO §2, §5).
//
// Three things live here and nothing else:
//
// - **the registries** a `getState()` exposes — `viewtemplates`, `types`,
//   `keyFieldviews`, `fileviews` — built from v1's own base plugin the way v1's
//   `State.registerPlugin` builds them;
// - **the library**: every v1 specifier the bundle answers (TODO §5's left-hand
//   column), keyed by that specifier and shaped the way a v1 `require` shapes it,
//   so `module-host.mjs` can hand the same objects to an installed plugin;
// - **the entry points** the worker calls, which dispatch into a pattern with the
//   arguments v1's `View` passes it.
import SPECIFIERS from "./library-specifiers.json";
import * as pluginHelper from "./plugin-helper.js";

import * as markupIndex from "../vendor/saltcorn-markup/index.js";
import * as markupTags from "../vendor/saltcorn-markup/tags.js";
import * as markupLayout from "../vendor/saltcorn-markup/layout.js";
import * as markupHelpers from "../vendor/saltcorn-markup/helpers.js";
import * as markupLayoutUtils from "../vendor/saltcorn-markup/layout_utils.js";
import * as markupForm from "../vendor/saltcorn-markup/form.js";
import * as markupTable from "../vendor/saltcorn-markup/table.js";
import * as markupTabs from "../vendor/saltcorn-markup/tabs.js";
import * as markupBuilder from "../vendor/saltcorn-markup/builder.js";
import * as markupWorkflow from "../vendor/saltcorn-markup/workflow.js";
import * as markupMktag from "../vendor/saltcorn-markup/mktag.js";
import * as markupEmergencyLayout from "../vendor/saltcorn-markup/emergency_layout.js";

import * as form from "../vendor/saltcorn-data/models/form.js";
import * as fieldrepeat from "../vendor/saltcorn-data/models/fieldrepeat.js";
import * as expression from "../vendor/saltcorn-data/models/expression.js";
import * as viewableFields from "../vendor/saltcorn-data/viewable_fields.js";
import * as viewtemplatesViewableFields from "../vendor/saltcorn-data/base-plugin/viewtemplates/viewable_fields.js";
// `_`-suffixed: the unsuffixed names are the registries exported below.
import * as types_ from "../vendor/saltcorn-data/base-plugin/types.js";
import * as fieldviews_ from "../vendor/saltcorn-data/base-plugin/fieldviews.js";
import * as fileviews_ from "../vendor/saltcorn-data/base-plugin/fileviews.js";
import * as utils from "../vendor/saltcorn-data/utils.js";
import * as layoutModel from "../vendor/saltcorn-data/models/layout.js";
import { Evaluator } from "../vendor/saltcorn-data/evaluator.js";

import * as list from "../vendor/saltcorn-data/base-plugin/viewtemplates/list.js";
import * as show from "../vendor/saltcorn-data/base-plugin/viewtemplates/show.js";
import * as edit from "../vendor/saltcorn-data/base-plugin/viewtemplates/edit.js";
import * as feed from "../vendor/saltcorn-data/base-plugin/viewtemplates/feed.js";
import * as filter from "../vendor/saltcorn-data/base-plugin/viewtemplates/filter.js";
import * as listshowlist from "../vendor/saltcorn-data/base-plugin/viewtemplates/listshowlist.js";

type Obj = Record<string, any>;

// ---------------------------------------------------------------------------
// The shape of a v1 `require`
// ---------------------------------------------------------------------------

/** What `require("@saltcorn/data/…")` answers in v1: its CommonJS shim unwraps
 * the default export and hangs every other named export off it as a getter.
 * Ported from saltcorn-data's `gen-cjs-shims.cjs`. */
function dataShim(m: Obj): any {
  const e = m && "default" in m ? m.default : m;
  if (e === m) return { ...m };
  if (e && (typeof e === "object" || typeof e === "function")) {
    for (const k in m) {
      if (k === "default" || k === "__esModule" || k in e) continue;
      try {
        Object.defineProperty(e, k, { get: () => m[k], enumerable: true, configurable: true });
      } catch {
        // a frozen default keeps what it has, as in v1
      }
    }
  }
  return e;
}

/** `require` of a `@saltcorn/markup` module with a `cjs/` shim (layout, table,
 * tabs, builder, workflow, mktag): the default export, bare. */
const markupShim = (m: Obj): any => m.default;

/** `require` of a `@saltcorn/markup` module with no shim: Node's `require(esm)`,
 * which is the namespace, flagged `__esModule` because it has a default. */
function esmNamespace(m: Obj): Obj {
  const ns: Obj = { ...m };
  Object.defineProperty(ns, "__esModule", { value: true, enumerable: false });
  return ns;
}

/** plugin-helper, as a plugin sees it: v1's export names, each answered by the
 * partition — and the absent ones present as `undefined`, which is what a
 * feature-detecting plugin tests for (TODO §5). */
function pluginHelperLibrary(): Obj {
  const out: Obj = {};
  for (const name of pluginHelper.UPSTREAM_EXPORTS) {
    out[name] = name in pluginHelper.ABSENT ? undefined : (pluginHelper as Obj)[name];
  }
  return out;
}

/** The library: every v1 specifier this bundle answers, keyed as v1 names it. */
export const library: Record<string, unknown> = {
  "@saltcorn/markup": esmNamespace(markupIndex),
  "@saltcorn/markup/tags": esmNamespace(markupTags),
  "@saltcorn/markup/layout": markupShim(markupLayout),
  "@saltcorn/markup/helpers": esmNamespace(markupHelpers),
  "@saltcorn/markup/layout_utils": esmNamespace(markupLayoutUtils),
  "@saltcorn/markup/form": esmNamespace(markupForm),
  "@saltcorn/markup/table": markupShim(markupTable),
  "@saltcorn/markup/tabs": markupShim(markupTabs),
  "@saltcorn/markup/builder": markupShim(markupBuilder),
  "@saltcorn/markup/workflow": markupShim(markupWorkflow),
  "@saltcorn/markup/mktag": markupShim(markupMktag),
  "@saltcorn/markup/emergency_layout": esmNamespace(markupEmergencyLayout),
  "@saltcorn/data/models/form": dataShim(form),
  "@saltcorn/data/models/fieldrepeat": dataShim(fieldrepeat),
  "@saltcorn/data/models/expression": dataShim(expression),
  "@saltcorn/data/plugin-helper": pluginHelperLibrary(),
  "@saltcorn/data/viewable_fields": dataShim(viewableFields),
  "@saltcorn/data/base-plugin/viewtemplates/viewable_fields": dataShim(viewtemplatesViewableFields),
  "@saltcorn/data/base-plugin/types": dataShim(types_),
  "@saltcorn/data/base-plugin/fieldviews": dataShim(fieldviews_),
  "@saltcorn/data/base-plugin/fileviews": dataShim(fileviews_),
  "@saltcorn/data/base-plugin/viewtemplates/list": dataShim(list),
  "@saltcorn/data/base-plugin/viewtemplates/show": dataShim(show),
  "@saltcorn/data/base-plugin/viewtemplates/edit": dataShim(edit),
  "@saltcorn/data/base-plugin/viewtemplates/feed": dataShim(feed),
  "@saltcorn/data/base-plugin/viewtemplates/filter": dataShim(filter),
  "@saltcorn/data/base-plugin/viewtemplates/listshowlist": dataShim(listshowlist),
};

// The list in library-specifiers.json is also what vendor/refresh.sh asks v1
// about, so the two must name the same modules. A bundle where they drift fails
// on evaluation rather than answering half a library.
{
  const listed = [...SPECIFIERS].sort().join("\n");
  const built = Object.keys(library).sort().join("\n");
  if (listed !== built) {
    throw new Error(
      "the Saltcorn UI library and src/library-specifiers.json name different modules",
    );
  }
}

/** The vendored helpers the **host's** v1 models are written with (TODO Phase 4):
 * `View.run` strips empty strings as v1's does, `Page.run` walks a layout with
 * v1's own `eachView` and `traverse`, and `getState().evaluator` is v1's own
 * `Evaluator`. Not the library — no v1 specifier answers these — and not for
 * plugins: they are how `module-host.mjs` avoids writing a second copy of them. */
export const internals = {
  removeEmptyStrings: utils.removeEmptyStrings,
  removeEmptyStringsKeepNull: utils.removeEmptyStringsKeepNull,
  satisfies: utils.satisfies,
  dollarizeObject: utils.dollarizeObject,
  objectToQueryString: utils.objectToQueryString,
  getSessionId: utils.getSessionId,
  interpolate: utils.interpolate,
  eachView: layoutModel.eachView,
  eachPage: layoutModel.eachPage,
  traverse: layoutModel.traverse,
  Evaluator,
};

/** The plugin-helper partition (TODO §2), for the test that holds it. */
export const pluginHelperPartition = {
  upstream: pluginHelper.UPSTREAM_EXPORTS,
  kept: [...pluginHelper.KEPT],
  refused: Object.keys(pluginHelper.REFUSED),
  absent: Object.keys(pluginHelper.ABSENT),
};

// ---------------------------------------------------------------------------
// The registries
// ---------------------------------------------------------------------------

/** v1's six base view patterns, in v1's base-plugin order. `room` and
 * `workflow-room` are not here: they are socket views (TODO, Explicitly OUT). */
const PATTERNS: Obj[] = [list, edit, show, listshowlist, feed, filter].map((m) => m.default);

export const viewtemplates: Record<string, Obj> = Object.fromEntries(
  PATTERNS.map((vt) => [vt.name, vt]),
);

export const types: Record<string, Obj> = Object.fromEntries(
  [types_.string, types_.int, types_.bool, types_.date, types_.float, types_.color].map((t: Obj) => [
    t.name,
    t,
  ]),
);

export const fileviews: Record<string, Obj> = { ...fileviews_.default };

/** v1's `State.registerPlugin` for `fieldviews`: a `Key` fieldview is a key
 * fieldview, and any other joins its type's own `fieldviews`. */
export const keyFieldviews: Record<string, Obj> = {};
{
  const process = (name: string, fv: Obj) => {
    if (!fv || !fv.type) return;
    if (Array.isArray(fv.type)) {
      for (const t of fv.type) process(name, { ...fv, type: t });
      return;
    }
    if (fv.type === "Key") {
      keyFieldviews[name] = fv;
      return;
    }
    const type = types[fv.type];
    if (type) type.fieldviews = { ...(type.fieldviews || {}), [name]: fv };
  };
  for (const [name, fv] of Object.entries(fieldviews_)) process(name, fv as Obj);
}

// ---------------------------------------------------------------------------
// The entry points
// ---------------------------------------------------------------------------

/** A pattern by name, or an error naming it. */
export function findPattern(name: string): Obj {
  const vt = viewtemplates[name];
  if (!vt) throw new Error(`there is no view pattern named ${name}`);
  return vt;
}

/** The registry manifest — data only, so it can cross to the host at load
 * (TODO §3.3). A step's fields are not here: they need a table (TODO §6). */
export function viewPatterns(): Obj[] {
  return PATTERNS.map((vt) => ({
    name: vt.name,
    description: vt.description ?? "",
    table_required: !vt.tableless,
    view_quantity: vt.view_quantity ?? null,
    routes: Object.keys(vt.routes ?? {}),
  }));
}

/** What a view is to a pattern: v1's `View` fields the dispatch reads. */
export interface ViewRecord {
  name: string;
  viewtemplate: string;
  table_id?: number | string;
  configuration: Obj;
  [key: string]: unknown;
}

/** v1's `View.queries`: the pattern's own query object for this view. */
function queriesFor(vt: Obj, view: ViewRecord, extra: Obj): Obj {
  return vt.queries ? vt.queries({ ...view, req: extra.req, res: extra.res }) : {};
}

/** `View.run` → `pattern.run(table_id, name, configuration, state, extra, queries)`. */
export async function runView(view: ViewRecord, state: Obj, extra: Obj): Promise<unknown> {
  const vt = findPattern(view.viewtemplate);
  return vt.run(view.table_id, view.name, view.configuration, state, extra, queriesFor(vt, view, extra));
}

/** `View.runPost` → `pattern.runPost(table_id, name, configuration, state, body, extra, queries)`. */
export async function runPost(view: ViewRecord, state: Obj, body: Obj, extra: Obj): Promise<unknown> {
  const vt = findPattern(view.viewtemplate);
  if (!vt.runPost) throw new Error(`the ${vt.name} view pattern does not accept a POST (view ${view.name})`);
  return vt.runPost(view.table_id, view.name, view.configuration, state, body, extra, queriesFor(vt, view, extra));
}

/** `View.runRoute` → `pattern.routes[route](table_id, name, configuration, body, extra, queries)`. */
export async function runRoute(view: ViewRecord, route: string, body: Obj, extra: Obj): Promise<unknown> {
  const vt = findPattern(view.viewtemplate);
  const handler = vt.routes?.[route];
  if (!handler) throw new Error(`the ${vt.name} view pattern has no route ${route} (view ${view.name})`);
  return handler(view.table_id, view.name, view.configuration, body, extra, queriesFor(vt, view, extra));
}

/** One step of a pattern's `configuration_workflow`, over the context gathered
 * so far — a call per step, because a step's form does not exist without its
 * context (TODO §6). */
export async function configStep(pattern: string, step: number, context: Obj, req: Obj): Promise<Obj> {
  const vt = findPattern(pattern);
  if (!vt.configuration_workflow) throw new Error(`the ${vt.name} view pattern has no configuration`);
  const workflow = vt.configuration_workflow(req);
  const steps: Obj[] = workflow.steps ?? [];
  const current = steps[step];
  if (!current) throw new Error(`the ${vt.name} view pattern has no configuration step ${step}`);
  return {
    name: current.name,
    count: steps.length,
    builder: !!current.builder,
    form: current.form ? await current.form(context) : null,
  };
}
