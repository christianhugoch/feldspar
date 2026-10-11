// A scrollable modal whose header, body and footer sit inside a `<form>` only
// scrolls if the form passes `.modal-content`'s bounded height on to the body
// (see the rule at the end of `admin.css`). Without it a long dialog — the
// automated backup one — clips its bottom fields and Save button. The layout
// itself was checked in Chromium; this pins the rule and the dialog using it.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

// Read from disk: vitest does not hand a `.css?raw` import the file's text.
const read = (path: string) => readFileSync(fileURLToPath(new URL(path, import.meta.url)), "utf8");
const css = read("./admin.css");
const backupTab = read("./screens/BackupTab.tsx");

describe("scrollable modals wrapped in a form", () => {
  it("make the form a shrinkable flex column", () => {
    const rule = /\.modal-dialog-scrollable \.modal-content > form \{([^}]*)\}/.exec(css);
    expect(rule).not.toBeNull();
    const body = rule![1];
    expect(body).toMatch(/display:\s*flex/);
    expect(body).toMatch(/flex-direction:\s*column/);
    expect(body).toMatch(/min-height:\s*0/);
    expect(body).toMatch(/overflow:\s*hidden/);
  });

  it("include the automated backup dialog", () => {
    expect(backupTab).toMatch(
      /<Modal show=\{editing !== null\}.*\bscrollable>\s*\{editing && \(\s*<Form /,
    );
  });
});
