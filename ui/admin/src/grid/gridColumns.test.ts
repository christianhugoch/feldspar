/**
 * Which columns the grid shows, in which order.
 *
 * The part worth asserting is reconciliation, because it is the part that runs
 * against a table that has changed since the arrangement was saved: a field that
 * was dropped must not leave a hole in the order, a field that was added must
 * arrive **visible** rather than silently hidden, and a saved value that is
 * nonsense must produce a usable arrangement rather than an exception on a
 * screen that has not drawn yet.
 *
 * The movement rules get the same treatment: nudging a column moves it one place
 * among the columns that are *shown*, so a hidden neighbour cannot make a press
 * appear to do nothing.
 */

import { describe, expect, it } from "vitest";

import {
  defaultLayout,
  isVisible,
  moveBefore,
  nudge,
  reconcileLayout,
  showAll,
  toggleField,
  visibleFields,
} from "./gridColumns";

const fields = ["id", "title", "pages", "published"];

describe("reconcileLayout", () => {
  it("is the table's own order when nothing was saved", () => {
    expect(reconcileLayout(fields, null)).toEqual({ order: fields, hidden: [] });
  });

  it("keeps the saved order and puts new fields at the end", () => {
    const saved = { order: ["pages", "title"], hidden: ["title"] };
    expect(reconcileLayout(fields, saved)).toEqual({
      order: ["pages", "title", "id", "published"],
      hidden: ["title"],
    });
  });

  it("forgets a field the table no longer has, in the order and in the hidden set", () => {
    const saved = { order: ["gone", "title", "id"], hidden: ["gone", "id"] };
    expect(reconcileLayout(fields, saved)).toEqual({
      order: ["title", "id", "pages", "published"],
      hidden: ["id"],
    });
  });

  it("survives a saved value that is nonsense", () => {
    expect(reconcileLayout(fields, "not a layout")).toEqual(defaultLayout(fields));
    expect(reconcileLayout(fields, { order: [1, 2], hidden: 7 })).toEqual(defaultLayout(fields));
  });
});

describe("visibility", () => {
  it("hides and shows one field without disturbing the order", () => {
    const layout = defaultLayout(fields);
    const hidden = toggleField(layout, "pages");
    expect(isVisible(hidden, "pages")).toBe(false);
    expect(hidden.order).toEqual(fields);
    expect(visibleFields(hidden)).toEqual(["id", "title", "published"]);
    expect(toggleField(hidden, "pages")).toEqual(layout);
  });

  it("shows or hides the lot", () => {
    const layout = toggleField(defaultLayout(fields), "id");
    expect(visibleFields(showAll(layout, true))).toEqual(fields);
    expect(visibleFields(showAll(layout, false))).toEqual([]);
  });
});

describe("moving columns", () => {
  it("drops a column before another one", () => {
    expect(moveBefore(fields, "published", "title")).toEqual([
      "id",
      "published",
      "title",
      "pages",
    ]);
    expect(moveBefore(fields, "id", null)).toEqual(["title", "pages", "published", "id"]);
    // Dropped on itself, nothing happens.
    expect(moveBefore(fields, "title", "title")).toEqual(fields);
  });

  it("nudges a column one place among the ones that are shown", () => {
    // `pages` is hidden, so nudging `published` left must put it before `title`
    // and not merely swap it past something invisible.
    const layout = toggleField(defaultLayout(fields), "pages");
    expect(visibleFields(nudge(layout, "published", -1))).toEqual([
      "id",
      "published",
      "title",
    ]);
    expect(visibleFields(nudge(layout, "id", 1))).toEqual(["title", "id", "published"]);
  });

  it("will not nudge a column past either end", () => {
    const layout = defaultLayout(fields);
    expect(nudge(layout, "id", -1)).toEqual(layout);
    expect(nudge(layout, "published", 1)).toEqual(layout);
  });
});
