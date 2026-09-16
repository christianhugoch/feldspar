// What an application is doing right now: building, having its generated code
// rewritten, and the news the last of those left behind.
//
// Both are started from two places — a row on the applications list and the
// current application's links in the sidebar — and an admin who presses Build in
// the sidebar and then opens the list should see the same build in progress
// there, not a row claiming the app was never built. So the state lives here,
// outside any screen, the way popped-out chats do (`chatWindows.ts`), and a build
// outlives the screen it was started from: it finishes, and says so, wherever the
// admin has got to by then.
//
// The server keeps no persisted build status (design §13.2), so this is the only
// record of one: a freshly loaded app is **not built yet** rather than pretending
// a save deployed anything. As in `chatWindows.ts`, the transitions are pure and
// the store is a thin shell around them, so the interesting part is testable
// without a browser.

import { useSyncExternalStore } from "react";

import { api, errorMessage } from "./api";
import type { ListApplicationsResponse } from "./client";
import type { Notice } from "./notice";

type AppItem = ListApplicationsResponse[number];

/** Per-app build state. */
export type BuildStatus = "unbuilt" | "building" | "built" | "failed";

export type AppActionsState = {
  /** Build state by application id; an app with no entry is `unbuilt`. */
  build: Record<string, BuildStatus>;
  /** Applications whose generated code is being rewritten. Separate from the
   * build status: regenerating does not build, so it must not claim an app is
   * built — nor forget that it was. */
  updating: Record<string, true>;
  /** The most recent outcome, shown until dismissed. */
  outcome: Notice | null;
  /** Bumped whenever the set of applications (or their agents) changes without
   * a route change, so the sidebar knows to fetch them again. */
  version: number;
};

export const INITIAL_APP_ACTIONS: AppActionsState = {
  build: {},
  updating: {},
  outcome: null,
  version: 0,
};

/** The build state of one application. */
export function buildStatus(state: AppActionsState, appId: string): BuildStatus {
  return state.build[appId] ?? "unbuilt";
}

/** A build has started: the app is building, and the previous news is stale. */
export function buildStarted(state: AppActionsState, appId: string): AppActionsState {
  return { ...state, build: { ...state.build, [appId]: "building" }, outcome: null };
}

/** A build has finished, one way or the other. */
export function buildFinished(
  state: AppActionsState,
  app: Pick<AppItem, "id" | "name">,
  result: { ok: true; log: string } | { ok: false; error: string },
): AppActionsState {
  return {
    ...state,
    build: { ...state.build, [app.id]: result.ok ? "built" : "failed" },
    outcome: result.ok
      ? {
          ok: true,
          title: `Build succeeded — ${app.name}`,
          text: result.log.trim() || "Build succeeded.",
        }
      : { ok: false, title: `Build failed — ${app.name}`, text: result.error },
  };
}

/** A rewrite of the generated code has started. */
export function updateStarted(state: AppActionsState, appId: string): AppActionsState {
  return { ...state, updating: { ...state.updating, [appId]: true }, outcome: null };
}

/** A rewrite of the generated code has finished. It says whether the server
 * rewrote the generated files or rescaffolded an emptied project, because they
 * are not the same news. */
export function updateFinished(
  state: AppActionsState,
  app: Pick<AppItem, "id" | "name">,
  result: { ok: true; scaffolded: boolean; log: string } | { ok: false; error: string },
): AppActionsState {
  const updating = { ...state.updating };
  delete updating[app.id];
  return {
    ...state,
    updating,
    outcome: result.ok
      ? {
          ok: true,
          title: result.scaffolded
            ? `Project scaffolded — ${app.name}`
            : `Generated code updated — ${app.name}`,
          text: result.log,
        }
      : {
          ok: false,
          title: `Could not update the generated code — ${app.name}`,
          text: result.error,
        },
  };
}

/* ------------------------------------------------------------------ *
 * The store.
 * ------------------------------------------------------------------ */

let state: AppActionsState = INITIAL_APP_ACTIONS;
const listeners = new Set<() => void>();

function publish(next: AppActionsState): void {
  if (next === state) return;
  state = next;
  for (const listener of listeners) listener();
}

function current(): AppActionsState {
  return state;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Subscribe a component to builds, updates and their outcome. */
export function useAppActions(): AppActionsState {
  return useSyncExternalStore(subscribe, current, current);
}

/** Build (and mount) an application. */
export async function buildApplication(app: AppItem): Promise<void> {
  publish(buildStarted(state, app.id));
  try {
    const report = await api.buildApplication(app.id);
    publish(buildFinished(state, app, { ok: true, log: report.log }));
  } catch (err) {
    publish(buildFinished(state, app, { ok: false, error: errorMessage(err, "The build failed.") }));
  }
}

/** Rewrite an application's generated code (`src/feldspar/**`) without building
 * it. The server does this by itself whenever the API definition changes, so this
 * is the "now, please" case: a store that was unreachable when a table changed, or
 * a project directory that was emptied — which the server rescaffolds. */
export async function updateApplicationClient(app: AppItem): Promise<void> {
  publish(updateStarted(state, app.id));
  try {
    const report = await api.updateApplicationClient(app.id);
    publish(updateFinished(state, app, { ok: true, scaffolded: report.scaffolded, log: report.log }));
  } catch (err) {
    publish(
      updateFinished(state, app, {
        ok: false,
        error: errorMessage(err, "The generated code could not be rewritten."),
      }),
    );
  }
}

/** Show a message about an application (a scaffold, a deletion). */
export function showAppOutcome(outcome: Notice | null): void {
  publish({ ...state, outcome });
}

/** Say that applications or agents were added, removed or renamed. */
export function noteApplicationsChanged(): void {
  publish({ ...state, version: state.version + 1 });
}
