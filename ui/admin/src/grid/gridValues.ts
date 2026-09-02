// A cell's value on three sides of the same wire: shown, typed, and pasted.
//
// Rows arrive as arbitrary JSON keyed by column name and go back the same way,
// with the server coercing each value to its column's type. So the grid only has
// to decide two things, and it has to decide them consistently: what a value
// *looks like* in a cell, and what a piece of text *means* when it is typed into
// one. Doing that in the component would mean doing it three times — the editor,
// the paste and the row form all go the same way — so it is one function each,
// here.
//
// The round trip is the property worth holding onto: `parse(kind, display(v))`
// is `v` for every value a cell can hold. Copying a range and pasting it back
// where it came from must change nothing, and that only stays true if the two
// directions are written beside each other.

/** A row as `listRows` answers with it — arbitrary JSON keyed by column name. */
export type RowRecord = Record<string, unknown>;

/** The types whose cells are a tick rather than text. */
const BOOL_TYPES = new Set(["bool"]);

/** The types a cell aligns to the right, as a spreadsheet does with numbers. */
const NUMERIC_TYPES = new Set(["int", "integer", "float", "decimal"]);

/** Whether a column of this type is edited as a checkbox. */
export function isBoolType(type: string): boolean {
  return BOOL_TYPES.has(type);
}

/** Whether a column of this type is shown right-aligned. */
export function isNumericType(type: string): boolean {
  return NUMERIC_TYPES.has(type);
}

/**
 * A JSON value as the text a cell shows and an editor opens with.
 *
 * Null and undefined are the empty cell — not the words "null" and "undefined",
 * which would be indistinguishable from a text column that contains them. An
 * object or an array is its JSON, because a `json` column's value has no shorter
 * honest rendering and the admin who put it there can read it.
 */
export function display(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

/**
 * Text as the JSON to send for a column of `type`.
 *
 * Empty is null everywhere: clearing a cell means clearing it, and the server
 * refuses the null if the column will not have one, which is the right place for
 * that to be decided.
 *
 * Otherwise the column's own type decides, rather than the shape of the text:
 * `0` in a `bool` column is false and `0` in a `text` column is the string
 * `"0"`. Reading the text instead would make a `text` column full of digits come
 * back as numbers after a paste — which is the round trip this module exists to
 * keep.
 */
export function parseCell(type: string, raw: string): unknown {
  const text = raw.trim();
  if (text === "") return null;
  if (isBoolType(type)) return parseBool(text);
  if (isNumericType(type)) {
    const n = Number(text);
    // Not a number stays text: the server's own coercion gives the better error
    // ("`pages` expects an integer"), naming the column, than anything here can.
    return Number.isFinite(n) ? n : raw;
  }
  if (type === "json") {
    try {
      return JSON.parse(text) as unknown;
    } catch {
      return raw;
    }
  }
  return raw;
}

/** `true`/`false`/`t`/`f`/`yes`/`no`/`1`/`0`, in any case; anything else stays text. */
function parseBool(text: string): unknown {
  const lower = text.toLowerCase();
  if (["true", "t", "yes", "y", "1"].includes(lower)) return true;
  if (["false", "f", "no", "n", "0"].includes(lower)) return false;
  return text;
}

/**
 * Whether two JSON cell values are the same — what decides that an edit was not
 * an edit and needs no write.
 *
 * Structural rather than `===`, because a `json` column's value is an object and
 * two equal objects are two objects.
 */
export function sameValue(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if ((a === null || a === undefined) && (b === null || b === undefined)) return true;
  return JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
}
