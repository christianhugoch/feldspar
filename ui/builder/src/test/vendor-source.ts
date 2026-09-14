// The vendored builder as source, for the partition tests: every file parsed as
// JSX, and a walk over the syntax tree.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { Parser, type Comment } from "acorn";
import jsx from "acorn-jsx";

/** `ui/builder/`. */
export const UI = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
export const VENDOR = path.join(UI, "vendor", "saltcorn-builder");
/** `crates/sc-server/tests/fixtures/`. */
export const FIXTURES = path.resolve(UI, "../../crates/sc-server/tests/fixtures");
/** Saltcorn UI's browser assets, which the builder's host document loads. */
export const SALTCORN_UI_PUBLIC = path.resolve(UI, "../saltcorn-ui/public");

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type AstNode = any;

export interface SourceFile {
  /** Relative to `vendor/saltcorn-builder/`. */
  name: string;
  ast: AstNode;
  comments: Comment[];
}

const JsxParser = Parser.extend(jsx());

export function vendorFiles(): SourceFile[] {
  const out: SourceFile[] = [];
  const visit = (dir: string) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) visit(full);
      else if (entry.name.endsWith(".js")) {
        const comments: Comment[] = [];
        const ast = JsxParser.parse(fs.readFileSync(full, "utf8"), {
          ecmaVersion: "latest",
          sourceType: "module",
          locations: true,
          onComment: comments,
        });
        out.push({ name: path.relative(VENDOR, full), ast, comments });
      }
    }
  };
  visit(VENDOR);
  return out;
}

/** Visit every node under `node`, with its parent and the key it sits under. */
export function walk(
  node: AstNode,
  visit: (node: AstNode, parent: AstNode | null, key: string | null) => void,
  parent: AstNode | null = null,
  key: string | null = null,
): void {
  if (!node || typeof node.type !== "string") return;
  visit(node, parent, key);
  for (const [k, value] of Object.entries(node)) {
    if (k === "loc" || k === "start" || k === "end") continue;
    if (Array.isArray(value)) {
      for (const child of value) walk(child, visit, node, k);
    } else if (value && typeof value === "object") {
      walk(value, visit, node, k);
    }
  }
}
