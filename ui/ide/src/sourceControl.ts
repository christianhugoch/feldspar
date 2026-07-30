/**
 * Source control: the minimal SCM view (design §12.1, phase 5).
 *
 * See what changed, commit it, exchange it with the remote, switch branch. That
 * is the whole subset, and it is drawn with VS Code's own SCM API — the Source
 * Control viewlet, its message box, its title buttons, the status bar — over the
 * git operations the file store already declares (§14.1). No new git is written
 * in the browser.
 *
 * What is **not** here is as deliberate: no index (there is one group, and
 * Commit commits the working copy), no diff editor or gutter quick-diff, no
 * history, no discard, no merge. Each needs something the backend has no
 * operation for — reading a blob at a revision, listing the log — and none of
 * them is what stops an admin committing the file they just edited.
 *
 * The rule every command follows: run the operation, then treat the working copy
 * as changed underneath the editor. The store's listings are dropped, what is
 * open is re-read, and the view redraws from the status the operation itself
 * reported.
 */

import * as vscode from "vscode";
// The filesystem service's own URI class, not `vscode.Uri`: what is announced to
// the provider goes to the file service, and the two are only structurally alike.
import { URI } from "@codingame/monaco-vscode-api/vscode/vs/base/common/uri";

import {
  StoreGit,
  UNKNOWN_STATUS,
  branchNameProblem,
  commitMessageProblem,
  describeChange,
  isClean,
  isDeletion,
  trackingSuffix,
  type GitChange,
  type GitOperationResult,
  type GitStatus,
} from "./git";
import type { StoreFileSystemProvider } from "./fileSystemProvider";
import { restartLanguageClient } from "./languageClient";
import { StoreFiles, toUriPath } from "./storeFiles";

/** The command ids, as the manifest contributes them. */
export const REFRESH_COMMAND = "saltcorn.git.refresh";
export const COMMIT_COMMAND = "saltcorn.git.commit";
export const PULL_COMMAND = "saltcorn.git.pull";
export const PUSH_COMMAND = "saltcorn.git.push";
export const CHECKOUT_COMMAND = "saltcorn.git.checkout";

/**
 * The context key the manifest's `when` clauses test.
 *
 * A store that is not a git working copy gets no source control at all — not an
 * empty view and not buttons that fail when pressed — so the commands are hidden
 * from the palette and the title bar rather than registered and disabled.
 */
export const GIT_STORE_CONTEXT = "saltcorn.gitStore";

/** The entry that offers to make a branch, rather than to switch to one. */
const CREATE_BRANCH = "$(add) Create new branch…";

/** How long several saves in a row are allowed to coalesce into one refresh. */
const REFRESH_DEBOUNCE_MS = 400;

/**
 * Bring up the Source Control view for a git-backed store.
 *
 * Returns what tears it down, and — for the Build command — a `refresh` to call
 * when something outside this module has written into the working copy.
 */
export function registerSourceControl(
  files: StoreFiles,
  provider: StoreFileSystemProvider,
  git: StoreGit,
): { disposables: vscode.Disposable[]; refresh: () => void } {
  const control = vscode.scm.createSourceControl(
    "saltcorn",
    `Saltcorn: ${git.store}`,
    vscode.Uri.file(`/${git.store}`),
  );
  // One group, because there is one thing an admin can do to a change here:
  // commit it. A "Staged Changes" group would be a promise of `git add -p`,
  // which the backend has no operation for.
  const changes = control.createResourceGroup("changes", "Changes");
  changes.hideWhenEmpty = true;
  control.inputBox.placeholder = `Message (commit into ${git.store})`;
  control.acceptInputCommand = { command: COMMIT_COMMAND, title: "Commit" };

  void vscode.commands.executeCommand("setContext", GIT_STORE_CONTEXT, true);

  let status: GitStatus = UNKNOWN_STATUS;
  const draw = (next: GitStatus): void => {
    status = next;
    changes.resourceStates = next.changes.map((change) => resourceState(git.store, change));
    control.count = next.changes.length;
    control.inputBox.placeholder =
      next.branch === ""
        ? `Message (commit into ${git.store})`
        : `Message (commit on ${next.branch})`;
    control.statusBarCommands = [branchIndicator(next)];
  };
  draw(status);

  const refresh = debounce(() => {
    void git
      .status()
      .then((result) => {
        if (result.status !== null) draw(result.status);
      })
      .catch((err: unknown) => {
        // Not a notification: the working copy's state is something the view
        // asks for on its own initiative, and failing to get it is not an act
        // the admin took. The console keeps the reason for whoever looks.
        console.error("[saltcorn] could not read the git status", err);
      });
  }, REFRESH_DEBOUNCE_MS);

  const disposables = [
    control,
    vscode.commands.registerCommand(REFRESH_COMMAND, refresh),
    vscode.commands.registerCommand(COMMIT_COMMAND, () =>
      commit(control, git, files, provider, draw),
    ),
    vscode.commands.registerCommand(PULL_COMMAND, () =>
      run("Pull", () => git.pull(), files, provider, draw),
    ),
    vscode.commands.registerCommand(PUSH_COMMAND, () =>
      run("Push", () => git.push(), files, provider, draw),
    ),
    vscode.commands.registerCommand(CHECKOUT_COMMAND, () =>
      checkout(() => status, git, files, provider, draw),
    ),
    // A save is the ordinary way the working copy becomes dirty, and there is no
    // watcher to notice it (§12.1). Debounced, because "Save All" is one action
    // to an admin and would otherwise be one status request per file.
    vscode.workspace.onDidSaveTextDocument(refresh),
  ];

  refresh();
  return { disposables, refresh };
}

/** One changed file, as the view shows it. */
function resourceState(store: string, change: GitChange): vscode.SourceControlResourceState {
  const uri = vscode.Uri.file(toUriPath(store, change.path));
  const deleted = isDeletion(change);
  return {
    resourceUri: uri,
    // Opening the file, not a diff: a diff needs the blob at `HEAD`, which no
    // endpoint serves (§12.1, and the phase's own exclusions). Opening what an
    // admin clicked is the honest thing this can do.
    command: deleted ? undefined : { command: "vscode.open", title: "Open File", arguments: [uri] },
    decorations: {
      tooltip: describeChange(change),
      strikeThrough: deleted,
      faded: deleted,
    },
  };
}

/** The status-bar entry: the branch, what it has to exchange, and a way to switch. */
function branchIndicator(status: GitStatus): vscode.Command {
  const branch = status.branch === "" ? "(no branch)" : status.branch;
  return {
    command: CHECKOUT_COMMAND,
    title: `$(git-branch) ${branch}${trackingSuffix(status)}`,
    tooltip: status.cloned ? `${describeTracking(status)} — switch branch` : "Not cloned yet",
  };
}

/** Ahead/behind as a sentence, for the indicator's tooltip. */
function describeTracking(status: GitStatus): string {
  const parts: string[] = [];
  if (status.ahead > 0) parts.push(`${status.ahead} to push`);
  if (status.behind > 0) parts.push(`${status.behind} to pull`);
  if (!isClean(status)) parts.push(`${status.changes.length} uncommitted`);
  return parts.length === 0 ? "Up to date" : parts.join(", ");
}

/** Commit what the input box says, refusing an empty message here. */
async function commit(
  control: vscode.SourceControl,
  git: StoreGit,
  files: StoreFiles,
  provider: StoreFileSystemProvider,
  draw: (status: GitStatus) => void,
): Promise<void> {
  const message = control.inputBox.value.trim();
  const problem = commitMessageProblem(message);
  if (problem !== null) {
    void vscode.window.showWarningMessage(problem);
    return;
  }
  const result = await run("Commit", () => git.commit(message), files, provider, draw);
  if (result !== null) control.inputBox.value = "";
}

/**
 * Switch branch, or make one.
 *
 * The picker is built from the status the view already holds — the branch list
 * rides the same payload (§14.1) — so pressing the indicator opens immediately
 * rather than after a request.
 */
async function checkout(
  status: () => GitStatus,
  git: StoreGit,
  files: StoreFiles,
  provider: StoreFileSystemProvider,
  draw: (next: GitStatus) => void,
): Promise<void> {
  const now = status();
  const picked = await vscode.window.showQuickPick(
    [CREATE_BRANCH, ...now.branches.filter((branch) => branch !== now.branch)],
    { title: `Switch branch (currently ${now.branch === "" ? "no branch" : now.branch})` },
  );
  if (picked == null) return;

  let branch = picked;
  const create = picked === CREATE_BRANCH;
  if (create) {
    const typed = await vscode.window.showInputBox({
      title: "New branch",
      prompt: `Create a branch from ${now.branch === "" ? "the current commit" : now.branch}`,
      validateInput: (value) => branchNameProblem(value, now.branches),
    });
    if (typed == null || typed.trim() === "") return;
    branch = typed.trim();
  }

  const result = await run(
    "Switch branch",
    () => git.checkout(branch, create),
    files,
    provider,
    draw,
  );
  if (result === null) return;
  // A switch changes the whole tree at once, which is the one case the language
  // server cannot be reasoned about file by file: its project was built from the
  // branch that is no longer checked out.
  restartLanguageClient(git.store);
}

/**
 * Run one operation with the refresh discipline every one of them needs.
 *
 * The order matters. The store's listings are dropped first, so anything asked
 * afterwards is asked of the store; what is open is then announced as changed,
 * so an editor showing the old branch's contents re-reads; and the view is drawn
 * from the status the operation *itself* reported, which cannot disagree with
 * what just happened.
 *
 * Returns `null` when it failed, having shown git's own words — the sentence
 * naming the files a checkout would have overwritten is the entire value of that
 * failure.
 */
async function run(
  what: string,
  operation: () => Promise<GitOperationResult>,
  files: StoreFiles,
  provider: StoreFileSystemProvider,
  draw: (status: GitStatus) => void,
): Promise<GitOperationResult | null> {
  let result: GitOperationResult;
  try {
    result = await vscode.window.withProgress(
      { location: vscode.ProgressLocation.SourceControl, title: `${what}…` },
      operation,
    );
  } catch (err) {
    void vscode.window.showErrorMessage(
      `${what} failed: ${err instanceof Error ? err.message : String(err)}`,
    );
    return null;
  } finally {
    files.forgetEverything();
    announceOpenDocumentsChanged(provider);
  }

  if (result.status !== null) draw(result.status);
  void vscode.window.showInformationMessage(summarize(what, result));
  return result;
}

/** One line about what happened, falling back to git's own output. */
function summarize(what: string, result: GitOperationResult): string {
  const first = result.output
    .split("\n")
    .map((line) => line.trim())
    .find((line) => line !== "");
  return first == null || first === "" ? `${what}: done.` : `${what}: ${first}`;
}

/**
 * Tell VS Code that every open document may now hold different bytes.
 *
 * Only the open ones: they are what a stale editor would be showing, and the
 * explorer re-reads from the listings that were just dropped. A change event for
 * a file nobody has open would be an event with nothing to redraw.
 */
function announceOpenDocumentsChanged(provider: StoreFileSystemProvider): void {
  const open = vscode.workspace.textDocuments
    .filter((document) => document.uri.scheme === "file" && !document.isDirty)
    .map((document) => URI.file(document.uri.path));
  provider.announceChanged(open);
}

/** Call `run` at most once per `delayMs`, on the trailing edge. */
function debounce(run: () => void, delayMs: number): () => void {
  let timer: number | null = null;
  return () => {
    if (timer !== null) window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      timer = null;
      run();
    }, delayMs);
  };
}
