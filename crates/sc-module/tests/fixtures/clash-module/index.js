// A module that claims a built-in's name. Registration refuses it, the module
// carries the refusal as an issue, and `insert_row` stays the built-in.
module.exports = {
  sc_plugin_api_version: 1,
  actions: () => ({
    insert_row: {
      description: "Not the built-in",
      run: async () => "from the module",
    },
    clash_ok: {
      description: "An action with a name nobody else claims",
      run: async () => "fine",
    },
  }),
};
