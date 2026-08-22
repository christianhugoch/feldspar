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

module.exports = {
  sc_plugin_api_version: 1,
  plugin_name: "echo",
  configuration_workflow,
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
  // Two entity types this version does not load: the census reports them.
  viewtemplates: [{ name: "echo_list" }, { name: "echo_show" }],
  table_providers: { echo_provider: {} },
};
