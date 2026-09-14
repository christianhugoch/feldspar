// A Saltcorn UI application's Library tab (TODO "The builder" §8, 9.4).
//
// Library items ("shared components" in current v1) are made and edited in the
// builder, as in v1, whose `/library/list` has no editor either. This tab is
// where an admin finds them and tidies them: what each is used by, rename, and
// delete. What is worked out here rather than in the component:
//
// - **"Used by" expands to links**: a view to its configuration, a page to the
//   builder (or to its properties on a server without the builder), and an item
//   placed inside another item to nothing, since an item has no editor here.
// - **A delete says what it leaves blank.** A placed item that is deleted
//   renders blank where it was placed (v1's `resolveSegment`), which is not an
//   error anywhere, so it has to be said before rather than noticed after.

import type { ListLibraryResponse, SaveLibraryItemRequest } from "./client";
import { builderPageUrl } from "./builder";
import { pagePropertiesHref, quotedList, viewEditorHref } from "./views";

export type LibraryItem = ListLibraryResponse[number];
export type LibraryReferences = LibraryItem["used_by"];

/** What an application with an empty library is told. */
export const NO_LIBRARY =
  "This application's library is empty. A library item is made in the builder: select " +
  "an element and choose Save as library component. It can then be placed in any of " +
  "this application's views and pages, and editing it in one place changes it everywhere.";

/** One place a library item is used, and where that place is edited. */
export type UsedBy = { kind: "view" | "page" | "library item"; name: string; href: string | null };

/** Everything that places `item`, views then pages then items, each with the
 * link to its editor. */
export function usedByLinks(
  appId: string,
  references: LibraryReferences,
  builderAvailable: boolean,
): UsedBy[] {
  return [
    ...references.views.map((name) => ({
      kind: "view" as const,
      name,
      href: viewEditorHref(appId, name),
    })),
    ...references.pages.map((name) => ({
      kind: "page" as const,
      name,
      href: builderAvailable ? builderPageUrl(appId, name) : pagePropertiesHref(appId, name),
    })),
    ...references.library.map((name) => ({ kind: "library item" as const, name, href: null })),
  ];
}

/** `1 view, 2 pages`, or `Not used`. */
export function usedBySummary(references: LibraryReferences): string {
  const parts = [
    count(references.views.length, "view"),
    count(references.pages.length, "page"),
    count(references.library.length, "library item"),
  ].filter((part): part is string => part !== null);
  return parts.length ? parts.join(", ") : "Not used";
}

/** The confirmation a delete asks for, naming what places the item. */
export function libraryDeleteConfirmation(item: LibraryItem, appName: string): string {
  const places = [
    references(item.used_by.views, "view"),
    references(item.used_by.pages, "page"),
    references(item.used_by.library, "library item"),
  ].filter((part): part is string => part !== null);
  const consequence = places.length
    ? `It is placed by ${places.join(" and ")}, which will show nothing where it was.`
    : "Nothing places it.";
  return `Delete the library item "${item.name}" from ${appName}? ${consequence} This cannot be undone.`;
}

/** Whether anything places the item, and so whether the delete must confirm. */
export function isUsed(references: LibraryReferences): boolean {
  return references.views.length + references.pages.length + references.library.length > 0;
}

/** Why `name` cannot be `item`'s new name yet, or `null`. */
export function renameLibraryError(
  items: Pick<LibraryItem, "id" | "name">[],
  item: Pick<LibraryItem, "id">,
  name: string,
): string | null {
  const trimmed = name.trim();
  if (!trimmed) return "A library item needs a name.";
  if (items.some((other) => other.id !== item.id && other.name === trimmed)) {
    return `The library already has an item named "${trimmed}".`;
  }
  return null;
}

/** The `saveLibraryItem` body that renames `item`, keeping its icon and
 * description. Its layout is the builder's and is not part of it. */
export function renameLibraryBody(item: LibraryItem, name: string): SaveLibraryItemRequest {
  return { name: name.trim(), icon: item.icon, description: item.description };
}

/** An item's layout as the read-only JSON it is shown as. */
export function libraryLayoutJson(item: Pick<LibraryItem, "layout">): string {
  return JSON.stringify(item.layout ?? {}, null, 2);
}

function count(n: number, noun: string): string | null {
  return n === 0 ? null : `${n} ${noun}${n === 1 ? "" : "s"}`;
}

function references(names: string[], noun: string): string | null {
  if (!names.length) return null;
  return `the ${noun}${names.length === 1 ? "" : "s"} ${quotedList(names)}`;
}
