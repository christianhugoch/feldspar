/**
 * The file manager's selection model.
 *
 * What is worth asserting without a browser is the part an admin would notice
 * immediately if it were wrong, because every other file browser gets it right:
 * that shift-click measures from the row picked *deliberately* rather than from
 * the end of the last range, that ctrl-click adds and removes one row without
 * disturbing the rest, and that a selection cannot survive the row leaving the
 * screen — the last of which is the difference between deleting four files and
 * deleting five.
 */

import { describe, expect, it } from "vitest";

import {
  NOTHING_SELECTED,
  clickSelection,
  copyablePaths,
  formatModified,
  formatSize,
  keepPresent,
  parentDir,
  rangeBetween,
  selectAll,
  selectionSummary,
  type FileEntry,
  type Selection,
} from "./fileSelection";

const order = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"];

const plain = { shiftKey: false, ctrlKey: false, metaKey: false };
const ctrl = { shiftKey: false, ctrlKey: true, metaKey: false };
const shift = { shiftKey: true, ctrlKey: false, metaKey: false };
const ctrlShift = { shiftKey: true, ctrlKey: true, metaKey: false };

/** Click a sequence of rows, starting from nothing. */
function clicks(...steps: [string, typeof plain][]): Selection {
  return steps.reduce(
    (current, [path, modifiers]) => clickSelection(order, current, path, modifiers),
    NOTHING_SELECTED,
  );
}

describe("clicking a row", () => {
  it("selects only that row, and makes it the anchor", () => {
    expect(clicks(["b.txt", plain])).toEqual({ selected: ["b.txt"], anchor: "b.txt" });
    // A second plain click replaces rather than adds.
    expect(clicks(["b.txt", plain], ["d.txt", plain])).toEqual({
      selected: ["d.txt"],
      anchor: "d.txt",
    });
  });

  it("adds and removes one row with ctrl (and cmd), keeping listing order", () => {
    const two = clicks(["d.txt", plain], ["b.txt", ctrl]);
    expect(two.selected).toEqual(["b.txt", "d.txt"]);
    // The row just ticked is the anchor, so a shift-click extends from it.
    expect(two.anchor).toBe("b.txt");

    // Ctrl-clicking a selected row unticks it and leaves the others alone.
    const back = clickSelection(order, two, "d.txt", ctrl);
    expect(back.selected).toEqual(["b.txt"]);

    const cmd = { shiftKey: false, ctrlKey: false, metaKey: true };
    expect(clicks(["a.txt", plain], ["c.txt", cmd]).selected).toEqual(["a.txt", "c.txt"]);
  });

  it("takes the run between the anchor and the row with shift, in either direction", () => {
    expect(clicks(["b.txt", plain], ["d.txt", shift]).selected).toEqual([
      "b.txt",
      "c.txt",
      "d.txt",
    ]);
    // Upwards is the same run: the order is the listing's, not the click's.
    expect(clicks(["d.txt", plain], ["b.txt", shift]).selected).toEqual([
      "b.txt",
      "c.txt",
      "d.txt",
    ]);
  });

  it("resizes one run when shift-clicked again rather than leaving a trail", () => {
    // The anchor stays where the plain click put it, so the second shift-click
    // shrinks the run instead of starting another from `d.txt`.
    const resized = clicks(["b.txt", plain], ["e.txt", shift], ["c.txt", shift]);
    expect(resized.selected).toEqual(["b.txt", "c.txt"]);
    expect(resized.anchor).toBe("b.txt");
  });

  it("adds a second block with ctrl+shift", () => {
    const two = clicks(["a.txt", plain], ["b.txt", shift], ["d.txt", ctrl], ["e.txt", ctrlShift]);
    expect(two.selected).toEqual(["a.txt", "b.txt", "d.txt", "e.txt"]);
  });

  it("treats shift with nothing to measure from as a plain click", () => {
    expect(clicks(["c.txt", shift])).toEqual({ selected: ["c.txt"], anchor: "c.txt" });
  });
});

describe("Ctrl-A and the listing changing under the selection", () => {
  it("selects everything on screen", () => {
    expect(selectAll(order).selected).toEqual(order);
    expect(selectAll([]).anchor).toBeNull();
  });

  it("drops what is no longer listed", () => {
    // `c.txt` was deleted, or the search box now excludes it. Carrying it in the
    // selection invisibly would put it in the next bulk action.
    const after = keepPresent({ selected: ["b.txt", "c.txt"], anchor: "c.txt" }, [
      "a.txt",
      "b.txt",
    ]);
    expect(after).toEqual({ selected: ["b.txt"], anchor: null });
  });

  it("has no range to give when a row has gone", () => {
    expect(rangeBetween(order, "b.txt", "gone.txt")).toEqual([]);
  });
});

describe("the badge above the listing", () => {
  const entries: FileEntry[] = [
    { name: "notes", path: "notes", is_dir: true, size: null },
    { name: "a.txt", path: "a.txt", is_dir: false, size: 10 },
    { name: "b.txt", path: "b.txt", is_dir: false, size: 20 },
  ];

  it("counts what kind of thing is selected", () => {
    expect(selectionSummary(entries, ["a.txt", "b.txt"])).toBe("2 files selected");
    expect(selectionSummary(entries, ["a.txt"])).toBe("1 file selected");
    expect(selectionSummary(entries, ["notes"])).toBe("1 folder selected");
    expect(selectionSummary(entries, ["notes", "a.txt"])).toBe("2 items selected");
  });
});

describe("Copy relative path", () => {
  const entry = (path: string): FileEntry => ({
    name: path.split("/").pop() ?? path,
    path,
    is_dir: false,
    size: 1,
  });

  it("copies one path as exactly itself, spaces and all", () => {
    expect(copyablePaths([entry("docs/my notes.txt")])).toBe("docs/my notes.txt");
  });

  it("separates several paths by a space", () => {
    expect(copyablePaths([entry("a.txt"), entry("src/b.tsx"), entry("notes")])).toBe(
      "a.txt src/b.tsx notes",
    );
  });

  it("quotes a path that would split or expand when pasted into a shell", () => {
    expect(copyablePaths([entry("my notes.txt"), entry("it's.txt"), entry("b.txt")])).toBe(
      `'my notes.txt' 'it'\\''s.txt' b.txt`,
    );
  });
});

describe("the columns", () => {
  it("sizes a file and refuses to invent one for a folder", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(999)).toBe("999 B");
    expect(formatSize(2048)).toBe("2.0 KB");
    expect(formatSize(5 * 1024 * 1024)).toBe("5.0 MB");
    // A directory has no size, and "0 B" would claim it is empty.
    expect(formatSize(null)).toBe("—");
  });

  it("writes a modification time by how far away it is", () => {
    const now = new Date(2026, 8, 2, 15, 30);
    const at = (y: number, m: number, d: number, h = 9, min = 5) =>
      new Date(y, m, d, h, min).toISOString();
    expect(formatModified(at(2026, 8, 2, 14, 3), now)).toBe("14:03");
    expect(formatModified(at(2026, 7, 30), now)).toBe("30 Aug");
    expect(formatModified(at(2025, 11, 24), now)).toBe("24 Dec 2025");
    // A backend that does not record one says so rather than showing an epoch.
    expect(formatModified(null, now)).toBe("—");
    expect(formatModified("not a date", now)).toBe("—");
  });

  it("names the folder a search hit came from", () => {
    expect(parentDir("src/deep/list.tsx")).toBe("src/deep");
    expect(parentDir("readme.md")).toBe("");
  });
});
