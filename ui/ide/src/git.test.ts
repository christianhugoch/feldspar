/**
 * The source-control model: what the view is drawn from (design §12.1, phase 5).
 *
 * Everything here is the store's side of git — the payload an operation answers
 * with, the requests each command makes, and which stores have source control at
 * all. None of it needs a workbench, which is the same split `storeFiles.test.ts`
 * relies on: the VS Code layer translates, and the layer under it is what can be
 * wrong.
 */

import { describe, expect, it, vi } from "vitest";

import type { ApiClient, ListFileStoresResponse } from "./client";
import {
  StoreGit,
  badgeRow,
  branchNameProblem,
  changeRows,
  commitMessageProblem,
  isClean,
  parseGitStatus,
  rowColor,
  rowsIn,
  storeGit,
  trackingSuffix,
} from "./git";

/** The payload the git backend fills `data` with, as JSON off the wire. */
function payload(over: Record<string, unknown> = {}): unknown {
  return {
    cloned: true,
    branch: "main",
    branches: ["main", "feature"],
    ahead: 0,
    behind: 0,
    last_commit: "abc1234 initial (Someone, 2 days ago)",
    changes: [{ status: " M", path: "src/App.tsx" }],
    ...over,
  };
}

/** A stub client recording what the operations were called with. */
function stubClient(response: { output: string; data?: unknown } = { output: "" }) {
  const calls: { id: string; operation: string; body: unknown }[] = [];
  const client = {
    runFileStoreOperation: vi.fn(async (id: string, operation: string, body: unknown) => {
      calls.push({ id, operation, body });
      return { config: {}, connected: true, ...response };
    }),
  } as unknown as ApiClient;
  return { client, calls };
}

/** A store as `listFileStores` returns one. */
function summary(
  over: Partial<ListFileStoresResponse[number]> = {},
): ListFileStoresResponse[number] {
  return {
    id: "11111111-1111-1111-1111-111111111111",
    name: "app-source",
    description: "",
    backend: "git",
    config: {},
    min_role: null,
    connected: true,
    error: null,
    is_git_repo: true,
    ...over,
  };
}

describe("the status payload", () => {
  it("becomes the working copy the view draws", () => {
    const status = parseGitStatus(payload({ ahead: 2, behind: 1 }));
    expect(status).not.toBeNull();
    expect(status?.branch).toBe("main");
    expect(status?.branches).toEqual(["main", "feature"]);
    expect(status?.ahead).toBe(2);
    expect(status?.behind).toBe(1);
    expect(status?.lastCommit).toContain("initial");
    expect(status?.changes).toEqual([{ status: " M", path: "src/App.tsx" }]);
    expect(isClean(status!)).toBe(false);
  });

  it("is absent for a backend that does not fill it, and that is not an error", () => {
    // `data` is optional by design (§14.1): a local-directory store's operations
    // carry none, and neither does a backend written later. No status is a state
    // the view has, not a failure it reports.
    expect(parseGitStatus(null)).toBeNull();
    expect(parseGitStatus(undefined)).toBeNull();
    expect(parseGitStatus("on branch main")).toBeNull();
    expect(parseGitStatus({ branch: "main" })).toBeNull();
  });

  it("drops what it cannot use rather than producing undefined fields", () => {
    const status = parseGitStatus(
      payload({
        branches: ["main", 7, null],
        ahead: "two",
        changes: [{ status: "??", path: "new.txt" }, { status: "??" }, "nonsense"],
      }),
    );
    expect(status?.branches).toEqual(["main"]);
    expect(status?.ahead).toBe(0);
    expect(status?.changes).toEqual([{ status: "??", path: "new.txt" }]);
  });

  it("reads a store that has never been cloned", () => {
    const status = parseGitStatus({ cloned: false });
    expect(status?.cloned).toBe(false);
    expect(status?.branch).toBe("");
    expect(status?.branches).toEqual([]);
  });
});

describe("the rows the view draws", () => {
  /** The rows of one porcelain code, as `(group, letter)` pairs. */
  function rows(status: string, path = "a.txt") {
    return changeRows(parseGitStatus(payload({ changes: [{ status, path }] }))!).map((row) => [
      row.group,
      row.letter,
    ]);
  }

  it("puts a change in the group its column says", () => {
    expect(rows("??")).toEqual([["unstaged", "U"]]);
    expect(rows(" M")).toEqual([["unstaged", "M"]]);
    expect(rows("M ")).toEqual([["staged", "M"]]);
    expect(rows("A ")).toEqual([["staged", "A"]]);
    expect(rows(" D")).toEqual([["unstaged", "D"]]);
    expect(rows("D ")).toEqual([["staged", "D"]]);
    expect(rows("R ")).toEqual([["staged", "R"]]);
    expect(rows("C ")).toEqual([["staged", "C"]]);
    expect(rows(" T")).toEqual([["unstaged", "T"]]);
  });

  it("puts a file that is staged and edited again in both groups at once", () => {
    // The whole reason there are two groups: one of these rows the next commit
    // takes, and one it does not.
    expect(rows("MM")).toEqual([
      ["staged", "M"],
      ["unstaged", "M"],
    ]);
    expect(rows("AM")).toEqual([
      ["staged", "A"],
      ["unstaged", "M"],
    ]);
    expect(rows("AD")).toEqual([
      ["staged", "A"],
      ["unstaged", "D"],
    ]);
  });

  it("puts a conflict in neither, and marks it with VS Code's own !", () => {
    for (const code of ["DD", "AU", "UD", "UA", "DU", "AA", "UU"]) {
      expect(rows(code)).toEqual([["merge", "!"]]);
    }
  });

  it("keeps a code it does not understand as a row rather than losing the file", () => {
    const row = changeRows(
      parseGitStatus(payload({ changes: [{ status: "XY", path: "a.txt" }] }))!,
    )[0];
    expect(row?.group).toBe("staged");
    expect(row?.label).toContain("Changed");
  });

  it("says what each letter means, and which files are gone", () => {
    const status = parseGitStatus(
      payload({
        changes: [
          { status: "??", path: "new.txt" },
          { status: "MM", path: "both.txt" },
          { status: " D", path: "gone.txt" },
        ],
      }),
    )!;
    const all = changeRows(status);
    expect(rowsIn(all, "staged").map((row) => row.path)).toEqual(["both.txt"]);
    expect(rowsIn(all, "unstaged").map((row) => row.path)).toEqual([
      "new.txt",
      "both.txt",
      "gone.txt",
    ]);
    expect(all.find((row) => row.path === "new.txt")?.label).toBe("Untracked");
    expect(rowsIn(all, "staged")[0]?.label).toContain("Staged");
    expect(all.filter((row) => row.deleted).map((row) => row.path)).toEqual(["gone.txt"]);
  });

  it("colours a letter the way VS Code's git view does", () => {
    const colourOf = (status: string, group: "staged" | "unstaged" | "merge") => {
      const row = changeRows(
        parseGitStatus(payload({ changes: [{ status, path: "a" }] }))!,
      ).find((candidate) => candidate.group === group)!;
      return rowColor(row);
    };
    expect(colourOf("??", "unstaged")).toContain("untracked");
    expect(colourOf("A ", "staged")).toContain("added");
    expect(colourOf(" M", "unstaged")).toBe("gitDecoration.modifiedResourceForeground");
    expect(colourOf("M ", "staged")).toBe("gitDecoration.stageModifiedResourceForeground");
    expect(colourOf(" D", "unstaged")).toBe("gitDecoration.deletedResourceForeground");
    expect(colourOf("D ", "staged")).toBe("gitDecoration.stageDeletedResourceForeground");
    expect(colourOf("UU", "merge")).toContain("conflicting");
  });

  it("gives a file one badge, preferring the working tree and then a conflict", () => {
    const of = (status: string) =>
      badgeRow(changeRows(parseGitStatus(payload({ changes: [{ status, path: "a" }] }))!));
    // Staged-added and edited since: the badge says what the file is now.
    expect(of("AM")?.group).toBe("unstaged");
    expect(of("A ")?.group).toBe("staged");
    expect(of("UU")?.group).toBe("merge");
    expect(badgeRow([])).toBeUndefined();
  });
});

describe("the branch indicator", () => {
  it("shows what there is to do, and nothing when there is nothing", () => {
    const clean = parseGitStatus(payload({ changes: [] }))!;
    expect(trackingSuffix(clean)).toBe("");
    expect(trackingSuffix(parseGitStatus(payload())!)).toBe("*");
    expect(trackingSuffix(parseGitStatus(payload({ changes: [], ahead: 3 }))!)).toBe("↑3");
    expect(trackingSuffix(parseGitStatus(payload({ changes: [], behind: 2, ahead: 1 }))!)).toBe(
      "↓2↑1",
    );
  });
});

describe("the operations", () => {
  it("commit takes the index, and reports the state it left", async () => {
    const { client, calls } = stubClient({
      output: "1 file changed",
      data: payload({ changes: [] }),
    });
    const git = new StoreGit("app-source", "store-id", client);

    const result = await git.commit("fix the header");
    expect(calls).toEqual([
      {
        id: "store-id",
        operation: "commit",
        // `staged_only`: a view with an index must not commit the rows the admin
        // left out of it.
        body: { input: { message: "fix the header", staged_only: true } },
      },
    ]);
    expect(result.output).toBe("1 file changed");
    expect(result.status?.changes).toEqual([]);
  });

  it("commitAll is the other answer, for a commit with nothing staged", async () => {
    const { client, calls } = stubClient();
    await new StoreGit("app-source", "store-id", client).commitAll("everything");
    expect(calls[0]?.body).toEqual({ input: { message: "everything", staged_only: false } });
  });

  it("stage and unstage name the paths, one per line", async () => {
    const { client, calls } = stubClient({ output: "", data: payload() });
    const git = new StoreGit("app-source", "store-id", client);

    await git.stage(["src/App.tsx", "notes/today.md"]);
    await git.unstage(["src/App.tsx"]);
    // No paths is every path, which is what the group's own button means.
    await git.stage([]);
    expect(calls.map((call) => [call.operation, call.body])).toEqual([
      ["stage", { input: { paths: "src/App.tsx\nnotes/today.md" } }],
      ["unstage", { input: { paths: "src/App.tsx" } }],
      ["stage", { input: { paths: "" } }],
    ]);
  });

  it("checkout says which branch, and whether to create it", async () => {
    const { client, calls } = stubClient({ output: "Switched to branch 'feature'" });
    const git = new StoreGit("app-source", "store-id", client);

    await git.checkout("feature", false);
    await git.checkout("new-thing", true);
    expect(calls.map((call) => call.body)).toEqual([
      { input: { branch: "feature", create: false } },
      { input: { branch: "new-thing", create: true } },
    ]);
  });

  it("pull, push and status ask for exactly their own operation", async () => {
    const { client, calls } = stubClient();
    const git = new StoreGit("app-source", "store-id", client);
    await git.pull();
    await git.push();
    await git.status();
    expect(calls.map((call) => call.operation)).toEqual(["pull", "push", "status"]);
  });

  it("keeps git's own words when it fails", async () => {
    const client = {
      runFileStoreOperation: vi.fn(() =>
        Promise.reject(
          new Error(
            "runFileStoreOperation failed: 400: git checkout failed: error: Your local changes to the following files would be overwritten by checkout:\n\tsrc/App.tsx",
          ),
        ),
      ),
    } as unknown as ApiClient;

    // The sentence naming the file is the entire value of that failure, so it
    // reaches the admin rather than being summarised into "checkout failed".
    await expect(
      new StoreGit("app-source", "id", client).checkout("feature", false),
    ).rejects.toThrow(/src\/App\.tsx/);
  });
});

describe("which stores have source control", () => {
  it("a git working copy does", () => {
    expect(storeGit(summary(), {} as ApiClient)).not.toBeNull();
  });

  it("a plain directory does not — not an empty view, none at all", () => {
    expect(storeGit(summary({ is_git_repo: false, backend: "local" }), {} as ApiClient)).toBeNull();
    expect(storeGit(summary({ is_git_repo: null }), {} as ApiClient)).toBeNull();
  });

  it("nor does a store with no id, which the operations are addressed by", () => {
    expect(storeGit(summary({ id: null }), {} as ApiClient)).toBeNull();
  });
});

describe("a commit message", () => {
  it("is refused when empty, without a request", () => {
    expect(commitMessageProblem("")).toContain("message");
    expect(commitMessageProblem("   \n ")).toContain("message");
    expect(commitMessageProblem("fix the header")).toBeNull();
  });
});

describe("a new branch's name", () => {
  it("accepts what git would", () => {
    expect(branchNameProblem("feature/logo", ["main"])).toBeNull();
    expect(branchNameProblem("  fix-123  ", ["main"])).toBeNull();
  });

  it("refuses what git would, before the round trip", () => {
    expect(branchNameProblem("", [])).toContain("name");
    expect(branchNameProblem("two words", [])).toContain("spaces");
    expect(branchNameProblem("wip..old", [])).toContain("..");
    expect(branchNameProblem("branch.lock", [])).toContain(".lock");
    expect(branchNameProblem("-dash", [])).toContain("-");
    expect(branchNameProblem("main", ["main", "feature"])).toContain("already exists");
  });
});
