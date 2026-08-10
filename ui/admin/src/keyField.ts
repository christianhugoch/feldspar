// The model behind the field editor's `Key` form (design §3.4).
//
// A Key is the one field kind whose parameters are not independent of each
// other: the target field and the summary field must be fields *of the chosen
// target table*, and the column's storage type is the target field's — not a
// choice at all. The generic spec-driven form (`settings.tsx`) renders each
// setting on its own, which is right for a file store's config and wrong here:
// it would offer three free-text boxes, two of which can only be filled
// correctly by someone who already knows the other table's columns, and a
// "stored as" picker whose only correct answer is one the server can work out.
//
// So the Key form is bespoke, and the parts of it worth asserting without a
// browser live here: which field a key points at by default, what happens to a
// selection when the target table changes under it, and the wire `kind` the
// form produces.

/** The fields of a candidate target table, as `listFields` reports them (only
 * the parts this module reads — so a test need not build a whole response). */
export type TargetField = {
  name: string;
  sql_type: string;
  unique: boolean;
  primary_key: boolean;
};

/** The Key form's state: what the three selects hold. `summary_field` is "" for
 * "none", which is the shape a select's placeholder option produces. */
export type KeyKind = {
  target_table: string;
  target_field: string;
  summary_field: string;
};

/** A Key form with nothing chosen yet. */
export const EMPTY_KEY: KeyKind = {
  target_table: "",
  target_field: "",
  summary_field: "",
};

/**
 * The field a new key points at unless the admin says otherwise: the target's
 * primary key, else its first unique column, else its first column.
 *
 * A foreign key can only reference a unique column, so the default is the one
 * column every table created here has and the one every legacy table almost
 * always has. The fallbacks matter for a table introspected from a database that
 * was not built here: it may have no primary key at all, and offering *some*
 * field beats an empty select the admin cannot tell is empty on purpose.
 */
export function defaultTargetField(fields: TargetField[]): string {
  const pk = fields.find((f) => f.primary_key);
  if (pk) return pk.name;
  const unique = fields.find((f) => f.unique);
  if (unique) return unique.name;
  return fields[0]?.name ?? "";
}

/**
 * A Key selection re-checked against the fields of the table it now points at.
 *
 * Changing the target table invalidates both other selects, and leaving a stale
 * name in them would submit a reference to a field of a *different* table. A
 * target field that no longer exists falls back to the default; a summary field
 * that no longer exists falls back to none, since a summary is optional and
 * inventing one is worse than leaving it unset.
 */
export function reconcileKey(value: KeyKind, fields: TargetField[]): KeyKind {
  const names = fields.map((f) => f.name);
  return {
    target_table: value.target_table,
    target_field: names.includes(value.target_field)
      ? value.target_field
      : defaultTargetField(fields),
    summary_field: names.includes(value.summary_field) ? value.summary_field : "",
  };
}

/** Whether the form has enough to create the field. */
export function keyIsComplete(value: KeyKind): boolean {
  return value.target_table.trim() !== "" && value.target_field.trim() !== "";
}

/**
 * The `kind` object `createField` takes for a Key.
 *
 * `summary_field` is omitted rather than sent empty: the server reads a present
 * key as a named field and would refuse "" as a field the target does not have.
 */
export function keyKindRequest(value: KeyKind): Record<string, unknown> {
  const kind: Record<string, unknown> = {
    type: "key",
    target_table: value.target_table,
    target_field: value.target_field,
  };
  if (value.summary_field.trim() !== "") kind.summary_field = value.summary_field;
  return kind;
}

/**
 * How the column will be stored: the target field's SQL type, shown rather than
 * asked for.
 *
 * This is only a report — the server derives the same type from the same column
 * when it applies the change, so nothing here can put the two out of step. The
 * identity clause a projected (not yet introspected) column can carry is dropped,
 * since it belongs to the target, not to what references it.
 */
export function keyStorage(value: KeyKind, fields: TargetField[]): string {
  const field = fields.find((f) => f.name === value.target_field);
  if (!field) return "";
  return field.sql_type.split(" generated ")[0].trim();
}
