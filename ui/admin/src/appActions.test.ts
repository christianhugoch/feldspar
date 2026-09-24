/**
 * Build and client-update state shared by the applications list and the sidebar.
 */

import { describe, expect, it } from "vitest";

import {
  INITIAL_APP_ACTIONS,
  buildFinished,
  buildStarted,
  buildStatus,
  updateFinished,
  updateStarted,
} from "./appActions";

const app = { id: "a1", name: "Todo" };

describe("application builds", () => {
  it("is not built until a build succeeds", () => {
    let state = INITIAL_APP_ACTIONS;
    expect(buildStatus(state, "a1")).toBe("unbuilt");
    state = buildStarted(state, "a1");
    expect(buildStatus(state, "a1")).toBe("building");
    state = buildFinished(state, app, { ok: true, log: "  done\n" });
    expect(buildStatus(state, "a1")).toBe("built");
    expect(state.outcome).toEqual({ ok: true, title: "Build succeeded — Todo", text: "done" });
  });

  it("clears the previous news when a new build starts, and reports a failure", () => {
    let state = buildFinished(INITIAL_APP_ACTIONS, app, { ok: true, log: "" });
    expect(state.outcome?.text).toBe("Build succeeded.");
    state = buildStarted(state, "a1");
    expect(state.outcome).toBeNull();
    state = buildFinished(state, app, { ok: false, error: "tsc: 2 errors" });
    expect(buildStatus(state, "a1")).toBe("failed");
    expect(state.outcome).toEqual({ ok: false, title: "Build failed — Todo", text: "tsc: 2 errors" });
  });

  it("names a deep clean as one, while ending in the same build state", () => {
    let state = buildStarted(INITIAL_APP_ACTIONS, "a1");
    state = buildFinished(state, app, { ok: true, log: "Deleted node_modules; reinstalling." }, "Deep clean");
    expect(buildStatus(state, "a1")).toBe("built");
    expect(state.outcome?.title).toBe("Deep clean succeeded — Todo");
    state = buildFinished(state, app, { ok: false, error: "npm ERR! 404" }, "Deep clean");
    expect(buildStatus(state, "a1")).toBe("failed");
    expect(state.outcome?.title).toBe("Deep clean failed — Todo");
  });
});

describe("client updates", () => {
  it("does not touch the build status, and says whether it scaffolded", () => {
    let state = buildFinished(INITIAL_APP_ACTIONS, app, { ok: true, log: "ok" });
    state = updateStarted(state, "a1");
    expect(state.updating.a1).toBe(true);
    state = updateFinished(state, app, { ok: true, scaffolded: true, log: "wrote 3 files" });
    expect(state.updating.a1).toBeUndefined();
    expect(buildStatus(state, "a1")).toBe("built");
    expect(state.outcome?.title).toBe("Project scaffolded — Todo");
    state = updateFinished(updateStarted(state, "a1"), app, { ok: true, scaffolded: false, log: "" });
    expect(state.outcome?.title).toBe("Generated code updated — Todo");
  });
});
