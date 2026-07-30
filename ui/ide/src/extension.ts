/**
 * Saltcorn's own contributions to the workbench, as an extension that is never
 * packaged (design §12.1, decision 2).
 *
 * The workbench runs in this page's JavaScript context, so `registerExtension`
 * takes a manifest **object** and `setAsDefaultApi` makes the `vscode` import in
 * these modules that extension's API. What would be a `.vsix` elsewhere — a build
 * command, a formatter — is a manifest literal and two ordinary function calls.
 */

import { ExtensionHostKind, registerExtension } from "@codingame/monaco-vscode-api/extensions";
import type {
  IExtensionManifest,
  RegisterLocalProcessExtensionResult,
} from "@codingame/monaco-vscode-api/extensions";

import { BUILD_COMMAND, registerBuildCommand } from "./build";
import { registerPrettierFormatter } from "./formatter";
import { registerLanguageClient } from "./languageClient";
import type { StoreFiles } from "./storeFiles";

/**
 * The manifest. `contributes.commands` is what puts Build in the command palette
 * (with its category, so "Saltcorn: Build Application" finds it); the button in
 * the status bar is created by the command's own registration.
 */
const MANIFEST: IExtensionManifest = {
  name: "saltcorn",
  displayName: "Saltcorn",
  publisher: "saltcorn",
  version: "1.0.0",
  engines: { vscode: "*" },
  // No `main`: the extension's code is this bundle, already loaded. The wildcard
  // event is what makes the contributed command available from the first frame.
  activationEvents: ["*"],
  contributes: {
    commands: [
      {
        command: BUILD_COMMAND,
        title: "Build Application",
        category: "Saltcorn",
        icon: "$(tools)",
      },
    ],
  },
};

/** How long the extension's API may take to arrive before that is news. */
const API_OVERDUE_MS = 15_000;

/**
 * Declare the extension. **Must be called before `initialize`.**
 *
 * That ordering is not a style choice. `registerExtension` behaves differently
 * either side of initialization: before, the extension joins the built-in set the
 * workbench registers as it starts; after, it is a *delta* applied to a running
 * workbench, which takes the extension registry's lock and waits for every
 * extension host to accept it — including the web worker host, which this bundle
 * has no extension for and which does not start. Registering afterwards therefore
 * hangs (`handleDeltaExtensions has been holding on to the lock`), the promise
 * never settles, and everything the extension contributes silently never appears.
 */
export function declareSaltcornExtension(): RegisterLocalProcessExtensionResult {
  return registerExtension(MANIFEST, ExtensionHostKind.LocalProcess);
}

/**
 * Take over the `vscode` API and register what the extension contributes.
 *
 * Called after `initialize`, because the API talks to services that must exist:
 * `setAsDefaultApi` is what makes the `vscode` import in `formatter.ts` and
 * `build.ts` resolve to this extension's own API rather than the anonymous one.
 */
export async function activateSaltcornExtension(
  extension: RegisterLocalProcessExtensionResult,
  files: StoreFiles,
): Promise<void> {
  // The failure above is a *hang*, not a throw: the workbench comes up looking
  // perfectly healthy while none of this ever runs, which is what made it cost an
  // afternoon. So a wait this long says so out loud.
  const overdue = window.setTimeout(() => {
    console.error(
      "[saltcorn] the extension API has not arrived; Build and formatting are missing. " +
        "Is the extension being registered after initialize()?",
    );
  }, API_OVERDUE_MS);
  try {
    await extension.setAsDefaultApi();
  } finally {
    window.clearTimeout(overdue);
  }
  registerPrettierFormatter(files);
  registerBuildCommand(files);
  // Last, and not awaited: prettier, the Build command and the grammars are the
  // capabilities every store has, and a store that cannot host a language server
  // (§12.1) must still get all of them. The client says so for itself when the
  // socket refuses it.
  registerLanguageClient(files.store);
}
