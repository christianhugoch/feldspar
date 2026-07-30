/**
 * Which application this store holds the source of (design §12.1, §13.3).
 *
 * The Build command needs an application id, and the store name is what the IDE
 * has. An application built by a code framework derives `source` — the store and
 * the directory inside it that its project lives in — and `listApplications`
 * already carries that field, so the match is made **here, client-side**, and the
 * milestone adds no endpoint for it.
 */

import type { ListApplicationsResponse } from "./client";

/** One application, as the admin API lists them. */
export type ApplicationSummary = ListApplicationsResponse[number];

/** An application together with the source directory it builds from. */
export interface BuildableApplication {
  readonly application: ApplicationSummary;
  /**
   * Store-relative directory the build runs in — `""` when the project is at the
   * store root. Diagnostics are reported relative to it.
   */
  readonly sourcePath: string;
}

/**
 * The applications whose source is `store`, in name order.
 *
 * More than one is entirely possible — a store can hold several projects in
 * different sub-directories — so this returns all of them and lets the caller
 * ask which to build. None is the ordinary case for a store that holds assets
 * rather than an application, and is answered as an empty list, not an error.
 */
export function applicationsBuiltFrom(
  applications: ListApplicationsResponse,
  store: string,
): BuildableApplication[] {
  return applications
    .filter((application) => application.source?.store === store)
    .map((application) => ({
      application,
      sourcePath: normalizeSourcePath(application.source?.path ?? ""),
    }))
    .sort((a, b) => a.application.name.localeCompare(b.application.name));
}

/** A source path as the store spells one: no leading, trailing or `./` parts. */
function normalizeSourcePath(path: string): string {
  return path.replace(/^\.\//, "").replace(/^\/+/, "").replace(/\/+$/, "");
}
