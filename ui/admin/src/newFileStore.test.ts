/**
 * The "Create a new local file store" choice on a new application's store
 * picker: which settings get it, and what the admin is told once it was used.
 */

import { describe, expect, it } from "vitest";

import { NEW_LOCAL_FILE_STORE, createdStoresText, newStoreOptions } from "./newFileStore";

describe("newStoreOptions", () => {
  it("offers the choice on every setting that names a file store", () => {
    expect(newStoreOptions(["store"], true, "Create a new local file store")).toEqual({
      store: [{ value: NEW_LOCAL_FILE_STORE, label: "Create a new local file store" }],
    });
  });

  it("offers nothing when editing, since only a create acts on it", () => {
    expect(newStoreOptions(["store"], false, "Create")).toEqual({});
  });

  it("offers nothing for a framework whose settings name no store", () => {
    expect(newStoreOptions([], true, "Create")).toEqual({});
  });

  it("uses the value the server reserves", () => {
    // `sc_catalog::NEW_LOCAL_FILE_STORE` — a change on one side only would turn
    // the choice into a store name that does not exist.
    expect(NEW_LOCAL_FILE_STORE).toBe("__new_local_file_store__");
  });
});

describe("createdStoresText", () => {
  it("says nothing when no store was created", () => {
    expect(createdStoresText(undefined)).toBeNull();
    expect(createdStoresText(null)).toBeNull();
    expect(createdStoresText([])).toBeNull();
  });

  it("names the store that was created", () => {
    expect(createdStoresText(["todo1"])).toBe(
      "A new local file store, “todo1”, was created for its code.",
    );
  });
});
