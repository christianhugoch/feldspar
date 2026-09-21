// The languages this installation serves, as the admin UI needs them.
//
// Two keys in Settings → Localisation decide everything here (design §16.x): a
// default locale, and the enabled set a request may be negotiated into. Both
// arrive on `authStatus`, which is the call the SPA makes before it renders
// anything — so the list is in hand wherever a screen offers a choice of
// language without any screen having to ask for it a second time.
//
// **An installation with one enabled locale offers no choice at all**, which is
// decision D11 on this side of the wire: `isMultilingual` is false, and the
// screens that would show a picker show nothing instead. i18n is a thing an
// admin turns on, not a control every installation grows.

import { useEffect, useState } from "react";

import { api } from "./api";
import type { AuthStatusResponse } from "./client";

/** The default locale and the enabled set, as `authStatus` reports them. */
export type Locales = AuthStatusResponse["locales"];

/** An installation that has configured nothing: English, and nothing to pick. */
export const MONOLINGUAL: Locales = {
  default: "en",
  current: "en",
  enabled: ["en"],
};

/**
 * Load the enabled locales once.
 *
 * Failure yields [MONOLINGUAL] rather than a crash: not being able to offer a
 * language picker is not a reason for the users screen to fail to render.
 */
export function useLocales(): Locales {
  const [locales, setLocales] = useState<Locales>(MONOLINGUAL);
  useEffect(() => {
    void (async () => {
      try {
        setLocales((await api.authStatus()).locales ?? MONOLINGUAL);
      } catch {
        setLocales(MONOLINGUAL);
      }
    })();
  }, []);
  return locales;
}

/** Whether there is a choice of language to offer at all. */
export function isMultilingual(locales: Locales): boolean {
  return (locales?.enabled?.length ?? 0) > 1;
}

/**
 * A locale tag rendered for a human: the language's name **in that language**,
 * with the tag beside it.
 *
 * In the language itself, because the one person guaranteed to recognise
 * "Français" is the person who wants it, and a list of languages written in the
 * reader's language is a list the reader cannot use to find their own. The tag
 * stays visible because `pt` and `pt-BR` have the same name in some renderings
 * and an admin is choosing a catalogue, not a country.
 *
 * `Intl.DisplayNames` is in every engine this bundle targets; if it throws or
 * knows nothing about the tag, the tag is its own label, which is still correct.
 */
export function localeLabel(tag: string): string {
  try {
    const name = new Intl.DisplayNames([tag], { type: "language" }).of(tag);
    return name && name !== tag ? `${name} (${tag})` : tag;
  } catch {
    return tag;
  }
}

/**
 * The options for a language select, with `current` included even when the
 * server no longer lists it.
 *
 * The same rule `roleOptions` follows, for the same reason: a user whose stored
 * language has since been disabled must still see it as their current value,
 * because a select that silently snapped to something else would change what
 * they read the next time somebody pressed Save.
 */
export function localeOptions(current: string | null, locales: Locales): string[] {
  const listed = locales?.enabled ?? [];
  if (!current || listed.includes(current)) return [...listed];
  return [...listed, current];
}
