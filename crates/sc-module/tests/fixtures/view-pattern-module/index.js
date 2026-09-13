// A v1 plugin shaped like @saltcorn/kanban (TODO "Saltcorn UI" Phase 11): it
// requires the compatibility library the way a view pattern does, declares
// `viewtemplates` and `headers`, and ships a `public/` directory.
const { div, text } = require("@saltcorn/markup/tags");
const Table = require("@saltcorn/data/models/table");
const Form = require("@saltcorn/data/models/form");
const Workflow = require("@saltcorn/data/models/workflow");
const db = require("@saltcorn/data/db");
const { features } = require("@saltcorn/data/db/state");

// Feature detection, as kanban does it: absent, so the default.
const public_user_role = features?.public_user_role || 10;
const version = require("./package.json").version;

const configuration_workflow = () =>
  new Workflow({
    steps: [
      {
        name: "Greeting",
        form: async (context) => {
          const table = context.table_id ? Table.findOne({ id: context.table_id }) : null;
          const fields = table ? await table.getFields() : [];
          return new Form({
            fields: [
              { name: "salutation", label: "Salutation", type: "String", required: true, default: "Hello" },
              {
                name: "field",
                label: "Greet by",
                type: "String",
                attributes: { options: fields.map((f) => f.name).join() },
              },
              { name: "live", label: "Real-time updates", type: "Bool" },
            ],
          });
        },
      },
    ],
  });

const greeting = {
  name: "Greeting",
  description: "Greets every row",
  display_state_form: false,
  get_state_fields: async () => [],
  configuration_workflow,
  run: async (table_id, viewname, { salutation, field }, state, { req }) => {
    // The version tag a plugin builds a `/static_assets/` URL out of (11.5).
    const tag = db.connectObj.version_tag;
    if (!field) {
      return div({ class: "greeting", "data-version": tag }, text(`${salutation || "Hello"}, nobody`));
    }
    const table = Table.findOne({ id: table_id });
    const rows = await table.getRows({}, { orderBy: "id", forUser: req.user, forPublic: !req.user });
    return div(
      { class: "greeting", "data-version": tag },
      rows.map((row) => div({ "data-id": row.id }, text(`${salutation}, ${row[field]}`))),
    );
  },
  routes: {
    rename: async (table_id, viewname, { field }, body, { req }) => {
      const table = Table.findOne({ id: table_id });
      await table.updateRow({ [field]: body.value }, body.id, req.user || { role_id: public_user_role });
      return { json: { success: "ok", public_user_role } };
    },
  },
  // Kanban's shape: realtime events, only when the configuration asks.
  virtual_triggers: (table_id, viewname, { live }) =>
    live ? [{ when_trigger: "Insert", table_id, run: async () => {} }] : [],
};

// A name one of v1's built-in patterns already has: this pattern is lost, and
// the module keeps everything else.
const list = {
  name: "List",
  description: "Not the List you are looking for",
  run: async () => "not v1's List",
};

module.exports = {
  sc_plugin_api_version: 1,
  plugin_name: "greetings",
  headers: [
    { script: `/plugins/public/greetings@${version}/greet.js`, onlyViews: ["Greeting"] },
    { css: `/plugins/public/greetings@${version}/greet.css` },
    { headerTag: "<meta name='greeting'>" },
  ],
  viewtemplates: [greeting, list],
};
