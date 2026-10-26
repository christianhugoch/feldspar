/**
 * The Build command (design §12.1, §13.3).
 *
 * The IDE edits a store; an application is *built* from one. `listApplications`
 * carries each application's derived `source.store`, so the command matches this
 * store against that list in the browser (`applications.ts`) and calls
 * `buildApplication` — no endpoint is added for the IDE's sake.
 *
 * A build failure is where this earns its place. The server's error carries the
 * tail of what `tsc` and the bundler said (§16); parsed (`buildDiagnostics.ts`)
 * it becomes a `DiagnosticCollection`, so the Problems panel names the file and
 * the line and clicking it opens the tab. That is the whole reason the build's
 * errors are wired to a panel rather than a toast — until the language server
 * lands in phase 4, this *is* how an admin sees a type error.
 *
 * Beside it, **Build on save** (TODO-astro W2.5): a toggle in the status bar
 * that runs the quick build (`buildOnSave.ts`) whenever a file in the
 * application's source is saved, reporting into the same panel.
 */

import * as vscode from "vscode";

import { api, errorMessage } from "./api";
import { applicationsBuiltFrom, type BuildableApplication } from "./applications";
import { parseBuildDiagnostics, type BuildDiagnostic } from "./buildDiagnostics";
import { SaveScheduler, applicationForSave } from "./buildOnSave";
import { StoreFiles, toStorePathOrNull, toUriPath } from "./storeFiles";

/** The command id, as the manifest contributes it. */
export const BUILD_COMMAND = "saltcorn.buildApplication";
/** The Build on save toggle's command id. */
export const BUILD_ON_SAVE_COMMAND = "saltcorn.toggleBuildOnSave";

/** Where the toggle is remembered, per store, in this browser. */
const BUILD_ON_SAVE_KEY = (store: string) => `saltcorn.buildOnSave.${store}`;

/**
 * Contribute the command, its status-bar button and the collection its failures
 * land in.
 */
export function registerBuildCommand(
  files: StoreFiles,
  /** Called once a build has finished, for a view that redraws from the tree. */
  afterBuild?: () => void,
): vscode.Disposable[] {
  const output = vscode.window.createOutputChannel("Saltcorn Build");
  const problems = vscode.languages.createDiagnosticCollection("saltcorn-build");

  // The palette entry comes from the manifest; this is the *visible* button, so
  // building is one click away rather than something an admin has to know about.
  const button = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  button.text = "$(tools) Build";
  button.tooltip = `Build the application whose source is the file store ${files.store}`;
  button.command = BUILD_COMMAND;
  button.show();

  const command = vscode.commands.registerCommand(BUILD_COMMAND, async () => {
    const chosen = await chooseApplication(files.store);
    if (chosen !== null) await build(files, chosen, output, problems, false, afterBuild);
  });
  return [command, button, output, problems, ...registerBuildOnSave(files, output, problems, afterBuild)];
}

/**
 * The Build on save toggle: a status-bar switch, remembered per store, and the
 * save listener that runs a quick build of the application a saved file is in.
 */
function registerBuildOnSave(
  files: StoreFiles,
  output: vscode.OutputChannel,
  problems: vscode.DiagnosticCollection,
  afterBuild?: () => void,
): vscode.Disposable[] {
  let enabled = remembered(files.store);
  let candidates: BuildableApplication[] | null = null;
  const toggle = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 99);
  toggle.command = BUILD_ON_SAVE_COMMAND;
  const draw = (busy = false) => {
    toggle.text = busy
      ? "$(sync~spin) Building…"
      : enabled
        ? "$(sync) Build on save"
        : "$(circle-slash) Build on save";
    toggle.tooltip = enabled
      ? "Saving a file runs a quick build of its application (no install, no prerendering). Click to turn off."
      : "Click to run a quick build of the application whenever one of its files is saved.";
  };
  draw();
  toggle.show();

  const scheduler = new SaveScheduler(async (app) => {
    draw(true);
    try {
      await build(files, app, output, problems, true, afterBuild);
    } finally {
      draw();
    }
  });

  const command = vscode.commands.registerCommand(BUILD_ON_SAVE_COMMAND, () => {
    enabled = !enabled;
    // Asked again when next needed: an application may have been added since.
    candidates = null;
    remember(files.store, enabled);
    draw();
  });
  const listener = vscode.workspace.onDidSaveTextDocument(async (document) => {
    if (!enabled) return;
    const path = toStorePathOrNull(files.store, document.uri.path);
    if (path === null) return;
    if (candidates === null) {
      try {
        candidates = applicationsBuiltFrom(await api.listApplications(), files.store);
      } catch {
        return;
      }
    }
    const app = applicationForSave(candidates, path);
    if (app !== null) scheduler.saved(app);
  });
  return [toggle, command, listener];
}

function remembered(store: string): boolean {
  try {
    return localStorage.getItem(BUILD_ON_SAVE_KEY(store)) === "on";
  } catch {
    return false;
  }
}

function remember(store: string, on: boolean): void {
  try {
    localStorage.setItem(BUILD_ON_SAVE_KEY(store), on ? "on" : "off");
  } catch {
    // A browser that keeps nothing: the toggle lasts this session.
  }
}

/**
 * Run one build of `chosen` and report what happened. A `quick` build (on
 * save) reports success in the output channel only, not with a notification
 * for every save.
 */
async function build(
  files: StoreFiles,
  chosen: BuildableApplication,
  output: vscode.OutputChannel,
  problems: vscode.DiagnosticCollection,
  quick: boolean,
  afterBuild?: () => void,
): Promise<void> {
  const name = chosen.application.name;

  problems.clear();
  try {
    const report = quick
      ? await api.buildApplication(chosen.application.id, { quick: true })
      : await vscode.window.withProgress(
          { location: vscode.ProgressLocation.Notification, title: `Building ${name}…` },
          () => api.buildApplication(chosen.application.id),
        );
    log(output, quick ? `Rebuilt ${name} on save.` : `Built ${name}.`, report.log);
    if (!quick) void vscode.window.showInformationMessage(`Built ${name}.`);
  } catch (err) {
    const text = errorMessage(err, `building ${name} failed`);
    log(output, `Building ${name} failed.`, text);
    const found = parseBuildDiagnostics(text, chosen.sourcePath);
    problems.set(asDiagnostics(files.store, found));
    await reportFailure(name, found.length, output);
  } finally {
    // A build writes into the store: the generated client into the source tree
    // and the bundle into `dist/`. Both are changes made outside the editor by
    // something done inside it, and there is no watcher to notice — so what the
    // filesystem remembers is dropped rather than left to expire.
    files.forgetEverything();
    // And what it wrote is uncommitted work, so the Changes group is now wrong.
    afterBuild?.();
  }
}

/**
 * Which application to build, asking only when the answer is not obvious.
 *
 * A store holding one application's source — the ordinary case — never asks. A
 * store holding none says so, because "Build" doing nothing would be worse.
 */
async function chooseApplication(store: string): Promise<BuildableApplication | null> {
  let candidates: BuildableApplication[];
  try {
    candidates = applicationsBuiltFrom(await api.listApplications(), store);
  } catch (err) {
    void vscode.window.showErrorMessage(
      `Could not list applications: ${errorMessage(err, "the request failed")}`,
    );
    return null;
  }

  if (candidates.length === 0) {
    void vscode.window.showWarningMessage(
      `No application is built from the file store ${store}. An application with a code framework whose source is this store can be built from here.`,
    );
    return null;
  }
  if (candidates.length === 1) return candidates[0];

  const picked = await vscode.window.showQuickPick(
    candidates.map((candidate) => ({
      label: candidate.application.name,
      description: candidate.sourcePath === "" ? store : `${store}/${candidate.sourcePath}`,
      candidate,
    })),
    { title: "Build which application?" },
  );
  return picked?.candidate ?? null;
}

/** The parsed diagnostics, grouped by the file they are in. */
function asDiagnostics(
  store: string,
  found: BuildDiagnostic[],
): [vscode.Uri, vscode.Diagnostic[]][] {
  const byFile = new Map<string, vscode.Diagnostic[]>();
  for (const item of found) {
    // Zero-width at the position the tool named: VS Code widens it to the word
    // there, which is what a squiggle under the offending identifier is.
    const at = new vscode.Position(item.line - 1, item.column - 1);
    const diagnostic = new vscode.Diagnostic(
      new vscode.Range(at, at),
      item.message,
      item.severity === "warning"
        ? vscode.DiagnosticSeverity.Warning
        : vscode.DiagnosticSeverity.Error,
    );
    diagnostic.source = "build";
    const existing = byFile.get(item.path);
    if (existing) existing.push(diagnostic);
    else byFile.set(item.path, [diagnostic]);
  }
  return [...byFile].map(([path, diagnostics]) => [
    vscode.Uri.file(toUriPath(store, path)),
    diagnostics,
  ]);
}

/** Tell the admin what failed, and take them to where the detail is. */
async function reportFailure(
  name: string,
  problems: number,
  output: vscode.OutputChannel,
): Promise<void> {
  if (problems > 0) {
    const show = "Show Problems";
    const answer = await vscode.window.showErrorMessage(
      `Building ${name} failed with ${problems} problem${problems === 1 ? "" : "s"}.`,
      show,
    );
    if (answer === show) await vscode.commands.executeCommand("workbench.actions.view.problems");
    return;
  }
  // Nothing with a file and a line in it — an install that failed, a missing
  // directory, a bundler this parser does not know. The output is all there is.
  const show = "Show Output";
  const answer = await vscode.window.showErrorMessage(`Building ${name} failed.`, show);
  if (answer === show) output.show(true);
}

/** Append one build's outcome to the channel, whole and unparsed. */
function log(output: vscode.OutputChannel, headline: string, detail: string): void {
  output.appendLine(`${new Date().toISOString()} — ${headline}`);
  if (detail.trim() !== "") output.appendLine(detail);
  output.appendLine("");
}
