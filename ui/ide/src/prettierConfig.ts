/**
 * The project's own prettier configuration, read through the file store.
 *
 * Prettier in the browser (design §12.1) is `prettier/standalone` rather than the
 * project's installed copy — a store need not have `node_modules`, and formatting
 * must not be the capability that stops working on an object store. The price of
 * that choice is that nothing resolves the project's `.prettierrc` for us:
 * `resolveConfig` is part of prettier's *node* half. So this module does prettier's
 * own search, over the store's files instead of a filesystem.
 *
 * The search is prettier's: from the directory holding the file being formatted
 * upwards to the workspace root, the first configuration found wins, and within a
 * directory `package.json`'s `prettier` key is consulted before the dotfiles. A
 * `package.json` *without* that key does not end the search — it is not a prettier
 * configuration.
 *
 * There is no VS Code in here, which is what makes it testable: the reader is an
 * interface [`StoreFiles`](./storeFiles.ts) happens to satisfy.
 */

import type { Options as PrettierOptions } from "prettier";

import { parentPath, type StoreEntry } from "./storeFiles";

/** The reading half of [`StoreFiles`], which is all the search needs. */
export interface ConfigFileReader {
  stat(storePath: string): Promise<StoreEntry | null>;
  read(storePath: string): Promise<Uint8Array>;
}

/** A configuration that was found, and where it came from. */
export interface ResolvedPrettierConfig {
  /** What to pass prettier. Empty when the project configures nothing. */
  readonly options: PrettierOptions;
  /** Store path of the file the options came from, or `null` if none was found. */
  readonly from: string | null;
  /**
   * Set when a configuration file *was* found and could not be used: a JavaScript
   * or YAML one, which the browser cannot evaluate. The admin is told rather than
   * silently formatted against prettier's defaults.
   */
  readonly unreadable: { readonly path: string; readonly reason: string } | null;
}

/** Nothing configured, and nothing wrong. */
const NO_CONFIG: ResolvedPrettierConfig = { options: {}, from: null, unreadable: null };

/**
 * Configuration file names, in the order prettier consults them within one
 * directory. `package.json` is first because prettier's own search is.
 */
export const CONFIG_FILE_NAMES = [
  "package.json",
  ".prettierrc",
  ".prettierrc.json",
  ".prettierrc.jsonc",
  ".prettierrc.json5",
  ".prettierrc.yaml",
  ".prettierrc.yml",
  ".prettierrc.toml",
  ".prettierrc.js",
  ".prettierrc.mjs",
  ".prettierrc.cjs",
  ".prettierrc.ts",
  ".prettierrc.mts",
  ".prettierrc.cts",
  "prettier.config.js",
  "prettier.config.mjs",
  "prettier.config.cjs",
  "prettier.config.ts",
  "prettier.config.mts",
  "prettier.config.cts",
] as const;

/**
 * Names whose contents are code or YAML rather than JSON.
 *
 * A `.js` configuration is a module to execute and a `.yaml`/`.toml` one needs a
 * parser this bundle does not carry. Both are *found* — they end the search,
 * because they are the project's configuration — and both are reported as
 * unreadable rather than skipped, so an admin whose settings appear to be ignored
 * is told why.
 */
function unreadableReason(name: string): string | null {
  if (/\.(js|mjs|cjs|ts|mts|cts)$/.test(name)) {
    return "it is a JavaScript module, which the browser-side formatter cannot execute";
  }
  if (/\.(yaml|yml)$/.test(name)) return "it is YAML, which this editor cannot parse";
  if (/\.toml$/.test(name)) return "it is TOML, which this editor cannot parse";
  return null;
}

/**
 * Options prettier's *node* half handles and `format` must not be handed.
 *
 * `plugins` names modules to load from `node_modules`; `overrides` is applied by
 * the config resolution we are standing in for, not by `format`; `$schema` is an
 * editor's hint. Passing any of them through would at best be ignored and at
 * worst throw.
 */
const NOT_FORMAT_OPTIONS = ["plugins", "overrides", "$schema"];

/** A configuration object as prettier options, minus what `format` cannot take. */
export function toFormatOptions(config: Record<string, unknown>): PrettierOptions {
  const options: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(config)) {
    if (!NOT_FORMAT_OPTIONS.includes(key)) options[key] = value;
  }
  return options as PrettierOptions;
}

/**
 * Parse JSON as a configuration file may be written: with comments, and with
 * trailing commas.
 *
 * `.prettierrc` is documented as JSON *or* YAML, and `.prettierrc.jsonc` and
 * `.prettierrc.json5` exist, so strict `JSON.parse` would reject files that are
 * perfectly ordinary. Comments and trailing commas cover what is actually written;
 * anything further (unquoted keys, single quotes) is JSON5 proper and is reported
 * as unreadable rather than half-parsed.
 */
export function parseLenientJson(text: string): unknown {
  return JSON.parse(stripJsonComments(text).replace(/,(\s*[}\]])/g, "$1"));
}

/** Drop line and block comments, leaving anything inside a string alone. */
function stripJsonComments(text: string): string {
  let out = "";
  let index = 0;
  while (index < text.length) {
    const char = text[index];
    if (char === '"') {
      const start = index;
      index += 1;
      while (index < text.length && text[index] !== '"') {
        index += text[index] === "\\" ? 2 : 1;
      }
      index += 1;
      out += text.slice(start, index);
    } else if (char === "/" && text[index + 1] === "/") {
      while (index < text.length && text[index] !== "\n") index += 1;
    } else if (char === "/" && text[index + 1] === "*") {
      const end = text.indexOf("*/", index + 2);
      index = end === -1 ? text.length : end + 2;
    } else {
      out += char;
      index += 1;
    }
  }
  return out;
}

/** Every directory from the one holding `storePath` up to the store root. */
export function ancestorDirectories(storePath: string): string[] {
  const dirs: string[] = [];
  let dir = parentPath(storePath);
  for (;;) {
    dirs.push(dir);
    if (dir === "") return dirs;
    dir = parentPath(dir);
  }
}

/** Join a directory and a name, at the store root or below it. */
function join(dir: string, name: string): string {
  return dir === "" ? name : `${dir}/${name}`;
}

/**
 * The prettier configuration governing `storePath`, searched as prettier searches.
 *
 * Never throws: a store that cannot be read is a formatter without the project's
 * settings, which is worse than having them and much better than a format command
 * that fails.
 */
export async function resolvePrettierConfig(
  files: ConfigFileReader,
  storePath: string,
): Promise<ResolvedPrettierConfig> {
  for (const dir of ancestorDirectories(storePath)) {
    for (const name of CONFIG_FILE_NAMES) {
      const path = join(dir, name);
      let entry: StoreEntry | null;
      try {
        entry = await files.stat(path);
      } catch {
        return NO_CONFIG;
      }
      if (entry === null || entry.kind !== "file") continue;

      const reason = unreadableReason(name);
      if (reason !== null) return { options: {}, from: null, unreadable: { path, reason } };

      const found = await readConfigFile(files, path, name === "package.json");
      // `package.json` with no `prettier` key is not a configuration at all, so
      // the search carries on above it — the case of a monorepo package that
      // inherits the root's settings.
      if (found !== null) return found;
    }
  }
  return NO_CONFIG;
}

/** One candidate file, or `null` when it turns out not to configure prettier. */
async function readConfigFile(
  files: ConfigFileReader,
  path: string,
  isPackageJson: boolean,
): Promise<ResolvedPrettierConfig | null> {
  let parsed: unknown;
  try {
    parsed = parseLenientJson(new TextDecoder().decode(await files.read(path)));
  } catch (err) {
    if (isPackageJson) return null;
    // `.prettierrc` with no extension is documented as JSON *or* YAML, so this is
    // the most likely way to arrive here.
    const detail = err instanceof Error ? err.message : String(err);
    return {
      options: {},
      from: null,
      unreadable: { path, reason: `it is not JSON (${detail}); YAML cannot be read here` },
    };
  }

  const config = isPackageJson ? prettierKeyOf(parsed) : parsed;
  if (config === null) return null;
  if (typeof config !== "object" || Array.isArray(config)) {
    return {
      options: {},
      from: null,
      unreadable: { path, reason: "its prettier configuration is not an object" },
    };
  }
  return {
    options: toFormatOptions(config as Record<string, unknown>),
    from: path,
    unreadable: null,
  };
}

/**
 * `package.json`'s `prettier` key, or `null` when it has none.
 *
 * A *string* there names another file to read, which prettier supports and this
 * does not: it is rare, and following it would mean resolving a module specifier.
 * It is reported by returning an empty configuration from that file, so the
 * caller says "configured here" rather than searching on and using the wrong one.
 */
function prettierKeyOf(packageJson: unknown): unknown {
  if (typeof packageJson !== "object" || packageJson === null) return null;
  const value = (packageJson as Record<string, unknown>).prettier;
  if (value === undefined) return null;
  return typeof value === "string" ? {} : value;
}
