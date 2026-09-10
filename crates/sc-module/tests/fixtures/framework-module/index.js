// A module supplying **application frameworks** (§13.3), for the seam's tests.
//
// Four of them, and three are broken on purpose — a module with one
// mis-declared framework must still supply the others, with a sentence on its
// card naming each one that is missing.
//
// It also supplies an action, so a test can assert that the frameworks a module
// gets wrong cost it its frameworks and not its actions.

module.exports = {
  sc_plugin_api_version: 1,
  plugin_name: "framework-fixture",

  actions: {
    fixture_ping: {
      description: "Answers, so the module's other half can be seen to survive.",
      run: async () => ({ pong: true }),
    },
  },

  frameworks: {
    // The one that works, and the shape a real framework has.
    toy: {
      label: "Toy",
      description: "A framework that writes two files.",
      config_fields: [
        { name: "store", label: "File store", type: "String", required: true },
        { name: "project", label: "Project directory", type: "String", default: "" },
      ],
      build: {
        store: "{{ store }}",
        source: "{{ project }}",
        output: "{{ project }}/out",
        command: "toybuild --quiet",
        install: { command: "toyinstall deps", marker: "toy_modules" },
        runtime: "{{ project }}/gen",
        client: "client.ts",
      },
      csp: { "img-src": ["'self'", "data:"] },
      builder_prompt: "You build {{ app }} in {{ root }} of {{ store }}.",
      scaffold: async (ctx) => [
        { path: "toy.json", contents: JSON.stringify({ name: ctx.name, tables: ctx.tables.map((t) => t.name) }) },
        { path: `${ctx.runtime}/toy.ts`, contents: `export const client = "${ctx.client}";\n` },
      ],
      runtime: async (ctx) => [
        { path: `${ctx.runtime}/toy.ts`, contents: `export const client = "${ctx.client}";\n` },
      ],
    },

    // Reserved: `react` is one of this server's own, and an application stores a
    // framework by name.
    react: {
      label: "Not really React",
      build: { store: "{{ store }}", source: "", output: "dist", command: "nope" },
    },

    // No build: this version serves a framework's built bundle and nothing else.
    buildless: {
      label: "Buildless",
      config_fields: [{ name: "store", type: "String", required: true }],
    },

    // A template naming a setting it has not got — refused on the Rust side,
    // where the settings are known.
    mistyped: {
      label: "Mistyped",
      config_fields: [{ name: "store", type: "String", required: true }],
      build: {
        store: "{{ store }}",
        source: "{{ projekt }}",
        output: "{{ projekt }}/dist",
        command: "npm run build",
      },
    },
  },
};
