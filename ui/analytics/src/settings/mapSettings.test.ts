import { describe, expect, it } from "vitest";

import { baseMapLabel, changedValues } from "./mapSettings";

const saved = {
  map_style: "https://tiles.openfreemap.org/styles/positron",
  map_style_dark: "https://tiles.openfreemap.org/styles/dark",
  map_hosts: "",
};

describe("the Settings page's base maps", () => {
  it("sends only what changed, trimmed, so an untouched key is kept as stored", () => {
    expect(changedValues(saved, saved)).toEqual({});
    expect(changedValues(saved, { ...saved, map_hosts: "  https://a.example.com " })).toEqual({
      map_hosts: "https://a.example.com",
    });
    // Emptying a style is a change: no base map.
    expect(changedValues(saved, { ...saved, map_style_dark: " " })).toEqual({
      map_style_dark: "",
    });
    // Spaces alone are not.
    expect(changedValues(saved, { ...saved, map_style: `${saved.map_style}  ` })).toEqual({});
  });

  it("names a base map by its host and style", () => {
    expect(baseMapLabel(saved.map_style)).toBe("tiles.openfreemap.org · positron");
    expect(baseMapLabel("https://Maps.Example.com/styles/light.json?key=1")).toBe("maps.example.com · light");
    expect(baseMapLabel("https://maps.example.com")).toBe("maps.example.com");
    expect(baseMapLabel("")).toBeNull();
    expect(baseMapLabel(null)).toBeNull();
  });
});
