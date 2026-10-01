// The file manager's selection model, and the three columns it formats.
//
// The screen is a file browser now rather than a list with five buttons per row,
// which means it has to behave the way every other file browser does: a click
// selects, shift-click takes the run between, ctrl-click adds one, and Ctrl-A
// takes the lot. None of that is React — it is arithmetic over the visible order
// and what was clicked last — so it lives here, where it can be asserted without
// a browser.
//
// Two rules in it are easy to write and easy to get subtly wrong, which is why
// they are pinned down in `fileSelection.test.ts`:
//
// - **the anchor is what shift-click measures from, and only some clicks move
//   it.** A plain click and a ctrl-click set it; a shift-click does not, so
//   shift-clicking twice grows and shrinks one run rather than leaving a trail of
//   ranges behind it.
// - **a selection is a set of paths, not of rows.** The listing is refetched
//   after every action and re-fetched entirely when the search box is typed in,
//   so anything selected that is no longer on screen has to fall out of the
//   selection rather than be carried invisibly into the next delete.

import type { BrowseFilesResponse } from "./client";

/** One row of the listing — a browse entry, which is also what a search returns. */
export type FileEntry = BrowseFilesResponse[number];

/** What was held down when the row was clicked. */
export type ClickModifiers = {
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
};

/** The selection: the chosen paths, and the row a shift-click measures from. */
export type Selection = {
  selected: string[];
  /** The last row picked deliberately (not by a range), or `null`. */
  anchor: string | null;
};

/** The empty selection — what a screen holds before anything is clicked, and
 * what navigating to another directory resets it to. */
export const NOTHING_SELECTED: Selection = { selected: [], anchor: null };

/** The paths of every row between `from` and `to` inclusive, in listing order.
 *
 * Order is the *listing's*, not the click's: dragging a shift-click upwards
 * selects the same run as dragging it down, which is what makes the run stable
 * while the second end moves. */
export function rangeBetween(order: string[], from: string, to: string): string[] {
  const start = order.indexOf(from);
  const end = order.indexOf(to);
  if (start === -1 || end === -1) return end === -1 ? [] : [to];
  const [lo, hi] = start <= end ? [start, end] : [end, start];
  return order.slice(lo, hi + 1);
}

/** The selection after clicking `path`, given what was held down.
 *
 * - plain: only that row, and it becomes the anchor.
 * - ctrl/cmd: that row joins or leaves the selection, and becomes the anchor —
 *   so a following shift-click extends from the one just added.
 * - shift: the run from the anchor to that row *replaces* the selection, and the
 *   anchor stays put. With no anchor there is nothing to measure from, so it
 *   behaves as a plain click.
 * - ctrl+shift: the run is added to what is already selected, which is how a
 *   second block is picked up without losing the first.
 */
export function clickSelection(
  order: string[],
  current: Selection,
  path: string,
  modifiers: ClickModifiers,
): Selection {
  const additive = modifiers.ctrlKey || modifiers.metaKey;
  if (modifiers.shiftKey && current.anchor !== null) {
    const run = rangeBetween(order, current.anchor, path);
    const selected = additive ? union(current.selected, run) : run;
    // The anchor is deliberately unmoved: shift-clicking a second row must
    // resize the same run, not start a new one from the end of the last.
    return { selected: inOrder(order, selected), anchor: current.anchor };
  }
  if (additive) {
    const selected = current.selected.includes(path)
      ? current.selected.filter((p) => p !== path)
      : [...current.selected, path];
    return { selected: inOrder(order, selected), anchor: path };
  }
  return { selected: [path], anchor: path };
}

/** Everything on screen, selected — Ctrl-A. */
export function selectAll(order: string[]): Selection {
  return { selected: [...order], anchor: order.length ? order[order.length - 1] : null };
}

/** The selection narrowed to what is still on screen.
 *
 * Applied after every refetch. A path that has been deleted, renamed, or filtered
 * away by the search box is no longer something the admin can see they have
 * selected, and a hidden member of the selection is the next bulk delete taking
 * something nobody pointed at. */
export function keepPresent(current: Selection, order: string[]): Selection {
  const selected = order.filter((path) => current.selected.includes(path));
  const anchor = current.anchor !== null && order.includes(current.anchor) ? current.anchor : null;
  return { selected, anchor };
}

/** The badge above the listing: how many, and of what. */
export function selectionSummary(entries: FileEntry[], selected: string[]): string {
  const chosen = entries.filter((e) => selected.includes(e.path));
  const count = chosen.length;
  const plural = count === 1 ? "" : "s";
  if (count === 0) return "Nothing selected";
  if (chosen.every((e) => e.is_dir)) return `${count} folder${plural} selected`;
  if (chosen.every((e) => !e.is_dir)) return `${count} file${plural} selected`;
  return `${count} items selected`;
}

/** What "Copy relative path" puts on the clipboard: each entry's store-relative
 * path, separated by a space.
 *
 * Space-separated is what pastes into a shell command line, so a path that a
 * shell would split or expand — one with a space in it, say — is single-quoted,
 * or two files would paste as three. A plain path is left bare, which is every
 * path there is in the usual case, and a lone path copies as exactly itself. */
export function copyablePaths(entries: FileEntry[]): string {
  if (entries.length === 1) return entries[0].path;
  return entries.map((e) => shellQuote(e.path)).join(" ");
}

function shellQuote(path: string): string {
  if (/^[A-Za-z0-9._\-/+@%:,=]+$/.test(path)) return path;
  return `'${path.replace(/'/g, `'\\''`)}'`;
}

/** `selected` in the listing's order, without duplicates. */
function inOrder(order: string[], selected: string[]): string[] {
  return order.filter((path) => selected.includes(path));
}

/** The two lists, without duplicates and in no particular order (the caller
 * sorts by the listing). */
function union(a: string[], b: string[]): string[] {
  return [...a, ...b.filter((path) => !a.includes(path))];
}

// --- the columns -------------------------------------------------------------

/** Human-readable byte size. Directories have none, and get an em dash rather
 * than a `0 B` that would claim they are empty. */
export function formatSize(size: number | null | undefined): string {
  if (size == null) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = size;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** A modification time as a file browser writes it: the clock for today, the day
 * and month for this year, and the year as well for anything older.
 *
 * `now` is a parameter rather than a call to `Date.now()` so the rendering can be
 * asserted; the screen passes nothing and gets the real clock. The formatting is
 * spelled out rather than delegated to `toLocaleDateString`, because the column
 * has to line up and the locale's idea of a short date does not (and cannot be
 * pinned down in a test). */
export function formatModified(
  iso: string | null | undefined,
  now: Date = new Date(),
): string {
  if (!iso) return "—";
  const when = new Date(iso);
  if (Number.isNaN(when.getTime())) return "—";
  const time = `${pad(when.getHours())}:${pad(when.getMinutes())}`;
  const sameDay =
    when.getFullYear() === now.getFullYear() &&
    when.getMonth() === now.getMonth() &&
    when.getDate() === now.getDate();
  if (sameDay) return time;
  const day = `${when.getDate()} ${MONTHS[when.getMonth()]}`;
  return when.getFullYear() === now.getFullYear() ? day : `${day} ${when.getFullYear()}`;
}

function pad(n: number): string {
  return n < 10 ? `0${n}` : `${n}`;
}

/** The directory a search hit sits in, for the second line under its name; `""`
 * for one at the store root. */
export function parentDir(path: string): string {
  const slash = path.replace(/\/+$/, "").lastIndexOf("/");
  return slash === -1 ? "" : path.slice(0, slash);
}
