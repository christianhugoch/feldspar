/**
 * The Views and Pages tabs' model (TODO "Saltcorn UI" 9.4).
 *
 * What can go wrong quietly here, each with a test:
 *
 * - **The tabs are the server's call.** An application that is not views and
 *   pages has only Settings; the screen never names a framework.
 * - **A v1 name has spaces in it.** The link to the view on its subdomain and
 *   the path parameter the API is called with both have to encode it, or
 *   `List Books` is a 404 that looks like a missing view.
 * - **A view whose pattern is not here is flagged**, but not while the pattern
 *   list is still loading — a flash of every view marked broken is a lie.
 */

import { describe, expect, it } from "vitest";

import type { Roles } from "./roles";
import {
  NO_VIEWS,
  appTabs,
  deleteConfirmation,
  nameParam,
  pageRows,
  viewRows,
  viewUrl,
  type PageItem,
  type PatternItem,
  type ViewItem,
} from "./views";

const here = { protocol: "https:", host: "example.com:3032" };

const roles: Roles = [
  { role: 1, name: "admin" },
  { role: 100, name: "public" },
] as Roles;

function view(name: string, viewpattern: string, extra: Partial<ViewItem> = {}): ViewItem {
  return {
    id: `id-${name}`,
    name,
    description: "",
    viewpattern,
    table_name: "books",
    configuration: {},
    min_role: 1,
    slug: null,
    attributes: {},
    ...extra,
  };
}

function pattern(name: string, label = name): PatternItem {
  return {
    name,
    label,
    description: "",
    table_required: true,
    view_quantity: null,
    routes: [],
    steps: [],
    module: null,
  };
}

describe("appTabs", () => {
  it("gives a views-and-pages application its two tabs after Settings", () => {
    const tabs = appTabs({ id: "a1", has_views: true });
    expect(tabs.map((t) => t.id)).toEqual(["settings", "views", "pages"]);
    expect(tabs[1].href).toBe("#/applications/a1/views");
    expect(tabs[0].href).toBe("#/applications/a1/edit");
  });

  it("gives any other application only Settings", () => {
    expect(appTabs({ id: "a1", has_views: false }).map((t) => t.id)).toEqual(["settings"]);
  });
});

describe("names with spaces", () => {
  it("open the view on the application's subdomain", () => {
    expect(viewUrl("booksdb", "List Books", here)).toBe(
      "https://booksdb.example.com:3032/view/List%20Books",
    );
  });

  it("are encoded as a path parameter", () => {
    expect(nameParam("List Books")).toBe("List%20Books");
    expect(nameParam("a/b")).toBe("a%2Fb");
  });
});

describe("viewRows", () => {
  const views = [
    view("List Books", "List"),
    view("Chat", "Room", { min_role: 100, table_name: null }),
  ];

  it("shows the pattern's label, the table, the role and the link", () => {
    const [row] = viewRows(views, [pattern("List", "List view")], roles, "booksdb", here);
    expect(row).toEqual({
      name: "List Books",
      description: "",
      pattern: "List view",
      patternMissing: false,
      table: "books",
      role: "admin (1)",
      url: "https://booksdb.example.com:3032/view/List%20Books",
    });
  });

  it("flags a view whose pattern this server does not have", () => {
    const rows = viewRows(views, [pattern("List")], roles, "booksdb", here);
    expect(rows[1].patternMissing).toBe(true);
    expect(rows[1].pattern).toBe("Room");
    expect(rows[1].table).toBe("—");
    expect(rows[1].role).toBe("public (100)");
  });

  it("flags nothing while the pattern list is not there", () => {
    const rows = viewRows(views, null, roles, "booksdb", here);
    expect(rows.map((r) => r.patternMissing)).toEqual([false, false]);
  });

  it("is empty for an application with no views, which the tab explains", () => {
    expect(viewRows([], null, roles, "booksdb", here)).toEqual([]);
    expect(NO_VIEWS).toMatch(/serves nothing/);
  });
});

describe("pageRows", () => {
  const page: PageItem = {
    id: "p1",
    name: "BooksOverview",
    title: "Books",
    description: "",
    layout: {},
    min_role: 1,
    attributes: { root_page_for_roles: [1, "100"] },
  };

  it("names the roles the page is the home page of", () => {
    const [row] = pageRows([page], roles, "booksdb", here);
    expect(row.homeFor).toEqual(["admin (1)", "public (100)"]);
    expect(row.url).toBe("https://booksdb.example.com:3032/page/BooksOverview");
    expect(row.role).toBe("admin (1)");
  });

  it("is nobody's home page without the attribute", () => {
    const [row] = pageRows([{ ...page, attributes: {} }], roles, "booksdb", here);
    expect(row.homeFor).toEqual([]);
  });
});

describe("deleteConfirmation", () => {
  it("names the view, the application and what breaks", () => {
    const text = deleteConfirmation("view", "List Books", "BooksDB");
    expect(text).toContain('"List Books"');
    expect(text).toContain("BooksDB");
    expect(text).toMatch(/link to it/);
  });
});
