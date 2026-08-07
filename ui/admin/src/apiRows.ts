// The application form's **API rows**: one enabled provider, its mount, and the
// settings that provider declares.
//
// A module rather than state inside the screen, for the reason `graphqlExplorer.ts`
// is one: the part that has to be right is a *conversion* — a stored
// `{ provider, mount, config }` becomes form values and comes back as the same
// record, with each setting the type its provider's spec says — and that is
// testable without a browser (`apiRows.test.ts`). What is left in the screen is
// controls.
//
// Nothing here knows what a REST or a GraphQL setting is. The provider declares
// its `config_spec` (§13.3) and this renders values through it, so GraphQL's
// aggregation switch round-trips because it is a `bool` in a spec, not because
// this file has heard of aggregates.

import { buildConfig, readConfig, type FieldSpec } from "./settings";

/** What this module needs of a `listApiProviders` entry: the registry name and
 * the settings the provider declares. Structural, so the screen can pass the
 * endpoint's own response type. */
export type ProviderSpec = { name: string; config_spec: FieldSpec[] };

/** One row of the APIs list, as the form edits it. */
export type ApiRow = {
  provider: string;
  mount: string;
  /** The settings, as the strings a form holds. Keyed by setting name. */
  config: Record<string, string>;
  /** What was stored, untouched. Used only for a provider this server does not
   * register: its spec is unknown, so its settings cannot be re-typed, and
   * dropping them on save would quietly destroy the configuration of a provider
   * that is merely absent (a plugin not loaded, a name from a newer server). */
  storedConfig: Record<string, unknown>;
};

/** An empty row — what "Add" produces. */
export function blankApiRow(): ApiRow {
  return { provider: "", mount: "", config: {}, storedConfig: {} };
}

/** The settings spec for a row's provider, or `[]` when the server does not
 * register it (which is also what an unregistered provider's form shows: no
 * controls, rather than invented ones). */
export function specFor(providers: ProviderSpec[], provider: string): FieldSpec[] {
  return providers.find((p) => p.name === provider)?.config_spec ?? [];
}

/** Read the stored `apis` of an application into editable rows. */
export function apiRowsFromApp(
  apis: { provider: string; mount: string; config: unknown }[],
): ApiRow[] {
  return apis.map((a) => ({
    provider: a.provider,
    mount: a.mount,
    config: readConfig(a.config),
    storedConfig:
      a.config && typeof a.config === "object"
        ? { ...(a.config as Record<string, unknown>) }
        : {},
  }));
}

/** Turn the rows back into the `apis` a create/update request carries, each
 * setting coerced to the type its provider declared.
 *
 * A row whose provider is not registered keeps exactly the settings it arrived
 * with: this form cannot type them, and the server will refuse the unknown
 * provider anyway — with a message about the provider, which is the actual
 * problem, rather than about settings that were silently rewritten first. */
export function apiRowsToRequest(
  rows: ApiRow[],
  providers: ProviderSpec[],
): { provider: string; mount: string; config: Record<string, unknown> }[] {
  return rows
    .filter((r) => r.provider.trim() || r.mount.trim())
    .map((r) => {
      const known = providers.some((p) => p.name === r.provider);
      return {
        provider: r.provider,
        mount: r.mount,
        config: known ? buildConfig(specFor(providers, r.provider), r.config) : r.storedConfig,
      };
    });
}
