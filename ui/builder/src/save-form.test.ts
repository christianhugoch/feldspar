// @vitest-environment jsdom
//
// `#scbuildform`'s submit, saved through the typed client (TODO "The builder" 8.4).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { builderFetch } from "./builder-fetch";
import type { ApiClient } from "./client";
import { setBuilderContext } from "./context";
import { installSaveForm } from "./save-form";

const LAYOUT = { above: [{ type: "field", field_name: "title", fieldview: "as_text" }] };
const COLUMNS = [{ type: "Field", field_name: "title", fieldview: "as_text" }];

/** The form as v1's builder page renders it, filled in as `NextButton` fills
 * it: encoded JSON in the `value` attribute. */
function filledForm(layout: unknown, columns?: unknown): HTMLFormElement {
  document.body.innerHTML = `
    <form id="scbuildform" method="post">
      <input type="hidden" name="columns" value="">
      <input type="hidden" name="layout" value="">
    </form>`;
  const form = document.getElementById("scbuildform") as HTMLFormElement;
  const set = (name: string, value: unknown) =>
    form.querySelector(`input[name=${name}]`)!.setAttribute("value", encodeURIComponent(JSON.stringify(value)));
  set("layout", layout);
  if (columns !== undefined) set("columns", columns);
  return form;
}

function fakeClient(overrides: Partial<Record<keyof ApiClient, unknown>>): ApiClient {
  return overrides as unknown as ApiClient;
}

let notifyAlert: ReturnType<typeof vi.fn>;

beforeEach(() => {
  notifyAlert = vi.fn();
  (window as unknown as { notifyAlert: unknown }).notifyAlert = notifyAlert;
});

afterEach(() => {
  setBuilderContext(null);
  document.body.innerHTML = "";
});

describe("the builder's form", () => {
  it("saves a view's layout and columns at its step with v1's submit, then goes on", async () => {
    const saveViewLayout = vi.fn().mockResolvedValue({});
    const navigate = vi.fn();
    const form = filledForm(LAYOUT, COLUMNS);
    installSaveForm(
      form,
      { application: "app-1", target: { kind: "view", name: "List Books", step: 0 }, afterSave: "/#/next" },
      fakeClient({ saveViewLayout }),
      navigate,
    );

    form.submit(); // what `NextButton` calls, which fires no submit event

    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith("/#/next"));
    expect(saveViewLayout).toHaveBeenCalledWith("app-1", "List Books", {
      step: 0,
      columns: COLUMNS,
      layout: LAYOUT,
    });
    expect(notifyAlert).not.toHaveBeenCalled();
  });

  it("saves a page's layout and returns to the Pages tab", async () => {
    const savePageLayout = vi.fn().mockResolvedValue({});
    const navigate = vi.fn();
    const form = filledForm(LAYOUT);
    installSaveForm(
      form,
      { application: "app-1", target: { kind: "page", name: "Home" }, afterSave: "/#/applications/app-1/pages" },
      fakeClient({ savePageLayout }),
      navigate,
    );

    form.submit();

    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith("/#/applications/app-1/pages"));
    expect(savePageLayout).toHaveBeenCalledWith("app-1", "Home", { layout: LAYOUT });
  });

  it("shows a refused save with notifyAlert, stays on the canvas, and can save again", async () => {
    const saveViewLayout = vi
      .fn()
      .mockRejectedValueOnce(new Error("saveViewLayout failed: 400: the action Nope is not one of the application's"))
      .mockResolvedValueOnce({});
    const navigate = vi.fn();
    const form = filledForm(LAYOUT, COLUMNS);
    const save = installSaveForm(
      form,
      { application: "app-1", target: { kind: "view", name: "Show Books", step: 0 }, afterSave: "/#/next" },
      fakeClient({ saveViewLayout }),
      navigate,
    );

    expect(await save()).toBe(false);
    expect(notifyAlert).toHaveBeenCalledWith({
      type: "danger",
      text: "saveViewLayout failed: 400: the action Nope is not one of the application's",
    });
    expect(navigate).not.toHaveBeenCalled();
    expect(document.getElementById("scbuildform")).toBe(form);

    expect(await save()).toBe(true);
    expect(navigate).toHaveBeenCalledWith("/#/next");
  });

  it("treats a submit event as the same save, and never lets the browser post", async () => {
    const savePageLayout = vi.fn().mockResolvedValue({});
    const navigate = vi.fn();
    const form = filledForm(LAYOUT);
    installSaveForm(
      form,
      { application: "app-1", target: { kind: "page", name: "Home" }, afterSave: "/#/pages" },
      fakeClient({ savePageLayout }),
      navigate,
    );

    const event = new Event("submit", { cancelable: true });
    form.dispatchEvent(event);

    expect(event.defaultPrevented).toBe(true);
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith("/#/pages"));
  });

  it("waits for a shared component's edits, sent just before the submit, before leaving", async () => {
    let finishUpdates!: () => void;
    const saveLibraryUpdates = vi.fn(
      () => new Promise((resolve) => (finishUpdates = () => resolve({ updated: 1 }))),
    );
    const saveViewLayout = vi.fn().mockResolvedValue({});
    const client = fakeClient({ saveLibraryUpdates, saveViewLayout });
    setBuilderContext({
      application: "app-1",
      applicationOrigin: "",
      target: { kind: "view", name: "Show Books", step: 0 },
      client,
    });
    const navigate = vi.fn();
    const form = filledForm(LAYOUT, COLUMNS);
    installSaveForm(
      form,
      { application: "app-1", target: { kind: "view", name: "Show Books", step: 0 }, afterSave: "/#/next" },
      client,
      navigate,
    );

    // `NextButton`'s order: the library edits, not awaited, then the submit.
    void builderFetch("/library/save-updates", {
      method: "POST",
      body: JSON.stringify({ libraryUpdates: [{ library_id: "item-1", layout: LAYOUT, node_id: "n" }] }),
    });
    form.submit();

    await vi.waitFor(() => expect(saveViewLayout).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(navigate).not.toHaveBeenCalled();

    finishUpdates();
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith("/#/next"));
    expect(saveLibraryUpdates).toHaveBeenCalledWith("app-1", {
      libraryUpdates: [{ library_id: "item-1", layout: LAYOUT }],
    });
  });
});
