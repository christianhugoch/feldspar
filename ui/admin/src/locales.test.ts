// The language choices the admin UI offers (design §16.x).
//
// The decisions worth pinning are the three that would otherwise be quietly
// wrong: that one enabled locale is no choice at all (D11), that a language is
// named in its own language, and that a user's stored locale survives being
// disabled rather than silently snapping to another one.

import { describe, expect, it } from "vitest";

import { MONOLINGUAL, isMultilingual, localeLabel, localeOptions } from "./locales";

describe("isMultilingual", () => {
  it("is false for an installation that has configured nothing", () => {
    expect(isMultilingual(MONOLINGUAL)).toBe(false);
    expect(isMultilingual({ default: "fr", current: "fr", enabled: ["fr"] })).toBe(false);
  });

  it("is true the moment there are two", () => {
    expect(isMultilingual({ default: "en", current: "en", enabled: ["en", "fr"] })).toBe(true);
  });
});

describe("localeLabel", () => {
  it("names a language in its own language, with the tag beside it", () => {
    // `Intl.DisplayNames` is the browser's own CLDR data; the assertion is the
    // shape, not the exact spelling, which is the engine's to choose.
    const french = localeLabel("fr");
    expect(french).toContain("(fr)");
    expect(french.toLowerCase()).toContain("fran");
  });

  it("falls back to the tag when the engine refuses it", () => {
    // Stored text is not a guarantee: a tag that `Intl` throws on still has to
    // render as something, and itself is the only honest something.
    expect(localeLabel("not a tag")).toBe("not a tag");
  });

  it("always shows the tag, because a name alone is not a catalogue", () => {
    // `pt` and `pt-BR` are two catalogues and can render as one name.
    expect(localeLabel("pt-BR")).toContain("pt-BR");
    expect(localeLabel("pt")).toContain("(pt)");
  });
});

describe("localeOptions", () => {
  it("is the enabled set when the current value is in it", () => {
    expect(localeOptions("fr", { default: "en", current: "en", enabled: ["en", "fr"] })).toEqual(["en", "fr"]);
    expect(localeOptions(null, { default: "en", current: "en", enabled: ["en", "fr"] })).toEqual(["en", "fr"]);
  });

  it("keeps a stored locale that is no longer enabled", () => {
    // A user reading Portuguese must not be silently switched to English by
    // opening their own record: the next Save would make it true.
    expect(localeOptions("pt-BR", { default: "en", current: "en", enabled: ["en", "fr"] })).toEqual([
      "en",
      "fr",
      "pt-BR",
    ]);
  });
});
