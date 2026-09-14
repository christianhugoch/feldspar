/**
 * The page properties form (TODO "The builder" 9.3).
 *
 * What can go wrong quietly here, each with a test:
 *
 * - **A new page named like an existing one.** `savePage` creates or replaces
 *   by name, so the form must refuse the name, or **Create** replaces that
 *   page's layout with an empty one.
 * - **Editing a page's properties must not touch its layout**, nor the
 *   attributes this form does not show (`root_page_for_roles`).
 * - **A role nobody has** is refused naming it, once the roles are known.
 */

import { describe, expect, it } from "vitest";

import { newPageForm, pageFormErrors, pageFormOf, savePageBody } from "./pageForm";
import type { Roles } from "./roles";
import type { PageItem } from "./views";

const roles = [
  { role: 1, name: "admin" },
  { role: 100, name: "public" },
] as Roles;

const overview: PageItem = {
  id: "p1",
  name: "BooksOverview",
  title: "Books",
  description: "",
  layout: { above: [{ type: "view", view: "List Books" }] },
  min_role: 100,
  attributes: { root_page_for_roles: [100], no_menu: true },
};

describe("a new page", () => {
  it("is public, with a menu and a fixed width, and an empty layout", () => {
    const form = { ...newPageForm(), name: " Home ", title: " Library " };
    expect(savePageBody(form, null)).toEqual({
      name: "Home",
      title: "Library",
      description: "",
      layout: {},
      min_role: 100,
      attributes: {},
    });
  });

  it("needs a name no other page of the application has", () => {
    const pages = [overview];
    expect(pageFormErrors(newPageForm(), pages, null, roles).name).toMatch(/needs a name/);
    expect(
      pageFormErrors({ ...newPageForm(), name: "BooksOverview " }, pages, null, roles).name,
    ).toContain('already has a page named "BooksOverview"');
    expect(pageFormErrors({ ...newPageForm(), name: "Home" }, pages, null, roles)).toEqual({});
  });
});

describe("an existing page's properties", () => {
  it("open with the page's values and its two flags", () => {
    expect(pageFormOf(overview)).toEqual({
      name: "BooksOverview",
      title: "Books",
      description: "",
      min_role: 100,
      no_menu: true,
      request_fluid_layout: false,
    });
  });

  it("keep its own name, and refuse another page's", () => {
    const pages = [overview, { ...overview, id: "p2", name: "Home" }];
    const form = pageFormOf(overview);
    expect(pageFormErrors(form, pages, "BooksOverview", roles)).toEqual({});
    expect(pageFormErrors({ ...form, name: "Home" }, pages, "BooksOverview", roles).name).toContain(
      '"Home"',
    );
  });

  it("save without touching the layout or the attributes the form does not show", () => {
    const form = { ...pageFormOf(overview), no_menu: false, request_fluid_layout: true };
    expect(savePageBody(form, overview)).toEqual({
      name: "BooksOverview",
      title: "Books",
      description: "",
      layout: overview.layout,
      min_role: 100,
      attributes: { root_page_for_roles: [100], request_fluid_layout: true },
    });
  });
});

describe("the minimum role", () => {
  it("is one of the roles, once they are known", () => {
    const form = { ...newPageForm(), name: "Home", min_role: 40 };
    expect(pageFormErrors(form, [], null, roles).min_role).toContain("no role 40");
    expect(pageFormErrors(form, [], null, null)).toEqual({});
    expect(pageFormErrors({ ...form, min_role: 1 }, [], null, roles)).toEqual({});
  });
});
