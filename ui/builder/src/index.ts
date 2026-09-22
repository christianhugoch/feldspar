// The builder bundle's entry: v1's `renderBuilder`, with the host around it
// (TODO "The builder" §3, §4).
//
// `startBuilder` is what the host document calls, with what the builder route
// rendered into the page:
// - it names the target for every mapped call (`context.ts`);
// - it defines the v1 globals the page must have (`globals.ts`);
// - it installs the link listener (`links.ts`);
// - it renders v1's builder into its container.
//
// The builder route's document names what to build in its boot data
// (`boot.ts`), and importing this module starts it. A document without boot
// data, such as the jsdom test's, starts nothing.

import { renderBuilder } from "@saltcorn/builder";

import { bootFromDocument } from "./boot";
import { createClient, type ApiClient } from "./client";
import { setBuilderContext, type BuilderTarget } from "./context";
import { installGlobals, missingDocumentGlobals } from "./globals";
import { builderTranslations, withTranslations } from "./i18n";
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
  /** The locale the builder route negotiated for this admin (§16.1). */
  locale?: string;
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
  // v1's `translations` map, filled from the `builder` domain (task 3.5). The
  // catalogue is a chunk of its own and is fetched only when there is one, so
  // an English builder renders on the same tick it always did — `then` on an
  // already-resolved promise is a microtask, not a round trip.
  void builderTranslations(start.locale).then((translations) => {
    renderBuilder(
      start.containerId,
      encodeURIComponent(
        JSON.stringify(withTranslations(start.options, translations)),
      ),
      encodeURIComponent(JSON.stringify(start.layout ?? {})),
      start.mode,
    );
  });
}

export { builderFetch } from "./builder-fetch";
export { bootFromDocument, readBootData, type BootData } from "./boot";

if (typeof document !== "undefined") bootFromDocument(document, startBuilder);
