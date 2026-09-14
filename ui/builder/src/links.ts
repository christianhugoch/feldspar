// The links v1's builder renders (TODO "The builder" §3).
//
// The builder renders hrefs to v1's admin (`/viewedit/config/:name`,
// `/pageedit/edit/:page`, `/actions/configure/:name`) and to the application
// (`/view/:name`). They cannot be rewritten where they are rendered without
// editing the vendored files, so one delegated click listener on the document
// decides every click on a same-origin `a[href]`, against the same table as
// `builder-fetch.ts`:
//
// - a mapped link navigates to its mapping (in a new tab if it asked for one);
// - a refused one shows its sentence and goes nowhere, and is rendered disabled
//   with that sentence as its tooltip;
// - an unknown one is refused naming its URL, never followed.
//
// Links to other origins (v1's wiki) and in-page links (`#`) are the browser's.

import { builderContext, type BuilderContext } from "./context";
import { notify } from "./notify";
import { matchRoute, refusalSentence, type Params } from "./routes";

const enc = encodeURIComponent;

/** Where each mapped link goes, keyed by its path in `routes.ts`. */
export const HREF_TARGETS: Record<string, (params: Params, ctx: BuilderContext) => string> = {
  "/viewedit/config/:name": (p, ctx) => `/#/applications/${enc(ctx.application)}/views/${enc(p.name)}`,
  "/pageedit/edit/:page": (p, ctx) => `/builder/applications/${enc(ctx.application)}/pages/${enc(p.page)}`,
  "/view/:name": (p, ctx) => `${ctx.applicationOrigin}/view/${enc(p.name)}`,
  "/page/:name": (p, ctx) => `${ctx.applicationOrigin}/page/${enc(p.name)}`,
  "/files/serve/*": (p, ctx) => `${ctx.applicationOrigin}/files/serve/${p["*"]}`,
};

export type LinkDecision =
  | { kind: "browser" }
  | { kind: "navigate"; url: string }
  | { kind: "refused"; sentence: string };

/** What a click on a link with this `href` attribute does. */
export function decideLink(href: string, ctx: BuilderContext): LinkDecision {
  // Only a path on this origin is v1's server; `//host` is another origin.
  if (!href.startsWith("/") || href.startsWith("//")) return { kind: "browser" };
  const match = matchRoute(href);
  const target = match?.route.column === "mapped" ? HREF_TARGETS[match.route.path] : undefined;
  if (match && target) {
    const query = href.includes("?") ? href.slice(href.indexOf("?")) : "";
    return { kind: "navigate", url: target(match.params, ctx) + query };
  }
  return { kind: "refused", sentence: refusalSentence(href, match?.route ?? null, "href") };
}

function linkOf(target: EventTarget | null): HTMLAnchorElement | null {
  return target instanceof Element ? target.closest<HTMLAnchorElement>("a[href]") : null;
}

/** Render a refused link disabled, with its sentence as the tooltip. */
function markLinks(root: ParentNode, ctx: BuilderContext): void {
  for (const a of root.querySelectorAll<HTMLAnchorElement>('a[href^="/"]')) {
    const decision = decideLink(a.getAttribute("href") ?? "", ctx);
    if (decision.kind !== "refused" || a.getAttribute("aria-disabled") === "true") continue;
    a.setAttribute("aria-disabled", "true");
    a.classList.add("disabled");
    a.title = decision.sentence;
  }
}

/** Install the listener and the link marking on `doc`. Answers a function that
 * removes both. */
export function installLinkListener(doc: Document): () => void {
  const onClick = (event: MouseEvent) => {
    const a = linkOf(event.target);
    if (!a) return;
    const decision = decideLink(a.getAttribute("href") ?? "", builderContext());
    if (decision.kind === "browser") return;
    event.preventDefault();
    if (decision.kind === "refused") {
      notify({ type: "warning", text: decision.sentence });
    } else if (a.target === "_blank") {
      window.open(decision.url, "_blank", "noopener");
    } else {
      window.location.assign(decision.url);
    }
  };
  // Capture, so the decision is made before anything the builder does with
  // the click, and before the browser follows the link.
  doc.addEventListener("click", onClick, true);

  const observer = new MutationObserver(() => markLinks(doc, builderContext()));
  observer.observe(doc.documentElement, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: ["href"],
  });
  markLinks(doc, builderContext());

  return () => {
    doc.removeEventListener("click", onClick, true);
    observer.disconnect();
  };
}
