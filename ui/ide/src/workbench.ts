import * as vscode from "vscode";
import "vscode/localExtensionHost";
import {
  initialize as initializeVscodeApi,
  LogLevel,
  type IEditorOverrideServices,
  type IWorkbenchConstructionOptions,
} from "@codingame/monaco-vscode-api";
import type { EnvironmentOverride } from "@codingame/monaco-vscode-api/workbench";
import getWorkbenchServiceOverride from "@codingame/monaco-vscode-workbench-service-override";
import getConfigurationServiceOverride, {
  initUserConfiguration,
} from "@codingame/monaco-vscode-configuration-service-override";
import getKeybindingsServiceOverride, {
  initUserKeybindings,
} from "@codingame/monaco-vscode-keybindings-service-override";
import getDialogsServiceOverride from "@codingame/monaco-vscode-dialogs-service-override";
import getExplorerServiceOverride from "@codingame/monaco-vscode-explorer-service-override";
import getExtensionServiceOverride from "@codingame/monaco-vscode-extensions-service-override";
import getLanguagesServiceOverride from "@codingame/monaco-vscode-languages-service-override";
import getLifecycleServiceOverride from "@codingame/monaco-vscode-lifecycle-service-override";
import getMarkersServiceOverride from "@codingame/monaco-vscode-markers-service-override";
import getModelServiceOverride from "@codingame/monaco-vscode-model-service-override";
import getNotificationServiceOverride from "@codingame/monaco-vscode-notifications-service-override";
import getOutputServiceOverride from "@codingame/monaco-vscode-output-service-override";
import getPreferencesServiceOverride from "@codingame/monaco-vscode-preferences-service-override";
import getQuickAccessServiceOverride from "@codingame/monaco-vscode-quickaccess-service-override";
import getScmServiceOverride from "@codingame/monaco-vscode-scm-service-override";
import getSearchServiceOverride from "@codingame/monaco-vscode-search-service-override";
import getSecretStorageServiceOverride from "@codingame/monaco-vscode-secret-storage-service-override";
import getStatusBarServiceOverride from "@codingame/monaco-vscode-view-status-bar-service-override";
import getStorageServiceOverride from "@codingame/monaco-vscode-storage-service-override";
import getTextmateServiceOverride from "@codingame/monaco-vscode-textmate-service-override";
import getThemeServiceOverride from "@codingame/monaco-vscode-theme-service-override";
import getTitleBarServiceOverride from "@codingame/monaco-vscode-view-title-bar-service-override";
import getWorkingCopyServiceOverride from "@codingame/monaco-vscode-working-copy-service-override";

// The grammars and the theme, as VS Code's own built-in extensions. These are
// *declarative* extensions — TextMate grammars and language configuration — so
// they are what makes a React project's files look like code. Semantics (a
// language server) is phase 4; nothing here runs tsserver.
import "@codingame/monaco-vscode-theme-defaults-default-extension";
import "@codingame/monaco-vscode-typescript-basics-default-extension";
import "@codingame/monaco-vscode-javascript-default-extension";
import "@codingame/monaco-vscode-json-default-extension";
import "@codingame/monaco-vscode-css-default-extension";
import "@codingame/monaco-vscode-html-default-extension";
import "@codingame/monaco-vscode-markdown-basics-default-extension";

import defaultConfiguration from "./user/configuration.json?raw";
import defaultKeybindings from "./user/keybindings.json?raw";
import { api } from "./api";
import { activateSaltcornExtension, declareSaltcornExtension } from "./extension";
import { storeGit, type FileStoreSummary } from "./git";
import { configureWorkers } from "./workers";
import { registerStoreSearch } from "./searchProvider";
import { registerStoreFilesystem, storeFolderUri } from "./workspace";

/**
 * The services this IDE runs on.
 *
 * `getWorkbenchServiceOverride` is the whole point (design §12.1): it renders VS
 * Code's real workbench — activity bar, explorer tree, editor tabs, panels, status
 * bar — rather than an editor we have to surround with our own furniture. The rest
 * are the services that workbench then expects to find, and the list is
 * deliberately shorter than upstream's demo: no debug, no notebooks, no chat, no
 * terminal (a shell on the server is a decision of its own), no extension gallery
 * (installing extensions is out of scope for this milestone).
 */
const services: IEditorOverrideServices = {
  ...getWorkbenchServiceOverride(),
  ...getExplorerServiceOverride(),
  ...getTitleBarServiceOverride(),
  ...getStatusBarServiceOverride(),
  ...getQuickAccessServiceOverride({
    isKeybindingConfigurationVisible: () => true,
    shouldUseGlobalPicker: () => true,
  }),
  ...getSearchServiceOverride(),
  ...getMarkersServiceOverride(),
  ...getOutputServiceOverride(),
  ...getPreferencesServiceOverride(),
  ...getConfigurationServiceOverride(),
  ...getKeybindingsServiceOverride(),
  ...getStorageServiceOverride(),
  ...getSecretStorageServiceOverride(),
  ...getModelServiceOverride(),
  ...getLanguagesServiceOverride(),
  ...getTextmateServiceOverride(),
  ...getThemeServiceOverride(),
  ...getNotificationServiceOverride(),
  ...getDialogsServiceOverride(),
  ...getLifecycleServiceOverride(),
  ...getWorkingCopyServiceOverride(),
  ...getExtensionServiceOverride({ enableWorkerExtensionHost: true }),
  // Source control. What this adds is the ability to *host* a provider: the
  // Source Control viewlet and its icon are the workbench's own and are there
  // either way — leaving this out was tried, and all it changes is that the view
  // cannot work. Whether there is a provider to show is `sourceControl.ts`'s
  // question, and for a store that is not a git working copy the answer is no,
  // which VS Code renders as its own "no source control providers" empty state.
  ...getScmServiceOverride(),
};

/**
 * Otherwise VS Code takes the first workspace folder for the user's home
 * directory, which makes find-in-files report paths relative to the wrong root.
 */
const environment: EnvironmentOverride = {
  userHome: vscode.Uri.file("/"),
};

function constructionOptions(store: string): IWorkbenchConstructionOptions {
  return {
    workspaceProvider: {
      trusted: true,
      workspace: { folderUri: storeFolderUri(store) },
      // A page edits exactly one store: VS Code cannot be re-initialized without
      // a page load (§12.1), so "open another workspace" is refused rather than
      // half-implemented. Switching stores is a navigation.
      async open() {
        return false;
      },
    },
    developmentOptions: { logLevel: LogLevel.Info },
    windowIndicator: {
      label: `$(database) ${store}`,
      tooltip: `Saltcorn file store: ${store}`,
      command: "",
    },
    configurationDefaults: {
      "window.title": `${store}\${separator}\${dirty}\${activeEditorShort}`,
    },
    productConfiguration: {
      nameShort: "Saltcorn",
      nameLong: "Saltcorn IDE",
    },
  };
}

/**
 * Boot the workbench for `summary`'s store into `container`.
 *
 * The whole store record rather than its name: source control is addressed by
 * the store's **id** and offered only for a git working copy, and both facts are
 * in the listing the page has already read to decide it can open at all.
 */
export async function bootWorkbench(
  summary: FileStoreSummary,
  container: HTMLElement,
): Promise<void> {
  const store = summary.name;
  configureWorkers();
  // Before `initialize`, so the theme is right on the first frame rather than
  // after a flash of the default one.
  await Promise.all([
    initUserConfiguration(defaultConfiguration),
    initUserKeybindings(defaultKeybindings),
  ]);
  const { files, provider } = registerStoreFilesystem(store);
  // Declared before `initialize` so it is one of the workbench's built-in
  // extensions, and activated after it so the API it hands out has services to
  // talk to. Both halves matter; `extension.ts` says what goes wrong otherwise.
  const extension = declareSaltcornExtension();
  const git = storeGit(summary, api);
  await initializeVscodeApi(services, container, constructionOptions(store), environment);
  // After `initialize`, because it registers with a service that must exist by
  // then: find-in-files stops walking the tree a directory at a time and asks
  // the store instead (§12.1).
  await registerStoreSearch(store, api);
  await activateSaltcornExtension(extension, files, provider, git);
}
