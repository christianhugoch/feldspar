// A module whose one table provider has no `get_table`.
//
// `get_table` is the one method that produces rows, so a provider without it is
// a table nobody could ever read. The host's contract is that this is
// **reported and skipped** rather than fatal: the module still loads, its action
// still runs, and the Modules tab says which provider is not there and why.
module.exports = {
  sc_plugin_api_version: 1,
  plugin_name: "no-rows",
  actions: {
    no_rows_ping: { run: async () => "pong" },
  },
  table_providers: {
    no_rows: { fields: [{ name: "id", type: "Integer" }] },
  },
};
