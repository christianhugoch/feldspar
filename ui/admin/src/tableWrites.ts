// What may be done to a table's rows, from the admin UI's point of view (§8.3).
//
// A table in a database can be inserted into, updated and deleted from: writing
// is the driver's, and the server refuses only for reasons this screen cannot
// know in advance (a role, a constraint, a policy). A **provided** table can do
// exactly what its module's `get_table(cfg)` answered — v1 has no declaration
// of writability, so the object either carries `insertRow`/`updateRow`/
// `deleteRows` or it does not, and `@saltcorn/postgres-tables` withholds all
// three behind its `read_only` flag.
//
// Two consequences, and they are the reason this is a module of its own rather
// than a `Boolean(provider)` in two components:
//
//   * writability is a property of the **settings**, not of the provider — the
//     same provider, configured read-only, answers nothing — so it is read off
//     the table rather than inferred from a name;
//   * the three are **separate**. A provider that answers `insertRow` and
//     nothing else gets a New row form and no Edit or Delete button, which is
//     exactly what it can do.
//
// Offering a button the server will refuse is not a safety problem — the refusal
// is honest and names the missing method — it is a screen that wastes the
// admin's time and does not say why.

/** Which of the three writes a table allows. */
export type TableWrites = { insert: boolean; update: boolean; delete: boolean };

/** A table in a database: writing is the driver's, so all three. */
export const ALL_WRITES: TableWrites = { insert: true, update: true, delete: true };

/** A provided table nothing may be written to. */
export const NO_WRITES: TableWrites = { insert: false, update: false, delete: false };

/** The `provider` object a table listing carries, as far as this is concerned. */
type ProvidedTable = { writes: TableWrites } | null | undefined;

/**
 * What may be written to a table, from its listing entry's `provider`.
 *
 * `null`/absent is a table in a database and therefore all three; a provided
 * table is whatever its module answered for the settings it has.
 */
export function writesOf(provider: ProvidedTable): TableWrites {
  return provider ? provider.writes : ALL_WRITES;
}

/** Whether anything at all may be written — what decides "Edit" against "View". */
export function anyWrite(writes: TableWrites): boolean {
  return writes.insert || writes.update || writes.delete;
}

/**
 * Whether the row form should be shown at all.
 *
 * It is the *New row* form and the *Edit row* form, so it earns its place if
 * either is possible. A table that can only be deleted from is a list of rows
 * with Delete buttons and nothing to type into.
 */
export function formOffered(writes: TableWrites): boolean {
  return writes.insert || writes.update;
}

/** Whether the form's submit button does anything, in the mode it is in. */
export function canSubmit(writes: TableWrites, editing: boolean): boolean {
  return editing ? writes.update : writes.insert;
}
