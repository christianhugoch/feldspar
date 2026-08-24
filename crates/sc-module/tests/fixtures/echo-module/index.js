// A v1-shaped plugin, written the way the real ones are: it requires the
// `@saltcorn` API at the top of the file (which is the thing the host's stubs
// have to make survivable), configures itself with a `Workflow` of `Form`s, and
// exports its actions as a function of the module's configuration.
const Table = require("@saltcorn/data/models/table");
const { getState } = require("@saltcorn/data/db/state");
const { interpolate } = require("@saltcorn/data/utils");
const Workflow = require("@saltcorn/data/models/workflow");
const Form = require("@saltcorn/data/models/form");

const configuration_workflow = () =>
  new Workflow({
    steps: [
      {
        name: "Endpoint",
        form: () =>
          new Form({
            fields: [
              { name: "endpoint", label: "Endpoint", type: "String", required: true },
              { name: "token", label: "Token", type: "String", fieldview: "password" },
            ],
          }),
      },
    ],
  });

// What v1's `onLoad` hook was handed, kept at module level so an action can say
// whether it ran and with what. `@saltcorn/mqtt` builds its broker client here
// and its one action would throw without it, which is why the host calls it.
let loadedWith = "onLoad was never called";

module.exports = {
  sc_plugin_api_version: 1,
  plugin_name: "echo",
  configuration_workflow,
  onLoad: async (cfg) => {
    loadedWith = cfg;
  },
  actions: (cfg) => ({
    echo_row: {
      description: "Echo what the action was given",
      configFields: [
        { name: "greeting", label: "Greeting", type: "String", required: true },
        { name: "times", label: "Times", type: "Integer", default: 1 },
        { name: "loud", type: "Bool" },
      ],
      run: async ({ row, table, configuration, user, mode }) => ({
        greeting: configuration.greeting,
        row,
        table,
        user,
        mode,
        module_config: cfg || null,
      }),
    },
    echo_interpolate: {
      // A function rather than an array, and an async one: v1 allows both, and
      // the host has to call it to learn the fields.
      configFields: async () => [{ name: "template", label: "Template", type: "String" }],
      run: async ({ row, configuration }) => interpolate(configuration.template, row),
    },
    echo_missing_api: {
      description: "Call an API this version does not have",
      run: async () => await Table.findOne({ name: "books" }),
    },
    echo_state: {
      run: async () => getState().getConfig("x"),
    },
    echo_throw: {
      run: async () => {
        throw new Error("the module said no");
      },
    },
    echo_log: {
      description: "Write to the server's log the way a v1 plugin does",
      run: async ({ configuration }) => {
        // Four of the six `console` methods a v1 plugin actually reaches for,
        // and `%s` formatting, because the host formats with node's own
        // `util.format` rather than joining with spaces.
        console.log("echo says %s", (configuration || {}).greeting || "nothing");
        console.info({ from: "echo" });
        console.warn("echo is warning");
        console.error("echo is complaining");
        return "logged";
      },
    },
    echo_cycle: {
      description: "Answer with a value JSON cannot encode",
      run: async () => {
        const loop = { name: "echo" };
        loop.self = loop;
        return loop;
      },
    },
    echo_loaded: {
      description: "What onLoad was handed, and whether it ran at all",
      run: async () => ({ loaded_with: loadedWith }),
    },
    echo_exit: {
      description: "Take the host worker down",
      run: async () => process.exit(3),
    },
    echo_spin: {
      description: "Never yield, so the JS-slice watchdog has something to stop",
      run: async () => {
        // Deliberately not `await`ing anything: the point is JavaScript that
        // runs without ever giving the event loop a turn, which is the only
        // thing the slice can see. Real work rather than `for(;;){}`, because
        // an empty loop is exactly the shape V8 is free to compile into
        // something with no interrupt check in it — and a module that hangs
        // the server hangs it by computing, not by idling.
        let n = 0;
        for (;;) n = (n + JSON.parse('{"a":1}').a) % 1000000;
      },
    },
  }),
  // v1's `functions`, in all three shapes v1 allows — a function of the
  // module's own configuration at the top (which is `@saltcorn/large-language-
  // model`'s shape), a bare synchronous function inside it (`@saltcorn/markdown`
  // 's `md_to_html`), and a declared async one over the module's own state
  // (`@saltcorn/nominatim-geocode`'s `geocode_lat`).
  functions: (cfg) => ({
    // Synchronous in v1, and awaitable across the seam: the behaviour
    // difference §4a names.
    echo_upper: (s) => String(s === undefined || s === null ? "" : s).toUpperCase(),
    echo_join: {
      description: "Join what it was given",
      arguments: [
        { name: "a", type: "String" },
        { name: "b", type: "String" },
      ],
      run: (a, b) => `${a}-${b}`,
    },
    echo_endpoint: {
      description: "The module's own configured endpoint",
      isAsync: true,
      arguments: [{ name: "suffix", type: "String" }],
      // Closes over the module's configuration, which is what makes a module a
      // singleton and this call a hop onto the isolate it was loaded on.
      run: async (suffix) => `${(cfg || {}).endpoint || "none"}${suffix || ""}`,
    },
    echo_fetch: {
      description: "Do the module's own network, the way a geocoder does",
      isAsync: true,
      arguments: [{ name: "url", type: "String" }],
      run: (url) =>
        new Promise((resolve, reject) => {
          const http = require("node:http");
          http
            .get(url, (res) => {
              let body = "";
              res.on("data", (chunk) => {
                body += chunk;
              });
              res.on("end", () => resolve(JSON.parse(body)));
            })
            .on("error", reject);
        }),
    },
    echo_read: {
      description: "Read a file, the way a module with a data file of its own does",
      arguments: [{ name: "path", type: "String" }],
      // `node:fs`, not `require` — the two go through different gates, and the
      // difference is the whole of the phase 3 fence: a module may always read
      // the code it is made of, and may read nothing else it was not granted.
      run: (path) => require("node:fs").readFileSync(path, "utf8").slice(0, 40),
    },
    echo_env: {
      description: "What one environment variable looks like from in here",
      arguments: [{ name: "name", type: "String" }],
      run: (name) => {
        const value = process.env[name];
        return value === undefined ? null : String(value).length > 0;
      },
    },
    echo_unserialisable: {
      description: "Answer with a value JSON cannot encode",
      run: () => {
        const loop = { name: "echo" };
        loop.self = loop;
        return loop;
      },
    },
  }),
  // Two entity types this version does not load: the census reports them.
  viewtemplates: [{ name: "echo_list" }, { name: "echo_show" }],
  table_providers: { echo_provider: {} },
};
