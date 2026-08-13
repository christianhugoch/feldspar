/**
 * The table page's own arrangement.
 *
 * Two claims worth pinning down, both of which a rearrangement could silently
 * break:
 *
 *   - the "Triggers on this table" card shows the triggers on *this* table's
 *     rows, and only those — a scheduled trigger that happens to write here is
 *     not one of them;
 *   - the rows have moved to a screen of their own, and that screen still
 *     belongs to the Tables section of the sidebar rather than lighting nothing
 *     up.
 */

import { describe, expect, it } from "vitest";

import { NAV } from "../App";
import type { ListTriggersResponse } from "../client";
import { triggersOnTable } from "./TableDetail";

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
