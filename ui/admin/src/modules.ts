// The Modules tab's model: what an admin may install, and how an installed
// module reads.
//
// Kept out of the component for the reason `backup.ts` is: the arithmetic of a
// form — what counts as a filled-in specifier, what the census sentence says,
// whether a module is usable — is testable without a browser, and the component
// is then only flow.

import type { ListModulesResponse } from "./client";

/** One installed module, as `listModules` describes it. */
export type Module = ListModulesResponse["modules"][number];

/** Where a module's package comes from. The two the server accepts. */
export type ModuleSource = "npm" | "local";

/** What an admin fills in to install one. */
export type InstallForm = {
  source: ModuleSource;
  location: string;
};

/** A blank install form. npm first, because the registry is where a module
 * normally comes from and a local directory is the developer's case. */
export const EMPTY_INSTALL: InstallForm = { source: "npm", location: "" };

/** The label above the specifier box, which is a different thing for each
 * source: one is a package name, the other is a path on the *server*. */
export function locationLabel(source: ModuleSource): string {
  return source === "npm" ? "Package name" : "Directory on the server";
}

/** The placeholder in it. */
export function locationPlaceholder(source: ModuleSource): string {
  return source === "npm" ? "@saltcorn/mqtt" : "/srv/checkouts/mqtt";
}

/** Why the Install button is disabled, or `null` when it is not.
 *
 * A sentence rather than a boolean because the two reasons are worth saying: a
 * form with nothing in it, and a server with no npm to install with — the second
 * of which an admin cannot fix from this screen and should not discover from a
 * failed install.
 */
export function installBlocked(form: InstallForm, npm: boolean): string | null {
  if (!npm) {
    return "This server has no npm on its PATH, so it cannot install a module. Install Node.js and restart Saltcorn.";
  }
  if (form.location.trim() === "") {
    return source_is_npm(form)
      ? "Type the name of an npm package."
      : "Type the path of a directory on this server.";
  }
  if (!source_is_npm(form) && !form.location.trim().startsWith("/")) {
    return "A local module is installed from an absolute path, so that it means the same thing wherever the server was started from.";
  }
  return null;
}

function source_is_npm(form: InstallForm): boolean {
  return form.source === "npm";
}

/** The version line under a module's name: what is installed, and from where. */
export function moduleSubtitle(module: Module): string {
  const version = module.version ? `v${module.version}` : "not installed";
  const from = module.source === "npm" ? "npm" : module.location;
  return `${version} · ${from}`;
}

/** The names of the actions a module supplies, in the order it declared them. */
export function actionNames(module: Module): string[] {
  return module.actions.map((action) => action.name);
}

/** The sentence about what a module supplies that this version does not load.
 *
 * `null` when there is nothing to say, so the component renders nothing rather
 * than an empty note. */
export function unsupportedSentence(module: Module): string | null {
  if (module.unsupported.length === 0) return null;
  const parts = module.unsupported.map((entity) => {
    const count = entity.count ?? null;
    return count === null ? entity.key : `${count} × ${entity.key}`;
  });
  return `Also supplies ${parts.join(", ")}, which this version of Saltcorn does not load yet.`;
}

/** How a module's state reads in a badge: what it is, and the tone to use. */
export function moduleStatus(module: Module): {
  label: string;
  tone: "green" | "yellow" | "red";
} {
  if (!module.loaded) return { label: "Not loaded", tone: "red" };
  if (module.issues.length > 0) return { label: "Loaded with issues", tone: "yellow" };
  const count = module.actions.length;
  return { label: count === 1 ? "1 action" : `${count} actions`, tone: "green" };
}

/** Whether a module has settings of its own to configure. */
export function isConfigurable(module: Module): boolean {
  return module.config_spec.length > 0;
}

/** The stored configuration as the form's string values expect it.
 *
 * `configuration` crosses the wire as an opaque JSON object (it is whatever the
 * module declared), so it is narrowed here rather than in the component. */
export function configValues(module: Module): Record<string, string> {
  const config = module.configuration;
  if (typeof config !== "object" || config === null || Array.isArray(config)) return {};
  const values: Record<string, string> = {};
  for (const [key, value] of Object.entries(config as Record<string, unknown>)) {
    if (value === null || value === undefined) continue;
    values[key] = typeof value === "string" ? value : JSON.stringify(value);
  }
  return values;
}
