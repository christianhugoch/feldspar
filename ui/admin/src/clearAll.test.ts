// Clear all's dialog: every store starts ticked, and only the unticked ones
// stay on disk.

import { describe, expect, it } from "vitest";

import { clearAllBody, toggleKept } from "./clearAll";

const stores = [
  { name: "docs", backend: "local", directory: "/srv/docs" },
  { name: "site", backend: "git", directory: "/data/git-stores/site" },
];

describe("clearAllBody", () => {
  it("removes every store from disk when nothing was unticked", () => {
    expect(clearAllBody(stores, new Set())).toEqual({ delete_from_disk: ["docs", "site"] });
  });

  it("leaves an unticked store on disk", () => {
    const kept = toggleKept(new Set(), "docs");
    expect(clearAllBody(stores, kept)).toEqual({ delete_from_disk: ["site"] });
    expect(clearAllBody(stores, toggleKept(kept, "docs"))).toEqual({
      delete_from_disk: ["docs", "site"],
    });
  });
});
