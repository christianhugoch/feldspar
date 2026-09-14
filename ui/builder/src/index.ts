// The builder bundle's entry: v1's `renderBuilder`, with the host around it
// (TODO "The builder" §3, §4).
//
// `startBuilder` is what the host document calls, with what the builder route
// rendered into the page:
// - it names the target for every mapped call (`context.ts`);
// - it defines the v1 globals the page must have (`globals.ts`);
// - it installs the link listener (`links.ts`);
// - it renders v1's builder into its container.

import { renderBuilder } from "@saltcorn/builder";

import { createClient, type ApiClient } from "./client";
import { setBuilderContext, type BuilderTarget } from "./context";
import { installGlobals, missingDocumentGlobals } from "./globals";
import { installLinkListener } from "./links";

/** The modes v1's builder has a toolbox for that this server builds (§12). */
export const BUILDER_MODES = ["show", "edit", "list", "filter", "page"] as const;
export type BuilderMode = (typeof BUILDER_MODES)[number];

export interface StartBuilder {
  /** The id of the element v1's builder renders into (`saltcorn-builder`). */
  containerId: string;
  application: string;
  applicationOrigin: string;
  target: BuilderTarget;
  csrfToken: string;
  lightmode?: "light" | "dark";
  /** v1's `renderBuilder` options, as the worker computed them. */
  options: unknown;
  layout: unknown;
  mode: BuilderMode;
}

export function startBuilder(start: StartBuilder, client: ApiClient = createClient()): void {
  if (!BUILDER_MODES.includes(start.mode)) {
    throw new Error(`the builder does not build a ${String(start.mode)} layout`);
  }
  const missing = missingDocumentGlobals(window);
  if (missing.length) {
    console.error(`The builder's page is missing v1 globals: ${missing.join(", ")}.`);
  }
  setBuilderContext({
    application: start.application,
    applicationOrigin: start.applicationOrigin,
    target: start.target,
    client,
  });
  installGlobals(window, { csrfToken: start.csrfToken, lightmode: start.lightmode });
  installLinkListener(document);
  renderBuilder(
    start.containerId,
    encodeURIComponent(JSON.stringify(start.options)),
    encodeURIComponent(JSON.stringify(start.layout ?? {})),
    start.mode,
  );
}

export { builderFetch } from "./builder-fetch";
