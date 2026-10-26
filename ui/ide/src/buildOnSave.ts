/**
 * Build on save: the dev loop (TODO-astro W2.5).
 *
 * With it on, saving a file inside an application's source directory runs that
 * application's **quick** build (`buildApplication?quick=true`): the bundler
 * alone — no install step, no regenerated client, no prerendering — so the
 * preview shows the change in the time the bundler takes.
 *
 * The decisions are made here, as pure functions over what the API already
 * returns, so they are tested without a workbench: *which* application a saved
 * file belongs to, and *when* to build — once per burst of saves, and never two
 * builds of one application at a time.
 */

import type { BuildableApplication } from "./applications";

/**
 * Directories inside a project that a save never rebuilds from: what the build
 * writes, and what the installer does.
 */
const IGNORED = ["dist", "node_modules", ".git"];

/**
 * The application whose source `storePath` (a saved file, store-relative) is
 * in, or `null`. A file in two projects' directories (one nested in the other)
 * belongs to the innermost.
 */
export function applicationForSave(
  candidates: BuildableApplication[],
  storePath: string,
): BuildableApplication | null {
  let best: BuildableApplication | null = null;
  for (const candidate of candidates) {
    const root = candidate.sourcePath;
    const inside = root === "" || storePath.startsWith(`${root}/`);
    if (!inside) continue;
    const rest = root === "" ? storePath : storePath.slice(root.length + 1);
    if (IGNORED.some((dir) => rest === dir || rest.startsWith(`${dir}/`))) continue;
    if (best === null || root.length > best.sourcePath.length) best = candidate;
  }
  return best;
}

/** The timer functions the scheduler uses — the real ones, or a test's. */
export interface Timers {
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

/**
 * When to build: `delayMs` after the last save of a burst, and, when a save
 * arrives while that application is building, once more after the build ends —
 * so the last save is always built, and never by two builds racing.
 */
export class SaveScheduler {
  private readonly timers = new Map<string, unknown>();
  private readonly running = new Set<string>();
  private readonly again = new Set<string>();

  constructor(
    private readonly build: (app: BuildableApplication) => Promise<void>,
    private readonly delayMs = 400,
    private readonly clock: Timers = {
      setTimeout: (fn, ms) => setTimeout(fn, ms),
      clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
    },
  ) {}

  /** A file of `app`'s was saved. */
  saved(app: BuildableApplication): void {
    const id = app.application.id;
    const pending = this.timers.get(id);
    if (pending !== undefined) this.clock.clearTimeout(pending);
    this.timers.set(
      id,
      this.clock.setTimeout(() => {
        this.timers.delete(id);
        void this.start(app);
      }, this.delayMs),
    );
  }

  private async start(app: BuildableApplication): Promise<void> {
    const id = app.application.id;
    if (this.running.has(id)) {
      this.again.add(id);
      return;
    }
    this.running.add(id);
    try {
      await this.build(app);
    } finally {
      this.running.delete(id);
      if (this.again.delete(id)) await this.start(app);
    }
  }
}
