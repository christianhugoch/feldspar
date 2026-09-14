// The admin UI's side of the builder (TODO "The builder" §9).
//
// The builder is not a screen here: it is a document of its own on the admin
// server (`/builder/…`, §2), and the admin UI only sends an admin to it and
// takes them back. Kept out of the components for the reason `views.ts` is, so
// what can go wrong quietly has a test:
//
// - **The way back carries a step.** The builder's *Next* returns to
//   `#/applications/:id/views/:name?step=n`, so the router matches a route's
//   path without its query, and the wizard opens at `n` rather than at the
//   first step.
// - **A step v1 skips is passed over**, in the direction of travel, the same
//   way when a view is created as when the wizard steps: a new Show lands on its
//   layout, and a new List whose only step before the layout is skipped does too.
// - **A server without the bundle** is told apart from one with it by
//   `builderStatus`, and nothing then links to a builder that answers "not
//   built".

import type { ViewConfigStepResponse } from "./client";

type StepItem = ViewConfigStepResponse;

const enc = encodeURIComponent;

/** A hash route split into the path the router matches and its query. */
export function splitRoute(route: string): { path: string; query: URLSearchParams } {
  const at = route.indexOf("?");
  return at < 0
    ? { path: route, query: new URLSearchParams() }
    : { path: route.slice(0, at), query: new URLSearchParams(route.slice(at + 1)) };
}

/** The step a route asks the wizard to open at, counting from 0, or `null` for
 * none, or for anything that is not a whole non-negative number. */
export function stepParam(query: URLSearchParams): number | null {
  const raw = query.get("step");
  if (raw === null || !/^\d+$/.test(raw)) return null;
  return Number(raw);
}

/** The builder route for step `step` of a view (§2). It leaves the SPA. */
export function builderViewUrl(appId: string, name: string, step: number): string {
  return `/builder/applications/${enc(appId)}/views/${enc(name)}?step=${step}`;
}

/** The builder route for a page. */
export function builderPageUrl(appId: string, name: string): string {
  return `/builder/applications/${enc(appId)}/pages/${enc(name)}`;
}

/** What a layout step says where the builder opens it. */
export const OPEN_IN_BUILDER =
  "This step is the view's layout, edited in the drag-and-drop builder. Opening it " +
  "saves the configuration so far first, so the builder starts from what is on screen.";

/** What a layout says on a server built without the builder bundle, which is
 * not obvious: the layout exists and is rendered, and nothing here edits it. */
export const NO_BUILDER =
  "This server was built without the builder (ui/builder), so a layout is shown here " +
  "as it is saved, and saving keeps it unchanged. Build the builder with " +
  "`npm ci && npm run build` in ui/builder and rebuild the server to edit layouts.";

/** Open step `index`, passing over a step v1 skips in the direction of travel
 * — unless there is nowhere further to go, and then the skipped step is the
 * answer. `fetchStep` is `viewConfigStep` over the configuration gathered. */
export async function openStep(
  fetchStep: (index: number) => Promise<StepItem>,
  index: number,
  direction: 1 | -1,
): Promise<StepItem> {
  let at = index;
  for (;;) {
    const step = await fetchStep(at);
    const beyond = at + direction;
    if (step.skip && beyond >= 0 && beyond < step.count) {
      at = beyond;
      continue;
    }
    return step;
  }
}

/** Where a newly created view goes. */
export type Landing =
  | { kind: "builder"; url: string }
  | { kind: "wizard"; route: string };

/** Where creating a view lands, given the first step its wizard stops at: the
 * builder when that step is a layout and the builder is here, as v1's
 * *Configure* does, and the wizard at that step otherwise. */
export function viewLanding(
  appId: string,
  name: string,
  first: Pick<StepItem, "index" | "builder" | "skip">,
  builderAvailable: boolean,
): Landing {
  if (first.builder && !first.skip && builderAvailable) {
    return { kind: "builder", url: builderViewUrl(appId, name, first.index) };
  }
  return {
    kind: "wizard",
    route: `/applications/${enc(appId)}/views/${enc(name)}?step=${first.index}`,
  };
}

/** Whether what the wizard gathered differs from what is saved, and so has to be
 * saved before the builder opens over the saved configuration. */
export function configurationChanged(saved: unknown, gathered: unknown): boolean {
  return JSON.stringify(saved ?? {}) !== JSON.stringify(gathered ?? {});
}
