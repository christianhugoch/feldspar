/**
 * Prettier's configuration search, and formatting with what it finds.
 *
 * The search is the part with judgement in it — nearest ancestor wins, a
 * `package.json` without a `prettier` key is not a configuration, a `.js` one
 * cannot be run in a browser — so it is tested directly rather than through the
 * formatter. The last test then checks the thing that matters to an admin: that
 * the options found actually change the output.
 */

import { describe, expect, it } from "vitest";

import {
  ancestorDirectories,
  parseLenientJson,
  resolvePrettierConfig,
  toFormatOptions,
  type ConfigFileReader,
} from "./prettierConfig";
import { formatDocument, parserForLanguage } from "./prettierFormat";
import { parentPath, type StoreEntry } from "./storeFiles";

/** A store of text files, as much of [`StoreFiles`] as the search uses. */
function reader(files: Record<string, string>): ConfigFileReader {
  const directories = new Set<string>();
  for (const path of Object.keys(files)) {
    for (let dir = parentPath(path); dir !== ""; dir = parentPath(dir)) directories.add(dir);
  }
  return {
    async stat(path: string): Promise<StoreEntry | null> {
      const name = path.slice(path.lastIndexOf("/") + 1);
      if (files[path] !== undefined) return { name, kind: "file", size: files[path].length };
      if (directories.has(path)) return { name, kind: "directory", size: 0 };
      return null;
    },
    async read(path: string): Promise<Uint8Array> {
      const contents = files[path];
      if (contents === undefined) throw new Error(`${path} does not exist`);
      return new TextEncoder().encode(contents);
    },
  };
}

describe("the search", () => {
  it("looks in every directory from the file's own up to the store root", () => {
    expect(ancestorDirectories("web/src/App.tsx")).toEqual(["web/src", "web", ""]);
    expect(ancestorDirectories("package.json")).toEqual([""]);
  });

  it("finds nothing in a project that configures nothing", async () => {
    const files = reader({ "package.json": "{}", "src/App.tsx": "" });
    expect(await resolvePrettierConfig(files, "src/App.tsx")).toEqual({
      options: {},
      from: null,
      unreadable: null,
    });
  });

  it("takes the nearest ancestor's configuration", async () => {
    const files = reader({
      ".prettierrc": '{ "semi": false }',
      "web/.prettierrc.json": '{ "semi": true, "singleQuote": true }',
      "web/src/App.tsx": "",
    });
    const found = await resolvePrettierConfig(files, "web/src/App.tsx");
    expect(found.from).toBe("web/.prettierrc.json");
    expect(found.options).toEqual({ semi: true, singleQuote: true });

    // A file above it gets the root's, not the nearer one.
    expect((await resolvePrettierConfig(files, "README.md")).from).toBe(".prettierrc");
  });

  it("reads package.json's prettier key, and keeps searching when it has none", async () => {
    const configured = reader({
      "web/package.json": '{ "name": "web", "prettier": { "printWidth": 120 } }',
      "web/src/App.tsx": "",
    });
    const found = await resolvePrettierConfig(configured, "web/src/App.tsx");
    expect(found.from).toBe("web/package.json");
    expect(found.options).toEqual({ printWidth: 120 });

    // The monorepo case: the package has a package.json, the root has the config.
    const inherited = reader({
      ".prettierrc": '{ "printWidth": 100 }',
      "web/package.json": '{ "name": "web" }',
      "web/src/App.tsx": "",
    });
    expect((await resolvePrettierConfig(inherited, "web/src/App.tsx")).from).toBe(".prettierrc");
  });

  it("prefers package.json's key to a dotfile beside it, as prettier does", async () => {
    const files = reader({
      "package.json": '{ "prettier": { "printWidth": 60 } }',
      ".prettierrc": '{ "printWidth": 120 }',
      "src/App.tsx": "",
    });
    expect((await resolvePrettierConfig(files, "src/App.tsx")).from).toBe("package.json");
  });

  it("accepts comments and trailing commas, which a hand-written config has", async () => {
    const files = reader({
      ".prettierrc": '{\n  // no semicolons here\n  "semi": false,\n}\n',
      "src/App.tsx": "",
    });
    expect((await resolvePrettierConfig(files, "src/App.tsx")).options).toEqual({ semi: false });
    expect(parseLenientJson('{ "a": "// not a comment" }')).toEqual({ a: "// not a comment" });
  });

  it("says why a configuration it cannot read is being ignored", async () => {
    const js = reader({ "prettier.config.js": "export default { semi: false };", "a.ts": "" });
    const found = await resolvePrettierConfig(js, "a.ts");
    expect(found.from).toBeNull();
    expect(found.unreadable?.path).toBe("prettier.config.js");
    expect(found.unreadable?.reason).toMatch(/JavaScript/);

    const yaml = reader({ ".prettierrc.yaml": "semi: false\n", "a.ts": "" });
    expect((await resolvePrettierConfig(yaml, "a.ts")).unreadable?.path).toBe(".prettierrc.yaml");

    // And it stops there: a config that exists is the project's answer, so the
    // one further up must not be used behind the admin's back.
    const both = reader({
      ".prettierrc": '{ "semi": true }',
      "web/.prettierrc.yaml": "semi: false\n",
      "web/a.ts": "",
    });
    const shadowed = await resolvePrettierConfig(both, "web/a.ts");
    expect(shadowed.from).toBeNull();
    expect(shadowed.unreadable?.path).toBe("web/.prettierrc.yaml");
  });

  it("drops the options prettier's browser half cannot honour", () => {
    expect(
      toFormatOptions({
        $schema: "https://json.schemastore.org/prettierrc",
        semi: false,
        plugins: ["prettier-plugin-tailwindcss"],
        overrides: [{ files: "*.md", options: { proseWrap: "always" } }],
      }),
    ).toEqual({ semi: false });
  });
});

describe("formatting", () => {
  it("knows which parser a React project's languages need", () => {
    expect(parserForLanguage("typescriptreact")).toBe("typescript");
    expect(parserForLanguage("javascript")).toBe("babel");
    expect(parserForLanguage("css")).toBe("css");
    expect(parserForLanguage("rust")).toBeNull();
  });

  it("formats with the project's own options rather than prettier's defaults", async () => {
    const source = "export const greeting = 'hello'\n";
    const files = reader({
      ".prettierrc": '{ "semi": false, "singleQuote": true }',
      "src/App.tsx": source,
    });

    const configured = await formatDocument(files, "src/App.tsx", "typescriptreact", source);
    expect(configured?.configuredBy).toBe(".prettierrc");
    // Prettier's defaults would have made this `"hello";`.
    expect(configured?.text).toBe("export const greeting = 'hello'\n");

    const unconfigured = await formatDocument(reader({}), "src/App.tsx", "typescriptreact", source);
    expect(unconfigured?.configuredBy).toBeNull();
    expect(unconfigured?.text).toBe('export const greeting = "hello";\n');
  });

  it("falls back to the editor's indentation when the project says nothing", async () => {
    const source = "function f() {\nreturn 1;\n}\n";
    const formatted = await formatDocument(reader({}), "a.ts", "typescript", source, {
      tabWidth: 4,
      useTabs: false,
    });
    expect(formatted?.text).toBe("function f() {\n    return 1;\n}\n");
  });

  it("leaves a language it has no parser for alone", async () => {
    expect(await formatDocument(reader({}), "main.rs", "rust", "fn main(){}")).toBeNull();
  });

  it("throws prettier's own error for a file that does not parse", async () => {
    await expect(
      formatDocument(reader({}), "a.ts", "typescript", "const x = {\n"),
    ).rejects.toThrow();
  });
});
