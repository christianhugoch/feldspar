// The seam between v1's builder and this server (TODO "The builder" §3).
//
// v1's builder talks to v1's server through about twenty URL shapes: `fetch`
// calls (`/library/content/:id`, `/viewedit/savebuilder/:id`, the previews) and
// hrefs it renders (`/viewedit/config/:name`, `/actions/configure/:name`). None of
// those paths exist on this server, and some collide with paths that do
// (`/api/:table/distinct/:field` against this server's own `/api/`). So every
// shape is written down here, **in exactly one column**:
//
// - **mapped**: it becomes a typed-client call (`builder-fetch.ts`) or a URL on
//   this server (`links.ts`);
// - **refused**: it answers v1's error JSON, `{ error }`, with a sentence naming
//   the feature, and a link to it renders disabled with that sentence;
// - **unreachable**: the builder cannot reach it with what this server gives it,
//   and the reason is written beside it.
//
// `routes.test.ts` walks every URL-shaped literal in `vendor/` and fails on one
// that lands in no column, or in two. That is what makes a refresh of the
// vendored builder safe: a URL v1 adds is a failing test, not a blank panel.
// A URL that is in no column at run time is **refused naming it**, never
// passed through.

/** What happens to a URL the builder reaches. */
export type Column = "mapped" | "refused" | "unreachable";

/** How the builder reaches it: fetched, or rendered as a link or an image. */
export type Reach = "fetch" | "href";

export interface Route {
  /** v1's path: `:name` is one segment, a trailing `*` is the rest. */
  path: string;
  column: Column;
  reach: Reach;
  /** mapped: what it becomes. refused: the sentence the builder shows.
   * unreachable: why the builder cannot reach it. */
  says: string;
  /** The vendored files that reach it, for the reader. */
  from: string;
}

const NOT_IN_THIS_VERSION = "is not in this version of Saltcorn";

export const ROUTES: readonly Route[] = [
  // --- mapped: fetched -----------------------------------------------------
  {
    path: "/viewedit/savebuilder/:id",
    column: "mapped",
    reach: "fetch",
    says: "saveViewLayout, at the step this document is building",
    from: "Library.js (the autosave)",
  },
  {
    path: "/pageedit/savebuilder/:id",
    column: "mapped",
    reach: "fetch",
    says: "savePageLayout",
    from: "Library.js (the autosave)",
  },
  {
    path: "/viewedit/getlayout/:id",
    column: "mapped",
    reach: "fetch",
    says: "getView, answering its configuration's layout",
    from: "Library.js (reloading a tab that comes back into view)",
  },
  {
    path: "/pageedit/getlayout/:id",
    column: "mapped",
    reach: "fetch",
    says: "getPage, answering its layout",
    from: "Library.js (reloading a tab that comes back into view)",
  },
  {
    path: "/library/content/:id",
    column: "mapped",
    reach: "fetch",
    says: "getLibraryItem",
    from: "Library.js, storage.js",
  },
  {
    path: "/library/savefrombuilder",
    column: "mapped",
    reach: "fetch",
    says: "createLibraryItem",
    from: "Library.js (Save as library component)",
  },
  {
    path: "/library/save-updates",
    column: "mapped",
    reach: "fetch",
    says: "saveLibraryUpdates",
    from: "Builder.js (Next)",
  },
  {
    path: "/field/preview/:table/:field/:fieldview",
    column: "mapped",
    reach: "fetch",
    says: "builderFieldPreview",
    from: "elements/utils.js (fetchFieldPreview)",
  },
  {
    path: "/field/fieldviewcfgform/:table",
    column: "mapped",
    reach: "fetch",
    says: "builderFieldviewConfigForm",
    from: "elements/Field.js, JoinField.js, Aggregation.js",
  },
  {
    path: "/view/:name/preview",
    column: "mapped",
    reach: "fetch",
    says: "builderViewPreview",
    from: "elements/utils.js (fetchViewPreview)",
  },
  {
    path: "/page/:name/preview",
    column: "mapped",
    reach: "fetch",
    says: "builderPagePreview",
    from: "elements/utils.js (fetchPagePreview)",
  },
  {
    // v1's public row API. Answered for the admin, for a table in the
    // application's subset only, and never added as a public route.
    path: "/api/:table/distinct/:field",
    column: "mapped",
    reach: "fetch",
    says: "builderDistinctValues",
    from: "elements/Tabs.js",
  },
  {
    path: "/crashlog/",
    column: "mapped",
    reach: "fetch",
    says: "the browser console and a notice",
    from: "elements/utils.js (ErrorBoundary)",
  },

  // --- mapped: links -------------------------------------------------------
  {
    path: "/viewedit/config/:name",
    column: "mapped",
    reach: "href",
    says: "the view's configuration in the admin UI",
    from: "elements/View.js, ViewLink.js, Link.js",
  },
  {
    path: "/pageedit/edit/:page",
    column: "mapped",
    reach: "href",
    says: "the page's builder route",
    from: "elements/Page.js, Link.js",
  },
  {
    path: "/view/:name",
    column: "mapped",
    reach: "href",
    says: "the view on the application's own origin",
    from: "elements/Link.js",
  },
  {
    path: "/page/:name",
    column: "mapped",
    reach: "href",
    says: "the page on the application's own origin",
    from: "elements/Link.js",
  },
  {
    // Rendered as an image `src` and a CSS `url()` as well as a link. Those do
    // not pass through the link listener: the builder route's own server
    // answers `/files/serve/*` for them (TODO 8.5).
    path: "/files/serve/*",
    column: "mapped",
    reach: "href",
    says: "the application's file-store serve URL",
    from: "elements/Card.js, Container.js, Image.js",
  },

  // --- refused -------------------------------------------------------------
  {
    path: "/viewedit/copilot-generate-layout",
    column: "refused",
    reach: "fetch",
    says: `Generating a layout with copilot ${NOT_IN_THIS_VERSION}.`,
    from: "Builder.js, elements/Prompt.js",
  },
  {
    path: "/files/upload",
    column: "refused",
    reach: "fetch",
    says: `Uploading a file from the builder ${NOT_IN_THIS_VERSION}. Upload it in the application's file manager, then choose it here.`,
    from: "elements/Image.js",
  },
  {
    path: "/admin/help/:topic",
    column: "refused",
    reach: "href",
    says: `The builder's help topics are not in this version of Saltcorn.`,
    from: "elements/utils.js (through window.ajax_modal)",
  },
  {
    path: "/admin/ts-declares",
    column: "refused",
    reach: "fetch",
    says: `TypeScript completions in the builder's formula editors are not in this version of Saltcorn.`,
    from: "elements/MonacoEditor.js",
  },
  {
    path: "/actions/configure/:name",
    column: "refused",
    reach: "href",
    says: `Configuring an action from the builder ${NOT_IN_THIS_VERSION}. Configure it on the application's Triggers tab.`,
    from: "elements/Action.js, Field.js",
  },
  {
    // v1's page group editor and its public route. Nothing links to either while
    // `page_groups` is `[]`, but page groups are out of this milestone by name,
    // so a URL naming one is refused by name too.
    path: "/page_groupedit/*",
    column: "refused",
    reach: "href",
    says: `Page groups are not in this version of Saltcorn.`,
    from: "(none today)",
  },
  {
    path: "/page_group/*",
    column: "refused",
    reach: "href",
    says: `Page groups are not in this version of Saltcorn.`,
    from: "(none today)",
  },

  // --- unreachable ---------------------------------------------------------
  {
    path: "/monaco",
    column: "unreachable",
    reach: "fetch",
    says: "MonacoEditor.js gives it to the AMD loader as `paths.vs`; src/shims/monaco-editor-react.tsx drops `paths` and hands the loader the Monaco this bundle carries, so nothing is fetched from it",
    from: "elements/MonacoEditor.js",
  },
];

/** URLs on other origins that the vendored builder names, each with what it is.
 * They are not this server's to answer. A link to one is the browser's, and
 * nothing fetches one; `routes.test.ts` holds both. */
export const OTHER_ORIGINS: Readonly<Record<string, string>> = {
  "https://wiki.saltcorn.com/view/ShowPage/formulas":
    "elements/utils.js: a link to v1's formula documentation, opened in a new tab",
  "https://saltcorn.com/": "elements/Link.js: the URL a new Link element starts with, written into the layout",
};

/** Split a path into segments, ignoring a trailing slash. */
function segments(path: string): string[] {
  const parts = path.split("/").slice(1);
  if (parts.length > 1 && parts[parts.length - 1] === "") parts.pop();
  return parts;
}

/** The path of a URL, without its query or fragment. */
export function pathOf(url: string): string {
  return url.replace(/[?#].*$/, "");
}

export type Params = Record<string, string>;

/** The route a concrete path is, with its parameters decoded (and the rest of
 * the path, for a trailing `*`, under `"*"` as written). */
export function matchRoute(url: string): { route: Route; params: Params } | null {
  const path = segments(pathOf(url));
  for (const route of ROUTES) {
    const pattern = segments(route.path);
    const params: Params = {};
    let ok = true;
    for (let i = 0; i < pattern.length && ok; i++) {
      const p = pattern[i];
      if (p === "*") {
        if (i >= path.length) ok = false;
        else params["*"] = path.slice(i).join("/");
        break;
      }
      if (i >= path.length) ok = false;
      else if (p.startsWith(":")) params[p.slice(1)] = decodeURIComponent(path[i]);
      else if (p !== path[i]) ok = false;
    }
    if (ok && (pattern[pattern.length - 1] === "*" || pattern.length === path.length)) {
      return { route, params };
    }
  }
  return null;
}

/** The routes a URL *shape* could be, where `:param` stands for a segment the
 * vendored source computes (a template hole). The partition test wants exactly
 * one column among them.
 *
 * A `*` route is everything under a literal prefix, so a shape is under it only
 * when the shape spells that prefix out. `/${urlroot}/savebuilder/${id}` could
 * in principle be anything at all, and is not claimed by `/page_groupedit/*`. */
export function routesForShape(shape: string): Route[] {
  const path = segments(pathOf(shape));
  return ROUTES.filter((route) => {
    const pattern = segments(route.path);
    const prefix = pattern[pattern.length - 1] === "*";
    for (let i = 0; i < pattern.length; i++) {
      if (pattern[i] === "*") return i < path.length;
      if (i >= path.length) return false;
      const seg = path[i];
      if (pattern[i] === seg || pattern[i].startsWith(":")) continue;
      if (seg === ":param" && !prefix) continue;
      return false;
    }
    return pattern.length === path.length;
  });
}

/** What the builder is told about a URL this file does not name. */
export function unknownUrlSentence(url: string): string {
  return `The builder asked for ${url}, which this version of Saltcorn does not answer (it is not in ui/builder/src/routes.ts).`;
}

/** The sentence a URL that is not mapped for this use is refused with. */
export function refusalSentence(url: string, route: Route | null, reach: Reach): string {
  if (!route) return unknownUrlSentence(url);
  if (route.column === "refused") return route.says;
  if (route.column === "unreachable") {
    return `The builder reached ${url}, which should be unreachable: ${route.says}.`;
  }
  return reach === "fetch"
    ? `The builder fetched ${url}, which is a link, not a request.`
    : `The builder linked to ${url}, which is a request, not a page.`;
}
