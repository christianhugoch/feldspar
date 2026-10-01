// Bootstrap's right-to-left stylesheet is on the page for an `rtl` locale and
// only then.
//
// It used to be a lazy CSS `import()`, which the build's `cssCodeSplit: false`
// folds into the one global stylesheet — so every left-to-right page got
// `.form-check .form-check-input { float: right }`, and every checkbox and radio
// in the admin UI sat to the right of its label.

import { describe, expect, it } from "vitest";

import { RTL_STYLESHEET_ID, syncRtlStylesheet } from "./i18n";

type FakeLink = { id: string; rel: string; href: string; remove(): void };

/** Just enough of a `Document` for `syncRtlStylesheet`: a `<head>` of links. */
function fakeDocument() {
  const head: FakeLink[] = [];
  const doc = {
    head: {
      appendChild(link: FakeLink) {
        head.push(link);
        return link;
      },
    },
    createElement(): FakeLink {
      const link: FakeLink = {
        id: "",
        rel: "",
        href: "",
        remove() {
          head.splice(head.indexOf(link), 1);
        },
      };
      return link;
    },
    getElementById(id: string) {
      return head.find((l) => l.id === id) ?? null;
    },
  };
  return { head, doc: doc as unknown as Document };
}

describe("syncRtlStylesheet", () => {
  it("links nothing for a left-to-right page", () => {
    const { head, doc } = fakeDocument();
    syncRtlStylesheet(doc, "ltr", "/rtl.css");
    expect(head).toEqual([]);
  });

  it("links the stylesheet once for a right-to-left page", () => {
    const { head, doc } = fakeDocument();
    syncRtlStylesheet(doc, "rtl", "/rtl.css");
    syncRtlStylesheet(doc, "rtl", "/rtl.css");
    expect(head).toHaveLength(1);
    expect(head[0]).toMatchObject({ id: RTL_STYLESHEET_ID, rel: "stylesheet", href: "/rtl.css" });
  });

  it("unlinks it when the page goes back to left-to-right", () => {
    const { head, doc } = fakeDocument();
    syncRtlStylesheet(doc, "rtl", "/rtl.css");
    syncRtlStylesheet(doc, "ltr", "/rtl.css");
    expect(head).toEqual([]);
  });
});

/** Every non-test source file, the vendored tree excepted, as text. */
const SOURCES = import.meta.glob<string>(
  ["./**/*.{ts,tsx}", "!./vendor/**", "!./**/*.test.ts"],
  { query: "?raw", import: "default", eager: true },
);

describe("stylesheets", () => {
  // Under `cssCodeSplit: false` there is no such thing as a lazy stylesheet: a
  // dynamic import of a `.css` file lands in the one global CSS file, exactly as
  // a static import at the top of `main.tsx` would.
  it("are never imported lazily", () => {
    expect(Object.keys(SOURCES).length).toBeGreaterThan(50);
    const lazy = Object.entries(SOURCES)
      .filter(([, text]) => /import\(\s*["'][^"']+\.css["']\s*\)/.test(text))
      .map(([path]) => path);
    expect(lazy).toEqual([]);
  });
});
