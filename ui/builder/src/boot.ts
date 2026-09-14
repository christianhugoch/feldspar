// The builder's document, read (TODO "The builder" §4, 8.2).
//
// The builder route (`sc-server`'s `builder.rs`) renders v1's builder page
// around what the bundle needs, and hands that over as JSON in
// `<script type="application/json" id="builder-boot">`, not as the inline
// `builder.renderBuilder(...)` call v1 writes: the route's policy has no
// `'unsafe-inline'` script (8.3). The bundle is a module script, and a module
// script runs after the document is parsed, so the element is there when it
// looks.

import { createClient, type ApiClient } from "./client";
import type { BuilderTarget } from "./context";
import type { BuilderMode, StartBuilder } from "./index";
import { installSaveForm } from "./save-form";

/** The ids the builder route renders, and v1's builder page has. */
export const BOOT_ELEMENT_ID = "builder-boot";
export const CONTAINER_ID = "saltcorn-builder";
export const FORM_ID = "scbuildform";

/** What the builder route renders into the document. */
export interface BootData {
  /** The application's id, as the admin API names it. */
  application: string;
  applicationName: string;
  /** Where the application is served; empty on a server with no base domain. */
  applicationOrigin: string;
  target: BuilderTarget;
  /** A view's: how many steps its configuration has, and this one's name. */
  stepCount?: number;
  stepName?: string;
  csrfToken: string;
  lightmode?: "light" | "dark";
  /** v1's `renderBuilder` options, as the worker computed them. */
  options: unknown;
  layout: unknown;
  mode: BuilderMode;
  /** Where a saved layout goes next. */
  afterSave: string;
}

/** The boot data in `doc`, or `null` for a document that has none. */
export function readBootData(doc: Document): BootData | null {
  const element = doc.getElementById(BOOT_ELEMENT_ID);
  if (!element) return null;
  try {
    return JSON.parse(element.textContent ?? "") as BootData;
  } catch (e) {
    throw new Error(
      `the builder's boot data (#${BOOT_ELEMENT_ID}) is not JSON: ${e instanceof Error ? e.message : String(e)}`,
    );
  }
}

/** Start the builder `doc` describes, with `start` (`index.ts`'s
 * `startBuilder`). Answers whether there was one to start. */
export function bootFromDocument(
  doc: Document,
  start: (start: StartBuilder, client: ApiClient) => void,
  client: ApiClient = createClient(),
): boolean {
  const boot = readBootData(doc);
  if (!boot) return false;
  const form = doc.getElementById(FORM_ID);
  if (!(form instanceof HTMLFormElement)) {
    throw new Error(`the builder's page has no form#${FORM_ID}`);
  }
  installSaveForm(form, { application: boot.application, target: boot.target, afterSave: boot.afterSave }, client);
  start(
    {
      containerId: CONTAINER_ID,
      application: boot.application,
      applicationOrigin: boot.applicationOrigin,
      target: boot.target,
      csrfToken: boot.csrfToken,
      lightmode: boot.lightmode,
      options: boot.options,
      layout: boot.layout,
      mode: boot.mode,
    },
    client,
  );
  return true;
}
