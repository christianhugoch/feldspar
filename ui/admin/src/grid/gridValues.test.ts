/**
 * A cell's value, shown and typed.
 *
 * The property this file exists to hold is the round trip: copying a range and
 * pasting it straight back must change nothing. That only stays true if the
 * column's **type** decides what a piece of text means — `0` is `false` in a
 * `bool` column and the string `"0"` in a `text` one — rather than the shape of
 * the text, which would quietly turn a text column full of digits into numbers.
 */

import { describe, expect, it } from "vitest";

import { display, isBoolType, isNumericType, parseCell, sameValue } from "./gridValues";

describe("display", () => {
  it("shows an empty cell for a missing value rather than the word for it", () => {
    expect(display(null)).toBe("");
    expect(display(undefined)).toBe("");
  });

  it("shows scalars as themselves and objects as their JSON", () => {
    expect(display(412)).toBe("412");
    expect(display(false)).toBe("false");
    expect(display("Dune")).toBe("Dune");
    expect(display({ a: 1 })).toBe('{"a":1}');
    expect(display([1, 2])).toBe("[1,2]");
  });
});

describe("parseCell", () => {
  it("clears a cell for empty text, whatever the column is", () => {
    expect(parseCell("text", "")).toBeNull();
    expect(parseCell("int", "   ")).toBeNull();
  });

  it("lets the column's type decide, not the shape of the text", () => {
    expect(parseCell("int", "412")).toBe(412);
    expect(parseCell("text", "412")).toBe("412");
    expect(parseCell("bool", "0")).toBe(false);
    expect(parseCell("text", "0")).toBe("0");
    expect(parseCell("bool", "YES")).toBe(true);
    expect(parseCell("json", '{"a":1}')).toEqual({ a: 1 });
  });

  it("leaves text the column cannot take as text, for the server to name", () => {
    // The server's own coercion gives the better message — it can name the
    // column and its type — than a guess made here could.
    expect(parseCell("int", "lots")).toBe("lots");
    expect(parseCell("json", "{oops")).toBe("{oops");
    expect(parseCell("bool", "maybe")).toBe("maybe");
  });

  it("round-trips every value a cell can hold", () => {
    for (const [type, value] of [
      ["text", "Dune"],
      ["text", "0"],
      ["int", 412],
      ["float", 1.5],
      ["bool", true],
      ["bool", false],
      ["json", { a: [1, 2] }],
      ["date", "2026-01-01"],
    ] as Array<[string, unknown]>) {
      expect(sameValue(parseCell(type, display(value)), value)).toBe(true);
    }
  });
});

describe("kinds and sameness", () => {
  it("knows which columns are ticks and which are right-aligned", () => {
    expect(isBoolType("bool")).toBe(true);
    expect(isBoolType("text")).toBe(false);
    expect(isNumericType("decimal")).toBe(true);
    expect(isNumericType("date")).toBe(false);
  });

  it("treats null and undefined as the same empty cell", () => {
    expect(sameValue(null, undefined)).toBe(true);
    expect(sameValue(null, "")).toBe(false);
    expect(sameValue({ a: 1 }, { a: 1 })).toBe(true);
  });
});
