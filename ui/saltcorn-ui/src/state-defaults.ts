// v1's built-in `getState().fonts`, `icons` and `keyframes` (TODO "The builder"
// 5.4): what the builder's font, icon and animation pickers list.
//
// They live in v1's `db/state.ts`, which is v1's whole server state and is not
// vendored, so the three values are ported here, each from the line that sets it.
// A plugin can add to them in v1 (`withCfg("fonts")`, `withCfg("icons")`); no
// plugin does here, so these are the whole of each. The icon names themselves
// are v1's file, vendored.
import faIcons from "../vendor/saltcorn-data/db/fa5-icons.js";

/** `standard_fonts`, from packages/saltcorn-data/db/state.ts. */
const fonts: Record<string, string> = {
  Arial: "Arial, Helvetica Neue, Helvetica, sans-serif",
  Baskerville: "Baskerville, Baskerville Old Face, Garamond, Times New Roman, serif",
  "Bodoni MT":
    "Bodoni MT, Bodoni 72, Didot, Didot LT STD, Hoefler Text, Garamond, Times New Roman, serif",
  Calibri: "Calibri, Candara, Segoe, Segoe UI, Optima, Arial, sans-serif",
  "Calisto MT":
    "Calisto MT, Bookman Old Style, Bookman, Goudy Old Style, Garamond, Hoefler Text, Bitstream Charter, Georgia, serif",
  Cambria: "Cambria, Georgia, serif",
  Candara: "Candara, Calibri, Segoe, Segoe UI, Optima, Arial, sans-serif",
  "Century Gothic": "Century Gothic, CenturyGothic, AppleGothic, sans-serif",
  Consolas: "Consolas, monaco, monospace",
  "Copperplate Gothic": "Copperplate, Copperplate Gothic Light, fantasy",
  "Courier New": "Courier New, Courier, Lucida Sans Typewriter, Lucida Typewriter, monospace",
  "Dejavu Sans": "Dejavu Sans, Arial, Verdana, sans-serif",
  Didot: "Didot, Didot LT STD, Hoefler Text, Garamond, Calisto MT, Times New Roman, serif",
  "Franklin Gothic": "Franklin Gothic, Arial Bold",
  Garamond: "Garamond, Baskerville, Baskerville Old Face, Hoefler Text, Times New Roman, serif",
  Georgia: "Georgia, Times, Times New Roman, serif",
  "Gill Sans": "Gill Sans, Gill Sans MT, Calibri, sans-serif",
  "Goudy Old Style": "Goudy Old Style, Garamond, Big Caslon, Times New Roman, serif",
  Helvetica: "Helvetica Neue, Helvetica, Arial, sans-serif",
  Impact: "Impact, Charcoal, Helvetica Inserat, Bitstream Vera Sans Bold, Arial Black, sans serif",
  "Lucida Bright": "Lucida Bright, Georgia, serif",
  "Lucida Sans": "Lucida Sans, Helvetica, Arial, sans-serif",
  Optima: "Optima, Segoe, Segoe UI, Candara, Calibri, Arial, sans-serif",
  Palatino: "Palatino, Palatino Linotype, Palatino LT STD, Book Antiqua, Georgia, serif",
  Perpetua: "Perpetua, Baskerville, Big Caslon, Palatino Linotype, Palatino, serif",
  Rockwell: "Rockwell, Courier Bold, Courier, Georgia, Times, Times New Roman, serif",
  "Segoe UI": "Segoe UI, Frutiger, Dejavu Sans, Helvetica Neue, Arial, sans-serif",
  Tahoma: "Tahoma, Verdana, Segoe, sans-serif",
  "Trebuchet MS": "Trebuchet MS, Lucida Grande, Lucida Sans Unicode, Lucida Sans, sans-serif",
  Verdana: "Verdana, Geneva, sans-serif",
};

/** `get_standard_icons()`, from packages/saltcorn-data/db/state.ts: the Font
 * Awesome 5 names, then three unicode stars v1 appends. v1 keeps them in a
 * `Set`, whose order is insertion order, so this is the order `icons` lists. */
const icons: string[] = [
  ...new Set([
    ...(faIcons as string[]),
    "unicode-2605-black-star",
    "unicode-2606-white-star",
    "unicode-2608-thunderstorm",
  ]),
];

/** The `State` constructor's `keyframes`, from packages/saltcorn-data/db/state.ts. */
const keyframes: string[] = [
  "fadeIn",
  "fadeInLeft",
  "fadeInRight",
  "fadeInUp",
  "fadeInDown",
  "rollIn",
  "zoomIn",
  "zoomInUp",
  "bounce",
  "tada",
];

export const stateDefaults = Object.freeze({
  fonts: Object.freeze(fonts),
  icons: Object.freeze(icons),
  keyframes: Object.freeze(keyframes),
});
