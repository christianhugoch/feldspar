// The `builder` domain's map, and what it is for (task 3.5).
//
// The assertion that matters is the shape: v1's `useTranslation` is
// `translations[phrase] || phrase`, so what this module produces has to be a
// flat `Record<string, string>` keyed by the English phrase, merged into the
// options the worker computed without disturbing anything else in them.

import { describe, expect, it } from "vitest";

import { builderTranslations, withTranslations } from "./i18n";

describe("builderTranslations", () => {
  it("is empty for the source language, and asks for nothing", async () => {
    // D11: the key *is* the English, so an English builder needs no map — and
    // must not pay for one.
    expect(await builderTranslations("en")).toEqual({});
    expect(await builderTranslations(undefined)).toEqual({});
  });

  it("is empty for a locale with no catalogue, rather than a failure", async () => {
    // A phrase is its own key, so an empty map renders correct English. A
    // facility whose normal state is "60% translated" cannot treat this as an
    // error.
    expect(await builderTranslations("qqq")).toEqual({});
  });
});

describe("withTranslations", () => {
  it("merges the map into the options without disturbing them", () => {
    const options = { mode: "show", fields: ["a"], roles: [] };
    const merged = withTranslations(options, { Delete: "Supprimer" }) as Record<
      string,
      unknown
    >;
    expect(merged.mode).toBe("show");
    expect(merged.fields).toEqual(["a"]);
    expect(merged.translations).toEqual({ Delete: "Supprimer" });
    // The options the worker computed are not mutated: they are also the
    // options `save-form.ts` reads back.
    expect(options).not.toHaveProperty("translations");
  });

  it("leaves options that are not an object alone", () => {
    // The worker answers the options; this module is not the place to decide
    // that a runtime answered the wrong shape.
    expect(withTranslations(null, { a: "b" })).toBe(null);
    expect(withTranslations("nonsense", { a: "b" })).toBe("nonsense");
  });
});
