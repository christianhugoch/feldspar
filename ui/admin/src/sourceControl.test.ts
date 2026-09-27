/**
 * The source-control panel's model: which rows land in which group with which
 * letter, and which button sits beside the commit message — the two things an
 * admin who knows VS Code would notice at once if they were wrong.
 */

import { describe, expect, it } from "vitest";

import {
  branchNameProblem,
  changeRows,
  commitInput,
  discardConfirmation,
  isScmOperation,
  parseScmStatus,
  primaryAction,
  rowsIn,
  splitPath,
  type ScmStatus,
} from "./sourceControl";

/** A cloned, clean, up-to-date working copy on `main`, with overrides. */
function status(over: Partial<ScmStatus> = {}): ScmStatus {
  return {
    cloned: true,
    canClone: false,
    branch: "main",
    branches: ["main"],
    upstream: true,
    ahead: 0,
    behind: 0,
    lastCommit: "abc123 initial (Seed, 1 day ago)",
    changes: [],
    ...over,
  };
}

describe("parseScmStatus", () => {
  it("reads the git backend's payload", () => {
    const parsed = parseScmStatus({
      cloned: true,
      can_clone: false,
      branch: "main",
      branches: ["main", "feature", 7],
      upstream: true,
      ahead: 2,
      behind: 0,
      last_commit: "abc123 initial",
      changes: [{ status: " M", path: "README.md" }, { status: "??" }],
    });
    expect(parsed).toEqual({
      cloned: true,
      canClone: false,
      branch: "main",
      branches: ["main", "feature"],
      upstream: true,
      ahead: 2,
      behind: 0,
      lastCommit: "abc123 initial",
      changes: [{ status: " M", path: "README.md" }],
    });
  });

  it("is not fooled into drawing a panel for a backend without one", () => {
    expect(parseScmStatus(null)).toBeNull();
    expect(parseScmStatus("On branch main")).toBeNull();
    expect(parseScmStatus({ branch: "main" })).toBeNull();
  });

  it("carries whether the directory may be cloned into", () => {
    expect(parseScmStatus({ cloned: false, can_clone: true })?.canClone).toBe(true);
    expect(parseScmStatus({ cloned: false })?.canClone).toBe(false);
  });
});

describe("changeRows", () => {
  const rows = changeRows(
    status({
      changes: [
        { status: " M", path: "src/app.ts" },
        { status: "A ", path: "notes/new.md" },
        { status: "??", path: "scratch.txt" },
        { status: "MM", path: "README.md" },
        { status: " D", path: "old.txt" },
        { status: "UU", path: "conflict.ts" },
      ],
    }),
  );

  it("puts each change in VS Code's group with VS Code's letter", () => {
    const summary = (group: "merge" | "staged" | "unstaged") =>
      rowsIn(rows, group).map((row) => `${row.letter} ${row.path}`);
    expect(summary("staged")).toEqual(["A notes/new.md", "M README.md"]);
    expect(summary("unstaged")).toEqual([
      "M src/app.ts",
      "U scratch.txt",
      "M README.md",
      "D old.txt",
    ]);
    expect(summary("merge")).toEqual(["! conflict.ts"]);
  });

  it("marks what discarding would delete, and what is gone", () => {
    expect(rows.find((r) => r.path === "scratch.txt")?.untracked).toBe(true);
    expect(rows.find((r) => r.path === "src/app.ts")?.untracked).toBe(false);
    expect(rows.find((r) => r.path === "old.txt")?.deleted).toBe(true);
  });
});

describe("primaryAction", () => {
  const dirty = status({ changes: [{ status: " M", path: "a.txt" }] });

  it("is Commit while there are changes, once there is a message", () => {
    expect(primaryAction(dirty, "")).toEqual({
      kind: "commit",
      blocked: "Type a commit message first.",
    });
    expect(primaryAction(dirty, "  fix  ")).toEqual({ kind: "commit", blocked: null });
  });

  it("turns into Push once the commit has landed", () => {
    expect(primaryAction(status({ ahead: 1 }), "").kind).toBe("push");
  });

  it("pulls first when the remote has moved on", () => {
    expect(primaryAction(status({ ahead: 1, behind: 2 }), "").kind).toBe("pull");
  });

  it("publishes a branch that has no upstream yet", () => {
    expect(primaryAction(status({ upstream: false, branch: "topic" }), "").kind).toBe(
      "publish",
    );
    // Nothing to publish in a repository with no commits.
    expect(primaryAction(status({ upstream: false, lastCommit: "" }), "").blocked).not.toBeNull();
  });

  it("has nothing to do on a clean, up-to-date copy", () => {
    expect(primaryAction(status(), "message")).toEqual({
      kind: "commit",
      blocked: "There are no changes to commit.",
    });
  });
});

describe("commitInput", () => {
  it("commits only what is staged when anything is", () => {
    const s = status({
      changes: [
        { status: "M ", path: "a.txt" },
        { status: " M", path: "b.txt" },
      ],
    });
    expect(commitInput(s, " msg ")).toEqual({ message: "msg", staged_only: true });
  });

  it("commits everything when nothing is staged", () => {
    const s = status({ changes: [{ status: " M", path: "b.txt" }] });
    expect(commitInput(s, "msg").staged_only).toBe(false);
  });
});

describe("discardConfirmation", () => {
  const [edited, untracked] = changeRows(
    status({
      changes: [
        { status: " M", path: "a.txt" },
        { status: "??", path: "b.txt" },
      ],
    }),
  );

  it("says a new file will be deleted, not reverted", () => {
    expect(discardConfirmation([edited])).toContain("Discard the changes to a.txt");
    expect(discardConfirmation([untracked])).toContain("Delete b.txt");
    expect(discardConfirmation([edited, untracked])).toContain("1 untracked file(s) will be deleted");
  });
});

describe("branchNameProblem", () => {
  it("accepts a good name and refuses the ones git would", () => {
    expect(branchNameProblem("feature/login", ["main"])).toBeNull();
    expect(branchNameProblem("", ["main"])).not.toBeNull();
    expect(branchNameProblem("has space", ["main"])).not.toBeNull();
    expect(branchNameProblem("a..b", ["main"])).not.toBeNull();
    expect(branchNameProblem("main", ["main"])).toBe("main already exists.");
  });
});

describe("helpers", () => {
  it("splits a path into name and directory", () => {
    expect(splitPath("src/lib/app.ts")).toEqual({ name: "app.ts", dir: "src/lib" });
    expect(splitPath("README.md")).toEqual({ name: "README.md", dir: "" });
  });

  it("knows which operations the panel draws", () => {
    expect(isScmOperation("discard")).toBe(true);
    expect(isScmOperation("generate_deploy_key")).toBe(false);
  });
});
