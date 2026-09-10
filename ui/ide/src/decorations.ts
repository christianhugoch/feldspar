/**
 * The file-decoration service, which is what draws a change's letter.
 *
 * A one-service override, and it needs its own module because of what it is
 * working around. `monaco-vscode-api` registers a **stub** `IDecorationsService`
 * in its `missing-services` fallbacks — `registerDecorationsProvider` returns a
 * disposable and nothing else, `getDecoration` answers `undefined` — so a
 * `FileDecorationProvider` registered against the default workbench is accepted,
 * never consulted, and never draws anything. That is the failure mode this
 * codebase has met before (see `extension.ts`): everything looks healthy and one
 * feature silently is not there.
 *
 * The real implementation lives in `base-service-override`, whose `default`
 * export replaces nineteen services — the label service, the path service, the
 * request service, the working-copy file service among them. Taking all of that
 * to get one is a change to the workbench's service graph in exchange for a
 * letter at the end of a row, so what is taken is the one entry, in the shape the
 * package itself builds it: a `SyncDescriptor`, keyed by the service id.
 *
 * Its own dependencies are `IUriIdentityService` and `IThemeService`, both of
 * which this workbench already has — which is the other reason it can be lifted
 * out on its own.
 */

import type { IEditorOverrideServices } from "@codingame/monaco-vscode-api";
import { SyncDescriptor } from "@codingame/monaco-vscode-api/vscode/vs/platform/instantiation/common/descriptors";
import { IDecorationsService } from "@codingame/monaco-vscode-api/vscode/vs/workbench/services/decorations/common/decorations.service";
import { DecorationsService } from "@codingame/monaco-vscode-base-service-override/vscode/vs/workbench/services/decorations/browser/decorationsService";

/**
 * The real decorations service, as a service override.
 *
 * Both consumers are in this bundle: the SCM view's rows (`sourceControl.ts`,
 * whose letters are file decorations because VS Code's own git extension's are)
 * and the explorer, which shows the same letters on the same files.
 */
export function getDecorationsServiceOverride(): IEditorOverrideServices {
  return {
    [IDecorationsService.toString()]: new SyncDescriptor(DecorationsService, [], true),
  };
}
