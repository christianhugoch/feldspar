// The model behind the "New table" dialog — the half of it that is not React.
//
// One dialog, two ways to make a table: an empty one (a name, and the identity
// primary key the server gives it), or one deduced from a CSV file (its fields
// named and typed by the file's header and contents, and every row in it
// imported). They are one dialog because they answer one question — "what table
// do you want?" — and because the *name* is asked for in both cases, so two
// buttons on the list page would have been two places to ask it.
//
// The rules here are the ones a test can pin without a browser: when the Create
// button may be pressed, and what a chosen file suggests the table be called.

/** Which of the two things the dialog is making. */
export type NewTableSource = "blank" | "csv";

/** What the dialog holds while it is open. */
export type NewTableForm = {
  name: string;
  source: NewTableSource;
  /** The chosen CSV, for `source === "csv"`. */
  file: File | null;
};

/** A dialog just opened. */
export const EMPTY_NEW_TABLE_FORM: NewTableForm = {
  name: "",
  source: "blank",
  file: null,
};

/**
 * Why this form cannot be submitted yet, or `null` when it can.
 *
 * A message rather than a boolean: the same answer disables the button and says
 * what is missing, and "Create is greyed out and I cannot tell why" is the
 * failure mode of every dialog that only returns the boolean.
 */
export function newTableError(form: NewTableForm): string | null {
  if (!form.name.trim()) return "The table needs a name.";
  if (form.source === "csv" && !form.file) return "Choose a CSV file to create the table from.";
  return null;
}

/**
 * The table name a chosen file suggests: its base name, as an identifier.
 *
 * Offered only into an *empty* name box (see `Tables.tsx`), so it is a
 * suggestion and never a correction — an admin who has already typed a name
 * keeps it. The transformation is the server's own field-name rule (`sc-api`'s
 * `label_to_name`): lower case, spaces and dashes as underscores, punctuation
 * dropped, and a leading digit pushed behind an underscore, because none of
 * those can be a SQL identifier.
 */
export function tableNameFromFile(fileName: string): string {
  const dot = fileName.lastIndexOf(".");
  const base = dot > 0 ? fileName.slice(0, dot) : fileName;
  let name = "";
  for (const c of base.trim()) {
    if (c === " " || c === "-" || c === "_") name += "_";
    else if (/[A-Za-z0-9]/.test(c)) name += c.toLowerCase();
  }
  if (/^[0-9]/.test(name)) name = `_${name}`;
  return name;
}

/**
 * What to say after a table was made from a file.
 *
 * Creating from a CSV is all-or-nothing on the server — a row it will not take
 * drops the table rather than leaving a half-filled one (§13.1) — so there is
 * one number to report here, not the two an import into an existing table has.
 */
export function importedMessage(table: string, inserted: number): string {
  return `${inserted} row${inserted === 1 ? "" : "s"} imported into ${table}.`;
}
