// The restricted shell's rules (analytics TODO A9.3): what `analyticsShell`
// says is read safely, a fixed application with one workspace opens on it, and
// an application's shell does not offer what it does not show.

import { describe, expect, it } from "vitest";

import type { Route } from "./router";
import { shareableRoles } from "./share";
import { landing, readApplication, shows, type AppInfo } from "./shell";

const fixed: AppInfo = {
  id: "app",
  name: "Board",
  mode: "fixed",
  datasetEditor: false,
  workspaceKinds: ["dashboard"],
  createWorkspaces: false,
  workspaces: [{ id: "w1", name: "Incidents", kind: "dashboard" }],
};

const selfServe: AppInfo = {
  ...fixed,
  mode: "self_serve",
  datasetEditor: true,
  workspaceKinds: ["data_explorer"],
  createWorkspaces: true,
  workspaces: [],
};

describe("the shell", () => {
  it("reads what the server says, and nothing for the unrestricted UI", () => {
    expect(readApplication(null)).toBeNull();
    expect(
      readApplication({
        id: "app",
        name: "Board",
        mode: "fixed",
        dataset_editor: false,
        workspace_kinds: ["dashboard"],
        create_workspaces: false,
        workspaces: [{ id: "w1", name: "Incidents", kind: "dashboard" }, { id: 3 }],
      }),
    ).toEqual(fixed);
    // Anything it does not recognise grants nothing.
    expect(readApplication({ mode: "everything", dataset_editor: "yes" })).toMatchObject({
      mode: "fixed",
      datasetEditor: false,
      workspaces: [],
    });
  });

  it("opens a fixed application showing one workspace on that workspace", () => {
    const home: Route = { name: "home" };
    expect(landing(fixed, home)).toEqual({ name: "workspace", id: "w1" });
    const two = { ...fixed, workspaces: [...fixed.workspaces, { id: "w2", name: "Report", kind: "report" }] };
    expect(landing(two, home)).toEqual(home);
    expect(landing(selfServe, home)).toEqual(home);
    expect(landing(null, home)).toEqual(home);
  });

  it("shows only what the application allows", () => {
    const routes: Route[] = [
      { name: "workspace", id: "w1" },
      { name: "workspace", id: "other" },
      { name: "dataset", id: "d" },
      { name: "newDataset", table: null },
      { name: "model", id: "m" },
      { name: "fit", id: "f" },
    ];
    expect(routes.map((r) => shows(null, r))).toEqual([true, true, true, true, true, true]);
    expect(routes.map((r) => shows(fixed, r))).toEqual([true, false, false, false, false, false]);
    expect(routes.map((r) => shows(selfServe, r))).toEqual([true, true, true, true, false, false]);
    expect(shows({ ...selfServe, datasetEditor: false }, { name: "newDataset", table: null })).toBe(false);
  });

  it("shares with the roles below the admin, most privileged first", () => {
    expect(
      shareableRoles([
        { role: 100, name: "Public" },
        { role: 1, name: "Admin" },
        { role: 40, name: "Staff" },
      ]),
    ).toEqual([
      { role: 40, name: "Staff" },
      { role: 100, name: "Public" },
    ]);
  });
});
