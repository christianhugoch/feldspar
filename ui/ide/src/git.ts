/**
 * The store's git working copy, in the store's own vocabulary (design §12.1).
 *
 * This is the half of source control that has nothing to do with VS Code: the
 * declared backend operations of §14.1 (`status`, `pull`, `push`, `commit`,
 * `checkout`) and the payload they answer with. `sourceControl.ts` is the
 * translation into VS Code's SCM API and nothing more — the same split as
 * `storeFiles.ts` and `fileSystemProvider.ts`, and for the same reason: what is
 * worth testing is testable without booting a workbench.
 *
 * Every operation reports the working copy's state *after* it, because the
 * server computes it anyway (§14.1) and a view that redrew from a second request
 * would be a view that can disagree with the operation it just ran.
 */

import { errorMessage } from "./api";
import type { ApiClient, ListFileStoresResponse } from "./client";

/** One file store, as the admin API lists them. */
export type FileStoreSummary = ListFileStoresResponse[number];

/** One changed path, as `git status --porcelain` reports it. */
export interface GitChange {
  /**
   * git's two-character `XY` code — `??` untracked, ` M` modified but unstaged,
   * `A ` added, `D ` deleted, and so on. Kept as git wrote it: a view showing
   * "M" is showing git's own letter, not a vocabulary invented here.
   */
  readonly status: string;
  /** The path, relative to the store root. */
  readonly path: string;
}

/** What the working copy is right now. */
export interface GitStatus {
  /** Whether there is a clone at all; everything else is empty when there is not. */
  readonly cloned: boolean;
  /** The checked-out branch, or `""` on a detached head or an empty repository. */
  readonly branch: string;
  /** Every branch that can be checked out, local ones first. */
  readonly branches: readonly string[];
  /** Commits this working copy has that its upstream does not. */
  readonly ahead: number;
  /** Commits the upstream has that this working copy does not. */
  readonly behind: number;
  /** The last commit, as one line; `""` in a repository with no commits. */
  readonly lastCommit: string;
  /** Every uncommitted change, in the order git listed them. */
  readonly changes: readonly GitChange[];
}

/** The state of a store nothing has been read from yet. */
export const UNKNOWN_STATUS: GitStatus = {
  cloned: false,
  branch: "",
  branches: [],
  ahead: 0,
  behind: 0,
  lastCommit: "",
  changes: [],
};

/**
 * Read the `data` an operation answered with, or `null` when there is none.
 *
 * `data` is optional by design — it is the git backend's payload, not a shape
 * every backend has to produce (§14.1) — so this is a parser, not a cast: a
 * response without it, or with something unrecognisable in it, means *no status*
 * rather than a status full of `undefined`. That is the same defensiveness the
 * filesystem provider applies to the file API, and it is what lets a backend
 * change its payload without breaking the workbench.
 */
export function parseGitStatus(data: unknown): GitStatus | null {
  if (data == null || typeof data !== "object") return null;
  const raw = data as Record<string, unknown>;
  if (typeof raw.cloned !== "boolean") return null;
  return {
    cloned: raw.cloned,
    branch: typeof raw.branch === "string" ? raw.branch : "",
    branches: stringList(raw.branches),
    ahead: countOf(raw.ahead),
    behind: countOf(raw.behind),
    lastCommit: typeof raw.last_commit === "string" ? raw.last_commit : "",
    changes: changeList(raw.changes),
  };
}

/** Whether there is nothing to commit. */
export function isClean(status: GitStatus): boolean {
  return status.changes.length === 0;
}

/**
 * How the branch stands against its upstream, as the suffix VS Code's own git
 * view uses: `*` for uncommitted work, then the counts that have something to
 * exchange.
 */
export function trackingSuffix(status: GitStatus): string {
  const parts: string[] = [];
  if (!isClean(status)) parts.push("*");
  if (status.behind > 0) parts.push(`↓${status.behind}`);
  if (status.ahead > 0) parts.push(`↑${status.ahead}`);
  return parts.join("");
}

/** What a change's two-letter code means, for a tooltip. */
export function describeChange(change: GitChange): string {
  const code = change.status.trim();
  switch (code) {
    case "??":
      return "Untracked";
    case "M":
    case "MM":
      return "Modified";
    case "A":
      return "Added";
    case "D":
      return "Deleted";
    case "R":
      return "Renamed";
    case "C":
      return "Copied";
    case "U":
    case "UU":
    case "AA":
    case "DD":
      return "Conflicted";
    default:
      return `Changed (${change.status})`;
  }
}

/**
 * Why this commit message will not do, or `null` when it will.
 *
 * The server refuses an empty one too — `message` is a declared, required
 * argument (§6.2) — but being told by a round trip what the box in front of the
 * admin already shows is a round trip for nothing.
 */
export function commitMessageProblem(message: string): string | null {
  return message.trim() === "" ? "A commit needs a message." : null;
}

/**
 * Why this branch name will not do, or `null` when it will.
 *
 * The rules are git's, narrowed to the ones an admin can trip over in a text
 * box: what is caught here is caught while they are still typing, and everything
 * else git refuses for itself with its own message. Nothing is *silently*
 * corrected — a name that is not accepted is said to be, rather than becoming
 * some other branch.
 */
export function branchNameProblem(name: string, existing: readonly string[]): string | null {
  const trimmed = name.trim();
  if (trimmed === "") return "A branch needs a name.";
  if (/[\s~^:?*[\\]/.test(trimmed)) {
    return "A branch name cannot contain spaces or any of ~^:?*[\\";
  }
  if (trimmed.startsWith("-") || trimmed.startsWith("/") || trimmed.endsWith("/")) {
    return "A branch name cannot start with - or /, or end with /.";
  }
  if (trimmed.includes("..") || trimmed.endsWith(".lock")) {
    return "A branch name cannot contain .. or end with .lock.";
  }
  if (existing.some((branch) => branch === trimmed)) return `${trimmed} already exists.`;
  return null;
}

/** Whether a change means the file is no longer there. */
export function isDeletion(change: GitChange): boolean {
  return change.status.includes("D");
}

/** What one operation did, and the state it left behind. */
export interface GitOperationResult {
  /** git's own output — the actionable part of a failure, and of a success. */
  readonly output: string;
  /** The working copy afterwards, or `null` if the backend reported none. */
  readonly status: GitStatus | null;
}

/**
 * The git operations of one store, addressed by the id the API wants.
 *
 * A store's *name* is what the IDE is opened with and what its files are read
 * by; the operation endpoints take the **id**, which is why one is carried here
 * alongside the other rather than looked up per call.
 */
export class StoreGit {
  constructor(
    /** The store's name, for messages an admin reads. */
    readonly store: string,
    private readonly storeId: string,
    private readonly client: ApiClient,
  ) {}

  /** Re-read the working copy without changing it. */
  status(): Promise<GitOperationResult> {
    return this.run("status", {});
  }

  /** Fetch and merge the remote. */
  pull(): Promise<GitOperationResult> {
    return this.run("pull", {});
  }

  /** Send committed work to the remote. */
  push(): Promise<GitOperationResult> {
    return this.run("push", {});
  }

  /** Stage everything and commit it. */
  commit(message: string): Promise<GitOperationResult> {
    return this.run("commit", { message });
  }

  /** Switch branch, creating it from the current one when `create`. */
  checkout(branch: string, create: boolean): Promise<GitOperationResult> {
    return this.run("checkout", { branch, create });
  }

  /**
   * Run one declared operation.
   *
   * A failure is rethrown as an `Error` carrying the **server's** message, which
   * for git is git's own — "your local changes would be overwritten by
   * checkout", naming the files. That sentence is the entire value of a failed
   * git command, so nothing here summarises it away.
   */
  private async run(
    operation: string,
    input: Record<string, unknown>,
  ): Promise<GitOperationResult> {
    try {
      const response = await this.client.runFileStoreOperation(this.storeId, operation, { input });
      return { output: response.output, status: parseGitStatus(response.data) };
    } catch (err) {
      throw new Error(errorMessage(err, `the ${operation} failed`));
    }
  }
}

/**
 * The git side of a store, or `null` when there is none.
 *
 * Two things have to be true, and both come from the store's own listing: it is
 * a **git working copy** (`is_git_repo`, a property of the connected instance,
 * not of the configuration), and it has an **id** — the operation endpoints are
 * addressed by id, and a store without one is not saved. Anything else gets no
 * source control at all: an empty Changes group over a plain directory would be
 * a promise the store cannot keep.
 */
export function storeGit(summary: FileStoreSummary, client: ApiClient): StoreGit | null {
  if (summary.is_git_repo !== true) return null;
  if (summary.id == null || summary.id === "") return null;
  return new StoreGit(summary.name, summary.id, client);
}

/** A JSON array of strings, or an empty list for anything else. */
function stringList(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((item): item is string => typeof item === "string");
}

/** A count from the payload; anything that is not a number is none. */
function countOf(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

/** The `changes` array, dropping any entry that does not name a path. */
function changeList(value: unknown): GitChange[] {
  if (!Array.isArray(value)) return [];
  const changes: GitChange[] = [];
  for (const item of value) {
    if (item == null || typeof item !== "object") continue;
    const raw = item as Record<string, unknown>;
    if (typeof raw.path !== "string" || raw.path === "") continue;
    changes.push({
      status: typeof raw.status === "string" ? raw.status : "",
      path: raw.path,
    });
  }
  return changes;
}
