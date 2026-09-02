/**
 * The grid's clipboard.
 *
 * The rules worth pinning down are the ones that decide whether a range survives
 * a trip through another spreadsheet: a cell containing a tab or a newline has to
 * come back as one cell, a doubled quote has to come back as one quote, and the
 * trailing newline every spreadsheet emits must not become an extra row of
 * empties that a paste would then write over real data.
 *
 * The paste geometry gets the same treatment, because both of its rules are ones
 * an admin only notices when they are wrong: a paste clips at the last row and
 * the last column rather than failing or wrapping, and a single copied cell fills
 * the selection instead of landing in its corner.
 */

import { describe, expect, it } from "vitest";

import { decodeTsv, encodeTsv, pastePlan } from "./gridClipboard";

describe("encodeTsv and decodeTsv", () => {
  it("round-trips a plain rectangle", () => {
    const grid = [
      ["1", "Dune", "412"],
      ["2", "Emma", "474"],
    ];
    expect(encodeTsv(grid)).toBe("1\tDune\t412\n2\tEmma\t474");
    expect(decodeTsv(encodeTsv(grid))).toEqual(grid);
  });

  it("round-trips the three characters that would otherwise end a cell", () => {
    const grid = [['say "hi"', "a\tb", "one\ntwo"]];
    expect(encodeTsv(grid)).toBe('"say ""hi"""\t"a\tb"\t"one\ntwo"');
    expect(decodeTsv(encodeTsv(grid))).toEqual(grid);
  });

  it("reads a single cell as a one-by-one grid", () => {
    expect(decodeTsv("Dune")).toEqual([["Dune"]]);
  });

  it("drops the trailing newline a spreadsheet emits but keeps an empty row inside", () => {
    expect(decodeTsv("a\tb\n")).toEqual([["a", "b"]]);
    expect(decodeTsv("a\n\nb")).toEqual([["a"], [""], ["b"]]);
  });

  it("reads CRLF as one line ending", () => {
    expect(decodeTsv("a\tb\r\nc\td\r\n")).toEqual([
      ["a", "b"],
      ["c", "d"],
    ]);
  });

  it("keeps empty cells at both ends of a row", () => {
    expect(decodeTsv("\ta\t")).toEqual([["", "a", ""]]);
  });
});

describe("pastePlan", () => {
  const bounds = { rows: 4, columns: 3 };

  it("lands at the top-left of the selection and runs down and right", () => {
    const plan = pastePlan(
      [
        ["a", "b"],
        ["c", "d"],
      ],
      { top: 1, left: 1, bottom: 1, right: 1 },
      bounds,
    );
    expect(plan).toEqual([
      { row: 1, column: 1, text: "a" },
      { row: 1, column: 2, text: "b" },
      { row: 2, column: 1, text: "c" },
      { row: 2, column: 2, text: "d" },
    ]);
  });

  it("clips at the last row and the last column rather than wrapping", () => {
    const plan = pastePlan(
      [
        ["a", "b", "c"],
        ["d", "e", "f"],
      ],
      { top: 3, left: 2, bottom: 3, right: 2 },
      bounds,
    );
    expect(plan).toEqual([{ row: 3, column: 2, text: "a" }]);
  });

  it("fills the selection when one cell was copied", () => {
    const plan = pastePlan([["x"]], { top: 0, left: 0, bottom: 1, right: 1 }, bounds);
    expect(plan.map((c) => c.text)).toEqual(["x", "x", "x", "x"]);
    expect(plan.map((c) => [c.row, c.column])).toEqual([
      [0, 0],
      [0, 1],
      [1, 0],
      [1, 1],
    ]);
  });

  it("pads a ragged paste with empty cells rather than leaving them alone", () => {
    // A row shorter than the widest one means those cells were empty where the
    // range was copied from, and a paste that skipped them would leave whatever
    // was underneath — which is not what was copied.
    const plan = pastePlan([["a", "b"], ["c"]], { top: 0, left: 0, bottom: 0, right: 0 }, bounds);
    expect(plan).toEqual([
      { row: 0, column: 0, text: "a" },
      { row: 0, column: 1, text: "b" },
      { row: 1, column: 0, text: "c" },
      { row: 1, column: 1, text: "" },
    ]);
  });

  it("writes nothing for an empty clipboard", () => {
    expect(pastePlan([], { top: 0, left: 0, bottom: 0, right: 0 }, bounds)).toEqual([]);
  });
});
