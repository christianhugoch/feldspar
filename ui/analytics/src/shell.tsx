// What the shell is (analytics TODO A9.3): the unrestricted Analytics UI on the
// admin host, or an Analytics application on its own subdomain — the server
// says which (`analyticsShell`), and every screen that would offer something
// an application does not asks here first.
//
// The server enforces all of it whatever this says (A9.4); this only keeps the
// screen from offering what would be refused.

import { createContext, useContext } from "react";

import type { Route } from "./router";

/** A workspace a fixed application shows. */
export type ShownWorkspace = { id: string; name: string; kind: string };

/** An Analytics application, as the shell is told about it. */
export type AppInfo = {
  id: string;
  name: string;
  mode: "fixed" | "self_serve";
  /** Whether its users may create and edit datasets. */
  datasetEditor: boolean;
  /** The kinds of workspace it opens. */
  workspaceKinds: string[];
  /** Whether its users may create workspaces. */
  createWorkspaces: boolean;
  /** Fixed mode: the workspaces it shows, in order. */
  workspaces: ShownWorkspace[];
};

/** A role, for sharing. */
export type RoleInfo = { role: number; name: string };

/** What the shell is. */
export type ShellInfo = {
  /** The application; `null` for the unrestricted Analytics UI. */
  application: AppInfo | null;
  roles: RoleInfo[];
};

const UNRESTRICTED: ShellInfo = { application: null, roles: [] };

const ShellContext = createContext<ShellInfo>(UNRESTRICTED);

export const ShellProvider = ShellContext.Provider;

/** What the shell is. */
export function useShell(): ShellInfo {
  return useContext(ShellContext);
}

/** The application `analyticsShell` describes, or `null` for none. */
export function readApplication(raw: unknown): AppInfo | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const strings = (v: unknown): string[] =>
    Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  const workspaces = Array.isArray(r.workspaces)
    ? r.workspaces.flatMap((w): ShownWorkspace[] => {
        const ws = (w ?? {}) as Record<string, unknown>;
        return typeof ws.id === "string" && typeof ws.name === "string" && typeof ws.kind === "string"
          ? [{ id: ws.id, name: ws.name, kind: ws.kind }]
          : [];
      })
    : [];
  return {
    id: typeof r.id === "string" ? r.id : "",
    name: typeof r.name === "string" ? r.name : "",
    mode: r.mode === "self_serve" ? "self_serve" : "fixed",
    datasetEditor: r.dataset_editor === true,
    workspaceKinds: strings(r.workspace_kinds),
    createWorkspaces: r.create_workspaces === true,
    workspaces,
  };
}

/** The screen a route opens on, in `app`: the route itself, or — in a fixed
 * application showing one workspace — that workspace for the front page. */
export function landing(app: AppInfo | null, route: Route): Route {
  if (app?.mode === "fixed" && route.name === "home" && app.workspaces.length === 1) {
    return { name: "workspace", id: app.workspaces[0].id };
  }
  return route;
}

/** Whether `app` shows the screen `route` names. The unrestricted UI shows
 * everything; an application never shows a model; a fixed one shows its
 * workspaces and the front page listing them, and no dataset. */
export function shows(app: AppInfo | null, route: Route): boolean {
  if (!app) return true;
  switch (route.name) {
    case "home":
    case "notFound":
      return true;
    case "workspace":
      return app.mode === "self_serve" || app.workspaces.some((w) => w.id === route.id);
    case "dataset":
      return app.mode === "self_serve";
    case "newDataset":
      return app.mode === "self_serve" && app.datasetEditor;
    case "model":
    case "newModel":
    case "compareModels":
    case "fit":
      return false;
  }
}
