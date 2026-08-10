/**
 * The sidebar's shape.
 *
 * Two sections own a screen that is not one of their own entries — Agents owns
 * the LLM providers list, Users owns the roles list — and each of those screens
 * is reached from a button in its owner's page header. The claim worth pinning
 * down is that such a screen has no sidebar entry of its own *and* still lights
 * up its owner: without the second half, opening it would leave the sidebar
 * pointing nowhere and an admin with no idea where they are.
 */

import { describe, expect, it } from "vitest";

import { NAV } from "./App";

/** Which entry, if any, the sidebar marks as current for a route. */
function activeLabels(route: string): string[] {
  return NAV.filter((item) => item.matches.some((prefix) => route.startsWith(prefix))).map(
    (item) => item.label,
  );
}

describe("the admin sidebar", () => {
  it("has no entry for roles", () => {
    expect(NAV.map((item) => item.label)).not.toContain("Roles");
    expect(NAV.map((item) => item.href)).not.toContain("#/roles");
  });

  it("marks Users as the section the roles screen belongs to", () => {
    expect(activeLabels("/roles")).toEqual(["Users"]);
    expect(activeLabels("/users")).toEqual(["Users"]);
  });

  it("keeps the same arrangement for agents and their providers", () => {
    expect(NAV.map((item) => item.label)).not.toContain("LLM providers");
    expect(activeLabels("/llm-providers")).toEqual(["Agents"]);
  });

  it("gives every entry a route that lights it up", () => {
    for (const item of NAV) {
      expect(activeLabels(item.href.replace(/^#/, ""))).toContain(item.label);
    }
  });
});
