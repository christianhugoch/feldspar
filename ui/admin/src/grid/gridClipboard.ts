// The grid's clipboard: a rectangle of cells as text, and text back again.
//
// Copying out of a spreadsheet and pasting into another one works because they
// all agree on one format — tab-separated columns, newline-separated rows, with
// a cell that contains a tab, a newline or a quote wrapped in double quotes and
// its own quotes doubled. Excel, Numbers, Google Sheets and Airtable all speak
// it, so a range copied here pastes into any of them and a range copied from any
// of them pastes in here.
//
// It is a parser, so it is here rather than in the component: the cases that
// matter — a quoted cell with a newline inside it, a trailing empty column, a
// paste that arrived with CRLF line endings — are all decided by rules that can
// be asserted directly.

/** Whether a cell has to be quoted to survive the round trip. */
function needsQuoting(cell: string): boolean {
  return cell.includes("\t") || cell.includes("\n") || cell.includes("\r") || cell.includes('"');
}

/**
 * A rectangle of already-formatted cells as clipboard text.
 *
 * Rows are joined with `\n` rather than `\r\n`: every consumer accepts either,
 * and a lone `\n` is what a browser's own `clipboardData` hands over.
 */
export function encodeTsv(grid: string[][]): string {
  return grid
    .map((row) =>
      row.map((cell) => (needsQuoting(cell) ? `"${cell.replace(/"/g, '""')}"` : cell)).join("\t"),
    )
    .join("\n");
}

/**
 * Clipboard text as a rectangle of cells.
 *
 * A single cell with no tab and no newline parses as a 1×1 grid, which is what
 * makes "copy one cell, paste it over a range" work without a special case. A
 * trailing newline is dropped — every spreadsheet emits one and none of them
 * means an extra empty row by it — but a *blank line in the middle* is kept,
 * because that is a row of empty cells somebody selected.
 */
export function decodeTsv(text: string): string[][] {
  const grid: string[][] = [];
  let row: string[] = [];
  let cell = "";
  let quoted = false;
  let i = 0;
  const endCell = () => {
    row.push(cell);
    cell = "";
  };
  const endRow = () => {
    endCell();
    grid.push(row);
    row = [];
  };
  while (i < text.length) {
    const c = text[i];
    if (quoted) {
      if (c === '"') {
        // A doubled quote is a quote; a lone one closes the cell.
        if (text[i + 1] === '"') {
          cell += '"';
          i += 2;
          continue;
        }
        quoted = false;
        i += 1;
        continue;
      }
      cell += c;
      i += 1;
      continue;
    }
    if (c === '"' && cell === "") {
      quoted = true;
      i += 1;
      continue;
    }
    if (c === "\t") {
      endCell();
      i += 1;
      continue;
    }
    if (c === "\r" || c === "\n") {
      endRow();
      // CRLF is one line ending, not two.
      i += c === "\r" && text[i + 1] === "\n" ? 2 : 1;
      continue;
    }
    cell += c;
    i += 1;
  }
  // Whatever is left is the last cell of the last row, unless the text ended on
  // a line ending — in which case there is no last row to end.
  if (cell !== "" || row.length > 0 || grid.length === 0) endRow();
  return grid;
}

/** One cell a paste will write: where it lands and what goes in it. */
export type PasteCell = { row: number; column: number; text: string };

/**
 * Where a pasted rectangle lands, clipped to the grid.
 *
 * Anchored at the **top-left of the selection** and running down and right from
 * there, which is what every spreadsheet does, and clipped at the last row and
 * the last column — a paste of five columns into the second-to-last column
 * writes two and drops three rather than failing, and cannot write past the end
 * of the table.
 *
 * The one flourish is the spreadsheet **fill**: pasting a single cell over a
 * selected range fills the range with it, rather than writing one cell and
 * leaving the rest of a deliberate selection alone.
 */
export function pastePlan(
  values: string[][],
  target: { top: number; left: number; bottom: number; right: number },
  bounds: { rows: number; columns: number },
): PasteCell[] {
  if (values.length === 0) return [];
  const single = values.length === 1 && values[0].length === 1;
  const height = single ? target.bottom - target.top + 1 : values.length;
  const width = single ? target.right - target.left + 1 : Math.max(...values.map((r) => r.length));
  const cells: PasteCell[] = [];
  for (let r = 0; r < height; r += 1) {
    const row = target.top + r;
    if (row >= bounds.rows) break;
    for (let c = 0; c < width; c += 1) {
      const column = target.left + c;
      if (column >= bounds.columns) break;
      const source = single ? values[0][0] : (values[r]?.[c] ?? "");
      cells.push({ row, column, text: source });
    }
  }
  return cells;
}
