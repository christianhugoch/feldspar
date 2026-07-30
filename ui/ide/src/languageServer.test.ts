/**
 * The two decisions the IDE makes about the language server without one: where
 * its socket is, and what to say when there will not be one (design §12.1).
 */

import { describe, expect, it } from "vitest";

import { languageServerUrl, noSemanticsMessage } from "./languageServer";

describe("where the language server's socket is", () => {
  it("is the same origin as the page, on the route the server mounts", () => {
    expect(languageServerUrl({ protocol: "http:", host: "localhost:3000" }, "app-source")).toBe(
      "ws://localhost:3000/ide/lsp/app-source",
    );
  });

  /**
   * A `ws:` socket opened from an `https:` page is mixed content and the browser
   * blocks it, which would present as "this store has no semantics" on exactly
   * the deployments that are configured correctly.
   */
  it("uses wss: wherever the page itself is over TLS", () => {
    expect(languageServerUrl({ protocol: "https:", host: "saltcorn.example.com" }, "app")).toBe(
      "wss://saltcorn.example.com/ide/lsp/app",
    );
  });

  it("encodes a store name that is not URL-safe", () => {
    expect(languageServerUrl({ protocol: "http:", host: "h" }, "my store/x")).toBe(
      "ws://h/ide/lsp/my%20store%2Fx",
    );
  });
});

describe("what the admin is told when there are no semantics", () => {
  /**
   * The server writes these sentences (a store with no local path, a project with
   * no `node_modules`, a machine at its limit) and sends them in the close frame.
   * Rewording them here would be a second place they are written.
   */
  it("shows the server's own reason, verbatim", () => {
    expect(
      noSemanticsMessage(
        "assets",
        "assets has no local path, so it can be edited but not type-checked",
      ),
    ).toBe("assets has no local path, so it can be edited but not type-checked");
  });

  it("says something rather than nothing when the socket closed silently", () => {
    expect(noSemanticsMessage("app-source", "")).toBe(
      "No TypeScript semantics for app-source: the language server is not available.",
    );
    expect(noSemanticsMessage("app-source", "   ")).toContain("not available");
  });
});
