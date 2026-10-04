// The Clear all dialog's model: which file stores leave the disk as well as the
// database.
//
// The dialog keeps the stores the admin **unticked**, not the ones ticked, so
// that every store starts ticked (removing a store's files is the default) and
// the request names exactly what the admin left ticked when they pressed OK.

import type { ClearAllRequest, GetClearAllPreviewResponse } from "./client";

/** One file store as the dialog lists it. */
export type ClearAllStore = GetClearAllPreviewResponse["file_stores"][number];

/** The request for Clear all: every store but the unticked ones is removed
 * from disk. */
export function clearAllBody(stores: ClearAllStore[], kept: ReadonlySet<string>): ClearAllRequest {
  return { delete_from_disk: stores.map((s) => s.name).filter((n) => !kept.has(n)) };
}

/** Tick or untick one store, returning the new set of stores kept on disk. */
export function toggleKept(kept: ReadonlySet<string>, name: string): Set<string> {
  const next = new Set(kept);
  if (next.has(name)) next.delete(name);
  else next.add(name);
  return next;
}
