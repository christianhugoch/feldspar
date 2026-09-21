// The `builder` domain: v1's builder, translated (design §16.x, task 3.5).
//
// The vendored builder already has the whole mechanism. `useTranslation` in
// `vendor/saltcorn-builder/hooks/useTranslation.js` is
//
//     const t = (phrase) => translations[phrase] || phrase;
//
// — a lookup keyed by the English phrase, which is decision D1 written in 2021.
// Every `t("Delete")` in those components has been calling it since the bundle
// was vendored, and the only thing missing was the map: v1's server filled
// `options.translations` from its own i18n and this server filled it with
// nothing, so every phrase fell through to itself.
//
// So this file is the map, and nothing else. It loads
// `src/locales/{locale}.json` — the same flat catalogue shape the `core` and
// `admin` domains use, written by the same `feldspar i18n translate` — and
// flattens it to the `Record<string, string>` v1's hook expects.
//
// **Nothing is loaded for the source language** (D11): the key is the English,
// so an English builder needs no map at all.

/** The source language, which is also every key in every catalogue (D1). */
const SOURCE_LOCALE = "en";

/** A catalogue entry: a string, or the plural forms of one. */
type Message = string | Record<string, string>;

/**
 * Every catalogue in `src/locales`, as a lazy `import()` each.
 *
 * **A written-out map rather than `import.meta.glob`**, which is the one place
 * this file differs from the admin SPA's `i18n.tsx`: that bundle is built by
 * Vite, and this one by esbuild (`build.mjs`), which has no glob import and no
 * way to resolve a dynamic `import()` whose specifier is computed. So adding a
 * locale here is adding a file *and* a line — three seconds of work, and the
 * alternative is a bundler plugin nobody would remember exists.
 *
 * Each entry is a lazy `import()`, so esbuild emits one chunk per locale and
 * the builder fetches none of them until it is opened in one.
 */
const CATALOGUES: Record<
  string,
  () => Promise<{ default: Record<string, Message> }>
> = {
  // Filled as locales are shipped (`feldspar i18n translate --domain builder`).
};

/**
 * The `translations` map for `locale`, for v1's `renderBuilder` options.
 *
 * Flat, because v1's `t` is `translations[phrase]` and has no arguments to
 * select a plural on. An entry that *is* a plural object contributes its
 * `other` form — the one a phrase with no count in hand should be — rather than
 * being dropped, because a partly-wrong plural still reads as the language and
 * a missing entry reads as English in the middle of a French toolbox.
 *
 * A locale with no catalogue yields `{}`, which is not a failure: the phrase is
 * its own key, so an empty map renders correct English.
 */
export async function builderTranslations(
  locale: string | undefined,
): Promise<Record<string, string>> {
  if (!locale || locale === SOURCE_LOCALE) return {};
  const load = CATALOGUES[locale];
  if (!load) return {};
  try {
    const catalogue = (await load()).default ?? {};
    const flat: Record<string, string> = {};
    for (const [key, message] of Object.entries(catalogue)) {
      if (typeof message === "string") {
        flat[key] = message;
      } else if (message && typeof message === "object") {
        const form = message.other ?? Object.values(message)[0];
        if (typeof form === "string") flat[key] = form;
      }
    }
    return flat;
  } catch {
    return {};
  }
}

/**
 * v1's `renderBuilder` options with `translations` filled in.
 *
 * Merged rather than replaced: the options are the view runtime's, computed by
 * the worker, and whatever else is in them is none of this function's business.
 */
export function withTranslations(
  options: unknown,
  translations: Record<string, string>,
): unknown {
  if (!options || typeof options !== "object") return options;
  return { ...(options as Record<string, unknown>), translations };
}
