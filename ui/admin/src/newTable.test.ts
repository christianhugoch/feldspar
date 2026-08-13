/**
 * The "New table" dialog's rules: when Create may be pressed, and what a chosen
 * file suggests the table be called.
 *
 * Both are worth pinning because both are silent when wrong. A dialog that lets
 * Create through with no file chosen sends `{name, csv: ""}` and the admin gets
 * a server error for a mistake the form could have caught; and a suggested name
 * that is not a SQL identifier gets refused by the server *after* the file has
 * been read and uploaded, which reads as "CSV import is broken" rather than as
 * "rename the file".
 */

import { describe, expect, it } from "vitest";

import {
  EMPTY_NEW_TABLE_FORM,
  importedMessage,
  newTableError,
  tableNameFromFile,
  type NewTableForm,
} from "./newTable";

/** A form with only what a test states set. */
function form(over: Partial<NewTableForm>): NewTableForm {
  return { ...EMPTY_NEW_TABLE_FORM, ...over };
}

/** A stand-in for a chosen file; only its presence matters here. */
const chosen = { name: "invoice.csv" } as File;

describe("newTableError", () => {
  it("wants a name for either kind of table", () => {
    expect(newTableError(form({}))).toMatch(/name/);
    expect(newTableError(form({ name: "   " }))).toMatch(/name/);
    expect(newTableError(form({ name: "invoice" }))).toBe(null);
  });

  it("wants a file only when the table comes from one", () => {
    expect(newTableError(form({ name: "invoice", source: "csv" }))).toMatch(/CSV/);
    expect(newTableError(form({ name: "invoice", source: "csv", file: chosen }))).toBe(null);
    // A blank table with a file left over from a switched-away choice is fine:
    // the file is simply not sent.
    expect(newTableError(form({ name: "invoice", source: "blank", file: chosen }))).toBe(null);
  });
});

describe("tableNameFromFile", () => {
  it("makes an identifier out of the file's base name", () => {
    expect(tableNameFromFile("invoice.csv")).toBe("invoice");
    expect(tableNameFromFile("Invoice List.csv")).toBe("invoice_list");
    expect(tableNameFromFile("invoice-list.CSV")).toBe("invoice_list");
    expect(tableNameFromFile("invoice_list.csv")).toBe("invoice_list");
    // Punctuation a SQL identifier cannot carry is dropped, not kept.
    expect(tableNameFromFile("2024 sales (final).csv")).toBe("_2024_sales_final");
    // A file with no extension is all base name.
    expect(tableNameFromFile("invoices")).toBe("invoices");
    // Nothing usable in it: the box stays empty and the admin types a name.
    expect(tableNameFromFile("!!.csv")).toBe("");
  });
});

describe("importedMessage", () => {
  it("counts in the singular when there is one row", () => {
    expect(importedMessage("invoice", 1)).toBe("1 row imported into invoice.");
    expect(importedMessage("invoice", 12)).toBe("12 rows imported into invoice.");
  });
});
