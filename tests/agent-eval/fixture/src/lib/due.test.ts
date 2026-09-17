import { describe, expect, it } from "vitest";

import { daysUntil, formatDue } from "./due";

describe("daysUntil", () => {
  it("counts whole days forwards", () => {
    const from = new Date("2026-01-01T00:00:00Z");
    expect(daysUntil(new Date("2026-01-04T00:00:00Z"), from)).toBe(3);
  });
});

describe("formatDue", () => {
  it("says so when there is no due date", () => {
    expect(formatDue(null)).toBe("no due date");
  });

  it("names a date three days out", () => {
    const now = new Date("2026-01-01T00:00:00Z");
    expect(formatDue("2026-01-04T00:00:00Z", now)).toBe("in 3 days");
  });
});
