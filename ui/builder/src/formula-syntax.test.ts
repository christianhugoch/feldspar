// A formula's syntax checked by a parse (`formula-syntax.ts`): the same answers
// as v1's `Function` construction, with nothing evaluated, and the vendored
// builder using `Function` only the way the replacement can answer.

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { checkFormulaSyntax, NOT_EVALUATED, syntaxCheckedFunction } from "./formula-syntax";
import { vendorFiles, walk } from "./test/vendor-source";

describe("checkFormulaSyntax", () => {
  // Any construction of a function from a string during these tests fails
  // them: the check must be a parse, because the builder's CSP refuses `eval`.
  const RealFunction = globalThis.Function;
  beforeEach(() => {
    globalThis.Function = new Proxy(RealFunction, {
      apply: () => {
        throw new Error("Function was called");
      },
      construct: () => {
        throw new Error("Function was constructed");
      },
    });
  });
  afterEach(() => {
    globalThis.Function = RealFunction;
  });

  it("accepts what the Function constructor would compile", () => {
    for (const body of [
      "return undefined", // `"return " + undefined`: an empty setting, which v1 checks too
      "return ",
      "return {id: row.id, author: author}",
      "return a ? b : c // a trailing comment",
      "return `${first_name} ${last_name}`",
    ]) {
      expect(() => checkFormulaSyntax([], body), body).not.toThrow();
    }
    expect(() => checkFormulaSyntax([], "return await fetch(url)", true)).not.toThrow();
    expect(() => checkFormulaSyntax(["row", "{id}"], "return id + row.x")).not.toThrow();
  });

  it("refuses what it would not, with the parser's message and no wrapper position", () => {
    for (const body of ["return {id: ", "return a b", "return )"]) {
      let thrown: unknown;
      try {
        checkFormulaSyntax([], body);
      } catch (error) {
        thrown = error;
      }
      expect(thrown, body).toBeInstanceOf(SyntaxError);
      expect((thrown as Error).message, body).toMatch(/^Unexpected token$|^Unexpected character|^Unexpected/);
      expect((thrown as Error).message, body).not.toMatch(/\(\d+:\d+\)/);
    }
    // `await` is a syntax error outside an async function, as in v1's two checks.
    expect(() => checkFormulaSyntax([], "return await x")).toThrow(SyntaxError);
  });

  it("cannot be closed from inside the formula", () => {
    expect(() => checkFormulaSyntax([], "return 1 }); (function () {")).toThrow(SyntaxError);
  });

  it("evaluates nothing, as the vendored files' Function", () => {
    const g = globalThis as unknown as { __ran?: number };
    const fn = syntaxCheckedFunction("return globalThis.__ran = 1");
    expect(g.__ran).toBeUndefined();
    expect(() => fn()).toThrow(NOT_EVALUATED);
    expect(g.__ran).toBeUndefined();
    expect(() => syntaxCheckedFunction("return {")).toThrow(SyntaxError);
  });
});

describe("the vendored builder's Function", () => {
  it("is only ever called for its syntax check, with the result discarded", () => {
    // `build.mjs` replaces `Function` in every vendored file with
    // `syntaxCheckedFunction`, which is right only for a call whose function is
    // never used. A refresh that adds any other use fails here.
    const uses: string[] = [];
    const wrong: string[] = [];
    for (const file of vendorFiles()) {
      walk(file.ast, (node, parent, key) => {
        if (node.type !== "Identifier" || node.name !== "Function") return;
        if (parent?.type === "MemberExpression" && key === "property" && !parent.computed) return;
        const where = `${file.name}:${node.loc.start.line}`;
        uses.push(where);
        if (!(parent?.type === "CallExpression" && key === "callee")) wrong.push(`${where} is not a call`);
      });
      walk(file.ast, (node) => {
        if (node.type !== "ExpressionStatement") return;
        const call = node.expression;
        if (call.type === "CallExpression" && call.callee.type === "Identifier" && call.callee.name === "Function") {
          uses.splice(uses.indexOf(`${file.name}:${call.loc.start.line}`), 1);
        }
      });
    }
    // Every call left in `uses` was not a statement of its own, so its result
    // is used.
    expect([...wrong, ...uses.map((where) => `${where}'s result is used`)]).toEqual([]);
  });

  it("is found at v1's three checks", () => {
    // A walk that finds nothing passes the test above, so it must find these.
    const found: string[] = [];
    for (const file of vendorFiles()) {
      walk(file.ast, (node) => {
        if (node.type === "CallExpression" && node.callee.type === "Identifier" && node.callee.name === "Function") {
          found.push(file.name);
        }
      });
    }
    expect(found.sort()).toEqual(["components/elements/View.js", "components/elements/ViewLink.js", "components/elements/utils.js"]);
  });
});
