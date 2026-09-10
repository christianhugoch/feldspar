/**
 * Source control: the SCM view, with an index (design §12.1, phase 5).
 *
 * See what changed, stage what belongs in the next commit, commit it, exchange
 * it with the remote, switch branch. That is the whole subset, and it is drawn
 * with VS Code's own SCM API — the Source Control viewlet's groups, its message
 * box, its title buttons, the inline `+`/`−` actions, the status bar — over the
 * git operations the file store declares (§14.1). No new git is written in the
 * browser.
 *
 * **The index is the substance of this module's shape.** git's porcelain code is
 * two columns, so one changed file can be two rows — one the next commit will
 * take and one it will not — and a view with a single group cannot say that. So
 * there are three groups (Merge Changes, Staged Changes, Changes), `+` stages a
 * row, `−` unstages one, and **Commit commits what is staged and nothing else**.
 * The splitting itself is `git.ts`'s, which is where it can be tested.
 *
 * What is still not here is as deliberate: no diff editor or gutter quick-diff,
 * no history, no discard, no conflict resolution. Each needs something the
 * backend has no operation for — reading a blob at a revision, listing the log —
 * and none of them is what stops an admin committing the file they just edited.
 *
 * The rule every operation follows: run it, then treat the working copy as
 * changed underneath the editor. The store's listings are dropped, what is open
 * is re-read, and the view redraws from the status the operation itself reported.
 * Staging is the exception, and only because it moves nothing on disk: it redraws
 * and leaves the files alone.
 */

import * as vscode from "vscode";
// The filesystem service's own URI class, not `vscode.Uri`: what is announced to
// the provider goes to the file service, and the two are only structurally alike.
import { URI } from "@codingame/monaco-vscode-api/vscode/vs/base/common/uri";

import {
  StoreGit,
  UNKNOWN_STATUS,
  badgeRow,
  branchNameProblem,
  changeRows,
  commitMessageProblem,
  isClean,
  rowColor,
  rowsIn,
  trackingSuffix,
  type ChangeGroup,
  type ChangeRow,
  type GitOperationResult,
  type GitStatus,
} from "./git";
import type { StoreFileSystemProvider } from "./fileSystemProvider";
import { restartLanguageClient } from "./languageClient";
import { StoreFiles, toStorePathOrNull, toUriPath } from "./storeFiles";

/** The command ids, as the manifest contributes them. */
export const REFRESH_COMMAND = "saltcorn.git.refresh";
export const COMMIT_COMMAND = "saltcorn.git.commit";
export const PULL_COMMAND = "saltcorn.git.pull";
export const PUSH_COMMAND = "saltcorn.git.push";
export const CHECKOUT_COMMAND = "saltcorn.git.checkout";
export const STAGE_COMMAND = "saltcorn.git.stage";
export const UNSTAGE_COMMAND = "saltcorn.git.unstage";
export const STAGE_ALL_COMMAND = "saltcorn.git.stageAll";
export const UNSTAGE_ALL_COMMAND = "saltcorn.git.unstageAll";

/**
 * The context key the manifest's `when` clauses test.
 *
 * A store that is not a git working copy gets no source control at all — not an
 * empty view and not buttons that fail when pressed — so the commands are hidden
 * from the palette and the title bar rather than registered and disabled.
 */
export const GIT_STORE_CONTEXT = "saltcorn.gitStore";

/**
 * The `contextValue` each group and its rows carry, which is the only thing the
 * manifest's `when` clauses have to go on: `scmResourceGroupState` for a group's
 * own `+`, `scmResourceState` for a row's.
 *
 * Exported because the manifest is a literal in `extension.ts` and a `when`
 * clause comparing against a string typed twice is a `when` clause that silently
 * stops matching.
 */
export const GROUP_CONTEXT: Record<ChangeGroup, string> = {
  merge: "saltcornMerge",
  staged: "saltcornStaged",
  unstaged: "saltcornUnstaged",
};

/** What each group is called, in VS Code's own words. */
const GROUP_LABEL: Record<ChangeGroup, string> = {
  merge: "Merge Changes",
  staged: "Staged Changes",
  unstaged: "Changes",
};

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
  // Created in the order they are drawn, which is VS Code's: a conflict first
  // because nothing else can be committed until it is dealt with, then what the
  // next commit will take, then what it will not.
  const groups = new Map<ChangeGroup, vscode.SourceControlResourceGroup>();
  for (const group of ["merge", "staged", "unstaged"] as const) {
    const resourceGroup = control.createResourceGroup(group, GROUP_LABEL[group]);
    resourceGroup.hideWhenEmpty = true;
    resourceGroup.contextValue = GROUP_CONTEXT[group];
    groups.set(group, resourceGroup);
  }
  const badges = new ChangeBadges(git.store);
  control.inputBox.placeholder = `Message (commit into ${git.store})`;
  control.acceptInputCommand = { command: COMMIT_COMMAND, title: "Commit" };

  void vscode.commands.executeCommand("setContext", GIT_STORE_CONTEXT, true);

  let status: GitStatus = UNKNOWN_STATUS;
  let rows: ChangeRow[] = [];
  const draw = (next: GitStatus): void => {
    status = next;
    rows = changeRows(next);
    for (const [group, resourceGroup] of groups) {
      resourceGroup.resourceStates = rowsIn(rows, group).map((row) =>
        resourceState(git.store, row),
      );
    }
    // Every row, which is how VS Code counts: a file that is staged and edited
    // again is two things to decide about, not one.
    control.count = rows.length;
    control.inputBox.placeholder =
      next.branch === ""
        ? `Message (commit into ${git.store})`
        : `Message (commit on ${next.branch})`;
    control.statusBarCommands = [branchIndicator(next)];
    badges.update(rows);
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
    badges.register(),
    vscode.commands.registerCommand(REFRESH_COMMAND, refresh),
    vscode.commands.registerCommand(COMMIT_COMMAND, () =>
      commit(control, () => rows, git, files, provider, draw),
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
    // The four index commands. Each one's argument list is whatever VS Code
    // handed the menu item — one row, or every row of a multiple selection —
    // and an empty list means the group's own button, which is all of them.
    vscode.commands.registerCommand(STAGE_COMMAND, (...args: unknown[]) =>
      stage("Stage", (paths) => git.stage(paths), selectedPaths(git.store, args), draw),
    ),
    vscode.commands.registerCommand(UNSTAGE_COMMAND, (...args: unknown[]) =>
      stage("Unstage", (paths) => git.unstage(paths), selectedPaths(git.store, args), draw),
    ),
    vscode.commands.registerCommand(STAGE_ALL_COMMAND, () =>
      stage("Stage", (paths) => git.stage(paths), [], draw),
    ),
    vscode.commands.registerCommand(UNSTAGE_ALL_COMMAND, () =>
      stage("Unstage", (paths) => git.unstage(paths), [], draw),
    ),
    // A save is the ordinary way the working copy becomes dirty, and there is no
    // watcher to notice it (§12.1). Debounced, because "Save All" is one action
    // to an admin and would otherwise be one status request per file.
    vscode.workspace.onDidSaveTextDocument(refresh),
  ];

  refresh();
  return { disposables, refresh };
}

/** One row, as the view shows it. */
function resourceState(store: string, row: ChangeRow): vscode.SourceControlResourceState {
  const uri = vscode.Uri.file(toUriPath(store, row.path));
  return {
    resourceUri: uri,
    // Opening the file, not a diff: a diff needs the blob at `HEAD`, which no
    // endpoint serves (§12.1, and the phase's own exclusions). Opening what an
    // admin clicked is the honest thing this can do.
    command: row.deleted
      ? undefined
      : { command: "vscode.open", title: "Open File", arguments: [uri] },
    // What the inline `+` or `−` is contributed against: the group a row is in
    // is the whole of what can be done to it.
    contextValue: GROUP_CONTEXT[row.group],
    decorations: {
      tooltip: row.label,
      strikeThrough: row.deleted,
      faded: row.deleted,
      // No `iconPath`, and that is load-bearing: the SCM tree draws a row's
      // badge *or* its icon, and the badge — the letter, in its colour — is the
      // one that looks like desktop VS Code.
    },
  };
}

/**
 * The letters at the ends of the rows, as a file decoration provider.
 *
 * This is how VS Code's git extension does it and there is no other way to do
 * it: `SourceControlResourceDecorations` has no letter, and the SCM tree asks
 * the *decoration service* for one badge per URI. Which means the badge belongs
 * to the file rather than to the row — see `badgeRow` for the precedence that
 * follows — and it also means the same letters appear on the explorer's files,
 * which is exactly what desktop VS Code shows.
 */
class ChangeBadges implements vscode.FileDecorationProvider {
  private readonly changed = new vscode.EventEmitter<vscode.Uri[]>();
  readonly onDidChangeFileDecorations = this.changed.event;
  /** The row decorating each URI path, keyed the way a lookup arrives. */
  private byPath = new Map<string, ChangeRow>();

  constructor(private readonly store: string) {}

  register(): vscode.Disposable {
    return vscode.window.registerFileDecorationProvider(this);
  }

  /** Take the new rows, and tell VS Code which files it must redraw. */
  update(rows: readonly ChangeRow[]): void {
    const next = new Map<string, ChangeRow>();
    for (const path of new Set(rows.map((row) => row.path))) {
      const row = badgeRow(rows.filter((candidate) => candidate.path === path));
      if (row !== undefined) next.set(toUriPath(this.store, path), row);
    }
    // Both sides of the change: a file that *stopped* being modified needs its
    // badge taken away, and nothing else would ask about it again.
    const touched = new Set([...this.byPath.keys(), ...next.keys()]);
    this.byPath = next;
    this.changed.fire([...touched].map((path) => vscode.Uri.file(path)));
  }

  provideFileDecoration(uri: vscode.Uri): vscode.FileDecoration | undefined {
    const row = this.byPath.get(uri.path);
    if (row === undefined) return undefined;
    return {
      badge: row.letter,
      tooltip: row.label,
      color: new vscode.ThemeColor(rowColor(row)),
      // A deleted file has no ancestors worth colouring, and colouring them
      // would say the directory is deleted.
      propagate: !row.deleted,
    };
  }
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

/**
 * The store-relative paths a menu command was invoked on.
 *
 * VS Code hands a resource-state menu item the row it was pressed on, and every
 * row of the selection when several are selected — so the arguments are a list,
 * and anything in it that is not a resource state is ignored rather than guessed
 * at. An empty answer means *everything*, which is what both operations do with
 * no paths: that is the group-level button's meaning and it arrives here the
 * same way.
 */
function selectedPaths(store: string, args: readonly unknown[]): string[] {
  const paths: string[] = [];
  for (const arg of args) {
    if (arg == null || typeof arg !== "object") continue;
    const uriPath = (arg as { resourceUri?: { path?: unknown } }).resourceUri?.path;
    if (typeof uriPath !== "string") continue;
    const path = toStorePathOrNull(store, uriPath);
    // A path twice is a path once: the same file can be selected in two groups,
    // and `git add a a` is not what the admin pressed.
    if (path !== null && !paths.includes(path)) paths.push(path);
  }
  return paths;
}

/**
 * Stage or unstage, and redraw.
 *
 * Deliberately *not* [`run`]: nothing on disk moved, so dropping the store's
 * listings and re-reading every open document would be work for a change no
 * editor can see. Nor is there a notification — VS Code's own `+` does not
 * announce itself, and an admin staging six files does not want six of them. A
 * failure is still said out loud, because that is the case where git has
 * something to tell them.
 */
async function stage(
  what: string,
  operation: (paths: readonly string[]) => Promise<GitOperationResult>,
  paths: readonly string[],
  draw: (status: GitStatus) => void,
): Promise<void> {
  try {
    const result = await vscode.window.withProgress(
      { location: vscode.ProgressLocation.SourceControl, title: `${what}…` },
      () => operation(paths),
    );
    if (result.status !== null) draw(result.status);
  } catch (err) {
    void vscode.window.showErrorMessage(
      `${what} failed: ${err instanceof Error ? err.message : String(err)}`,
    );
  }
}

/**
 * Commit what the input box says, refusing an empty message here.
 *
 * Commit means **the index**, so a press with nothing staged is a question
 * rather than an error: VS Code asks whether to stage everything and commit that,
 * and so does this. Answering it with "nothing to commit" while the Changes
 * group is full of the admin's work would be technically true and useless.
 */
async function commit(
  control: vscode.SourceControl,
  rows: () => readonly ChangeRow[],
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
  const now = rows();
  let all = false;
  if (rowsIn(now, "staged").length === 0) {
    const unstaged = rowsIn(now, "unstaged").length;
    if (unstaged === 0) {
      void vscode.window.showInformationMessage("There is nothing to commit.");
      return;
    }
    const stageAll = "Stage all and commit";
    const answer = await vscode.window.showWarningMessage(
      `Nothing is staged. Commit all ${unstaged} change(s)?`,
      { modal: true },
      stageAll,
    );
    if (answer !== stageAll) return;
    all = true;
  }
  const result = await run(
    "Commit",
    () => (all ? git.commitAll(message) : git.commit(message)),
    files,
    provider,
    draw,
  );
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
