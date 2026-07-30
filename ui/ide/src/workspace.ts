import * as monaco from "monaco-editor";

import { api } from "./api";
import { registerStoreFileSystem } from "./fileSystemProvider";
import { StoreFiles } from "./storeFiles";

/**
 * The workspace folder a store is edited as.
 *
 * One store, one folder, named after the store — so the workbench's own storage
 * (which is keyed by workspace) keeps each store's open tabs and layout apart,
 * and so a path in the explorer reads the way it does in the file manager.
 */
export function storeFolderUri(store: string): monaco.Uri {
  return monaco.Uri.file(`/${store}`);
}

/**
 * Serve the workspace folder from the file store's own API (design §12.1),
 * returning the store's files — the Build command needs the same instance, so
 * that what it writes into the source tree is dropped from the same cache the
 * explorer reads.
 */
export function registerStoreFilesystem(store: string): StoreFiles {
  const files = new StoreFiles(store, api);
  registerStoreFileSystem(files);
  // There is no watcher over an HTTP file API, so the IDE cannot be told when the
  // store changes under it. Regaining focus is the cheapest honest approximation:
  // it is the moment an admin is most likely to have just done something in
  // another tab — pulled the git store, edited in the file manager, run a build —
  // and it costs one listing per directory they then look at.
  window.addEventListener("focus", () => files.forgetEverything());
  return files;
}
