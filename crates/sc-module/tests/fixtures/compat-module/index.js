// The v1 models a Saltcorn UI view reaches (TODO "Saltcorn UI" Phase 4), probed
// from module actions. Required the way a view pattern requires them, and run
// with a view snapshot on the call, so each answers from an application exactly
// as it does inside a render.
const View = require("@saltcorn/data/models/view");
const Page = require("@saltcorn/data/models/page");
const Trigger = require("@saltcorn/data/models/trigger");
const File = require("@saltcorn/data/models/file");
const User = require("@saltcorn/data/models/user");
const Crash = require("@saltcorn/data/models/crash");
const Library = require("@saltcorn/data/models/library");
const PageGroup = require("@saltcorn/data/models/page_group");
const Field = require("@saltcorn/data/models/field");
const Form = require("@saltcorn/data/models/form");
const FieldRepeat = require("@saltcorn/data/models/fieldrepeat");
const Workflow = require("@saltcorn/data/models/workflow");
const state = require("@saltcorn/data/db/state");
const helper = require("@saltcorn/data/plugin-helper");
const { getState } = state;

const settle = (f) => {
  try {
    return { value: f() };
  } catch (e) {
    return { error: e.message };
  }
};

const settleAsync = async (f) => {
  try {
    return { value: await f() };
  } catch (e) {
    return { error: e.message };
  }
};

/** A `req`/`res` pair as a view call builds one. */
const shims = (request) => globalThis.__scViewRequest(request || {});

module.exports = {
  sc_plugin_api_version: 1,
  plugin_name: "compat",
  // v1's own Form, built at load: what the settings screen gets from it.
  configuration_workflow: () =>
    new Workflow({
      steps: [
        {
          name: "Settings",
          form: () =>
            new Form({
              fields: [
                { name: "endpoint", label: "Endpoint", type: "String", required: true },
                { name: "retries", type: "Integer", default: 3 },
              ],
            }),
        },
      ],
    }),
  actions: {
    models: {
      run: async () => {
        const list = View.findOne({ name: "List Books" });
        View.findOne({ name: "List Books" }).configuration.scribbled = true;
        const page = Page.findOne({ name: "Overview" });
        return {
          view: {
            name: list.name,
            viewtemplate: list.viewtemplate,
            table_id: list.table_id,
            min_role: list.min_role,
            attributes: list.attributes,
            is_view: list instanceof View,
            pattern_runs: typeof list.viewtemplateObj.run,
            menu_label: list.menu_label,
            select_option: list.select_option,
          },
          by_id: View.findOne({ id: "v-show" }).name,
          by_where: View.findOne({ viewtemplate: "Feed" }).name,
          missing: View.findOne({ name: "Nope" }) === undefined,
          unscribbled: View.findOne({ name: "List Books" }).configuration.scribbled === undefined,
          find: (await View.find()).map((v) => v.name),
          find_where: (await View.find({ table_id: "books" })).map((v) => v.name),
          combined: list.combine_state_and_default_state({ id: 3 }),
          role_gate: await View.findOne({ name: "Show Book" }).run({}, { req: {} }),
          page: { name: page.name, title: page.title, menu_label: page.menu_label },
          pages: (await Page.find()).map((p) => p.name),
          trigger: Trigger.findOne({ name: "notify_author" }).name,
          undeclared_trigger: Trigger.findOne({ name: "wipe_everything" }) === undefined,
          triggers: Trigger.find().map((t) => t.name),
          action_options: Trigger.action_options({
            builtIns: ["Delete"],
            builtInLabel: "View actions",
            apiNeverTriggers: true,
          }),
          roles: await User.get_roles(),
          users_table: User.table.id === undefined,
          file: {
            url: File.pathToServeUrl("/books/cover.png"),
            download: File.pathToServeUrl("cover.png", { download: true }),
            absolute: File.pathToServeUrl("https://img.example/a.png"),
            mime: File.nameToMimeType("covers/a.PNG"),
            unknown: File.nameToMimeType("README"),
            relative: File.fieldValueFromRelative("/a\\b.txt"),
          },
          library: await Library.find({}),
          page_groups: PageGroup.find(),
          crash: (await Crash.create(new Error("boom"), { originalUrl: "/view/List" })) === undefined,
        };
      },
    },
    state: {
      run: async () => {
        const s = getState();
        s.getConfigCopy("menu_items").push({ label: "scribbled" });
        s.getConfig("menu_items").push({ label: "scribbled" });
        return {
          same_state: s === getState(),
          site_name: s.getConfig("site_name"),
          base_url: s.getConfig("base_url"),
          menu_items: s.getConfig("menu_items"),
          locale: s.getConfig("default_locale"),
          locale_with_default: s.getConfig("default_locale", "nb"),
          login_form: s.getConfig("login_form", ""),
          set_and_declared: s.getConfig("exttables_min_role_read"),
          dynamic_updates: s.getConfig("enable_dynamic_updates", true),
          undeclared_with_default: s.getConfig("pagination_size", 20),
          undeclared: s.getConfig("pagination_size") === undefined,
          undeclared_but_set: s.getConfig("secret_setting", "fallback"),
          roles: s.roles,
          views: s.views.map((v) => v.name),
          types: ["String", "Integer", "Bool", "Date", "Float", "Color"].every(
            (t) => s.types[t] && s.types[t].name === t,
          ),
          select: typeof s.keyFieldviews.select.run,
          fileviews: typeof s.fileviews,
          viewtemplates: Object.keys(s.viewtemplates).sort(),
          actions: s.actions,
          functions: Object.keys(s.functions),
          evaluated: s.evaluator.evaluate("price * qty", { price: 3, qty: 4 }),
          layout: {
            wrap: typeof s.getLayout().wrap,
            render_body: typeof s.getLayout({ role_id: 1 }).renderBody,
          },
          i18n: s.i18n.__({ phrase: "Save" }),
          translated: s.__("%s rows", 5),
          logged: s.log(5, "a verbose line") === undefined,
          emit_room: settle(() => s.emitRoom("kanban", {})),
        };
      },
    },
    functions: {
      run: async () => {
        const s = getState();
        return {
          names: Object.keys(s.functions),
          awaitable: s.functions.geocode.isAsync,
          called: await s.functions.geocode.run("Oslo", 2),
          in_context: typeof s.eval_context.geocode,
          through_a_formula: await s.evaluator.evaluate("geocode(city)", { city: "Bergen" }),
        };
      },
    },
    outside: {
      run: async () => ({
        types: typeof getState().types.String,
        get_config: settle(() => getState().getConfig("site_name")),
        find_one: settle(() => View.findOne({ name: "List Books" })),
        trigger: settle(() => Trigger.findOne({ name: "notify_author" })),
      }),
    },
    request: {
      run: async ({ request }) => {
        const { req, res, response } = shims(request);
        const shape = {
          method: req.method,
          path: req.path,
          original_url: req.originalUrl,
          query: req.query,
          body: req.body,
          params: req.params,
          user: req.user,
          authenticated: req.isAuthenticated(),
          xhr: req.xhr,
          referrer: req.get("Referrer"),
          csrf: req.csrfToken(),
          locale: req.getLocale(),
          translated: req.__("Hello %s", "Ada"),
          base_url: req.get_base_url(),
          no_files: req.files === undefined,
        };
        req.flash("success", "Saved");
        shape.flashes_read = req.flash("success");
        shape.sent_before = res.headersSent;
        res.status(201).set("Page-Title", "Books");
        res.json({ ok: true });
        shape.sent_after = res.headersSent;
        return { shape, response };
      },
    },
    refusals: {
      run: async () => {
        const owners = {
          View,
          view: View.findOne({ name: "List Books" }),
          Page,
          page: Page.findOne({ name: "Overview" }),
          Trigger,
          trigger: Trigger.findOne({ name: "notify_author" }),
          File,
          User,
          Crash,
          state: getState(),
        };
        const out = {};
        for (const path of globalThis.__scV1Refused()) {
          const dot = path.indexOf(".");
          const owner = owners[path.slice(0, dot)];
          if (!owner) continue;
          const name = path.slice(dot + 1);
          if (typeof owner[name] !== "function") {
            out[path] = "not reachable as a property";
            continue;
          }
          try {
            await owner[name]();
            out[path] = "did not throw";
          } catch (e) {
            out[path] = e.message;
          }
        }
        return out;
      },
    },
    absent: {
      run: async () => ({
        // `const public_user_role = features?.public_user_role || 10;` — @saltcorn/kanban
        public_user_role: state.features?.public_user_role || 10,
        features_present: "features" in state,
        // `runCollabEvents ? await runCollabEvents(…) : []` — @saltcorn/kanban
        collab: helper.runCollabEvents ? "detected" : [],
        run_collab_events_present: "runCollabEvents" in helper,
      }),
    },
    embeds_itself: {
      run: async () => {
        const { req, res } = shims();
        const loop = () => {
          const view = View.findOne({ name: "Loop" });
          view.viewtemplateObj = { run: async () => loop().run({}, { req, res }) };
          return view;
        };
        return settleAsync(() => loop().run({}, { req, res }));
      },
    },
    nested_failure: {
      run: async () => {
        const { req, res } = shims();
        const outer = View.findOne({ name: "Loop" });
        outer.viewtemplateObj = {
          run: async () => {
            const inner = View.findOne({ name: "books Feed" });
            inner.viewtemplateObj = {
              run: async () => {
                throw new Error("the inner view broke");
              },
            };
            return inner.run({}, { req, res });
          },
        };
        return settleAsync(() => outer.run({}, { req, res }));
      },
    },
    route: {
      run: async () => {
        const view = View.findOne({ name: "Loop" });
        view.viewtemplateObj = {
          routes: {
            hello: async (table_id, name, _configuration, body) => ({ json: { hello: body.who, table_id, name } }),
            quiet: async () => undefined,
          },
        };
        const first = shims();
        await view.runRoute("hello", { who: "Ada" }, first.res, first);
        const second = shims();
        await view.runRoute("quiet", {}, second.res, second);
        const third = shims();
        const missing = await settleAsync(() => view.runRoute("nope", {}, third.res, third));
        return { json: first.response.json, quiet: second.response.json, missing };
      },
    },
    forms: {
      run: async () => {
        const form = new Form({
          fields: [
            { name: "api_key", type: "String", required: true },
            { name: "retries", type: "Integer", attributes: { min: 0 } },
            { name: "author", type: "Key to authors" },
            new FieldRepeat({ name: "columns", fields: [{ name: "label", type: "String" }] }),
          ],
        });
        const [key, retries, author, repeat] = form.fields;
        return {
          form_style: form.formStyle,
          is_field: key instanceof Field,
          key: {
            label: key.label,
            type: key.type.name,
            required: key.required,
            input_type: key.input_type,
            form_name: key.form_name,
          },
          retries: { type: retries.type.name, attributes: retries.attributes },
          author: { type: author.type, reftable_name: author.reftable_name, input_type: author.input_type },
          repeat: { is_repeat: repeat.isRepeat, inner_is_field: repeat.fields[0] instanceof Field },
          validated: key.validate({ api_key: "abc" }),
          refused: key.validate({}),
          no_type: settle(() => new Field({ name: "untyped" })),
        };
      },
    },
  },
};
