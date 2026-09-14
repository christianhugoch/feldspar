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
import { getActionConfigFields } from "./plugin-helper.js";

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
