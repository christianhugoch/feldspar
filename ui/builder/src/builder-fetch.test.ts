// @vitest-environment jsdom
//
// `builderFetch` and the link listener: a mapped URL becomes its typed-client
// call, a refused one its sentence, and an unknown one a refusal naming it, with
// nothing reaching the network.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { builderFetch } from "./builder-fetch";
import type { ApiClient } from "./client";
import { setBuilderContext, type BuilderTarget } from "./context";
import { decideLink, installLinkListener } from "./links";
import { ROUTES } from "./routes";

type Call = [string, unknown[]];

function fakeClient(calls: Call[], answers: Record<string, unknown> = {}): ApiClient {
  return new Proxy({} as ApiClient, {
    get: (_target, op: string) =>
      async (...args: unknown[]) => {
        calls.push([op, args]);
        const answer = answers[op];
        if (answer instanceof Error) throw answer;
        return answer ?? {};
      },
  });
}

const VIEW: BuilderTarget = { kind: "view", name: "Show Books", step: 2 };
const PAGE: BuilderTarget = { kind: "page", name: "Home" };

function context(target: BuilderTarget, calls: Call[], answers: Record<string, unknown> = {}) {
  const ctx = {
    application: "app-1",
    applicationOrigin: "http://booksdb.localhost:3032",
    target,
    client: fakeClient(calls, answers),
  };
  setBuilderContext(ctx);
  return ctx;
}

const post = (body: unknown): RequestInit => ({ method: "POST", body: JSON.stringify(body) });

describe("builderFetch", () => {
  afterEach(() => setBuilderContext(null));

  it("saves a view's layout at the step it is building, without the builder's node ids", async () => {
    const calls: Call[] = [];
    context(VIEW, calls);
    const res = await builderFetch(
      "/viewedit/savebuilder/7",
      post({
        layout: { above: [] },
        columns: [{ type: "Field" }],
        libraryUpdates: [{ library_id: "lib-1", layout: { type: "blank" }, node_id: "n1" }],
      }),
    );
    expect(await res.json()).toEqual({ success: "ok" });
    expect(calls).toEqual([
      [
        "saveViewLayout",
        [
          "app-1",
          "Show Books",
          {
            step: 2,
            layout: { above: [] },
            columns: [{ type: "Field" }],
            libraryUpdates: [{ library_id: "lib-1", layout: { type: "blank" } }],
          },
        ],
      ],
    ]);
  });

  it("saves a page's layout", async () => {
    const calls: Call[] = [];
    context(PAGE, calls);
    await builderFetch("/pageedit/savebuilder/3", post({ layout: { above: [] }, libraryUpdates: [] }));
    expect(calls).toEqual([["savePageLayout", ["app-1", "Home", { layout: { above: [] }, libraryUpdates: [] }]]]);
  });

  it("refuses a view save from a page's builder, naming the page", async () => {
    context(PAGE, []);
    const res = await builderFetch("/viewedit/savebuilder/3", post({ layout: {} }));
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "this builder edits the page Home, not a view" });
  });

  it("answers a refused save with the server's sentence, which the autosave shows", async () => {
    const calls: Call[] = [];
    context(VIEW, calls, { saveViewLayout: new Error("saveViewLayout failed: 400: unknown action Foo") });
    const res = await builderFetch("/viewedit/savebuilder/7", post({ layout: {}, columns: [] }));
    expect(await res.json()).toEqual({ error: "saveViewLayout failed: 400: unknown action Foo" });
  });

  it("answers the previews as HTML and the lookups as v1's JSON", async () => {
    const calls: Call[] = [];
    context(VIEW, calls, {
      builderFieldPreview: { html: "<b>Dune</b>" },
      builderViewPreview: { html: "<table></table>" },
      builderDistinctValues: { success: ["a", "b"] },
      builderFieldviewConfigForm: [{ name: "format" }],
      getLibraryItem: { id: "lib-1", name: "Book header", layout: {} },
    });
    expect(await (await builderFetch("/field/preview/Books/title/as_text", post({ configuration: {} }))).text()).toBe(
      "<b>Dune</b>",
    );
    expect(await (await builderFetch("/view/List%20Books/preview", post({ author: 1 }))).text()).toBe("<table></table>");
    expect(await (await builderFetch("/api/Books/distinct/author")).json()).toEqual({ success: ["a", "b"] });
    expect(
      await (await builderFetch("/field/fieldviewcfgform/Books?accept=json", post({ field_name: "title", fieldview: "as_text" }))).json(),
    ).toEqual([{ name: "format" }]);
    expect(await (await builderFetch("/library/content/lib-1")).json()).toMatchObject({ name: "Book header" });
    expect(calls.map(([op, args]) => [op, args.slice(1)])).toEqual([
      ["builderFieldPreview", [{ table: "Books", field: "title", fieldview: "as_text", configuration: {}, row_id: null }]],
      ["builderViewPreview", [{ view: "List Books", state: { author: 1 } }]],
      ["builderDistinctValues", ["Books", "author"]],
      ["builderFieldviewConfigForm", [{ table: "Books", field_name: "title", fieldview: "as_text" }]],
      ["getLibraryItem", ["lib-1"]],
    ]);
  });

  it("refuses a refused feature with its sentence, calling nothing", async () => {
    const calls: Call[] = [];
    context(VIEW, calls);
    const upload = ROUTES.find((r) => r.path === "/files/upload")!;
    const res = await builderFetch("/files/upload", { method: "POST", body: new FormData() });
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: upload.says });
    expect(calls).toEqual([]);
  });

  it("refuses an unknown URL and another origin by name, and never reaches the network", async () => {
    const calls: Call[] = [];
    context(VIEW, calls);
    const network = vi.spyOn(globalThis, "fetch");
    const errors = vi.spyOn(console, "error").mockImplementation(() => {});
    for (const url of ["/viewedit/delete/7", "https://cdn.example.com/x.js", "/viewedit/config/Show%20Books"]) {
      const res = await builderFetch(url);
      expect(res.status).toBe(400);
      expect(((await res.json()) as { error: string }).error).toContain(url);
    }
    expect(calls).toEqual([]);
    expect(network).not.toHaveBeenCalled();
    expect(errors).toHaveBeenCalledTimes(3);
    errors.mockRestore();
    network.mockRestore();
  });
});

describe("the link listener", () => {
  let remove = () => {};
  beforeEach(() => {
    context(VIEW, []);
  });
  afterEach(() => {
    remove();
    document.body.innerHTML = "";
    setBuilderContext(null);
  });

  it("maps v1's admin and application links", () => {
    const ctx = context(VIEW, []);
    expect(decideLink("/viewedit/config/Show%20Books", ctx)).toEqual({
      kind: "navigate",
      url: "/#/applications/app-1/views/Show%20Books",
    });
    expect(decideLink("/pageedit/edit/Home", ctx)).toEqual({ kind: "navigate", url: "/builder/applications/app-1/pages/Home" });
    expect(decideLink("/view/Show%20Books?id=1", ctx)).toEqual({
      kind: "navigate",
      url: "http://booksdb.localhost:3032/view/Show%20Books?id=1",
    });
    expect(decideLink("https://wiki.saltcorn.com/view/ShowPage/formulas", ctx)).toEqual({ kind: "browser" });
    expect(decideLink("#", ctx)).toEqual({ kind: "browser" });
    expect(decideLink("/viewedit/delete/7", ctx)).toMatchObject({ kind: "refused" });
  });

  it("disables a refused link with its sentence, and a click on it goes nowhere", async () => {
    const notifyAlert = vi.fn();
    (window as unknown as { notifyAlert: unknown }).notifyAlert = notifyAlert;
    const assign = vi.fn();
    vi.spyOn(window, "location", "get").mockReturnValue({ ...window.location, assign, href: window.location.href });
    document.body.innerHTML = `<a id="cfg" href="/actions/configure/AddBook">Configure</a>`;
    remove = installLinkListener(document);
    const link = document.getElementById("cfg") as HTMLAnchorElement;
    const sentence = ROUTES.find((r) => r.path === "/actions/configure/:name")!.says;
    expect(link.getAttribute("aria-disabled")).toBe("true");
    expect(link.title).toBe(sentence);

    // A link the builder renders later is marked too.
    const later = document.createElement("a");
    later.href = "/admin/help/Formulas";
    document.body.appendChild(later);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(later.getAttribute("aria-disabled")).toBe("true");

    const click = new MouseEvent("click", { bubbles: true, cancelable: true });
    link.dispatchEvent(click);
    expect(click.defaultPrevented).toBe(true);
    expect(notifyAlert).toHaveBeenCalledWith({ type: "warning", text: sentence });
    expect(assign).not.toHaveBeenCalled();
    vi.restoreAllMocks();
  });

  it("opens a mapped link in a new tab when it asks for one", () => {
    const open = vi.spyOn(window, "open").mockImplementation(() => null);
    document.body.innerHTML = `<a id="v" target="_blank" href="/viewedit/config/List%20Books">List Books</a>`;
    remove = installLinkListener(document);
    const click = new MouseEvent("click", { bubbles: true, cancelable: true });
    document.getElementById("v")!.dispatchEvent(click);
    expect(click.defaultPrevented).toBe(true);
    expect(open).toHaveBeenCalledWith("/#/applications/app-1/views/List%20Books", "_blank", "noopener");
    open.mockRestore();
  });
});
