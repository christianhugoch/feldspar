// The scripts the builder's host document loads before the bundle, in its order
// (`globals.ts`'s DOCUMENT_GLOBALS), evaluated into the test's jsdom global the
// way a browser evaluates a classic script: in the global scope.

import fs from "node:fs";
import path from "node:path";

import { SALTCORN_UI_PUBLIC } from "./vendor-source";

/** In the order Saltcorn UI's own page loads them (`sc-viewpattern`'s
 * `framework.rs`), which is v1's. */
export const V1_PAGE_SCRIPTS = ["jquery-3.6.0.min.js", "bootstrap.bundle.min.js", "saltcorn-common.js", "saltcorn.js"];

type Globals = Record<string, unknown>;

export function loadV1PageScripts(): void {
  const g = globalThis as unknown as Globals;
  const win = window as unknown as Globals;
  // jsdom has no layout engine and so no media queries; the builder asks one
  // for the colour scheme.
  if (!win.matchMedia) {
    const matchMedia = (media: string) => ({
      matches: false,
      media,
      onchange: null,
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent: () => false,
    });
    win.matchMedia = matchMedia;
    g.matchMedia = matchMedia;
  }
  // jsdom has no layout, so no observers of it either. `saltcorn-common.js`
  // constructs an IntersectionObserver as it loads.
  class InertObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
    takeRecords() {
      return [];
    }
  }
  for (const name of ["IntersectionObserver", "ResizeObserver"]) {
    if (!win[name]) {
      win[name] = InertObserver;
      g[name] = InertObserver;
    }
  }
  for (const script of V1_PAGE_SCRIPTS) {
    const source = fs.readFileSync(path.join(SALTCORN_UI_PUBLIC, script), "utf8");
    // An indirect eval runs in the global scope, as a classic script does.
    (0, eval)(source);
    // A UMD script attaches to `window`; code in the bundle finds its globals
    // on `globalThis`. In a browser they are one object.
    for (const name of ["$", "jQuery", "bootstrap"]) {
      if (g[name] === undefined && win[name] !== undefined) g[name] = win[name];
    }
  }
}
