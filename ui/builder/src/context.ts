// What the builder is building, and how it reaches this server.
//
// v1's builder knows its target by the numeric id in its options (`view_id`,
// `page_id`) and reaches its server by path. Here the admin API names a view or
// page by application and name, and a view's layout by its step, so the
// document tells the host those once (`startBuilder`) and every mapped URL is
// answered from them. One document builds one view step or one page, so this
// is module state rather than something threaded through v1's components.

import type { ApiClient } from "./client";

/** The view step or page this document builds. */
export type BuilderTarget =
  | { kind: "view"; name: string; step: number }
  | { kind: "page"; name: string };

export interface BuilderContext {
  /** The application's id, as the admin API names it. */
  application: string;
  /** The application's own origin (`http://booksdb.localhost:3032`), where
   * its views, pages and files are served. */
  applicationOrigin: string;
  target: BuilderTarget;
  /** The typed admin client every mapped call goes through. */
  client: ApiClient;
}

let current: BuilderContext | null = null;

export function setBuilderContext(context: BuilderContext | null): void {
  current = context;
}

export function builderContext(): BuilderContext {
  if (!current) throw new Error("the builder has no context: startBuilder was not called");
  return current;
}
