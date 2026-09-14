// @vitest-environment jsdom
//
// The v1 page around the builder (TODO "The builder" §4). Every v1 global the
// vendored builder reaches is in exactly one of `globals.ts`'s two lists. The
// host globals are defined by `installGlobals`, and the document globals are
// defined by the Saltcorn UI scripts that list names.
//
// "A v1 global" is, conservatively, any of:
// - a name a vendored file declares in an ESLint `/* global … */` comment, which is
//   how v1's authors wrote down what the page must supply;
// - `window.<name>`, where a plain browser window has no such property;
// - a free identifier naming something v1's page scripts (`saltcorn.js`,
//   `saltcorn-common.js`) declare at top level, jQuery's `$`, or a v1 `_sc_` value.

import fs from "node:fs";
import path from "node:path";

import { Parser } from "acorn";
import { beforeAll, describe, expect, it, vi } from "vitest";

import { BUILDER_TS_STUBS, DOCUMENT_GLOBALS, HOST_GLOBALS, installGlobals, missingDocumentGlobals } from "./globals";
import { loadV1PageScripts } from "./test/v1-page";
import { SALTCORN_UI_PUBLIC, vendorFiles, walk, type AstNode } from "./test/vendor-source";

/** Names a v1 page script declares at its top level. */
function topLevelNames(script: string): Set<string> {
  const ast = Parser.parse(fs.readFileSync(path.join(SALTCORN_UI_PUBLIC, script), "utf8"), {
    ecmaVersion: "latest",
    sourceType: "script",
  });
  const names = new Set<string>();
  for (const node of ast.body) {
    if (node.type === "FunctionDeclaration" && node.id) names.add(node.id.name);
    if (node.type === "VariableDeclaration") {
      for (const d of node.declarations) if (d.id.type === "Identifier") names.add(d.id.name);
    }
  }
  return names;
}

/** Browser globals jsdom does not implement, so the plain window below lacks
 * them although a browser has them. */
const JSDOM_LACKS = new Set(["matchMedia"]);

/** Every name a file declares itself (variables, functions, parameters,
 * imports, classes), which a same-named v1 global does not reach into. */
function declaredNames(ast: AstNode): Set<string> {
  const names = new Set<string>();
  const bind = (pattern: AstNode) => {
    walk(pattern, (node, parent, key) => {
      if (node.type !== "Identifier") return;
      // `{ a: b }` binds `b`; `a = 1` binds `a`, not the default's names.
      if (parent?.type === "Property" && key === "key" && !parent.shorthand) return;
      if (parent?.type === "AssignmentPattern" && key === "right") return;
      names.add(node.name);
    });
  };
  walk(ast, (node) => {
    if (node.type === "VariableDeclarator") bind(node.id);
    if (/^(FunctionDeclaration|FunctionExpression|ArrowFunctionExpression)$/.test(node.type)) {
      if (node.id) names.add(node.id.name);
      node.params.forEach(bind);
    }
    if (/^Import(Default|Namespace)?Specifier$/.test(node.type)) names.add(node.local.name);
    if (node.type === "ClassDeclaration" && node.id) names.add(node.id.name);
    if (node.type === "CatchClause" && node.param) bind(node.param);
  });
  return names;
}

/** Every v1 global the vendored builder reaches, with where. */
function vendorGlobals(): Map<string, string> {
  const pageNames = new Set([
    ...topLevelNames("saltcorn.js"),
    ...topLevelNames("saltcorn-common.js"),
    "$",
    "jQuery",
  ]);
  // A browser window, before anything is installed on it: an iframe's.
  const frame = document.createElement("iframe");
  document.body.appendChild(frame);
  const plainWindow = frame.contentWindow as unknown as Record<string, unknown>;

  const found = new Map<string, string>();
  const add = (name: string, where: string) => {
    if (!found.has(name)) found.set(name, where);
  };
  for (const file of vendorFiles()) {
    for (const comment of file.comments) {
      const declared = /^\s*globals?\s+(.*)$/s.exec(comment.value);
      if (!declared) continue;
      for (const name of declared[1].split(",").map((n) => n.trim()).filter(Boolean)) {
        add(name, `${file.name} (/* global */)`);
      }
    }
    const declared = declaredNames(file.ast);
    walk(file.ast, (node, parent, key) => {
      const where = `${file.name}:${node.loc.start.line}`;
      if (
        node.type === "MemberExpression" &&
        !node.computed &&
        node.object.type === "Identifier" &&
        node.object.name === "window" &&
        !(node.property.name in plainWindow) &&
        !JSDOM_LACKS.has(node.property.name)
      ) {
        add(node.property.name, where);
      }
      if (node.type !== "Identifier" || declared.has(node.name)) return;
      if (parent?.type === "MemberExpression" && key === "property" && !parent.computed) return;
      if (parent?.type === "Property" && key === "key" && !parent.computed) return;
      if (pageNames.has(node.name) || node.name.startsWith("_sc_")) add(node.name, where); // v1's page values
    });
  }
  frame.remove();
  return found;
}

describe("the builder's v1 globals", () => {
  const reached = vendorGlobals();

  it("are found", () => {
    for (const known of ["notifyAlert", "validate_expression_elem", "_sc_globalCsrf", "$"]) { // all v1's
      expect([...reached.keys()], known).toContain(known);
    }
  });

  it("are each in exactly one list", () => {
    const problems: string[] = [];
    for (const [name, where] of reached) {
      const lists = [name in DOCUMENT_GLOBALS && "DOCUMENT_GLOBALS", name in HOST_GLOBALS && "HOST_GLOBALS"].filter(
        Boolean,
      );
      if (lists.length !== 1) problems.push(`${name} (${where}) is in ${lists.length ? lists.join(" and ") : "neither list"}`);
    }
    expect(problems).toEqual([]);
  });

  it("include every host global, so the list holds nothing stale", () => {
    // Except v1's builder.ts stubs, which the preview HTML reaches rather than
    // the vendored source; they stay for as long as v1 installs them.
    const stale = Object.keys(HOST_GLOBALS).filter((name) => !reached.has(name) && !BUILDER_TS_STUBS.includes(name));
    expect(stale).toEqual([]);
  });
});

describe("the host document's scripts", () => {
  beforeAll(() => loadV1PageScripts());

  it("define every document global", () => {
    expect(missingDocumentGlobals(window)).toEqual([]);
  });

  it("toast with v1's notifyAlert into the area installGlobals makes", () => {
    installGlobals(window, { csrfToken: "token-1" });
    const g = window as unknown as Record<string, unknown>;
    for (const name of Object.keys(HOST_GLOBALS)) expect(g[name], name).not.toBeUndefined();
    expect(g._sc_globalCsrf).toBe("token-1"); // v1's CSRF global
    expect(g._sc_lightmode).toBe("light"); // v1's theme global

    (g.notifyAlert as (note: unknown) => void)({ type: "danger", text: "Unable to save" });
    const toast = document.querySelector("#toasts-area .toast");
    expect(toast?.textContent).toContain("Unable to save");
  });

  it("refuse ajax_modal's help topics with the route's sentence", () => {
    const notifyAlert = vi.fn();
    const g = window as unknown as Record<string, unknown>;
    const original = g.notifyAlert;
    g.notifyAlert = notifyAlert;
    (g.ajax_modal as (url: string) => void)("/admin/help/Formulas?mode=show");
    expect(notifyAlert).toHaveBeenCalledWith({
      type: "warning",
      text: "The builder's help topics are not in this version of Saltcorn.",
    });
    g.notifyAlert = original;
  });
});
