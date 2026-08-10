/**
 * The multi-select's model: what a list of server options and a stored selection
 * make of each other.
 *
 * Two claims are worth asserting without a browser. The first is that a stored
 * name the server did not offer survives — it is why the application will not
 * mount, and a picker that only knew about things that exist would drop it on
 * the next save and take the explanation with it. The second is that ticking is
 * a pure function of what was selected, so "All" then "None" then one tick
 * cannot leave a duplicate in the request.
 */

import { describe, expect, it } from "vitest";

import {
  allValues,
  filterEntries,
  matchesFilter,
  mergeChoices,
  toggleValue,
  type MultiChoice,
} from "./multiSelect";

const tables: MultiChoice[] = [
  { value: "posts", description: "Blog posts" },
  { value: "comments" },
  { value: "users", label: "People" },
];

describe("the menu's rows", () => {
  it("lists the server's options in the order it gave them, labelled", () => {
    const entries = mergeChoices(tables, []);
    expect(entries.map((e) => e.value)).toEqual(["posts", "comments", "users"]);
    // An option with no label of its own is labelled by its name.
    expect(entries.map((e) => e.label)).toEqual(["posts", "comments", "People"]);
    expect(entries.every((e) => !e.missing)).toBe(true);
  });

  it("keeps a stored name the server does not offer, flagged, at the end", () => {
    const entries = mergeChoices(tables, ["comments", "legacy_tbl"]);
    expect(entries.map((e) => e.value)).toEqual(["posts", "comments", "users", "legacy_tbl"]);
    expect(entries.find((e) => e.value === "legacy_tbl")?.missing).toBe(true);
  });

  it("does not duplicate a stored name that is offered twice over", () => {
    const entries = mergeChoices(tables, ["legacy_tbl", "legacy_tbl"]);
    expect(entries.filter((e) => e.value === "legacy_tbl")).toHaveLength(1);
  });

  it("offers a stored name when the server listed nothing at all", () => {
    // A `listTables` that failed leaves the picker empty; the application still
    // names its tables, and this is the screen that has to show them.
    const entries = mergeChoices([], ["posts"]);
    expect(entries).toEqual([
      { value: "posts", label: "posts", description: "", missing: true },
    ]);
  });
});

describe("the filter box", () => {
  const entries = mergeChoices(tables, ["legacy_tbl"]);

  it("matches name, label and description, case-insensitively", () => {
    expect(filterEntries(entries, "COMM").map((e) => e.value)).toEqual(["comments"]);
    expect(filterEntries(entries, "people").map((e) => e.value)).toEqual(["users"]);
    expect(filterEntries(entries, "blog").map((e) => e.value)).toEqual(["posts"]);
  });

  it("matches everything when empty or only spaces", () => {
    expect(filterEntries(entries, "")).toHaveLength(4);
    expect(filterEntries(entries, "   ")).toHaveLength(4);
  });

  it("filters the flagged row like any other", () => {
    expect(filterEntries(entries, "legacy").map((e) => e.value)).toEqual(["legacy_tbl"]);
    expect(matchesFilter(entries[3], "nothing like it")).toBe(false);
  });
});

describe("ticking", () => {
  it("appends on tick and removes on untick", () => {
    expect(toggleValue(["posts"], "comments", true)).toEqual(["posts", "comments"]);
    expect(toggleValue(["posts", "comments"], "posts", false)).toEqual(["comments"]);
  });

  it("does not add a second copy of a value already selected", () => {
    expect(toggleValue(["posts"], "posts", true)).toEqual(["posts"]);
  });

  it("removes every copy of a duplicated value", () => {
    expect(toggleValue(["posts", "posts"], "posts", false)).toEqual([]);
  });
});

describe("All and None", () => {
  it("selects everything the menu is showing, the flagged row included", () => {
    const entries = mergeChoices(tables, ["legacy_tbl"]);
    expect(allValues(entries)).toEqual(["posts", "comments", "users", "legacy_tbl"]);
  });

  it("round-trips: All, then None, then one tick is that one name", () => {
    const entries = mergeChoices(tables, []);
    let selected = allValues(entries);
    expect(selected).toHaveLength(3);
    selected = []; // None
    selected = toggleValue(selected, "users", true);
    expect(selected).toEqual(["users"]);
  });

  it("All over a selection that already holds some leaves no duplicates", () => {
    const entries = mergeChoices(tables, ["comments"]);
    const all = allValues(entries);
    expect(new Set(all).size).toBe(all.length);
  });
});
