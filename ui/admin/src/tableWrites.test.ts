import { describe, expect, it } from "vitest";

import { ALL_WRITES, NO_WRITES, anyWrite, canSubmit, formOffered, writesOf } from "./tableWrites";

describe("what may be written to a table", () => {
  it("gives a table in a database all three", () => {
    // No `provider` object at all is a table in a database, and the screens must
    // not read absence as "read-only".
    expect(writesOf(null)).toEqual(ALL_WRITES);
    expect(writesOf(undefined)).toEqual(ALL_WRITES);
  });

  it("gives a provided table exactly what its module answered", () => {
    // Writability is a property of the settings: the same provider configured
    // read-only answers none of the three.
    expect(writesOf({ writes: NO_WRITES })).toEqual(NO_WRITES);
    expect(writesOf({ writes: ALL_WRITES })).toEqual(ALL_WRITES);
  });

  it("reads View against Edit off whether anything can be written", () => {
    expect(anyWrite(NO_WRITES)).toBe(false);
    expect(anyWrite({ insert: false, update: false, delete: true })).toBe(true);
  });

  it("offers the row form when there is a row to add or change", () => {
    // Delete-only is a list of rows with Delete buttons and nothing to type
    // into — the form would be a box whose button is always refused.
    expect(formOffered({ insert: false, update: false, delete: true })).toBe(false);
    expect(formOffered({ insert: true, update: false, delete: false })).toBe(true);
    expect(formOffered({ insert: false, update: true, delete: false })).toBe(true);
  });

  it("gates the submit button on the mode the form is in", () => {
    // A provider that inserts but cannot update: adding a row works, and the
    // Edit button that would put the form in the other mode is not offered.
    const insertOnly = { insert: true, update: false, delete: false };
    expect(canSubmit(insertOnly, false)).toBe(true);
    expect(canSubmit(insertOnly, true)).toBe(false);

    const updateOnly = { insert: false, update: true, delete: false };
    expect(canSubmit(updateOnly, false)).toBe(false);
    expect(canSubmit(updateOnly, true)).toBe(true);
  });
});
