/**
 * Find-in-files against the store's own search endpoint.
 *
 * What is worth testing is the part with judgement in it: which of the query's
 * globs the *server* can narrow by and which have to be applied to what it
 * returns, that the glob rule matches the server's own, and that a search which
 * hit its ceiling says so rather than presenting a partial answer as complete.
 * None of it needs a workbench, which is why `search.ts` has no VS Code in it.
 */

import { describe, expect, it, vi } from "vitest";

import type { ApiClient, SearchFilesResponse } from "./client";
import {
  DEFAULT_MAX_RESULTS,
  globMatches,
  groupByFile,
  searchRequest,
  searchStore,
  type StoreMatch,
  type TextQueryLike,
} from "./search";

/** A query as the Search view builds one. */
function query(overrides: Partial<TextQueryLike> = {}): TextQueryLike {
  return {
    contentPattern: { pattern: "todo" },
    ...overrides,
  };
}

/** One match, as the endpoint returns it. */
function match(path: string, line = 1, column = 1, length = 4): StoreMatch {
  return { path, line, column, length, text: `a ${path} line` };
}

/** An API client whose `searchFiles` is a stub, with the calls it received. */
function stubApi(response: Partial<SearchFilesResponse> = {}) {
  const searchFiles = vi.fn(async () => ({
    matches: [],
    files_searched: 0,
    truncated: false,
    ...response,
  }));
  return { api: { searchFiles } as unknown as ApiClient, searchFiles };
}

describe("the request a query becomes", () => {
  it("carries the pattern and the search box's three toggles", () => {
    const request = searchRequest(
      query({
        contentPattern: {
          pattern: "Todo",
          isRegExp: true,
          isCaseSensitive: true,
          isWordMatch: true,
        },
      }),
    );
    expect(request).toMatchObject({
      pattern: "Todo",
      regex: true,
      case_sensitive: true,
      whole_word: true,
      max_results: DEFAULT_MAX_RESULTS,
    });
  });

  it("lets the server narrow by a single include glob", () => {
    const request = searchRequest(query({ includePattern: { "*.ts": true } }));
    expect(request.glob).toBe("*.ts");
  });

  it("sends no glob when several were typed, so none of them is dropped", () => {
    // The endpoint narrows by one glob. Sending the first of two would silently
    // lose every `.tsx` match, which is a wrong answer rather than a slow one.
    const request = searchRequest(query({ includePattern: { "*.ts": true, "*.tsx": true } }));
    expect(request.glob).toBeNull();
  });

  it("ignores a glob that is switched off or is a sibling clause", () => {
    const request = searchRequest(
      query({
        includePattern: { "*.ts": true, "*.js": false, "*.map": { when: "$(basename).ts" } },
      }),
    );
    expect(request.glob).toBe("*.ts");
  });
});

describe("the glob rule", () => {
  it("matches a name anywhere in the tree when there is no slash", () => {
    expect(globMatches("*.ts", "src/deep/app.ts")).toBe(true);
    expect(globMatches("*.ts", "src/deep/app.tsx")).toBe(false);
  });

  it("matches the whole path when there is one, with ** spanning directories", () => {
    expect(globMatches("src/*.ts", "src/app.ts")).toBe(true);
    expect(globMatches("src/*.ts", "src/deep/app.ts")).toBe(false);
    expect(globMatches("src/**/*.ts", "src/deep/app.ts")).toBe(true);
    // `**` matching nothing at all is the case a monorepo path depends on.
    expect(globMatches("src/**/*.ts", "src/app.ts")).toBe(true);
    expect(globMatches("web/**", "src/app.ts")).toBe(false);
  });
});

describe("what comes back", () => {
  it("groups matches by file, in the order they arrived", () => {
    const files = groupByFile(
      [match("src/app.ts", 1), match("src/deep/list.tsx", 3), match("src/app.ts", 9)],
      query(),
    );
    expect(files.map((f) => f.path)).toEqual(["src/app.ts", "src/deep/list.tsx"]);
    expect(files[0].matches.map((m) => m.line)).toEqual([1, 9]);
  });

  it("applies the includes the server could not, and the excludes always", () => {
    const matches = [match("src/app.ts"), match("src/app.tsx"), match("src/app.css")];
    // Two includes: the server searched everything, so the filtering is here.
    const included = groupByFile(matches, query({ includePattern: { "*.ts": true, "*.tsx": true } }));
    expect(included.map((f) => f.path)).toEqual(["src/app.ts", "src/app.tsx"]);

    // "files to exclude" is the user's and the endpoint's one glob is spoken
    // for, so it is applied here even when there is a single include.
    const excluded = groupByFile(matches, query({ excludePattern: { "*.css": true } }));
    expect(excluded.map((f) => f.path)).toEqual(["src/app.ts", "src/app.tsx"]);
  });
});

describe("a whole search", () => {
  it("is one request, and reports a ceiling it hit", async () => {
    const { api, searchFiles } = stubApi({
      matches: [match("src/app.ts"), match("src/app.ts", 4)],
      truncated: true,
    });

    const found = await searchStore(api, "apps", query({ maxResults: 5 }));

    // One request for the whole tree — the point of the endpoint.
    expect(searchFiles).toHaveBeenCalledTimes(1);
    expect(searchFiles).toHaveBeenCalledWith(
      "apps",
      expect.objectContaining({ pattern: "todo", max_results: 5 }),
    );
    expect(found.files).toHaveLength(1);
    expect(found.files[0].matches).toHaveLength(2);
    // A view that dropped this would present a truncated answer as a complete
    // one, which is wrong exactly when it matters.
    expect(found.limitHit).toBe(true);
  });

  it("comes back empty rather than failing when nothing matched", async () => {
    const { api } = stubApi();
    const found = await searchStore(api, "apps", query());
    expect(found.files).toEqual([]);
    expect(found.limitHit).toBe(false);
  });
});
