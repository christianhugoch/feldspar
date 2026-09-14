// @vitest-environment jsdom
//
// The built bundle, in jsdom (TODO "The builder" 7.7).
//
// - **The mount.** `dist/builder.js`, started with each options object a real
//   Saltcorn 1 recorded (`crates/sc-server/tests/fixtures/builder-options/`),
//   renders the builder without an error. It reaches this server only through
//   the typed client, and never loads a script from another origin.
// - **The Craft round trip** (§6). Every view and page layout in the BooksDB
//   backup is loaded into the canvas and saved with v1's own *Next* button,
//   without a change. It comes back as it went in: `storage.js`'s
//   `layoutToNodes` and `craftToSaltcorn` agree, over this build of them.
//   `NORMALISATIONS` lists what `storage.js` itself changes on the way, each
//   with its reason.
//
// The bundle is what is tested, not the source, so run `node build.mjs` first
// (`npm test` does).

import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

import { beforeAll, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "./client";
import type { BuilderTarget } from "./context";
import type { BuilderMode, StartBuilder } from "./index";
import { loadV1PageScripts } from "./test/v1-page";
import { FIXTURES, UI } from "./test/vendor-source";
import { readZipEntry } from "./test/zip";

const DIST = path.join(UI, "dist", "builder.js");

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

interface Pack {
  views: Array<{ name: string; viewtemplate: string; configuration: { layout?: Json; columns?: Json } }>;
  pages: Array<{ name: string; layout: Json }>;
}

const pack = JSON.parse(readZipEntry(path.join(FIXTURES, "saltcorn-v1-BooksDB.zip"), "pack.json").toString("utf8")) as Pack;

const MODE_OF_PATTERN: Record<string, BuilderMode> = { Show: "show", Edit: "edit", List: "list", Filter: "filter" };

/** The recorded options for each mode, all over Books. A layout over Authors is
 * loaded with its mode's Books options: the options feed the toolbox and the
 * settings panels, and the round trip is over the layout. */
const OPTIONS_OF_MODE: Record<BuilderMode, string> = {
  show: "show-books.json",
  edit: "edit-books.json",
  list: "list-books.json",
  filter: "filter-books.json",
  page: "page-booksoverview.json",
};

interface Case {
  label: string;
  mode: BuilderMode;
  target: BuilderTarget;
  layout: Json;
  columns?: Json;
}

const CASES: Case[] = [
  ...pack.views
    .filter((v) => MODE_OF_PATTERN[v.viewtemplate] && v.configuration.layout)
    .map((v) => ({
      label: `the view ${v.name}`,
      mode: MODE_OF_PATTERN[v.viewtemplate],
      target: { kind: "view" as const, name: v.name, step: 0 },
      layout: v.configuration.layout!,
      columns: v.configuration.columns,
    })),
  ...pack.pages.map((p) => ({
    label: `the page ${p.name}`,
    mode: "page" as const,
    target: { kind: "page" as const, name: p.name },
    layout: p.layout,
  })),
];

/** `above` lists as the builder writes them: nulls dropped, and a wrapper
 * holding one segment replaced by the segment. */
function unwrapAbove(value: Json): Json {
  if (Array.isArray(value)) return value.map(unwrapAbove);
  if (value === null || typeof value !== "object") return value;
  const object: Record<string, Json> = Object.fromEntries(
    Object.entries(value).map(([key, child]) => [key, unwrapAbove(child)]),
  );
  if (Array.isArray(object.above)) {
    const above = object.above.filter((segment) => segment !== null);
    if (Object.keys(object).length === 1 && above.length === 1) return above[0];
    object.above = above;
  }
  return object;
}

/** What `storage.js` changes about a layout it did not otherwise change: name →
 * a function applied to both sides before they are compared. */
const NORMALISATIONS: Record<string, (value: Json) => Json> = {
  // The BooksDB layouts were written by an older v1 builder, which put a `null`
  // at the head of the view's `above` and wrapped each cell of a row as
  // `{ above: [null, segment] }`. `layoutToNodes` makes no node for a null, and
  // `craftToSaltcorn`'s `removeEmpty` writes an `above` of one as that one.
  "above: nulls dropped, a one-segment wrapper unwrapped": unwrapAbove,
};

/** Props `storage.js` **adds** to a segment that did not have them: name → the
 * value it adds. `layoutToNodes` gives the Craft node each missing prop a default
 * (`customClass={segment.customClass || ""}`), and `craftToSaltcorn` writes every
 * prop back out. Only an addition at exactly this value is forgiven: a prop the
 * layout had must come back with the value it had. */
const ADDED_AT_DEFAULT: Record<string, Json> = {
  // Every text, field, join field, container and action segment.
  customClass: "",
  font: "",
  inline: false,
  isFormula: {},
  labelFor: "",
  style: {},
  configuration: {},
  // A row (`besides`): the column the settings panel had open.
  setting_col_n: 0,
  // An action segment's button and multi-step settings.
  action_bgcol: "",
  action_bordercol: "",
  action_class: "",
  action_icon: "",
  action_label: "",
  action_size: "",
  action_style: "btn-primary",
  action_textcol: "",
  action_title: "",
  nsteps: "",
  run_async: false,
  step_action_names: "",
  step_only_ifs: "",
};

/** `after` without what `storage.js` added to `before` and nothing else
 * (`ADDED_AT_DEFAULT`, and two additions whose value is not a constant). */
function withoutAdditions(before: Json, after: Json): Json {
  if (Array.isArray(before) && Array.isArray(after)) {
    return after.map((segment, i) => (i < before.length ? withoutAdditions(before[i], segment) : segment));
  }
  const isObject = (v: Json): v is { [key: string]: Json } => v !== null && typeof v === "object" && !Array.isArray(v);
  if (!isObject(before) || !isObject(after)) return after;
  const out: { [key: string]: Json } = {};
  for (const [key, value] of Object.entries(after)) {
    if (!(key in before)) {
      if (key in ADDED_AT_DEFAULT && canonical(value) === canonical(ADDED_AT_DEFAULT[key])) continue;
      // `default_breakpoints`: one empty breakpoint per column of the row.
      if (key === "breakpoints" && Array.isArray(value) && value.every((b) => b === "")) continue;
      // An action with no `rndid` loads as "not_assigned" and saves with a newly
      // minted one, which is what `POST /page/:name/action/:rndid` then names.
      if (key === "rndid" && after.type === "action" && typeof value === "string" && /^[0-9a-f]+$/.test(value)) {
        continue;
      }
    }
    out[key] = key in before ? withoutAdditions(before[key], value) : value;
  }
  return out;
}

/** A JSON value with its object keys sorted, as a string: key order is not
 * part of a layout. */
function canonical(value: Json): string {
  const sort = (v: Json): Json =>
    Array.isArray(v)
      ? v.map(sort)
      : v && typeof v === "object"
        ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sort(v[k])]))
        : v;
  return JSON.stringify(sort(value));
}

/** Where two JSON values differ, one line per difference, so a failed round
 * trip names what `storage.js` changed rather than printing two layouts. */
function differences(before: Json, after: Json, at = "layout"): string[] {
  const short = (v: Json | undefined) => {
    const s = JSON.stringify(v);
    return s === undefined ? "undefined" : s.length > 80 ? `${s.slice(0, 77)}...` : s;
  };
  if (Array.isArray(before) && Array.isArray(after)) {
    return Array.from({ length: Math.max(before.length, after.length) }, (_, i) =>
      i >= after.length
        ? [`${at}[${i}]: removed ${short(before[i])}`]
        : i >= before.length
          ? [`${at}[${i}]: added ${short(after[i])}`]
          : differences(before[i], after[i], `${at}[${i}]`),
    ).flat();
  }
  const isObject = (v: Json) => v !== null && typeof v === "object" && !Array.isArray(v);
  if (isObject(before) && isObject(after)) {
    const b = before as Record<string, Json>;
    const a = after as Record<string, Json>;
    return [...new Set([...Object.keys(b), ...Object.keys(a)])].sort().flatMap((key) =>
      !(key in a)
        ? [`${at}.${key}: removed ${short(b[key])}`]
        : !(key in b)
          ? [`${at}.${key}: added ${short(a[key])}`]
          : differences(b[key], a[key], `${at}.${key}`),
    );
  }
  return JSON.stringify(before) === JSON.stringify(after) ? [] : [`${at}: ${short(before)} became ${short(after)}`];
}

function normalise(value: Json): Json {
  return Object.values(NORMALISATIONS).reduce((v, f) => f(v), structuredClone(value));
}

type Call = [string, unknown[]];

/** The admin API as the canvas uses it while it renders: previews and lookups,
 * answered with something plausible. Anything else fails the test. */
function canvasClient(calls: Call[]): ApiClient {
  const answers: Record<string, unknown> = {
    builderFieldPreview: { html: "<span>preview</span>" },
    builderViewPreview: { html: "<div>view preview</div>" },
    builderPagePreview: { html: "<div>page preview</div>" },
    builderFieldviewConfigForm: [],
    builderDistinctValues: { success: [] },
  };
  return new Proxy({} as ApiClient, {
    get: (_target, op: string) =>
      async (...args: unknown[]) => {
        calls.push([op, args]);
        if (op in answers) return answers[op];
        throw new Error(`the canvas called ${op} while rendering`);
      },
  });
}

async function waitFor<T>(what: string, probe: () => T | null | undefined, ms = 10_000): Promise<T> {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = probe();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

let startBuilder: (start: StartBuilder, client: ApiClient) => void;

beforeAll(async () => {
  if (!fs.existsSync(DIST)) throw new Error(`${DIST} is not built: run \`node build.mjs\` first`);
  loadV1PageScripts();
  ({ startBuilder } = await import(/* @vite-ignore */ pathToFileURL(DIST).href));
});

/** Mount the builder for `c` and answer what *Next* submits, with every error
 * logged along the way. */
async function mountAndSave(c: Case) {
  const errors: string[] = [];
  const logged = vi.spyOn(console, "error").mockImplementation((...args: unknown[]) => {
    errors.push(args.map(String).join(" "));
  });
  const submitted: HTMLFormElement[] = [];
  const submit = vi
    .spyOn(HTMLFormElement.prototype, "submit")
    .mockImplementation(function (this: HTMLFormElement) {
      submitted.push(this);
    });
  const calls: Call[] = [];
  try {
    document.body.innerHTML = `
      <div id="builder-header-actions"></div>
      <div id="saltcorn-builder"></div>
      <form id="scbuildform"><input type="hidden" name="columns"><input type="hidden" name="layout"></form>`;
    const options = JSON.parse(
      fs.readFileSync(path.join(FIXTURES, "builder-options", OPTIONS_OF_MODE[c.mode]), "utf8"),
    ) as Record<string, Json>;
    startBuilder(
      {
        containerId: "saltcorn-builder",
        application: "app-1",
        applicationOrigin: "http://booksdb.localhost:3032",
        target: c.target,
        csrfToken: "token-1",
        options,
        layout: c.layout,
        mode: c.mode,
      },
      canvasClient(calls),
    );
    const next = await waitFor("the Next button", () =>
      document.querySelector<HTMLButtonElement>("#builder-header-actions .builder-save"),
    );
    // The canvas is built from the layout in an effect after the first render.
    await waitFor("the canvas", () => document.querySelector("#builder-main-canvas")?.textContent?.trim());
    await new Promise((resolve) => setTimeout(resolve, 200));

    next.click();
    const form = await waitFor("the submit", () => submitted[0]);
    const read = (name: string) => {
      const value = form.querySelector(`input[name=${name}]`)?.getAttribute("value");
      return value ? (JSON.parse(decodeURIComponent(value)) as Json) : null;
    };
    const foreignScripts = [...document.querySelectorAll<HTMLScriptElement>("script[src]")]
      .map((s) => s.src)
      .filter((src) => new URL(src, window.location.href).origin !== window.location.origin);
    return { layout: read("layout"), columns: read("columns"), errors, calls, foreignScripts };
  } finally {
    logged.mockRestore();
    submit.mockRestore();
  }
}

describe("the builder bundle in jsdom", () => {
  it("has a case for every BooksDB view with a layout and every page", () => {
    expect(CASES.map((c) => c.label).sort()).toEqual([
      "the page BooksOverview",
      "the view Edit Authors",
      "the view Edit Books",
      "the view Filter books",
      "the view List Books",
      "the view Show Authors",
      "the view Show Books",
    ]);
  });

  for (const c of CASES) {
    it(`mounts ${c.label} and saves its layout unchanged`, { timeout: 30_000 }, async () => {
      const saved = await mountAndSave(c);

      expect(saved.errors).toEqual([]);
      expect(saved.foreignScripts).toEqual([]);
      const unexpected = saved.calls.map(([op]) => op).filter((op) => !op.startsWith("builder"));
      expect(unexpected).toEqual([]);

      const before = normalise(c.layout);
      const after = withoutAdditions(before, normalise(saved.layout));
      expect(differences(before, after)).toEqual([]);
      expect(canonical(after)).toBe(canonical(before));
      if (c.mode === "list") {
        const columnsBefore = normalise(c.columns ?? null);
        const columnsAfter = withoutAdditions(columnsBefore, normalise(saved.columns));
        expect(differences(columnsBefore, columnsAfter, "columns")).toEqual([]);
        expect(canonical(columnsAfter)).toBe(canonical(columnsBefore));
      }
    });
  }
});
