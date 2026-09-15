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
//   - `saltcorn-common.js`, which is v1's own `notifyAlert` (a Bootstrap toast)
//     and `apply_showif`, taken whole rather than copied;
//   - `saltcorn.js`, whose `$.debounce` `saltcorn-common.js` uses as the page
//     initialises.
// - **HOST_GLOBALS** are defined here by `installGlobals`, which runs after those
//   scripts: the bundle is a module script, and a module script runs after
//   every classic one.
//   - The stubs v1's `saltcorn-markup/builder.ts` installs over `saltcorn.js`'s
//     definitions in its `domReady`.
//   - `ajax_modal`, likewise replaced, and refused.
//   - The two values a v1 page's header sets.
//   - `saltcorn-common.js`'s two expression validators, ported: v1's check a
//     formula by constructing an `AsyncFunction` from it, which the builder's
//     CSP refuses as `eval` (`formula-syntax.ts`).

import { checkFormulaSyntax, nonBooleanConstant } from "./formula-syntax";
import { notify } from "./notify";
import { matchRoute, refusalSentence } from "./routes";

/** Globals the host document's scripts define: name → the Saltcorn UI asset
 * (`ui/saltcorn-ui/public/`) that defines it. */
export const DOCUMENT_GLOBALS: Readonly<Record<string, string>> = {
  $: "jquery-3.6.0.min.js",
  jQuery: "jquery-3.6.0.min.js",
  bootstrap: "bootstrap.bundle.min.js",
  notifyAlert: "saltcorn-common.js",
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
  validate_expression_elem:
    "server/public/saltcorn-common.js. Ported, with the syntax check a parse, not an AsyncFunction the CSP refuses",
  validate_bool_expression_elem:
    "server/public/saltcorn-common.js. Ported likewise; only a literal is recognised as a constant",
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

/** A jQuery collection, from the document's `jquery-3.6.0.min.js`. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type JQueryCollection = any;

/**
 * v1's `validate_expression_elem` (`bool` false) and
 * `validate_bool_expression_elem` (`bool` true), from `saltcorn-common.js`.
 *
 * `targetOrVal` is the input as a jQuery collection, or the formula as a string
 * with `ref` the element to put the message after (Monaco's editor calls it that
 * way). A previous message is removed, and a new one is put after the input.
 * Where this differs from v1:
 * - the syntax check is a parse (`checkFormulaSyntax`), not a construction;
 * - the message is set as text, where v1 interpolates it into HTML;
 * - the boolean check: v1 runs a formula that needs no row and says so if it
 *   does not return a boolean. Running it is what the policy refuses (v1's own
 *   code then catches the refusal and checks nothing), so here only a literal
 *   is recognised as a constant.
 */
function expressionValidator(win: Window, bool: boolean) {
  return (targetOrVal: unknown, ref: unknown = null): void => {
    const $ = (win as unknown as { $: (x: unknown) => JQueryCollection }).$;
    let val: unknown;
    let target: JQueryCollection;
    if (typeof targetOrVal === "string") {
      val = targetOrVal;
      target = $(ref);
    } else {
      target = targetOrVal;
      val = target.val();
    }
    const next = target.next();
    if (next.hasClass("expr-error")) next.remove();
    if (!bool && target.hasClass("validate-expression-conditional")) {
      // v1: a setting that is a formula only when its "_formula" box is ticked
      const box = target.closest(".form-namespace").find(`[name="${target.attr("name")}_formula"]`);
      if (!box.prop("checked")) return;
    }
    if (!val) return;
    const show = (text: string) =>
      target.after($('<small class="text-danger font-monospace d-block expr-error"></small>').text(text));
    const expression = String(val);
    try {
      checkFormulaSyntax([], "return " + expression, true);
    } catch (error) {
      show((error as Error).message);
      return;
    }
    if (bool && nonBooleanConstant(expression)) show("Expression must return a boolean"); // v1's sentence
  };
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
    validate_expression_elem: expressionValidator(win, false),
    validate_bool_expression_elem: expressionValidator(win, true),
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
