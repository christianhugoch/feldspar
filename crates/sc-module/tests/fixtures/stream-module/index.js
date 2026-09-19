// A module supplying **stream providers** — this system's `streamproviders` key
// (TODO "Streams" §12).
//
// Deliberately arithmetic and offline: what these tests are about is the seam —
// a declaration crossing into the manifest, a configuration crossing into
// `element_type` and `poll`, elements and an opaque cursor crossing back — and
// a real feed here would only make the assertions about somebody's network.
//
// Two of the four providers are broken on purpose. A module with one
// mis-declared provider must still supply the others, with a sentence on its
// card saying which one is missing and why.

/** Module-scope state, which is the reason a module is loaded once and every
 * call routed to the same worker: `failures` makes a poll that throws
 * reproducible without a clock. */
let polls = 0;

module.exports = {
  sc_plugin_api_version: 1,
  plugin_name: "stream",
  // A function of the module's own configuration, which is v1's `withCfg` rule
  // applied to this key like every other: the host has to call it to learn what
  // is here.
  streamproviders: (cfg) => ({
    // Counts. Each poll answers `batch` elements carrying on from where the
    // cursor says the last one stopped, which is what makes "the cursor is
    // carried across calls" an assertion rather than a hope.
    counter: {
      label: "Counter",
      description: "Counts, in batches",
      config_fields: [
        { name: "batch", label: "Batch", type: "Integer", default: 2 },
        { name: "interval_s", label: "Interval", type: "Float", default: 60 },
      ],
      element_type: ({ configuration }) => ({
        kind: "json",
        keys: [
          { name: "n", type: "int", required: true },
          { name: "from", type: "text" },
          // A key that is only declared when the settings ask for it, which is
          // what "the element type is a function of the configuration" means.
          ...(configuration.label ? [{ name: "label", type: "text" }] : []),
        ],
      }),
      poll: async ({ configuration, cursor }) => {
        polls += 1;
        const batch = Number(configuration.batch ?? 2);
        const start = Number(cursor ?? 0);
        const elements = [];
        for (let n = start; n < start + batch; n += 1)
          elements.push({
            n,
            from: (cfg || {}).endpoint || null,
            ...(configuration.label ? { label: configuration.label } : {}),
          });
        return { elements, cursor: start + batch };
      },
    },
    // Answers one element that is not what it declared, so that "malformed is
    // counted and not delivered" is testable without a broker.
    mixed: {
      description: "One good element and one that is not",
      element_type: () => ({ kind: "json", keys: [{ name: "n", type: "int" }] }),
      // A bare list rather than `{ elements, cursor }`: a provider with no
      // cursor should not have to wrap its answer to say so.
      poll: async () => [{ n: 1 }, { n: "not a number" }],
    },
    // Throws. The host ends the subscription and the supervisor backs off;
    // retrying in here would be a second, worse backoff nothing can see.
    unreachable: {
      description: "A poll that always fails",
      element_type: () => ({ kind: "text", encoding: "utf8" }),
      poll: async () => {
        throw new Error("the feed could not be reached");
      },
    },
    // Broken on purpose: no poll.
    unpollable: {
      description: "Declares an element type and no way to get one",
      element_type: () => ({ kind: "binary" }),
    },
    // Broken on purpose: no element type. A stream over it could only ever
    // deliver things nothing could read.
    shapeless: {
      description: "Polls, but says nothing about what it produces",
      poll: async () => [],
    },
  }),
};
