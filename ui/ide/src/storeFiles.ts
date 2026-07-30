/**
 * A file store as a filesystem, in the store's own vocabulary.
 *
 * This is the half of the IDE's filesystem that has nothing to do with VS Code:
 * store-relative paths, bytes, and the four outcomes a file operation can have.
 * The VS Code adapter is `fileSystemProvider.ts`, which is a translation layer
 * over this and nothing more.
 *
 * Splitting it this way is what makes the interesting part testable — path
 * mapping, base64, and which HTTP status means what — without booting a
 * workbench to do it.
 */

import { errorMessage, errorStatus } from "./api";
import type { ApiClient } from "./client";

/** What a store entry is. The store knows only these two. */
export type EntryKind = "file" | "directory";

/** One entry in a directory listing. */
export interface StoreEntry {
  readonly name: string;
  readonly kind: EntryKind;
  /** Bytes, for a file; `0` for a directory, which the store does not size. */
  readonly size: number;
}

/**
 * Why a file operation failed, in the terms a filesystem has.
 *
 * `notFound` and `exists` are ordinary control flow for an editor — asking
 * whether a path is there is how it decides to create or to open — so they are
 * *values*, not surprises. `noPermission` is the access rule of §9 refusing.
 * `failed` is everything else, and carries the server's own message.
 */
export type StoreFileErrorKind = "notFound" | "exists" | "noPermission" | "failed";

/** An error from a store operation, tagged with which of the four it is. */
export class StoreFileError extends Error {
  constructor(
    readonly kind: StoreFileErrorKind,
    message: string,
  ) {
    super(message);
    this.name = "StoreFileError";
  }
}

/**
 * The store-relative path a workspace URI's path names.
 *
 * The workspace folder is `/<store>` (see `workspace.ts`), so `/my-app/src/x.ts`
 * in the store `my-app` is `src/x.ts`, and the folder itself is `""` — which is
 * what the API means by the store root. A path outside the folder is a
 * programming error, not a user error: the provider is only ever asked about
 * URIs inside the workspace it registered.
 */
export function toStorePath(store: string, uriPath: string): string {
  const root = `/${store}`;
  if (uriPath === root || uriPath === `${root}/`) return "";
  if (!uriPath.startsWith(`${root}/`)) {
    throw new StoreFileError("failed", `${uriPath} is not in the file store ${store}`);
  }
  return uriPath.slice(root.length + 1).replace(/\/+$/, "");
}

/** The workspace URI path a store-relative path has. The inverse of the above. */
export function toUriPath(store: string, storePath: string): string {
  return storePath === "" ? `/${store}` : `/${store}/${storePath}`;
}

/** Decode base64 the API returned into the bytes it stands for. */
export function decodeBase64(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

/**
 * Encode bytes as the base64 the API takes.
 *
 * In chunks because `String.fromCharCode(...bytes)` is a spread whose argument
 * count is the file's length: it throws on anything of size, which a source tree
 * contains.
 */
export function encodeBase64(bytes: Uint8Array): string {
  const chunk = 0x8000;
  let binary = "";
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

/** Everything above the last `/`, or `""` for a path at the store root. */
export function parentPath(storePath: string): string {
  const cut = storePath.lastIndexOf("/");
  return cut === -1 ? "" : storePath.slice(0, cut);
}

/** The last segment of a path. */
export function baseName(storePath: string): string {
  const cut = storePath.lastIndexOf("/");
  return cut === -1 ? storePath : storePath.slice(cut + 1);
}

/** The file operations of one store, on top of the generated client. */
export class StoreFiles {
  constructor(
    readonly store: string,
    private readonly client: ApiClient,
  ) {}

  /** The entries of a directory. */
  async list(dir: string): Promise<StoreEntry[]> {
    const entries = await this.call("listing", () =>
      this.client.browseFiles(this.store, { dir }),
    );
    return entries.map((entry) => ({
      name: entry.name,
      kind: entry.is_dir ? "directory" : "file",
      size: entry.size ?? 0,
    }));
  }

  /**
   * What a path is, or `null` when nothing is there.
   *
   * There is no `stat` endpoint, so this reads the parent's listing and looks for
   * the name — which is also why a path whose *parent* is missing is `null` too
   * rather than an error: neither exists, and the caller asked whether the path
   * is there.
   */
  async stat(storePath: string): Promise<StoreEntry | null> {
    if (storePath === "") {
      return { name: this.store, kind: "directory", size: 0 };
    }
    let siblings: StoreEntry[];
    try {
      siblings = await this.list(parentPath(storePath));
    } catch (err) {
      if (err instanceof StoreFileError && err.kind === "notFound") return null;
      throw err;
    }
    return siblings.find((entry) => entry.name === baseName(storePath)) ?? null;
  }

  /** A file's bytes. */
  async read(storePath: string): Promise<Uint8Array> {
    const content = await this.call("reading", () =>
      this.client.readFile(this.store, { path: storePath }),
    );
    return decodeBase64(content.base64);
  }

  /** Write a file, creating it and any missing parent directories. */
  async write(storePath: string, bytes: Uint8Array): Promise<void> {
    await this.call("writing", () =>
      this.client.writeFile(this.store, {
        path: storePath,
        base64: encodeBase64(bytes),
      }),
    );
  }

  /** Create a directory. Succeeds if it is already there. */
  async makeDirectory(storePath: string): Promise<void> {
    await this.call("creating", () =>
      this.client.makeDirectory(this.store, { path: storePath }),
    );
  }

  /** Delete a file, or a directory and everything in it. */
  async remove(storePath: string): Promise<void> {
    await this.call("deleting", () =>
      this.client.deleteFile(this.store, { path: storePath }),
    );
  }

  /**
   * Move a path. The store never overwrites, so an occupied destination is
   * cleared first when the caller asked for that and refused otherwise.
   */
  async move(from: string, to: string, overwrite: boolean): Promise<void> {
    const destination = await this.stat(to);
    if (destination !== null) {
      if (!overwrite) {
        throw new StoreFileError("exists", `${to} already exists in ${this.store}`);
      }
      await this.remove(to);
    }
    await this.call("moving", () => this.client.renameFile(this.store, { from, to }));
  }

  /** Run one client call, turning its failure into a [`StoreFileError`]. */
  private async call<T>(doing: string, run: () => Promise<T>): Promise<T> {
    try {
      return await run();
    } catch (err) {
      if (err instanceof StoreFileError) throw err;
      throw new StoreFileError(
        kindOfStatus(errorStatus(err)),
        errorMessage(err, `${doing} failed in file store ${this.store}`),
      );
    }
  }
}

/**
 * Which failure an HTTP status is (§16: the error kinds, as the API reports
 * them). A 400 is `exists` because the one 400 a filesystem call provokes is the
 * store refusing to clobber a destination — every other bad request here would be
 * a bug in this client rather than something the admin did.
 */
export function kindOfStatus(status: number | null): StoreFileErrorKind {
  switch (status) {
    case 404:
      return "notFound";
    case 400:
      return "exists";
    case 401:
    case 403:
      return "noPermission";
    default:
      return "failed";
  }
}
