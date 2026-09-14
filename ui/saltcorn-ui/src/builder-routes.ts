// Ported from Saltcorn 1: v1's server routes the builder is fed by, at
// @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2).
//
// These live in `packages/server/routes/`, not in `@saltcorn/data`, so there is
// nothing to vendor: each function is a port, headed with the route it ports,
// reading the application through the same host-supplied models a vendored
// pattern does (TODO "The builder" §5, §10). Where a line differs from v1 it
// says why beside it.
import View from "@saltcorn/data/models/view";
import Page from "@saltcorn/data/models/page";
import PageGroup from "@saltcorn/data/models/page_group";
import File from "@saltcorn/data/models/file";
import User from "@saltcorn/data/models/user";
import Trigger from "@saltcorn/data/models/trigger";
import Table from "@saltcorn/data/models/table";
import Field from "@saltcorn/data/models/field";
import { getState } from "@saltcorn/data/db/state";
import Library from "../vendor/saltcorn-data/models/library.js";
import { calcfldViewConfig, getActionConfigFields } from "./plugin-helper.js";
import * as markupLayout from "../vendor/saltcorn-markup/layout.js";

/** `@saltcorn/markup/layout`'s default export, as the bundle's library answers
 * it. */
const renderLayout: (opts: Obj) => string = (markupLayout as Obj).default;

type Obj = Record<string, any>;

/** `pageBuilderData(req, context)`, from packages/server/routes/pageedit.ts:
 * the options a page's builder is opened with. `page_name` names the page; the
 * page itself is v1's `context`. */
export async function pageBuilderOptions(page_name: string, req: Obj): Promise<Obj> {
  const context: Obj | undefined = Page.findOne({ name: page_name });
  if (!context) throw new Error(`there is no page named ${page_name}`);
  // v1's page editor opens an HTML-file page in a file editor instead (the
  // builder, Explicitly OUT), so there is no layout to build.
  if (context.layout && context.layout.html_file) {
    throw new Error(
      `the page ${page_name} is an HTML file (${context.layout.html_file}), which has no layout to build`,
    );
  }
  const state: Obj = getState();
  const views: Obj[] = await View.find();
  const pages: Obj[] = await Page.find();
  const page_groups = (await PageGroup.find()).map((g: Obj) => ({ name: g.name }));
  const images = await File.findImagesForBuilder();
  const roles = await User.get_roles();
  const stateActions: Obj = state.actions;
  const actions = [
    "GoBack",
    ...Object.entries(stateActions)
      .filter(([_k, v]: [string, Obj]) => !v.requireRow && !v.disableInBuilder && !v.disableIf?.())
      .map(([k]) => k),
  ];
  // v1 finds the triggers whose event is "API call" or "Never". An
  // application's triggers are the ones its views and pages run by name
  // (§12.2), the snapshot carries no event for them, and
  // `trigger_actions({ apiNeverTriggers })` below already answers every one.
  const triggers: Obj[] = await Trigger.find({});
  triggers.forEach((tr) => {
    actions.push(tr.name);
  });
  const triggerActions = Trigger.trigger_actions({
    apiNeverTriggers: true,
  });
  const actionConfigForms: Obj = {};
  const actionDescriptions: Obj = {};
  for (const name of actions) {
    const action = stateActions[name];
    if (action && action.configFields) {
      actionConfigForms[name] = await getActionConfigFields(action, null as any, {
        mode: "page",
        req,
      });
    }
    if (action && action.description) actionDescriptions[name] = action.description;
  }
  const workflowActions = Trigger.trigger_actions({
    apiNeverTriggers: true,
    onlyWorkflows: true,
  });
  for (const name of workflowActions) {
    actionConfigForms[name] = [
      {
        name: "initial_context",
        label: "Initial context",
        type: "String",
        class: "validate-expression",
      },
    ];
  }
  const actionsNotRequiringRow = Trigger.action_options({
    notRequireRow: true,
    apiNeverTriggers: true,
    forBuilder: true,
    builtInLabel: "Page Actions",
    builtIns: ["GoBack"],
  });
  const library = (await Library.find({})).filter((l: Obj) => l.suitableFor("page"));
  const fixed_state_fields: Obj = {};
  for (const view of views) {
    fixed_state_fields[view.name] = [];
    const table: Obj | undefined = Table.findOne(view.table_id || view.exttable_name);
    if (table) view.table_name = table.name;
    const fs = await view.get_state_fields();
    const added_fields = new Set();
    for (const frec of fs) {
      const f: Obj = new (Field as any)(frec);
      if (f.input_type === "hidden") continue;
      if (f.name === "_fts") continue;

      f.required = false;
      if (f.type && f.type_name === "Bool") f.fieldview = "tristate";

      if (added_fields.has(f.name)) continue;
      added_fields.add(f.name);
      fixed_state_fields[view.name].push(f.toBuilder);
      // v1 reads `table.name` unguarded; a tableless view has no state fields
      // in v1's patterns, but one from a plugin might.
      if (table && table.name === "users" && f.primary_key)
        fixed_state_fields[view.name].push(
          new (Field as any)({
            name: "preset_" + f.name,
            label: req.__("Preset %s", f.label),
            type: "String",
            attributes: { options: ["LoggedIn"] },
          }).toBuilder,
        );
      if (f.presets) {
        fixed_state_fields[view.name].push(
          new (Field as any)({
            name: "preset_" + f.name,
            label: req.__("Preset %s", f.label),
            type: "String",
            attributes: { options: Object.keys(f.presets) },
          }).toBuilder,
        );
      }
    }
  }
  const { on_done_redirect: _on_done_redirect, ...current_filter_state } = req.query || {};
  const icons = state.icons;
  return {
    isRTL: !!req.isRTL,
    // i18n is the identity here (Explicitly OUT): v1's answer for "en".
    translations: {},
    views: views.map((v) => v.select_option),
    images,
    pages,
    page_groups,
    current_filter_state,
    actions: actionsNotRequiringRow,
    has_copilot_generate: !!state.functions.copilot_generate_layout,
    builtInActions: ["GoBack"],
    triggerActions,
    library,
    min_role: context.min_role,
    actionConfigForms,
    actionDescriptions,
    allowMultiStepAction: true,
    page_name: context.name,
    page_id: context.id,
    mode: "page",
    roles,
    icons,
    fixed_state_fields,
    next_button_label: "Done",
    fonts: state.fonts,
    tables: [],
    keyframes: state.keyframes,
  };
}

// ---------------------------------------------------------------------------
// The canvas's calls (TODO "The builder" §10): previews and lookups the
// builder makes while a layout is edited, each v1's route with what it
// `res.send`s or `res.json`s as the return value. They run as the admin; the
// host has checked that the table, view or page asked for is the
// application's before the call.
// ---------------------------------------------------------------------------

/** A table of the application, by name. `Table.find` answers inside the
 * application's table subset and `Table.findOne` by name does not (The
 * builder, 5.2), so a table these routes reach through a key is found here,
 * where v1 uses `Table.findOne`. */
async function applicationTable(name: string | undefined): Promise<Obj | undefined> {
  if (!name) return undefined;
  const tables: Obj[] = await Table.find({});
  return tables.find((t) => t.name === name);
}

/** The table the builder named, or the sentence saying the application has no
 * such table. v1 reads `Table.findOne(...)!` and fails on the null. */
async function requireApplicationTable(name: string): Promise<Obj> {
  const table = await applicationTable(name);
  if (!table) throw new Error(`the table ${name} is not one of the application's tables`);
  return table;
}

/** A table field as the host's `Field`: the snapshot's fields are read-only,
 * v1's routes write to the field they found, and the host's class has v1's
 * `fill_fkey_options` and `distinct_values(req, where)`. */
const hostField = (field: Obj): Obj => new (Field as any)(field);

/** v1's `applyAsync` (`@saltcorn/data/utils`): a value, or what a function
 * answers for the arguments. */
const applyAsync = async (f: any, ...args: any[]) => (typeof f === "function" ? await f(...args) : f);

/** `POST /field/preview/:tableName/:fieldName/:fieldview`, from
 * packages/server/routes/fields.ts: a fieldview over the first row the admin
 * can read (or `body.row_id`'s, from a Show), for the builder's canvas.
 * `fieldName` may be `key.field`, previewing the referenced row's field. */
export async function builderFieldPreview(
  tableName: string,
  fieldName: string,
  fieldview: string,
  body: Obj,
  req: Obj,
): Promise<string> {
  const table = await requireApplicationTable(tableName);
  const fields: Obj[] = table.getFields();
  const state: Obj = getState();

  let found: Obj | undefined, row: Obj | null, value: unknown;
  const row_id = (body || {}).row_id;
  const whereClause = row_id ? { id: row_id } : {};
  if (fieldName.includes(".")) {
    const [refNm, targetNm] = fieldName.split(".");
    const ref = fields.find((f: Obj) => f.name === refNm);
    if (!ref) return "";
    const reftable = await applicationTable(ref.reftable_name);
    if (!reftable) return "";
    const reffields: Obj[] = await reftable.getFields();
    found = reffields.find((f: Obj) => f.name === targetNm);
    if (row_id) {
      const mainRow = await table.getRow(whereClause, { forUser: req.user });
      const refId = mainRow && mainRow[refNm];
      row = refId
        ? await reftable.getRow({ id: refId }, { forUser: req.user })
        : await reftable.getRow({}, { forUser: req.user });
    } else {
      row = await reftable.getRow({}, { forUser: req.user });
    }
    value = row && row[targetNm];
  } else {
    found = fields.find((f: Obj) => f.name === fieldName);
    row = await table.getRow(whereClause, { forUser: req.user });
    value = row && row[fieldName];
  }

  const configuration = (body || {}).configuration;
  if (!found) return "";
  const field = hostField(found);
  const fieldviews =
    field.type === "Key" ? state.keyFieldviews : field.type === "File" ? state.fileviews : field.type?.fieldviews;
  if (!field.type || !fieldviews) return "";
  // v1: "Chrome 116 changes its behaviour to align with firefox - disabled
  // inputs do not dispatch click events".
  const firefox = true;
  const fv = fieldviews[fieldview];
  field.fieldviewObj = fv;
  field.attributes = { ...configuration, ...field.attributes };
  if (field.type === "Key") await field.fill_fkey_options(false, {}, {}, undefined, undefined, undefined, req.user);
  if (fv?.fill_options) await fv.fill_options(field);
  if (!fv && field.type === "Key" && fieldview === "select")
    return `<input ${firefox ? "readonly" : "disabled"} class="form-control form-select"></input>`;
  if (!fv) return "";
  if (fv.isEdit || fv.isFilter)
    return fv.run(
      field.name,
      undefined,
      {
        ...(firefox ? { readonly: true } : { disabled: true }),
        ...configuration,
        ...(field.attributes || {}),
      },
      "",
      false,
      field,
    );
  if (field.type === "File") return fv.run(value, "filename.ext");
  return `<span style="display:inline-block;min-height:1.5em;min-width:1.875em">${fv.run(value, req, configuration)}</span>`;
}

/** `POST /field/fieldviewcfgform/:tableName`, from
 * packages/server/routes/fields.ts, as the builder always asks it
 * (`?accept=json`): a fieldview's `configFields` as v1's form JSON, and `[]`
 * where v1 answers `"[]"`. The HTML form v1 sends without `accept=json` is not
 * ported, because the builder never asks for it. */
export async function builderFieldviewConfig(tableName: string, body: Obj, req: Obj): Promise<Obj[]> {
  let { type } = body || {};
  const {
    field_name,
    fieldview,
    join_field,
    join_fieldview,
    agg_outcome_type,
    agg_fieldview,
    agg_field,
    mode,
    _columndef,
  } = body || {};
  const table = await requireApplicationTable(tableName);
  if (agg_outcome_type && agg_fieldview) {
    const outcome = getState().types[agg_outcome_type];
    const fv = outcome?.fieldviews?.[agg_fieldview];
    if (!fv?.configFields) return [];
    // v1 calls `table.getField(agg_field)` whatever it holds; the shim refuses a
    // missing name, which v1 answers as no field.
    const field = agg_field ? table.getField(agg_field) : undefined;
    return await applyAsync(fv.configFields, field || { table }, { mode });
  }
  if (typeof type !== "string") {
    try {
      type = JSON.parse(_columndef).type;
    } catch {
      // v1 ignores a column definition it cannot read.
    }
  }
  const fieldName = type == "Field" ? field_name : join_field;
  const fv_name = type == "Field" ? fieldview : join_fieldview;
  if (!fieldName) return [];

  let field: Obj | undefined = table.getField(fieldName);
  if (!field && fieldName.split(".").length === 3) {
    const [inboundTableName, _inboundKey, refField] = fieldName.split(".");
    const inboundTable = await applicationTable(inboundTableName);
    field = inboundTable?.getField(refField);
  }
  if (!field && table.name === "users" && ["passwordRepeat", "password"].includes(fieldName)) {
    field = new (Field as any)({ name: fieldName, type: "String" });
  }
  if (!field) return [];
  const fieldViewConfigForms = await calcfldViewConfig([hostField(field)], false, 0, mode, req);
  const formFields: Obj[] | undefined = fieldViewConfigForms[field.name]?.[fv_name];
  if (!formFields) return [];
  formFields.forEach((ff: Obj) => {
    ff.class = ff.class ? `${ff.class} item-menu` : "item-menu";
  });
  return formFields;
}

/** `POST /view/:viewname/preview`, from packages/server/routes/view.ts: the
 * view run with the posted state, each required state field it lacks taken
 * from the first row the admin can read. */
export async function builderViewPreview(viewname: string, body: Obj, req: Obj, res: Obj): Promise<string> {
  // v1's `View.find({ name })[0]`: the host's `findOne` answers the same view.
  const view: Obj | undefined = View.findOne({ name: viewname });
  if (!view) return "";
  const query: Obj = { ...(body || {}) };
  let row: Obj | null | undefined;
  let table: Obj | undefined;
  const sfs: Obj[] = await view.get_state_fields();
  for (const sf of sfs) {
    if (sf.required && !query[sf.name]) {
      if (!row) {
        // Found as `pageBuilderOptions` above finds a view's table: the host's
        // `Table.findOne` takes the name a view's `table_id` holds.
        if (!table) table = Table.findOne(view.table_id || view.exttable_name);
        // v1 reads the table unguarded; a tableless view has no row to offer.
        row = table ? await table.getRow({}, { forUser: req.user }) : null;
      }
      if (row) query[sf.name] = row[sf.name];
    }
  }
  const contents = await view.run(query, { req, res, isPreview: true });
  return contents ?? "";
}

/** `POST /page/:pagename/preview`, from packages/server/routes/page.ts:
 * `page.run` over the query, then `res.sendWrap({}, contents)`, which for the
 * builder's XHR is the layout rendered with nothing around it. v1's
 * maintenance-mode refusal is not ported: this server has no maintenance mode. */
export async function builderPagePreview(pagename: string, req: Obj, res: Obj): Promise<string> {
  const page: Obj | undefined = Page.findOne({ name: pagename });
  if (!page) return "";
  const contents = await page.run(req.query, { res, req });
  // A page that redirected (an `on_page_load` action) has nothing to show.
  if (contents === null || contents === undefined) return "";
  return renderLayout({
    blockDispatch: {},
    layout: contents,
    role: req.user ? req.user.role_id : 100,
    req,
    is_owner: false,
  });
}

/** `GET /api/:tableName/distinct/:fieldName`, from packages/server/routes/api.ts,
 * as the builder's Tabs element asks it: `{ success: [...] }`. As the admin,
 * so v1's `potentiallyAccessAllowedRead` is always true; v1's 404s are
 * sentences naming the table or the field. */
export async function builderDistinctValues(tableName: string, fieldName: string, req: Obj): Promise<Obj> {
  const table = await requireApplicationTable(tableName);
  const field: Obj | undefined = table.getFields().find((f: Obj) => f.name === fieldName);
  if (!field) throw new Error(`the table ${tableName} has no field ${fieldName}`);
  const myReq = { user: req.user, __: req.__ };
  let dvs: unknown;
  if (field.is_fkey || (field.type_name === "String" && field.attributes?.options)) {
    // A key to a table outside the application has no values to offer here.
    if (field.is_fkey && !(await applicationTable(field.reftable_name))) dvs = [];
    else dvs = await hostField(field).distinct_values(myReq);
  } else {
    dvs = await table.distinctValues(fieldName, {});
  }
  return { success: dvs };
}
