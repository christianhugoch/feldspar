// The v1 page around the builder (TODO "The builder" §4).
//
// v1's builder runs inside a v1 page, and calls globals that page defines.
// Every global it reaches is in exactly one of the two lists below, and
// `globals.test.ts` holds the vendored source to them.
//
// - **DOCUMENT_GLOBALS** come from the Saltcorn UI scripts the host document
//   loads before the bundle, in the order Saltcorn UI's own page loads them,
//   which is v1's:
//   - jQuery;
//   - Bootstrap's bundle, so `.dropdown("toggle")` works;
//   - `saltcorn-common.js`, which is v1's own `notifyAlert` (a Bootstrap toast),
//     `validate_expression_elem` and `apply_showif`, taken whole rather than
//     copied;
//   - `saltcorn.js`, whose `$.debounce` `saltcorn-common.js` uses as the page
//     initialises.
// - **HOST_GLOBALS** are defined here by `installGlobals`, which runs after those
//   scripts: the bundle is a module script, and a module script runs after
//   every classic one.
//   - The stubs v1's `saltcorn-markup/builder.ts` installs over `saltcorn.js`'s
//     definitions in its `domReady`.
//   - `ajax_modal`, likewise replaced, and refused.
//   - The two values a v1 page's header sets.

import { notify } from "./notify";
import { matchRoute, refusalSentence } from "./routes";

/** Globals the host document's scripts define: name → the Saltcorn UI asset
 * (`ui/saltcorn-ui/public/`) that defines it. */
export const DOCUMENT_GLOBALS: Readonly<Record<string, string>> = {
  $: "jquery-3.6.0.min.js",
  jQuery: "jquery-3.6.0.min.js",
  bootstrap: "bootstrap.bundle.min.js",
  notifyAlert: "saltcorn-common.js",
  validate_expression_elem: "saltcorn-common.js",
  validate_bool_expression_elem: "saltcorn-common.js",
  apply_showif: "saltcorn-common.js",
};

/** Globals `installGlobals` defines: name → where v1 defines it. */
export const HOST_GLOBALS: Readonly<Record<string, string>> = {
  ajax_modal: "server/public/saltcorn.js. Refused: the builder opens only help topics with it",
  set_state_field: "server/public/saltcorn.js. A no-op, as saltcorn-markup/builder.ts stubs it",
  set_state_fields: "server/public/saltcorn.js. A no-op, as saltcorn-markup/builder.ts stubs it",
  pjax_to: "server/public/saltcorn.js. A no-op, as saltcorn-markup/builder.ts stubs it",
  _sc_lightmode: "a v1 layout's header, from the theme",
  _sc_globalCsrf: "a v1 layout's header, the session's CSRF token",
};

/** The host globals v1's `saltcorn-markup/builder.ts` stubs. Nothing in the
 * vendored builder names them: the preview HTML the canvas inserts can, since
 * a preview is what the subdomain would render, inline handlers included. */
export const BUILDER_TS_STUBS: readonly string[] = ["set_state_field", "set_state_fields", "pjax_to"];

export interface GlobalsOptions {
  /** The admin session's CSRF token (v1's `_sc_globalCsrf`). */
  csrfToken: string;
  /** v1's `_sc_lightmode`: the builder draws its dark variant for `"dark"`. */
  lightmode?: "light" | "dark";
}

/** `saltcorn-markup/layout.ts`'s toast container, which v1's `notifyAlert`
 * appends to and a v1 layout always renders. */
function ensureToastsArea(doc: Document): void {
  if (doc.getElementById("toasts-area")) return;
  const area = doc.createElement("div");
  area.id = "toasts-area";
  area.className = "toast-container position-fixed top-0 end-0 p-2";
  area.style.zIndex = "9999";
  area.setAttribute("aria-live", "polite");
  area.setAttribute("aria-atomic", "true");
  doc.body.appendChild(area);
}

/** Define every `HOST_GLOBALS` entry on `win`. */
export function installGlobals(win: Window, options: GlobalsOptions): void {
  const noop = () => {};
  const defined: Record<keyof typeof HOST_GLOBALS, unknown> = {
    ajax_modal: (url: string) => {
      notify({ type: "warning", text: refusalSentence(url, matchRoute(url)?.route ?? null, "href") });
    },
    set_state_field: noop,
    set_state_fields: noop,
    pjax_to: noop,
    _sc_lightmode: options.lightmode ?? "light", // v1 draws the light variant by default
    _sc_globalCsrf: options.csrfToken, // v1 sends it as the CSRF-Token header
  };
  Object.assign(win, defined);
  ensureToastsArea(win.document);
}

/** The `DOCUMENT_GLOBALS` `win` lacks, each with the script that should have
 * defined it. */
export function missingDocumentGlobals(win: Window): string[] {
  return Object.entries(DOCUMENT_GLOBALS)
    .filter(([name]) => (win as unknown as Record<string, unknown>)[name] === undefined)
    .map(([name, script]) => `${name} (from ${script})`);
}
