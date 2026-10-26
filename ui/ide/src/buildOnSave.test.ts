/**
 * Build on save (TODO-astro W2.5): which application a saved file belongs to,
 * and when its quick build runs.
 */

import { describe, expect, it } from "vitest";

import type { BuildableApplication } from "./applications";
import { SaveScheduler, applicationForSave, type Timers } from "./buildOnSave";

function app(id: string, sourcePath: string): BuildableApplication {
  return {
    application: { id, name: id } as BuildableApplication["application"],
    sourcePath,
  };
}

describe("which application a saved file belongs to", () => {
  const site = app("site", "site");
  const docs = app("docs", "site/docs");
  const root = app("root", "");

  it("is the project whose directory the file is in, the innermost first", () => {
    expect(applicationForSave([site, docs], "site/src/pages/index.astro")).toBe(site);
    expect(applicationForSave([site, docs], "site/docs/src/a.astro")).toBe(docs);
    expect(applicationForSave([site], "elsewhere/a.astro")).toBeNull();
    expect(applicationForSave([site], "site-old/a.astro")).toBeNull();
    expect(applicationForSave([root], "src/a.astro")).toBe(root);
  });

  it("is nobody for what the build or the installer writes", () => {
    expect(applicationForSave([site], "site/dist/index.html")).toBeNull();
    expect(applicationForSave([site], "site/node_modules/x/index.js")).toBeNull();
    expect(applicationForSave([root], "dist/index.html")).toBeNull();
  });
});

/** Timers a test runs by hand. */
function manualTimers(): Timers & { fire(): void; pending(): number } {
  let next = 0;
  const waiting = new Map<number, () => void>();
  return {
    setTimeout(fn) {
      next += 1;
      waiting.set(next, fn);
      return next;
    },
    clearTimeout(handle) {
      waiting.delete(handle as number);
    },
    fire() {
      const due = [...waiting.values()];
      waiting.clear();
      due.forEach((fn) => fn());
    },
    pending: () => waiting.size,
  };
}

describe("when a quick build runs", () => {
  it("once for a burst of saves", async () => {
    const timers = manualTimers();
    const built: string[] = [];
    const scheduler = new SaveScheduler(async (a) => void built.push(a.application.id), 10, timers);
    const site = app("site", "site");
    scheduler.saved(site);
    scheduler.saved(site);
    scheduler.saved(site);
    expect(timers.pending()).toBe(1);
    timers.fire();
    await Promise.resolve();
    expect(built).toEqual(["site"]);
  });

  it("again after a running build, when a save came in meanwhile", async () => {
    const timers = manualTimers();
    const finish: Array<() => void> = [];
    let builds = 0;
    const scheduler = new SaveScheduler(
      () =>
        new Promise<void>((resolve) => {
          builds += 1;
          finish.push(resolve);
        }),
      10,
      timers,
    );
    const site = app("site", "site");
    scheduler.saved(site);
    timers.fire();
    expect(builds).toBe(1);
    // Saved while building: not a second build at once…
    scheduler.saved(site);
    timers.fire();
    await Promise.resolve();
    expect(builds).toBe(1);
    // …but one more when the first ends.
    finish[0]();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(builds).toBe(2);
    finish[1]();
  });
});
