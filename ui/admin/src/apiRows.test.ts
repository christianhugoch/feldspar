/**
 * The application form's API-row model (TODO "API improvements" Phase 3): a
 * stored `{ provider, mount, config }` in, form values out, and the same record
 * back — with each setting the type its provider's `config_spec` declares.
 *
 * The round trip is the whole claim. An admin who opens an application to change
 * its mount and saves must not have its GraphQL aggregation switch turned off on
 * the way through, and a boolean that came back as the string `"true"` would be
 * refused by the server's own validation. Neither needs a browser to assert.
 */

import { describe, expect, it } from "vitest";

import { apiRowsFromApp, apiRowsToRequest, blankApiRow, specFor } from "./apiRows";
import type { FieldSpec } from "./settings";

/** A stand-in for what `listApiProviders` returns for the two providers — the
 * shape only, since nothing in the model knows what a setting means. */
const field = (
  name: string,
  type: string,
  dflt: unknown,
  extra: Partial<FieldSpec> = {},
): FieldSpec => ({
  name,
  label: name,
  type,
  required: false,
  default: dflt,
  options: [],
  multiline: false,
  ...extra,
});

const providers = [
  { name: "rest", config_spec: [field("row_cap", "int", 500)] },
  {
    name: "graphql",
    config_spec: [
      field("aggregates", "bool", false),
      field("max_depth", "int", 15),
      field("row_cap", "int", 500),
    ],
  },
];

describe("reading an application's APIs", () => {
  it("reads each provider's settings as the strings the form edits", () => {
    const rows = apiRowsFromApp([
      { provider: "rest", mount: "/api", config: { row_cap: 100 } },
      { provider: "graphql", mount: "/graphql", config: { aggregates: true } },
    ]);
    expect(rows.map((r) => r.provider)).toEqual(["rest", "graphql"]);
    expect(rows[0].config).toEqual({ row_cap: "100" });
    expect(rows[1].config).toEqual({ aggregates: "true" });
  });

  it("reads an API stored with no settings as a row with none", () => {
    const rows = apiRowsFromApp([{ provider: "rest", mount: "/api", config: {} }]);
    expect(rows[0].config).toEqual({});
    expect(apiRowsToRequest(rows, providers)[0].config).toEqual({ row_cap: 500 });
  });
});

describe("saving the rows back", () => {
  it("round-trips a provider's settings with the types its spec declares", () => {
    const rows = apiRowsFromApp([
      { provider: "graphql", mount: "/graphql", config: { aggregates: true, max_depth: 8 } },
    ]);
    const [api] = apiRowsToRequest(rows, providers);
    expect(api).toEqual({
      provider: "graphql",
      mount: "/graphql",
      // A boolean and a number, not the strings the form held: the server
      // validates against the same spec, and `"true"` is not a bool.
      config: { aggregates: true, max_depth: 8, row_cap: 500 },
    });
  });

  it("keeps the switch on when nothing but the mount was edited", () => {
    // The failure this guards: opening an application to fix its mount and
    // silently saving its aggregates off, because the form rebuilt the config
    // from controls it never rendered.
    const rows = apiRowsFromApp([
      { provider: "graphql", mount: "/graphql", config: { aggregates: true } },
    ]);
    rows[0].mount = "/gql";
    const [api] = apiRowsToRequest(rows, providers);
    expect(api.mount).toBe("/gql");
    expect(api.config.aggregates).toBe(true);
  });

  it("drops a row that is entirely blank and keeps a half-filled one", () => {
    // A blank row is the "Add" button's leftovers; a half-filled one is a
    // mistake the *server* should name, so it is sent rather than swallowed.
    const rows = [blankApiRow(), { ...blankApiRow(), provider: "rest" }];
    expect(apiRowsToRequest(rows, providers)).toHaveLength(1);
  });

  it("leaves an unregistered provider's settings exactly as they arrived", () => {
    // This server does not know `grpc`'s spec, so it cannot re-type its
    // settings. Rewriting them would destroy the configuration of a provider
    // that is merely absent; the server refuses the provider, which is the
    // actual problem.
    const rows = apiRowsFromApp([
      { provider: "grpc", mount: "/grpc", config: { reflection: true, port: 50051 } },
    ]);
    expect(specFor(providers, "grpc")).toEqual([]);
    expect(apiRowsToRequest(rows, providers)[0].config).toEqual({
      reflection: true,
      port: 50051,
    });
  });
});
