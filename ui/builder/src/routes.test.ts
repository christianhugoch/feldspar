// The URL partition (TODO "The builder" §3): every URL-shaped literal in the
// vendored builder is in exactly one column of `routes.ts`, and the table names
// nothing the builder does not reach.
//
// A literal is collected when it begins with `/` followed by a path segment, or
// when it is what `fetch` is called with, what a `url:` property holds or what an
// `href` attribute says. A template literal's holes become `:param`. Two URL
// spellings are not collected, and both are covered elsewhere:
// - a CSS `url('/files/serve/…')` built in a style string, the same file-store URL
//   the `src` beside it is collected as;
// - a URL assembled from variables with no literal at all (`fetch(url)` in
//   `fetchPreview`), whose callers' `url:` literals are collected instead.

import { describe, expect, it } from "vitest";

import { FETCH_HANDLERS } from "./builder-fetch";
import { HREF_TARGETS } from "./links";
import { matchRoute, OTHER_ORIGINS, pathOf, ROUTES, routesForShape } from "./routes";
import { vendorFiles, walk, type AstNode } from "./test/vendor-source";

interface Found {
  shape: string;
  where: string;
  /** Why it was collected: its shape, or what it was passed to. */
  via: "path" | "fetch" | "url" | "href";
}

/** A string or template literal as a URL shape, or null for anything else. */
function shapeOf(node: AstNode): string | null {
  if (node?.type === "Literal" && typeof node.value === "string" && !node.regex) return node.value;
  if (node?.type === "TemplateLiteral") {
    return node.quasis.map((q: AstNode) => q.value.cooked).join(":param");
  }
  return null;
}

const PATH_SHAPE = /^\/(?:[A-Za-z0-9_-]|:param)/;

/** The routes a literal names. A literal ending in `/` that is not a route
 * itself is a prefix (`Link.js`'s `url.startsWith("/page/")` and
 * `url.replace("/view/", "/viewedit/config/")`), so it names what it prefixes. */
function routesOfLiteral(shape: string): ReturnType<typeof routesForShape> {
  const routes = routesForShape(shape);
  if (routes.length || !pathOf(shape).endsWith("/")) return routes;
  return routesForShape(`${pathOf(shape)}:param`);
}

function collect(): Found[] {
  const found: Found[] = [];
  for (const file of vendorFiles()) {
    walk(file.ast, (node, parent, key) => {
      const shape = shapeOf(node);
      if (shape === null) return;
      const where = `${file.name}:${node.loc.start.line}`;
      let via: Found["via"] | null = null;
      if (parent?.type === "CallExpression" && key === "arguments" && parent.arguments[0] === node) {
        if (parent.callee.type === "Identifier" && parent.callee.name === "fetch") via = "fetch";
      } else if (parent?.type === "Property" && key === "value" && !parent.computed) {
        const name = parent.key.type === "Identifier" ? parent.key.name : parent.key.value;
        if (name === "url") via = "url";
      } else if (parent?.type === "JSXAttribute" && parent.name.name === "href") {
        via = "href";
      }
      if (via === null && PATH_SHAPE.test(shape)) via = "path";
      if (via !== null) found.push({ shape, where, via });
    });
    // `href={…}`: find each attribute and collect its expression.
    walk(file.ast, (node) => {
      if (node.type !== "JSXAttribute" || node.name.name !== "href") return;
      if (node.value?.type !== "JSXExpressionContainer") return;
      const expression = node.value.expression;
      const candidates = expression.type === "ConditionalExpression"
        ? [expression.consequent, expression.alternate]
        : [expression];
      for (const candidate of candidates) {
        const shape = shapeOf(candidate);
        if (shape !== null) found.push({ shape, where: `${file.name}:${candidate.loc.start.line}`, via: "href" });
      }
    });
  }
  return found;
}

describe("the vendored builder's URLs", () => {
  const found = collect();

  it("are found", () => {
    // A walk that finds nothing passes every assertion below, so it must find
    // the calls this file is about.
    const shapes = new Set(found.map((f) => f.shape));
    for (const known of [
      "/library/content/:param",
      "/:param/savebuilder/:param",
      "/files/upload",
      "/api/:param/distinct/:param",
      "/viewedit/config/:param",
    ]) {
      expect(shapes, known).toContain(known);
    }
  });

  it("each land in exactly one column", () => {
    const problems: string[] = [];
    for (const { shape, where, via } of found) {
      if (shape === "" || shape.startsWith("#")) continue;
      if (/^https?:\/\//.test(shape)) {
        if (via === "fetch") problems.push(`${where}: fetches another origin, ${shape}`);
        else if (!(shape in OTHER_ORIGINS)) {
          problems.push(`${where}: names another origin, ${shape}, which OTHER_ORIGINS does not list`);
        }
        continue;
      }
      if (!shape.startsWith("/")) {
        problems.push(`${where}: ${via} is given ${JSON.stringify(shape)}, which is not a path`);
        continue;
      }
      const columns = [...new Set(routesOfLiteral(shape).map((r) => r.column))];
      if (columns.length !== 1) {
        problems.push(
          `${where}: ${shape} is in ${columns.length ? columns.join(" and ") : "no column"} of routes.ts`,
        );
      }
    }
    expect(problems).toEqual([]);
  });

  it("are every route routes.ts names", () => {
    // A route no literal reaches is stale, except one written down as reached
    // by none today, with the reason beside it.
    const reached = new Set(found.flatMap((f) => routesOfLiteral(f.shape).map((r) => r.path)));
    const stale = ROUTES.filter((r) => !reached.has(r.path) && r.from !== "(none today)").map((r) => r.path);
    expect(stale).toEqual([]);
  });

  it("name every other origin OTHER_ORIGINS lists", () => {
    const shapes = new Set(found.map((f) => f.shape));
    expect(Object.keys(OTHER_ORIGINS).filter((url) => !shapes.has(url))).toEqual([]);
  });
});

describe("routes.ts", () => {
  it("has a handler for every mapped request and a target for every mapped link, and no others", () => {
    const mapped = (reach: string) =>
      ROUTES.filter((r) => r.column === "mapped" && r.reach === reach).map((r) => r.path).sort();
    expect(Object.keys(FETCH_HANDLERS).sort()).toEqual(mapped("fetch"));
    expect(Object.keys(HREF_TARGETS).sort()).toEqual(mapped("href"));
  });

  it("matches concrete URLs with their parameters decoded", () => {
    expect(matchRoute("/field/fieldviewcfgform/Books?accept=json")).toMatchObject({
      route: { path: "/field/fieldviewcfgform/:table" },
      params: { table: "Books" },
    });
    expect(matchRoute("/viewedit/config/Show%20Books")?.params).toEqual({ name: "Show Books" });
    expect(matchRoute("/files/serve/uploads/a%20b.png")?.params).toEqual({ "*": "uploads/a%20b.png" });
    expect(matchRoute("/crashlog/")?.route.path).toBe("/crashlog/");
    // `/view/:name` and `/view/:name/preview` are different routes.
    expect(matchRoute("/view/Books/preview")?.route.path).toBe("/view/:name/preview");
    expect(matchRoute("/view/Books")?.route.path).toBe("/view/:name");
    expect(matchRoute("/viewedit/delete/7")).toBeNull();
  });
});
