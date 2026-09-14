/**
 * The Library tab (TODO "The builder" 9.4, 9.6).
 *
 * What can go wrong quietly here, each with a test:
 *
 * - **"Used by" links to the wrong editor.** A view is configured in the
 *   wizard, a page is built in the builder, or opened at its properties on a
 *   server without the builder, and an item placed in an item has no editor.
 * - **A delete that leaves places blank says so**, naming each; v1 renders a
 *   missing item as nothing, so nothing afterwards would.
 * - **A rename onto another item's name** is refused before the server is asked.
 */

import { describe, expect, it } from "vitest";

import {
  isUsed,
  libraryDeleteConfirmation,
  libraryLayoutJson,
  renameLibraryBody,
  renameLibraryError,
  usedByLinks,
  usedBySummary,
  type LibraryItem,
} from "./library";

const APP = "0b1c2d3e-0000-4000-8000-000000000001";

function item(name: string, used_by: Partial<LibraryItem["used_by"]> = {}): LibraryItem {
  return {
    id: `id-${name}`,
    name,
    description: "The title card",
    icon: "fas fa-book",
    layout: { type: "card", contents: { type: "blank", contents: "Title" } },
    attributes: {},
    used_by: { views: [], pages: [], library: [], ...used_by },
  };
}

const header = item("Book header", {
  views: ["Show Books"],
  pages: ["Home"],
  library: ["Book page"],
});

describe("used by", () => {
  it("counts views, pages and items, or says it is not used", () => {
    expect(usedBySummary(header.used_by)).toBe("1 view, 1 page, 1 library item");
    expect(
      usedBySummary({ views: ["A", "B"], pages: [], library: [] }),
    ).toBe("2 views");
    expect(usedBySummary(item("Unused").used_by)).toBe("Not used");
    expect(isUsed(header.used_by)).toBe(true);
    expect(isUsed(item("Unused").used_by)).toBe(false);
  });

  it("links each view to its configuration and each page to the builder", () => {
    expect(usedByLinks(APP, header.used_by, true)).toEqual([
      { kind: "view", name: "Show Books", href: `#/applications/${APP}/views/Show%20Books` },
      { kind: "page", name: "Home", href: `/builder/applications/${APP}/pages/Home` },
      { kind: "library item", name: "Book page", href: null },
    ]);
  });

  it("links a page to its properties on a server without the builder", () => {
    expect(usedByLinks(APP, header.used_by, false)[1].href).toBe(
      `#/applications/${APP}/pages/Home/properties`,
    );
  });
});

describe("deleting an item", () => {
  it("names everything that places it, which will show nothing", () => {
    const text = libraryDeleteConfirmation(header, "BooksDB");
    expect(text).toContain('Delete the library item "Book header" from BooksDB?');
    expect(text).toContain('the view "Show Books"');
    expect(text).toContain('the page "Home"');
    expect(text).toContain('the library item "Book page"');
    expect(text).toContain("show nothing");
  });

  it("says when nothing places it", () => {
    expect(libraryDeleteConfirmation(item("Unused"), "BooksDB")).toContain("Nothing places it.");
  });
});

describe("renaming an item", () => {
  const items = [header, item("Footer")];

  it("needs a name no other item has, its own included", () => {
    expect(renameLibraryError(items, header, " ")).toMatch(/needs a name/);
    expect(renameLibraryError(items, header, "Footer")).toContain('"Footer"');
    expect(renameLibraryError(items, header, "Book header")).toBeNull();
    expect(renameLibraryError(items, header, "Book title")).toBeNull();
  });

  it("keeps the icon and the description", () => {
    expect(renameLibraryBody(header, " Book title ")).toEqual({
      name: "Book title",
      icon: "fas fa-book",
      description: "The title card",
    });
  });
});

it("shows an item's layout as the JSON it is saved as", () => {
  expect(JSON.parse(libraryLayoutJson(header))).toEqual(header.layout);
});
