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

/** Serve the workspace folder from the file store's own API (design §12.1). */
export function registerStoreFilesystem(store: string): void {
  registerStoreFileSystem(new StoreFiles(store, api));
}
