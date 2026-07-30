/**
 * Formatting, with prettier, in the browser (design §12.1).
 *
 * `prettier/standalone` and its plugins are bundled into this page rather than
 * taken from the project's `node_modules`: a store need not have any, need not
 * even have a local path, and formatting must keep working on all of them. The
 * project's *configuration* is still the project's — [`resolvePrettierConfig`]
 * reads it through the file store — so what changes between projects is the
 * options, not the formatter.
 *
 * This module has no VS Code in it; the editor provider that calls it is
 * `extension.ts`.
 */

import { format } from "prettier/standalone";
import type { Options as PrettierOptions } from "prettier";
import * as babelPlugin from "prettier/plugins/babel";
import * as estreePlugin from "prettier/plugins/estree";
import * as htmlPlugin from "prettier/plugins/html";
import * as markdownPlugin from "prettier/plugins/markdown";
import * as postcssPlugin from "prettier/plugins/postcss";
import * as typescriptPlugin from "prettier/plugins/typescript";

import { resolvePrettierConfig, type ConfigFileReader } from "./prettierConfig";

/**
 * The parsers, by VS Code language id — which is also the list of languages the
 * formatter is registered for.
 *
 * It is what a React project contains, and no more: the plugins are bundled into
 * the page, so every language here has a size in the download. Anything else
 * keeps VS Code's own (absent) formatter rather than being formatted badly.
 */
export const PARSER_BY_LANGUAGE: Readonly<Record<string, string>> = {
  typescript: "typescript",
  typescriptreact: "typescript",
  javascript: "babel",
  javascriptreact: "babel",
  json: "json",
  jsonc: "jsonc",
  css: "css",
  scss: "scss",
  less: "less",
  html: "html",
  markdown: "markdown",
};

/** The plugins those parsers live in. `estree` is what prints JavaScript ASTs. */
const PLUGINS = [
  estreePlugin,
  babelPlugin,
  typescriptPlugin,
  postcssPlugin,
  htmlPlugin,
  markdownPlugin,
];

/** The prettier parser for a VS Code language id, or `null` if it has none. */
export function parserForLanguage(languageId: string): string | null {
  return PARSER_BY_LANGUAGE[languageId] ?? null;
}

/** What formatting a document produced. */
export interface FormatResult {
  /** The formatted text. */
  readonly text: string;
  /** Store path of the configuration used, or `null` when none was found. */
  readonly configuredBy: string | null;
  /** A configuration that was found and could not be read; see §12.1. */
  readonly unreadableConfig: { readonly path: string; readonly reason: string } | null;
}

/**
 * Format one document's text.
 *
 * `defaults` is what the editor would have done — its tab size and whether it
 * indents with tabs — and is used only where the project says nothing, so a
 * project with a `.prettierrc` is formatted exactly as `npx prettier` would
 * format it and one without still follows the editor's settings.
 *
 * Throws prettier's own error when the document does not parse. That is the right
 * outcome: the caller shows it, because a format command that silently does
 * nothing to a file with a syntax error is a bug report.
 */
export async function formatDocument(
  files: ConfigFileReader,
  storePath: string,
  languageId: string,
  text: string,
  defaults: PrettierOptions = {},
): Promise<FormatResult | null> {
  const parser = parserForLanguage(languageId);
  if (parser === null) return null;
  const config = await resolvePrettierConfig(files, storePath);
  const formatted = await format(text, {
    ...defaults,
    ...config.options,
    parser,
    plugins: PLUGINS,
    // Prettier uses this only to name the file in errors; the parser is explicit
    // because standalone cannot infer one from a path without the node half.
    filepath: storePath,
  });
  return {
    text: formatted,
    configuredBy: config.from,
    unreadableConfig: config.unreadable,
  };
}
