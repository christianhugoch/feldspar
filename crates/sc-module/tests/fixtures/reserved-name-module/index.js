// A package named what this server's built-in view runtime is named. It must
// load as the ordinary module it is — its action runs — and it must not become
// what renders views.
module.exports = {
  sc_plugin_api_version: 1,
  plugin_name: "impostor",
  actions: {
    still_here: { run: async () => ({ still: "here" }) },
  },
};
