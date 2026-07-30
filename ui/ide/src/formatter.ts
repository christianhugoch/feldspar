/**
 * Prettier, as VS Code's formatter for this workspace (design §12.1).
 *
 * A `DocumentFormattingEditProvider` and nothing else: registering one is what
 * makes **Format Document**, the right-click menu and `editor.formatOnSave` all
 * work, rather than a command of our own that only one of those three finds.
 *
 * The formatting itself is `prettierFormat.ts` and the project's configuration is
 * `prettierConfig.ts`; what is here is the translation to and from VS Code.
 */

import * as vscode from "vscode";

import { formatDocument, PARSER_BY_LANGUAGE } from "./prettierFormat";
import { StoreFileError, StoreFiles, toStorePath } from "./storeFiles";

/** Every language the bundled plugins can format, in this workspace's files. */
const SELECTOR: vscode.DocumentSelector = Object.keys(PARSER_BY_LANGUAGE).map((language) => ({
  language,
  scheme: "file",
}));

/** Register prettier as the formatter for `files`'s workspace folder. */
export function registerPrettierFormatter(files: StoreFiles): vscode.Disposable {
  // An unreadable configuration is worth saying once — on every save it would be
  // noise, and the reason does not change between saves.
  const reported = new Set<string>();

  return vscode.languages.registerDocumentFormattingEditProvider(SELECTOR, {
    async provideDocumentFormattingEdits(document, options) {
      let storePath: string;
      try {
        storePath = toStorePath(files.store, document.uri.path);
      } catch (err) {
        // A file the workbench itself owns, outside the store's folder. Not ours
        // to format, and not an error either.
        if (err instanceof StoreFileError) return [];
        throw err;
      }

      const text = document.getText();
      try {
        const result = await formatDocument(files, storePath, document.languageId, text, {
          tabWidth: options.tabSize,
          useTabs: !options.insertSpaces,
        });
        if (result === null) return [];

        if (result.unreadableConfig !== null && !reported.has(result.unreadableConfig.path)) {
          reported.add(result.unreadableConfig.path);
          void vscode.window.showWarningMessage(
            `Formatting is not using ${result.unreadableConfig.path}: ${result.unreadableConfig.reason}. Prettier's defaults are being used instead.`,
          );
        }

        // Nothing to say when nothing changed: an empty edit list leaves the
        // document's undo history alone.
        if (result.text === text) return [];
        return [vscode.TextEdit.replace(wholeDocument(document), result.text)];
      } catch (err) {
        // Prettier failed to parse, which means the file does not compile. Say so
        // — a format command that quietly does nothing is a bug report.
        void vscode.window.showErrorMessage(
          `Prettier could not format ${storePath}: ${err instanceof Error ? err.message : String(err)}`,
        );
        return [];
      }
    },
  });
}

/** The range covering a whole document. */
function wholeDocument(document: vscode.TextDocument): vscode.Range {
  const end = document.lineCount === 0 ? new vscode.Position(0, 0) : document.lineAt(document.lineCount - 1).range.end;
  return new vscode.Range(new vscode.Position(0, 0), end);
}
