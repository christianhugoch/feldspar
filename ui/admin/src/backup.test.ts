/**
 * The Backup tab's model: what the dialog does to a selection.
 *
 * The screen is buttons and a modal, which a browser tests better than a suite
 * does. What can go wrong *silently* is the arithmetic underneath, and two rules
 * in it matter enough to pin down:
 *
 * - rows cannot be included without their table's metadata, and unticking a table
 *   has to take its rows with it — otherwise the dialog shows a selection the
 *   server will narrow behind the admin's back;
 * - the same shape describes what this server has and what an uploaded file has,
 *   so "everything" and the summary line have to work on both without knowing
 *   which they were handed.
 */

import { describe, expect, it } from "vitest";

import {
  choices,
  dataChoices,
  everything,
  isEmpty,
  itemDescription,
  summarise,
  withTableData,
  withTables,
  type BackupContents,
} from "./backup";

/** What the server offers — this installation's, or a backup file's. */
const contents: BackupContents = {
  tables: [
    { name: "books", label: "Books", count: 3 },
    { name: "authors", label: "authors", count: 1 },
  ],
  applications: [{ name: "blog", label: "The blog", count: null }],
  file_stores: [{ name: "assets", label: "Uploaded files", count: null }],
  users: 2,
  agents: 1,
  triggers: 4,
  ssl: true,
};

describe("what a backup includes", () => {
  it("starts with everything on offer", () => {
    const selection = everything(contents);
    expect(selection.tables).toEqual(["books", "authors"]);
    expect(selection.table_data).toEqual(["books", "authors"]);
    expect(selection.applications).toEqual(["blog"]);
    expect(selection.file_stores).toEqual(["assets"]);
    expect(selection.users && selection.agents && selection.triggers && selection.ssl).toBe(
      true,
    );
    expect(isEmpty(selection)).toBe(false);
  });

  /** A file that holds nothing offers nothing to tick — which is what keeps the
   * restore dialog from showing rows for things the backup does not carry. */
  it("selects nothing when nothing is on offer", () => {
    const selection = everything({
      tables: [],
      applications: [],
      file_stores: [],
      users: 0,
      agents: 0,
      triggers: 0,
      ssl: false,
    });
    expect(isEmpty(selection)).toBe(true);
  });

  /** A backup can hold a table's definition and not its rows: the restore dialog
   * must not offer to put back rows that are not in the file. */
  it("offers no rows for a table whose data is not there", () => {
    const selection = everything({
      ...contents,
      tables: [
        { name: "books", label: "Books", count: 3 },
        { name: "authors", label: "authors", count: null },
      ],
    });
    expect(selection.tables).toEqual(["books", "authors"]);
    expect(selection.table_data).toEqual(["books"]);
  });

  it("takes a table's rows out with the table", () => {
    const selection = withTables(everything(contents), ["books"]);
    expect(selection.tables).toEqual(["books"]);
    expect(selection.table_data).toEqual(["books"]);
    // …and the rows picker now offers only what is left.
    expect(dataChoices(contents, selection).map((c) => c.value)).toEqual(["books"]);
  });

  it("refuses rows for a table whose definition is not included", () => {
    const selection = withTableData(withTables(everything(contents), ["books"]), [
      "books",
      "authors",
    ]);
    expect(selection.table_data).toEqual(["books"]);
  });

  it("can include definitions with no rows at all", () => {
    const selection = withTableData(everything(contents), []);
    expect(selection.tables).toEqual(["books", "authors"]);
    expect(selection.table_data).toEqual([]);
    expect(isEmpty(selection)).toBe(false);
  });

  it("describes how much of each thing there is", () => {
    expect(itemDescription(contents.tables[0], "row")).toBe("3 rows");
    // Singular, because "1 rows" is the tell of a count that was not thought
    // about.
    expect(itemDescription(contents.tables[1], "row")).toBe("1 row");
    // Not counted: the item's own label stands in, and a label that is only the
    // name says nothing worth showing twice.
    expect(itemDescription(contents.applications[0], "")).toBe("The blog");
    expect(itemDescription({ name: "assets", label: "assets", count: null }, "file")).toBe("");
    expect(choices(contents.tables, "row")[0]).toEqual({
      value: "books",
      label: "books",
      description: "3 rows",
    });
  });

  it("says what is ticked in a sentence", () => {
    expect(summarise(everything(contents), contents)).toBe(
      "all 2 tables, 1 application, 1 file store, users, agents, triggers, SSL settings.",
    );

    const narrowed = withTableData(withTables(everything(contents), ["books"]), []);
    expect(summarise({ ...narrowed, agents: false, ssl: false }, contents)).toBe(
      "1 table, no rows, 1 application, 1 file store, users, triggers.",
    );

    expect(
      summarise(
        {
          tables: [],
          table_data: [],
          applications: [],
          file_stores: [],
          users: false,
          agents: false,
          triggers: false,
          ssl: false,
        },
        contents,
      ),
    ).toBe("Nothing selected.");
  });
});
