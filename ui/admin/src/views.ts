// The Views and Pages tabs of a Saltcorn UI application (TODO "Saltcorn UI" 9.2).
//
// Kept out of the component for the reason `modules.ts` and `backup.ts` are: the
// arithmetic of the list is testable without a browser, and the component is
// then only flow. What that arithmetic is:
//
// - **Which tabs an application has.** Only an application whose source is
//   views and pages has the two tabs, and the server says which ones are
//   (`has_views`) — this screen does not know a framework by name.
// - **The link that opens a view on the app's subdomain.** A v1 view name has
//   spaces in it (`List Books`), so the name is encoded into the path; and the
//   generated client interpolates path parameters as they are, so the same
//   encoding is what the API calls are made with (`nameParam`).
// - **A view whose pattern this server does not have** is shown with that fact
//   attached rather than as an ordinary row: it is a view that will not render.

import type {
  ListApplicationsResponse,
  ListPagesResponse,
  ListViewPatternsResponse,
  ListViewsResponse,
} from "./client";
import { roleLabel, type Roles } from "./roles";

export type AppItem = ListApplicationsResponse[number];
export type ViewItem = ListViewsResponse[number];
export type PageItem = ListPagesResponse[number];
export type PatternItem = ListViewPatternsResponse[number];

export type AppTab = "settings" | "views" | "pages";

/** Where the page is, as much of `window.location` as a link needs. */
export type Here = { protocol: string; host: string };

/** What an application with no views is told, because it is not obvious: its
 * subdomain serves nothing until it has one. */
export const NO_VIEWS =
  "This application has no views yet. A Saltcorn UI application serves nothing " +
  "until it has views: restore a Saltcorn 1 backup into it, or create one.";

/** And with no pages, which it can do without: `/` then lists the views. */
export const NO_PAGES =
  "This application has no pages. Its views are still served at /view/<name>; a " +
  "page places views on one screen and can be each role's home page.";

/** The tabs an application's screen has, in order. */
export function appTabs(
  app: Pick<AppItem, "id" | "has_views">,
): { id: AppTab; label: string; href: string }[] {
  const base = `#/applications/${encodeURIComponent(app.id)}`;
  const tabs: { id: AppTab; label: string; href: string }[] = [
    { id: "settings", label: "Settings", href: `${base}/edit` },
  ];
  if (app.has_views) {
    tabs.push({ id: "views", label: "Views", href: `${base}/views` });
    tabs.push({ id: "pages", label: "Pages", href: `${base}/pages` });
  }
  return tabs;
}

/** A view or page name as a path parameter: the generated client puts it in
 * the URL as it is given. */
export function nameParam(name: string): string {
  return encodeURIComponent(name);
}

/** The app's own origin: `<subdomain>.<the admin's host>`, the admin running on
 * the base domain. */
export function appOrigin(subdomain: string, here: Here): string {
  return `${here.protocol}//${subdomain}.${here.host}`;
}

/** Where a view is served on its application's subdomain. */
export function viewUrl(subdomain: string, name: string, here: Here): string {
  return `${appOrigin(subdomain, here)}/view/${encodeURIComponent(name)}`;
}

/** Where a page is served on its application's subdomain. */
export function pageUrl(subdomain: string, name: string, here: Here): string {
  return `${appOrigin(subdomain, here)}/page/${encodeURIComponent(name)}`;
}

/** One row of the Views tab. */
export type ViewRow = {
  name: string;
  description: string;
  pattern: string;
  /** Set when the pattern list has loaded and does not have this one. */
  patternMissing: boolean;
  table: string;
  role: string;
  url: string;
};

/** The Views tab's rows, in the server's order (by name). `patterns` is `null`
 * while the list is loading or when it could not be had, and then no view is
 * marked as missing its pattern. */
export function viewRows(
  views: ViewItem[],
  patterns: PatternItem[] | null,
  roles: Roles,
  subdomain: string,
  here: Here,
): ViewRow[] {
  return views.map((view) => {
    const pattern = patterns?.find((p) => p.name === view.viewpattern);
    return {
      name: view.name,
      description: view.description,
      pattern: pattern?.label || view.viewpattern,
      patternMissing: patterns !== null && !pattern,
      table: view.table_name || "—",
      role: roleLabel(view.min_role, roles),
      url: viewUrl(subdomain, view.name, here),
    };
  });
}

/** One row of the Pages tab. */
export type PageRow = {
  name: string;
  title: string;
  role: string;
  /** The roles this page is the home page of, from v1's `root_page_for_roles`;
   * empty when it is nobody's. */
  homeFor: string[];
  url: string;
};

/** The Pages tab's rows. */
export function pageRows(
  pages: PageItem[],
  roles: Roles,
  subdomain: string,
  here: Here,
): PageRow[] {
  return pages.map((page) => {
    const attributes = (page.attributes ?? {}) as Record<string, unknown>;
    const home = attributes.root_page_for_roles;
    return {
      name: page.name,
      title: page.title,
      role: roleLabel(page.min_role, roles),
      homeFor: Array.isArray(home)
        ? home
            .map(Number)
            .filter((r) => Number.isInteger(r))
            .map((r) => roleLabel(r, roles))
        : [],
      url: pageUrl(subdomain, page.name, here),
    };
  });
}

/** The confirmation a delete asks for. */
export function deleteConfirmation(kind: "view" | "page", name: string, appName: string): string {
  const consequence =
    kind === "view"
      ? "Pages and views that show or link to it will no longer find it."
      : "A role whose home page it is will land on the list of views instead.";
  return `Delete the ${kind} "${name}" from ${appName}? ${consequence} This cannot be undone.`;
}
