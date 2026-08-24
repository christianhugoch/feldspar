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

/** How a module's state reads in a badge: what it is, and the tone to use.
 *
 * A module may supply actions, table providers, or both, and the badge counts
 * what it actually has: `@saltcorn/rss` is a plugin with no actions at all, and
 * "0 actions" would read as "this module does nothing" about a module that
 * serves a table. */
export function moduleStatus(module: Module): {
  label: string;
  tone: "green" | "yellow" | "red";
} {
  if (!module.loaded) return { label: "Not loaded", tone: "red" };
  if (module.issues.length > 0) return { label: "Loaded with issues", tone: "yellow" };
  const parts: string[] = [];
  const actions = module.actions.length;
  const providers = module.table_providers.length;
  if (actions > 0) parts.push(actions === 1 ? "1 action" : `${actions} actions`);
  if (providers > 0) {
    parts.push(providers === 1 ? "1 table provider" : `${providers} table providers`);
  }
  return {
    label: parts.length === 0 ? "Loaded" : parts.join(", "),
    tone: "green",
  };
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

// --- permissions -----------------------------------------------------------
// What a module's worker may reach. Four allow-lists, closed by default, and
// every empty one means *nothing* rather than *everything* — which is the one
// place this screen could mislead, so the summary sentence says "nothing"
// out loud rather than rendering four empty boxes and leaving it to be inferred.

/** The four things a module can be granted. The keys are the server's. */
export type PermissionKind = "net" | "read" | "write" | "env";

/** A module's permission set as the API sends it. */
export type ModulePermissionSet = Record<PermissionKind, string[]>;

/** Nothing granted. */
export const CLOSED_PERMISSIONS: ModulePermissionSet = { net: [], read: [], write: [], env: [] };

/** One kind, as the form asks for it. */
export type PermissionKindSpec = {
  key: PermissionKind;
  label: string;
  help: string;
  placeholder: string;
};

/** The four, in the order the form shows them: the one a real module actually
 * needs first, and the filesystem below it. */
export const PERMISSION_KINDS: PermissionKindSpec[] = [
  {
    key: "net",
    label: "Hosts it may connect to",
    help: "A host, or a host and a port — broker.example or broker.example:1883. One per line.",
    placeholder: "broker.example:1883",
  },
  {
    key: "read",
    label: "Paths it may read",
    help: "Absolute paths; a directory allows everything under it. The module can always read its own package.",
    placeholder: "/srv/data",
  },
  {
    key: "write",
    label: "Paths it may write",
    help: "Absolute paths. One per line.",
    placeholder: "/srv/data/out",
  },
  {
    key: "env",
    label: "Environment variables it may read",
    help: "Variable names. One per line. A variable that is not listed reads as undefined rather than failing.",
    placeholder: "MQTT_PASSWORD",
  },
];

/** A module's stored permissions, narrowed from the wire's opaque JSON.
 *
 * Anything unreadable is the **closed** set, never an open one: a screen that
 * could not parse what it was sent must not draw a module as more restricted
 * than it is — and closed is the direction that cannot mislead. */
export function modulePermissions(module: Module): ModulePermissionSet {
  const raw = module.permissions;
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return CLOSED_PERMISSIONS;
  const source = raw as Record<string, unknown>;
  const set: ModulePermissionSet = { net: [], read: [], write: [], env: [] };
  for (const { key } of PERMISSION_KINDS) {
    const list = source[key];
    if (Array.isArray(list)) {
      set[key] = list.filter((entry): entry is string => typeof entry === "string");
    }
  }
  return set;
}

/** Whether nothing at all is granted. */
export function isClosed(set: ModulePermissionSet): boolean {
  return PERMISSION_KINDS.every(({ key }) => set[key].length === 0);
}

/** The one-line summary above the form: what this module can reach, in words. */
export function permissionSummary(set: ModulePermissionSet): string {
  if (isClosed(set)) {
    return "Reaches nothing: no host, no file, no environment variable.";
  }
  const parts: string[] = [];
  if (set.net.length > 0) parts.push(`connects to ${set.net.join(", ")}`);
  if (set.read.length > 0) parts.push(`reads ${set.read.join(", ")}`);
  if (set.write.length > 0) parts.push(`writes ${set.write.join(", ")}`);
  if (set.env.length > 0) parts.push(`reads ${set.env.join(", ")}`);
  return `Reaches only these: ${parts.join("; ")}.`;
}

/** A textarea's text for one list, and the list back out of it. One entry per
 * line, because the entries are paths and hosts and both may contain a comma. */
export function permissionText(list: string[]): string {
  return list.join("\n");
}

/** The list an admin typed: trimmed, with blank lines dropped. */
export function parsePermissionText(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

/** Why an entry will be refused, or `null` when it will not.
 *
 * The same rules the server applies, said before the save rather than after it.
 * The server remains the authority — this is the form being helpful, not the
 * check. */
export function permissionProblem(kind: PermissionKind, entry: string): string | null {
  if (kind === "net") {
    if (entry.includes("://") || entry.includes("/")) {
      return `${entry} is a URL — write a host or a host:port, like broker.example:1883.`;
    }
    const colon = entry.lastIndexOf(":");
    if (colon > 0 && !entry.endsWith("]")) {
      const port = Number(entry.slice(colon + 1));
      if (!Number.isInteger(port) || port < 1 || port > 65535) {
        return `${entry.slice(colon + 1)} in ${entry} is not a port number.`;
      }
    }
    return null;
  }
  if (kind === "env") {
    return entry.includes("=") ? `${entry} is not a variable name.` : null;
  }
  return entry.startsWith("/")
    ? null
    : `${entry} is not an absolute path, so it would mean a different directory depending on where the server was started.`;
}

/** Every problem in a set, so the Save button can say why it is disabled. */
export function permissionProblems(set: ModulePermissionSet): string[] {
  const problems: string[] = [];
  for (const { key } of PERMISSION_KINDS) {
    for (const entry of set[key]) {
      const problem = permissionProblem(key, entry);
      if (problem !== null) problems.push(problem);
    }
  }
  return problems;
}
