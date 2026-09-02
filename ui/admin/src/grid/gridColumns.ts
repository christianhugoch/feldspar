// Which columns the grid shows, in which order — and where that survives.
//
// A table with forty fields is unreadable until three of them are moved to the
// front and thirty are hidden, and an admin who does that once should not have
// to do it again on the next visit. So the arrangement is a value: an order and
// a hidden set, saved per table in the browser and reconciled against the fields
// the table actually has every time it is read back.
//
// Reconciliation is the whole of the difficulty. A saved order names fields that
// may since have been renamed or dropped, and cannot name the field added
// yesterday. The rule is: keep what is still there, in the saved order; put
// everything new at the end, in the table's own order; and forget the rest —
// which means a dropped field cannot leave a hole and a new field cannot arrive
// invisible.

/** The arrangement of a table's columns: an order, and which are hidden. */
export type ColumnLayout = {
  /** Every field, in display order. */
  order: string[];
  /** The fields not shown. Always a subset of `order`. */
  hidden: string[];
};

/** The arrangement of a table nobody has arranged: its own field order, nothing hidden. */
export function defaultLayout(fields: string[]): ColumnLayout {
  return { order: [...fields], hidden: [] };
}

/**
 * A saved arrangement, held against the fields the table has now.
 *
 * `saved` is whatever came out of storage, so it is `unknown`: a layout written
 * by an older build, or a value hand-edited into nonsense, has to produce a
 * usable arrangement rather than an exception on a screen that has not drawn
 * yet.
 */
export function reconcileLayout(fields: string[], saved: unknown): ColumnLayout {
  const known = new Set(fields);
  const record = isRecord(saved) ? saved : {};
  const savedOrder = stringArray(record.order).filter((f) => known.has(f));
  const seen = new Set(savedOrder);
  const order = [...savedOrder, ...fields.filter((f) => !seen.has(f))];
  const hidden = stringArray(record.hidden).filter((f) => known.has(f));
  return { order, hidden };
}

/** Whether a field is shown. */
export function isVisible(layout: ColumnLayout, field: string): boolean {
  return !layout.hidden.includes(field);
}

/** The shown fields, in display order — the grid's columns. */
export function visibleFields(layout: ColumnLayout): string[] {
  return layout.order.filter((f) => isVisible(layout, f));
}

/** The layout with `field` hidden if it was shown, and shown if it was hidden. */
export function toggleField(layout: ColumnLayout, field: string): ColumnLayout {
  const hidden = isVisible(layout, field)
    ? [...layout.hidden, field]
    : layout.hidden.filter((f) => f !== field);
  return { ...layout, hidden };
}

/** Every field shown, or — when `visible` is false — every field hidden. */
export function showAll(layout: ColumnLayout, visible: boolean): ColumnLayout {
  return { ...layout, hidden: visible ? [] : [...layout.order] };
}

/**
 * `field` moved to sit immediately before `before`, or to the end when `before`
 * is `null`.
 *
 * This is what a header dragged onto another header means, and it is expressed
 * as "before this one" rather than as an index because that is what a drop
 * target knows: the column the cursor is over. Dropping a column on itself
 * changes nothing.
 */
export function moveBefore(order: string[], field: string, before: string | null): string[] {
  if (field === before) return order;
  const without = order.filter((f) => f !== field);
  if (before === null) return [...without, field];
  const at = without.indexOf(before);
  if (at === -1) return order;
  return [...without.slice(0, at), field, ...without.slice(at)];
}

/**
 * `field` moved one place earlier (`-1`) or later (`+1`) among the fields that
 * are **shown**.
 *
 * Among the shown ones, because that is the movement the admin can see: a column
 * with two hidden fields behind it would otherwise take two presses to appear to
 * move once.
 */
export function nudge(layout: ColumnLayout, field: string, delta: number): ColumnLayout {
  const shown = visibleFields(layout);
  const at = shown.indexOf(field);
  if (at === -1) return layout;
  const to = at + delta;
  if (to < 0 || to >= shown.length) return layout;
  // Moving later means landing *after* the field currently in the way, which is
  // "before the one after it" — and before nothing at all at the end.
  const before = delta < 0 ? shown[to] : (shown[to + 1] ?? null);
  return { ...layout, order: moveBefore(layout.order, field, before) };
}

// --- where it survives ------------------------------------------------------

/** The storage key one table's arrangement is kept under. */
export function layoutKey(table: string): string {
  return `sc.grid.columns.${table}`;
}

/**
 * The saved arrangement for `table`, reconciled against `fields`.
 *
 * Every read is wrapped: storage is disabled in some browsers and throws on
 * access rather than returning nothing, and a data grid that cannot draw because
 * a preference could not be read would be the worse failure by a distance.
 */
export function loadLayout(table: string, fields: string[]): ColumnLayout {
  try {
    const raw = window.localStorage.getItem(layoutKey(table));
    return reconcileLayout(fields, raw === null ? null : JSON.parse(raw));
  } catch {
    return defaultLayout(fields);
  }
}

/** Save `layout` for `table`, or quietly do nothing when storage refuses. */
export function saveLayout(table: string, layout: ColumnLayout): void {
  try {
    window.localStorage.setItem(layoutKey(table), JSON.stringify(layout));
  } catch {
    // A preference that could not be written is not worth an error on screen.
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];
}
