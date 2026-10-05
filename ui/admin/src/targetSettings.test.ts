/**
 * A build target's own settings on the application form: split out of the
 * framework's, and a file picker's choices from the application's store.
 */

import { describe, expect, it } from "vitest";

import { NEW_LOCAL_FILE_STORE } from "./newFileStore";
import { isShown, type FieldSpec } from "./settings";
import { fileOptions, storeNameOf, splitTargetSettings } from "./targetSettings";

const field = (name: string): FieldSpec =>
  ({
    name,
    label: name,
    type: "text",
    required: false,
    options: [],
    multiline: false,
    secret: false,
    create_only: false,
  }) as FieldSpec;

describe("splitTargetSettings", () => {
  it("moves each target's settings into a card of its own", () => {
    const spec = ["store", "project", "app_id", "app_icon"].map(field);
    const split = splitTargetSettings(spec, [
      { name: "android", label: "Android APK", options: ["app_id", "app_icon"] },
    ]);
    expect(split.general.map((f) => f.name)).toEqual(["store", "project"]);
    expect(split.targets).toHaveLength(1);
    expect(split.targets[0].label).toBe("Android APK");
    expect(split.targets[0].spec.map((f) => f.name)).toEqual(["app_id", "app_icon"]);
    expect(split.targets[0].operations).toEqual([]);
  });

  it("gives a target without settings no card", () => {
    const split = splitTargetSettings([field("store")], [
      { name: "android", label: "Android APK", options: [] },
    ]);
    expect(split.general.map((f) => f.name)).toEqual(["store"]);
    expect(split.targets).toEqual([]);
  });
});

describe("storeNameOf", () => {
  it("is the name of the store the application uses", () => {
    expect(storeNameOf(["store"], { store: "apps" })).toBe("apps");
  });

  it("is none while the store is blank or still to be created", () => {
    expect(storeNameOf(["store"], { store: "" })).toBeNull();
    expect(storeNameOf(["store"], { store: NEW_LOCAL_FILE_STORE })).toBeNull();
  });
});

describe("fileOptions", () => {
  const settings = [
    { name: "app_icon", extensions: ["png"] },
    { name: "keystore", extensions: ["jks"] },
  ];

  it("offers each setting the files of its own kind", () => {
    const options = fileOptions(
      settings,
      { app_icon: ["a.png", "b.png"], keystore: ["keys/release.jks"] },
      {},
    );
    expect(options.app_icon.map((o) => o.value)).toEqual(["a.png", "b.png"]);
    expect(options.keystore.map((o) => o.value)).toEqual(["keys/release.jks"]);
  });

  it("keeps a current value the store no longer has", () => {
    const options = fileOptions(settings, { app_icon: ["a.png"] }, { app_icon: "gone.png" });
    expect(options.app_icon.map((o) => o.value)).toEqual(["a.png", "gone.png"]);
    expect(options.keystore).toEqual([]);
  });
});

describe("isShown", () => {
  const spec: FieldSpec[] = [
    { ...field("build_type"), default: "release" },
    {
      ...field("own_key"),
      type: "bool",
      default: false,
      show_if: [{ name: "build_type", values: ["release"] }],
    },
    {
      ...field("keystore_alias"),
      show_if: [
        { name: "build_type", values: ["release"] },
        { name: "own_key", values: [true] },
      ],
    },
  ];
  const [, ownKey, alias] = spec;

  it("shows the checkbox for a release build, by default", () => {
    expect(isShown(ownKey, spec, {})).toBe(true);
    expect(isShown(ownKey, spec, { build_type: "debug" })).toBe(false);
  });

  it("shows the keystore settings only once the checkbox is ticked", () => {
    expect(isShown(alias, spec, {})).toBe(false);
    expect(isShown(alias, spec, { own_key: "true" })).toBe(true);
    expect(isShown(alias, spec, { own_key: "true", build_type: "debug" })).toBe(false);
  });
});
