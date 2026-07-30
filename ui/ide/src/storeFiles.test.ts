/**
 * The store filesystem's own logic, against a stubbed client.
 *
 * What is worth testing here is exactly what has no VS Code in it: which path a
 * URI names, which bytes a base64 string is, and which of the four failures an
 * HTTP status means — the last being the one that decides whether the admin sees
 * "new file" or a red error box.
 */

import { describe, expect, it } from "vitest";

import type { ApiClient } from "./client";
import {
  StoreFileError,
  StoreFiles,
  decodeBase64,
  encodeBase64,
  kindOfStatus,
  parentPath,
  toStorePath,
  toUriPath,
} from "./storeFiles";

/** A store held in a map, standing in for the server's file endpoints. */
function stubClient(files: Record<string, string>): ApiClient {
  const failure = (name: string, status: number, message: string) =>
    new Error(`${name} failed: ${status}: ${message}`);
  const directories = () => {
    const dirs = new Set<string>();
    for (const path of Object.keys(files)) {
      const parts = path.split("/");
      for (let i = 1; i < parts.length; i += 1) dirs.add(parts.slice(0, i).join("/"));
    }
    return dirs;
  };
  const client = {
    async browseFiles(_store: string, body: { dir: string }) {
      const dir = body.dir.replace(/\/+$/, "");
      if (dir !== "" && !directories().has(dir)) {
        throw failure("browseFiles", 404, `"${dir}" does not exist`);
      }
      const prefix = dir === "" ? "" : `${dir}/`;
      const seen = new Map<string, { name: string; path: string; is_dir: boolean; size: number }>();
      for (const [path, contents] of Object.entries(files)) {
        if (!path.startsWith(prefix)) continue;
        const rest = path.slice(prefix.length);
        const cut = rest.indexOf("/");
        const name = cut === -1 ? rest : rest.slice(0, cut);
        seen.set(name, {
          name,
          path: `${prefix}${name}`,
          is_dir: cut !== -1,
          size: cut === -1 ? contents.length : 0,
        });
      }
      return [...seen.values()].sort((a, b) => a.name.localeCompare(b.name));
    },
    async readFile(_store: string, body: { path: string }) {
      const contents = files[body.path];
      if (contents === undefined) {
        throw failure("readFile", 404, `"${body.path}" does not exist`);
      }
      return { path: body.path, size: contents.length, base64: btoa(contents), text: contents };
    },
    async writeFile(_store: string, body: { path: string; base64?: string | null }) {
      files[body.path] = atob(body.base64 ?? "");
      return { name: body.path, path: body.path, is_dir: false, size: files[body.path].length };
    },
    async makeDirectory(_store: string, body: { path: string }) {
      files[`${body.path}/.keep`] = "";
      return { name: body.path, path: body.path, is_dir: true, size: 0 };
    },
    async deleteFile(_store: string, body: { path: string }) {
      let deleted = false;
      for (const path of Object.keys(files)) {
        if (path === body.path || path.startsWith(`${body.path}/`)) {
          delete files[path];
          deleted = true;
        }
      }
      return { deleted };
    },
    async renameFile(_store: string, body: { from: string; to: string }) {
      if (files[body.to] !== undefined) {
        throw failure("renameFile", 400, "the destination already exists");
      }
      const contents = files[body.from];
      if (contents === undefined) throw failure("renameFile", 404, "does not exist");
      delete files[body.from];
      files[body.to] = contents;
      return { name: body.to, path: body.to, is_dir: false, size: contents.length };
    },
  };
  return client as unknown as ApiClient;
}

describe("paths", () => {
  it("maps a workspace URI path to a store path and back", () => {
    expect(toStorePath("app", "/app")).toBe("");
    expect(toStorePath("app", "/app/")).toBe("");
    expect(toStorePath("app", "/app/src/App.tsx")).toBe("src/App.tsx");
    expect(toUriPath("app", "")).toBe("/app");
    expect(toUriPath("app", "src/App.tsx")).toBe("/app/src/App.tsx");
  });

  it("refuses a path outside the workspace folder", () => {
    expect(() => toStorePath("app", "/other/App.tsx")).toThrow(StoreFileError);
  });

  it("knows a path's parent", () => {
    expect(parentPath("src/App.tsx")).toBe("src");
    expect(parentPath("package.json")).toBe("");
  });
});

describe("base64", () => {
  it("round-trips bytes that are not text", () => {
    const bytes = new Uint8Array([0, 1, 2, 250, 255, 128, 10]);
    expect(decodeBase64(encodeBase64(bytes))).toEqual(bytes);
  });

  it("round-trips a file larger than one spread's worth of arguments", () => {
    const bytes = new Uint8Array(200_000).map((_, i) => i % 256);
    expect(decodeBase64(encodeBase64(bytes))).toEqual(bytes);
  });
});

describe("StoreFiles", () => {
  const store = () =>
    new StoreFiles(
      "app",
      stubClient({
        "package.json": "{}",
        "src/App.tsx": "export const App = () => null;\n",
        "src/main.tsx": "",
      }),
    );

  it("reads back what it wrote", async () => {
    const files = store();
    await files.write("src/App.tsx", new TextEncoder().encode("changed\n"));
    expect(new TextDecoder().decode(await files.read("src/App.tsx"))).toBe("changed\n");
  });

  it("lists a directory as files and directories", async () => {
    const entries = await store().list("");
    expect(entries).toEqual([
      { name: "package.json", kind: "file", size: 2 },
      { name: "src", kind: "directory", size: 0 },
    ]);
  });

  it("stats a path by looking in its parent, and reports a missing one as null", async () => {
    const files = store();
    expect(await files.stat("src/App.tsx")).toEqual({
      name: "App.tsx",
      kind: "file",
      size: 31,
    });
    expect(await files.stat("src")).toEqual({ name: "src", kind: "directory", size: 0 });
    expect(await files.stat("")).toEqual({ name: "app", kind: "directory", size: 0 });
    expect(await files.stat("src/Missing.tsx")).toBeNull();
    // The interesting case: not there *because its parent is not there either*.
    expect(await files.stat("nowhere/Missing.tsx")).toBeNull();
  });

  it("deletes a directory and everything in it", async () => {
    const files = store();
    await files.remove("src");
    expect(await files.list("")).toEqual([{ name: "package.json", kind: "file", size: 2 }]);
  });

  it("moves a file, and refuses to clobber unless asked", async () => {
    const files = store();
    await files.move("src/App.tsx", "src/Renamed.tsx", false);
    expect(await files.stat("src/App.tsx")).toBeNull();
    expect(await files.stat("src/Renamed.tsx")).not.toBeNull();

    await expect(files.move("src/Renamed.tsx", "src/main.tsx", false)).rejects.toMatchObject({
      kind: "exists",
    });
    await files.move("src/Renamed.tsx", "src/main.tsx", true);
    expect(new TextDecoder().decode(await files.read("src/main.tsx"))).toContain("export const App");
  });

  it("reports a missing file as notFound rather than a failure", async () => {
    await expect(store().read("src/Missing.tsx")).rejects.toMatchObject({
      kind: "notFound",
    });
  });
});

describe("kindOfStatus", () => {
  it("maps the statuses a file operation can produce", () => {
    expect(kindOfStatus(404)).toBe("notFound");
    expect(kindOfStatus(400)).toBe("exists");
    expect(kindOfStatus(401)).toBe("noPermission");
    expect(kindOfStatus(403)).toBe("noPermission");
    expect(kindOfStatus(500)).toBe("failed");
    expect(kindOfStatus(null)).toBe("failed");
  });
});
