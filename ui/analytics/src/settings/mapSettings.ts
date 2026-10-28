// The base maps as the Settings page edits them (`mapSettings`,
// `saveMapSettings`): the three values the server stores, each a string — a
// style's URL, empty for none, and the further hosts comma-separated.

import type { MapSettingsResponse } from "../client";

/** The form's values, keyed as stored. */
export type MapValues = MapSettingsResponse["values"];

/** The keys of {@link MapValues}, in the order the form shows them. */
export const MAP_KEYS = ["map_style", "map_style_dark", "map_hosts"] as const;

/** The values in `draft` that differ from `saved`, trimmed — what Save sends,
 * so a key the admin did not touch is kept as stored. */
export function changedValues(saved: MapValues, draft: MapValues): Partial<MapValues> {
  const out: Partial<MapValues> = {};
  for (const key of MAP_KEYS) {
    const value = draft[key].trim();
    if (value !== saved[key].trim()) out[key] = value;
  }
  return out;
}

/** A base map's short name for a sentence: the style URL's host and last path
 * segment (`tiles.openfreemap.org · positron`), or `null` for no base map. */
export function baseMapLabel(style: string | null | undefined): string | null {
  if (!style) return null;
  const m = /^https?:\/\/([^/?#\s]+)([^?#\s]*)/i.exec(style.trim());
  if (!m) return style;
  const last = m[2]
    .split("/")
    .filter((p) => p !== "")
    .pop()
    ?.replace(/\.json$/i, "");
  return last ? `${m[1].toLowerCase()} · ${last}` : m[1].toLowerCase();
}
