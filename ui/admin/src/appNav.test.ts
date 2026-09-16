/**
 * The Applications section of the sidebar: which links the current application
 * gets, and which of them is "here".
 */

import { describe, expect, it } from "vitest";

import { appIdFromRoute, appNavLinks, builderAgentFor, linkActive, onApplicationsList } from "./appNav";

const react = {
  id: "a1",
  subdomain: "todo",
  builds: true,
  has_views: false,
  source: { store: "code", path: "todo" },
};

const saltcornUi = { id: "a2", subdomain: "crm", builds: false, has_views: true, source: null };

const builder = {
  name: "build-todo",
  traits: [{ trait: "build_application", config: { application: "todo" } }],
};

describe("the current application's sidebar links", () => {
  it("gives a built application edit code, update client and build, then settings", () => {
    const links = appNavLinks(react, null);
    expect(links.map((l) => l.label)).toEqual(["Edit code", "Update client", "Build", "Settings"]);
    const edit = links[0];
    expect(edit.href).toBe("/ide/?store=code");
    expect(edit.external).toBe(true);
    // The two that do something rather than go somewhere have nowhere to go.
    expect(links[1].href).toBeUndefined();
    expect(links[2].href).toBeUndefined();
  });

  it("adds a new chat when the application has a coding agent", () => {
    const labels = appNavLinks(react, "build-todo").map((l) => l.label);
    expect(labels).toEqual(["Edit code", "Update client", "Build", "New chat", "Settings"]);
    expect(appNavLinks(react, "build-todo")[3].href).toBe("#/agents/build-todo/chat");
  });

  it("gives a Saltcorn UI application views, pages and library, then settings", () => {
    expect(appNavLinks(saltcornUi, null).map((l) => l.label)).toEqual([
      "Views",
      "Pages",
      "Library",
      "Settings",
    ]);
  });

  it("always ends with settings, and never offers a delete", () => {
    for (const app of [react, saltcornUi]) {
      for (const agent of [null, "x"]) {
        const links = appNavLinks(app, agent);
        expect(links[links.length - 1].id).toBe("settings");
        expect(links.map((l) => l.label.toLowerCase())).not.toContain("delete");
      }
    }
  });

  it("lights up settings for both of an application's settings tabs", () => {
    const settings = appNavLinks(saltcornUi, null).find((l) => l.id === "settings")!;
    expect(linkActive(settings, "/applications/a2/edit")).toBe(true);
    expect(linkActive(settings, "/applications/a2/app-settings")).toBe(true);
    expect(linkActive(settings, "/applications/a1/edit")).toBe(false);
  });

  it("lights up views for a view's editor, but not pages", () => {
    const links = appNavLinks(saltcornUi, null);
    const views = links.find((l) => l.id === "views")!;
    const pages = links.find((l) => l.id === "pages")!;
    expect(linkActive(views, "/applications/a2/views/orders_list")).toBe(true);
    expect(linkActive(pages, "/applications/a2/views/orders_list")).toBe(false);
    expect(linkActive(pages, "/applications/a2/pages/home/properties")).toBe(true);
  });
});

describe("finding an application's coding agent", () => {
  it("is the agent whose build trait names the application's subdomain", () => {
    expect(builderAgentFor(react, [builder])).toBe("build-todo");
    expect(builderAgentFor(saltcornUi, [builder])).toBeNull();
  });

  it("goes by the trait, not the conventional name", () => {
    const impostor = { name: "build-todo", traits: [{ trait: "subagent", config: {} }] };
    const renamed = { ...builder, name: "todo-coder" };
    expect(builderAgentFor(react, [impostor])).toBeNull();
    expect(builderAgentFor(react, [impostor, renamed])).toBe("todo-coder");
  });
});

describe("which application a route is about", () => {
  it("is the one in an application's own screens", () => {
    expect(appIdFromRoute("/applications/a1/edit")).toBe("a1");
    expect(appIdFromRoute("/applications/a%20b/views/x")).toBe("a b");
  });

  it("is none for the list or the new-application form", () => {
    expect(appIdFromRoute("/applications")).toBeNull();
    expect(appIdFromRoute("/applications/new")).toBeNull();
    expect(appIdFromRoute("/tables")).toBeNull();
    expect(onApplicationsList("/applications")).toBe(true);
    expect(onApplicationsList("/applications/new")).toBe(true);
    expect(onApplicationsList("/applications/a1/edit")).toBe(false);
  });
});
