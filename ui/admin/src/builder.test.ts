/**
 * The admin UI's side of the builder (TODO "The builder" 9.1, 9.2, 9.6).
 *
 * What can go wrong quietly here, each with a test:
 *
 * - **The builder's way back is a route with a query**, and a router matching
 *   the whole hash sends `views/Show%20Books?step=2` to the application list.
 * - **A step param that is not a step** (`?step=-1`, `?step=two`) opens the
 *   first step rather than asking the server for a step that is not there.
 * - **The landing after creating a view** is the builder only for a layout step
 *   that is not skipped, on a server that has the builder; a skipped step is
 *   passed over, as the wizard passes over it.
 */

import { describe, expect, it } from "vitest";

import {
  builderPageUrl,
  builderViewUrl,
  configurationChanged,
  openStep,
  splitRoute,
  stepParam,
  viewLanding,
} from "./builder";
import type { StepItem } from "./views";

const APP = "0b1c2d3e-0000-4000-8000-000000000001";

/** A step of a wizard with `count` steps. */
function step(index: number, count: number, extra: Partial<StepItem> = {}): StepItem {
  return {
    index,
    name: `Step ${index}`,
    count,
    builder: false,
    builder_options: null,
    skip: false,
    context_field: null,
    blurb: null,
    fields: [],
    values: {},
    issues: [],
    ...extra,
  };
}

describe("routes with a query", () => {
  it("are matched on their path, with the query apart", () => {
    const { path, query } = splitRoute(`/applications/${APP}/views/Show%20Books?step=2`);
    expect(path).toBe(`/applications/${APP}/views/Show%20Books`);
    expect(stepParam(query)).toBe(2);
    expect(splitRoute("/tables").path).toBe("/tables");
    expect(stepParam(splitRoute("/tables").query)).toBeNull();
  });

  it("open at no step for a step that is not a whole non-negative number", () => {
    for (const bad of ["-1", "two", "1.5", ""]) {
      expect(stepParam(new URLSearchParams({ step: bad })), bad).toBeNull();
    }
    expect(stepParam(new URLSearchParams({ step: "0" }))).toBe(0);
  });

  it("are what the builder route links back to", () => {
    // `sc-server/src/builder.rs` writes `afterSave` and *Back to configuration*
    // as `/#/applications/:id/views/:name?step=n`.
    const hash = `/applications/${APP}/views/${encodeURIComponent("Edit Books")}?step=1`;
    const { path, query } = splitRoute(hash);
    const match = path.match(/^\/applications\/([^/]+)\/views\/([^/]+)$/);
    expect(match && decodeURIComponent(match[2])).toBe("Edit Books");
    expect(stepParam(query)).toBe(1);
  });
});

describe("the builder routes", () => {
  it("encode the application and the name, and a view's step", () => {
    expect(builderViewUrl(APP, "Show Books", 0)).toBe(
      `/builder/applications/${APP}/views/Show%20Books?step=0`,
    );
    expect(builderPageUrl(APP, "Books/Overview")).toBe(
      `/builder/applications/${APP}/pages/Books%2FOverview`,
    );
  });
});

describe("opening a step", () => {
  /** A wizard whose steps are `steps`, recording which were asked for. */
  function wizard(steps: StepItem[]) {
    const asked: number[] = [];
    return {
      asked,
      fetch: async (index: number) => {
        asked.push(index);
        const found = steps[index];
        if (!found) throw new Error(`no step ${index}`);
        return found;
      },
    };
  }

  it("passes over skipped steps in the direction of travel", async () => {
    const steps = [step(0, 3, { skip: true }), step(1, 3, { skip: true }), step(2, 3, { builder: true })];
    const forward = wizard(steps);
    expect((await openStep(forward.fetch, 0, 1)).index).toBe(2);
    expect(forward.asked).toEqual([0, 1, 2]);
    const back = wizard([step(0, 3), step(1, 3, { skip: true }), step(2, 3)]);
    expect((await openStep(back.fetch, 1, -1)).index).toBe(0);
  });

  it("stops on a skipped step when there is nowhere further to go", async () => {
    const steps = [step(0, 2), step(1, 2, { skip: true })];
    expect((await openStep(wizard(steps).fetch, 1, 1)).index).toBe(1);
  });
});

describe("where creating a view lands", () => {
  it("is the builder for a layout step, at that step", () => {
    // Show and Filter: the layout is the first step.
    expect(viewLanding(APP, "Show Books", step(0, 1, { builder: true }), true)).toEqual({
      kind: "builder",
      url: `/builder/applications/${APP}/views/Show%20Books?step=0`,
    });
    // A List whose steps before the layout are skipped lands on the layout.
    expect(viewLanding(APP, "Recent books", step(1, 4, { builder: true }), true)).toEqual({
      kind: "builder",
      url: `/builder/applications/${APP}/views/Recent%20books?step=1`,
    });
  });

  it("is the wizard for a form step, and for any step without the builder", () => {
    expect(viewLanding(APP, "Edit Books", step(0, 3), true)).toEqual({
      kind: "wizard",
      route: `/applications/${APP}/views/Edit%20Books?step=0`,
    });
    expect(viewLanding(APP, "Show Books", step(0, 1, { builder: true }), false)).toEqual({
      kind: "wizard",
      route: `/applications/${APP}/views/Show%20Books?step=0`,
    });
    // A skipped layout step at the end of the wizard has nothing to build.
    expect(viewLanding(APP, "Show Books", step(0, 1, { builder: true, skip: true }), true).kind).toBe(
      "wizard",
    );
  });

  it("follows the steps the wizard would stop at", async () => {
    const steps = [step(0, 2, { skip: true }), step(1, 2, { builder: true })];
    const first = await openStep(async (i) => steps[i], 0, 1);
    expect(viewLanding(APP, "Find books", first, true)).toEqual({
      kind: "builder",
      url: `/builder/applications/${APP}/views/Find%20books?step=1`,
    });
  });
});

describe("opening the builder from the wizard", () => {
  it("saves first only when the wizard gathered something the saved view does not have", () => {
    expect(configurationChanged({ columns: [], layout: {} }, { columns: [], layout: {} })).toBe(false);
    expect(configurationChanged(null, {})).toBe(false);
    expect(configurationChanged({ columns: [] }, { columns: [], view_to_create: "Edit" })).toBe(true);
  });
});
