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
import type { IExtensionManifest } from "@codingame/monaco-vscode-api/extensions";

import { BUILD_COMMAND, registerBuildCommand } from "./build";
import { registerPrettierFormatter } from "./formatter";
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

/**
 * Register the extension and everything it contributes for `files`'s store.
 *
 * Called after `initialize`: the services the API talks to have to exist first.
 */
export async function registerSaltcornExtension(files: StoreFiles): Promise<void> {
  const extension = registerExtension(MANIFEST, ExtensionHostKind.LocalProcess);
  await extension.setAsDefaultApi();
  registerPrettierFormatter(files);
  registerBuildCommand(files);
}
