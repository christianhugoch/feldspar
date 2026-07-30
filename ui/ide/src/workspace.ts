import * as monaco from "monaco-editor";
import {
  RegisteredFileSystemProvider,
  RegisteredMemoryFile,
  registerFileSystemOverlay,
} from "@codingame/monaco-vscode-files-service-override";

/**
 * The workspace folder a store is edited as.
 *
 * One store, one folder, named after the store — so the workbench's own storage
 * (which is keyed by workspace) keeps each store's open tabs and layout apart.
 */
export function storeFolderUri(store: string): monaco.Uri {
  return monaco.Uri.file(`/${store}`);
}

/**
 * Register the workspace folder's filesystem.
 *
 * **Phase 1 only.** This is an in-memory provider holding a single file, which is
 * enough to prove the workbench boots and renders a project tree. Phase 2 replaces
 * it with a provider over the file-store API (`browseFiles`/`readFile`/`writeFile`
 * …), at which point the folder is the real store and this function goes away.
 */
export function registerPlaceholderFilesystem(store: string): void {
  const provider = new RegisteredFileSystemProvider(false);
  provider.registerFile(
    new RegisteredMemoryFile(
      monaco.Uri.joinPath(storeFolderUri(store), "WORKBENCH.md"),
      [
        `# ${store}`,
        "",
        "The workbench is running, and this folder is a placeholder: the file store's",
        "own files arrive with the filesystem provider (TODO phase 2). Until then this",
        "is an in-memory file — editing it changes nothing in the store.",
        "",
    ].join("\n"),
    ),
  );
  registerFileSystemOverlay(1, provider);
}
