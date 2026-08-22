/**
 * The Modules tab's model.
 *
 * Two things here are worth pinning without a browser. The **install form**,
 * because its two sources take different things and the failure modes are quiet
 * ones: a relative path installs something the server will resolve against its
 * own modules directory rather than against wherever the admin was standing, and
 * a server with no npm cannot install anything at all — which an admin should be
 * told before filling in a form, not by a failed install.
 *
 * And the **reading of a module**, because everything interesting about one is
 * something that went wrong: it did not load, it loaded but an action of its
 * name was taken, it supplies four other entity types this version ignores. Each
 * of those is a sentence the admin acts on.
 */

import { describe, expect, it } from "vitest";

import {
  EMPTY_INSTALL,
  actionNames,
  configValues,
  installBlocked,
  isConfigurable,
  locationLabel,
  locationPlaceholder,
  moduleStatus,
  moduleSubtitle,
  unsupportedSentence,
  type Module,
} from "./modules";

/** A loaded module with one action, as `listModules` sends it. */
function module_(overrides: Partial<Module> = {}): Module {
  return {
    id: "3f0d2c1e-0000-4000-8000-000000000001",
    name: "@saltcorn/mqtt",
    source: "npm",
    location: "@saltcorn/mqtt",
    version: "0.2.0",
    configuration: { broker_url: "mqtt://localhost", password: "•••••" },
    config_spec: [],
    actions: [{ name: "mqtt_publish", description: "Publish a message", config_spec: [] }],
    unsupported: [],
    issues: [],
    loaded: true,
    api_version: 1,
    ...overrides,
  };
}

describe("the install form", () => {
  it("asks for the thing the chosen source actually takes", () => {
    expect(locationLabel("npm")).toMatch(/package/i);
    expect(locationPlaceholder("npm")).toBe("@saltcorn/mqtt");
    expect(locationLabel("local")).toMatch(/directory/i);
    expect(locationPlaceholder("local")).toMatch(/^\//);
  });

  it("starts empty, on npm", () => {
    expect(EMPTY_INSTALL).toEqual({ source: "npm", location: "" });
    expect(installBlocked(EMPTY_INSTALL, true)).toMatch(/npm package/i);
  });

  it("refuses a relative local path, because the server would resolve it elsewhere", () => {
    expect(installBlocked({ source: "local", location: "../mqtt" }, true)).toMatch(
      /absolute path/i,
    );
    expect(installBlocked({ source: "local", location: "/srv/mqtt" }, true)).toBeNull();
  });

  it("says so when the server has no npm, whatever is typed", () => {
    const blocked = installBlocked({ source: "npm", location: "@saltcorn/mqtt" }, false);
    expect(blocked).toMatch(/no npm/i);
    // …and that reason wins over an empty box, because it is the one the admin
    // cannot fix here.
    expect(installBlocked(EMPTY_INSTALL, false)).toMatch(/no npm/i);
  });

  it("lets a filled-in npm package through", () => {
    expect(installBlocked({ source: "npm", location: "@saltcorn/mqtt@0.2.0" }, true)).toBeNull();
  });
});

describe("how an installed module reads", () => {
  it("summarises what is installed and where it came from", () => {
    expect(moduleSubtitle(module_())).toBe("v0.2.0 · npm");
    expect(
      moduleSubtitle(module_({ source: "local", location: "/srv/checkouts/mqtt" })),
    ).toBe("v0.2.0 · /srv/checkouts/mqtt");
    expect(moduleSubtitle(module_({ version: null }))).toMatch(/not installed/);
  });

  it("counts the actions it supplies", () => {
    expect(moduleStatus(module_())).toEqual({ label: "1 action", tone: "green" });
    expect(actionNames(module_())).toEqual(["mqtt_publish"]);
    const two = module_({
      actions: [
        { name: "a", description: "", config_spec: [] },
        { name: "b", description: "", config_spec: [] },
      ],
    });
    expect(moduleStatus(two).label).toBe("2 actions");
  });

  it("marks a module that did not load, and one that loaded with a complaint", () => {
    expect(moduleStatus(module_({ loaded: false, issues: ["its package is not installed"] })))
      .toEqual({ label: "Not loaded", tone: "red" });
    expect(
      moduleStatus(module_({ issues: ["its action `insert_row` is not available"] })).tone,
    ).toBe("yellow");
  });

  it("says what it also supplies and this version ignores", () => {
    expect(unsupportedSentence(module_())).toBeNull();
    const sentence = unsupportedSentence(
      module_({
        unsupported: [
          { key: "viewtemplates", count: 2 },
          { key: "eventTypes", count: null },
        ],
      }),
    );
    expect(sentence).toMatch(/2 × viewtemplates/);
    expect(sentence).toMatch(/eventTypes/);
    expect(sentence).toMatch(/does not load yet/);
  });

  it("knows whether there is anything to configure", () => {
    expect(isConfigurable(module_())).toBe(false);
    expect(
      isConfigurable(
        module_({
          config_spec: [
            {
              name: "broker_url",
              label: "Broker URL",
              type: "text",
              required: true,
              default: null,
              options: [],
              multiline: false,
              secret: false,
              create_only: false,
              code_language: null,
            },
          ],
        }),
      ),
    ).toBe(true);
  });

  it("reads the stored configuration as form values, whatever shape it is", () => {
    expect(configValues(module_())).toEqual({
      broker_url: "mqtt://localhost",
      password: "•••••",
    });
    // A module's settings are whatever it declared, so a number or a flag is
    // rendered as the text a form control holds.
    expect(configValues(module_({ configuration: { port: 1883, tls: true } }))).toEqual({
      port: "1883",
      tls: "true",
    });
    // Nothing configured, or something that is not an object at all.
    expect(configValues(module_({ configuration: {} }))).toEqual({});
    expect(configValues(module_({ configuration: null }))).toEqual({});
  });
});
