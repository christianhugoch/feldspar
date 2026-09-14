// @vitest-environment jsdom
//
// The builder route's document, read by the bundle (TODO "The builder" 8.2).

import { afterEach, describe, expect, it, vi } from "vitest";

import { BOOT_ELEMENT_ID, bootFromDocument, readBootData, type BootData } from "./boot";
import type { ApiClient } from "./client";

/** What `sc-server`'s `builder.rs` writes: JSON with `<`, `>` and `&` escaped,
 * so a layout's text cannot close the element. */
function scriptJson(value: unknown): string {
  return JSON.stringify(value).replace(/</g, "\\u003c").replace(/>/g, "\\u003e").replace(/&/g, "\\u0026");
}

const BOOT: BootData = {
  application: "app-1",
  applicationName: "BooksDB",
  applicationOrigin: "http://booksdb.localhost:3032",
  target: { kind: "view", name: "Show Books", step: 0 },
  stepCount: 1,
  stepName: "Layout",
  csrfToken: "token-1",
  lightmode: "light",
  options: { mode: "show", fields: [] },
  layout: { type: "blank", contents: "</script><b>& more</b>" },
  mode: "show",
  afterSave: "/#/applications/app-1/views",
};

function builderPage(boot: string | null): void {
  document.body.innerHTML = `
    ${boot === null ? "" : `<script type="application/json" id="${BOOT_ELEMENT_ID}">${boot}</script>`}
    <div id="builder-header-actions"></div>
    <div id="saltcorn-builder"></div>
    <form id="scbuildform" method="post"><input type="hidden" name="columns"><input type="hidden" name="layout"></form>`;
}

afterEach(() => {
  document.body.innerHTML = "";
});

describe("the builder's boot data", () => {
  it("is absent from a document the route did not render", () => {
    builderPage(null);
    expect(readBootData(document)).toBeNull();
    expect(bootFromDocument(document, vi.fn(), {} as ApiClient)).toBe(false);
  });

  it("reads back what the route wrote, text that looks like markup included", () => {
    builderPage(scriptJson(BOOT));
    expect(readBootData(document)).toEqual(BOOT);
  });

  it("names its element when it is not JSON", () => {
    builderPage("{ not json");
    expect(() => readBootData(document)).toThrow(`#${BOOT_ELEMENT_ID}`);
  });

  it("starts the builder in its container and takes over the form", async () => {
    builderPage(scriptJson(BOOT));
    const start = vi.fn();
    const saveViewLayout = vi.fn().mockResolvedValue({});
    const client = { saveViewLayout } as unknown as ApiClient;

    expect(bootFromDocument(document, start, client)).toBe(true);

    expect(start).toHaveBeenCalledWith(
      {
        containerId: "saltcorn-builder",
        application: "app-1",
        applicationOrigin: "http://booksdb.localhost:3032",
        target: BOOT.target,
        csrfToken: "token-1",
        lightmode: "light",
        options: BOOT.options,
        layout: BOOT.layout,
        mode: "show",
      },
      client,
    );
    const form = document.getElementById("scbuildform") as HTMLFormElement;
    form.querySelector("input[name=layout]")!.setAttribute("value", encodeURIComponent("{}"));
    const assign = vi.fn();
    vi.spyOn(window, "location", "get").mockReturnValue({ assign } as unknown as Location);
    form.submit();
    await vi.waitFor(() => expect(assign).toHaveBeenCalledWith(BOOT.afterSave));
    expect(saveViewLayout).toHaveBeenCalledWith("app-1", "Show Books", { step: 0, columns: null, layout: {} });
  });
});
