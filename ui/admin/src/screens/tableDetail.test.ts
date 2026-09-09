/**
 * The table page's own arrangement.
 *
 * Three claims worth pinning down, any of which a rearrangement could silently
 * break:
 *
 *   - the "Triggers on this table" card shows the triggers on *this* table's
 *     rows, and only those — a scheduled trigger that happens to write here is
 *     not one of them;
 *   - a Key field names the table it points at, and both directions of a key —
 *     out of this table and into it — are something to click rather than
 *     something to read and then go and find;
 *   - the rows have moved to a screen of their own, and that screen still
 *     belongs to the Tables section of the sidebar rather than lighting nothing
 *     up.
 */

import { describe, expect, it } from "vitest";

import { NAV } from "../App";
import type { ListInboundKeysResponse, ListTriggersResponse } from "../client";
import { inboundKeyGroups, keyTarget, tableHref, triggersOnTable } from "./TableDetail";

type TriggerItem = ListTriggersResponse[number];

/** A trigger with only the fields this filter reads set meaningfully. */
function trigger(name: string, when: string, channel: string | null): TriggerItem {
  return {
    id: name,
    name,
    description: "",
    when,
    channel,
    only_if: null,
    action: "run_js_code",
    configuration: {},
    min_role: null,
    enabled: true,
    minute: null,
    hour: null,
    day_of_week: null,
    error: null,
    last_run_at: null,
  };
}

describe("the triggers on a table", () => {
  const all: ListTriggersResponse = [
    trigger("on-insert", "insert", "books"),
    trigger("on-update", "update", "books"),
    trigger("on-delete", "delete", "books"),
    trigger("other-table", "insert", "authors"),
    // A nightly job. Its action may well write to `books`, but it does not fire
    // *on* books, so the table's page must not claim it does.
    trigger("nightly", "daily", null),
    trigger("manual", "none", null),
  ];

  it("are the row events whose channel is that table", () => {
    expect(triggersOnTable(all, "books").map((t) => t.name)).toEqual([
      "on-insert",
      "on-update",
      "on-delete",
    ]);
  });

  it("leave out another table's triggers and every non-row event", () => {
    expect(triggersOnTable(all, "authors").map((t) => t.name)).toEqual(["other-table"]);
    expect(triggersOnTable(all, "publishers")).toEqual([]);
  });
});

describe("the table-data screen", () => {
  /** Which sidebar entry, if any, is marked current for a route. */
  const activeLabels = (route: string) =>
    NAV.filter((item) => item.matches.some((prefix) => route.startsWith(prefix))).map(
      (item) => item.label,
    );

  it("is still part of the Tables section", () => {
    expect(activeLabels("/tables/books/data")).toEqual(["Tables"]);
  });
});

describe("a key field's target", () => {
  it("is the table a Key points at, and nothing for any other kind", () => {
    expect(keyTarget({ type: "key", target_table: "authors", target_field: "id" })).toBe(
      "authors",
    );
    expect(keyTarget({ type: "file", store: "uploads" })).toBeNull();
    expect(keyTarget({ type: "calc", expression: "1 + 1" })).toBeNull();
    expect(keyTarget({ type: "plain" })).toBeNull();
    expect(keyTarget(null)).toBeNull();
  });

  it("is nothing when the key names no table, so a broken overlay is not a link", () => {
    expect(keyTarget({ type: "key" })).toBeNull();
    expect(keyTarget({ type: "key", target_table: "" })).toBeNull();
  });

  it("addresses the target's own page, escaped", () => {
    expect(tableHref("authors")).toBe("#/tables/authors");
    expect(tableHref("odd name")).toBe("#/tables/odd%20name");
  });
});

describe("the tables that point at this one", () => {
  const keys: ListInboundKeysResponse = [
    { table: "reviews", field: "book" },
    { table: "loans", field: "book" },
    // Two keys from one table: still one table to open.
    { table: "reviews", field: "sequel" },
  ];

  it("are one line per table, in name order, listing every key", () => {
    expect(inboundKeyGroups(keys)).toEqual([
      { table: "loans", fields: ["book"] },
      { table: "reviews", fields: ["book", "sequel"] },
    ]);
  });

  it("are nothing at all when no table points here", () => {
    expect(inboundKeyGroups([])).toEqual([]);
  });
});
