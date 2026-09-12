// The first lines of @saltcorn/kanban, near enough: v1's markup, v1's state and
// v1's plugin-helper, required at load. Against stubs, `div(…)` throws; against
// the library it renders. And the two feature detections kanban makes, which a
// truthy stub turns into crashes.
const { div } = require("@saltcorn/markup/tags");
const state = require("@saltcorn/data/db/state");
const { features } = state;
const helper = require("@saltcorn/data/plugin-helper");

module.exports = {
  sc_plugin_api_version: 1,
  plugin_name: "library",
  actions: {
    reach_the_library: {
      run: async () => ({
        html: div({ class: "card" }, "hi"),
        public_user_role: features?.public_user_role || 10,
        features_present: "features" in state,
        collab: helper.runCollabEvents ? "detected" : "absent",
        get_state: typeof state.getState,
      }),
    },
  },
};
