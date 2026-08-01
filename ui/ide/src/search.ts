/**
 * Find-in-files, done by the server (design §12.1, TODO Phase 5).
 *
 * Until now the search walked the tree through the filesystem provider: one HTTP
 * request per directory, and a project with a `node_modules` in it was a search
 * nobody waited for. The store now searches itself (`searchFiles`), so a search
 * is **one request**, and it is the same search the `search_files` agent trait
 * runs — what a person finds in this editor is what a model finds in the same
 * store.
 *
 * No VS Code in this module. What the workbench asks for is translated into the
 * endpoint's request here, and what comes back is grouped by file here; the
 * adapter that turns that into `ISearchComplete` is `searchProvider.ts`. That
 * split is what makes the interesting parts — which glob narrows the search
 * server-side, which have to be applied to the results, and how a match becomes
 * a range — testable without booting a workbench.
 */

import type { ApiClient, SearchFilesRequest } from "./client";

/** One matching line, as the endpoint reports it. */
export interface StoreMatch {
  /** Store-relative path of the file. */
  readonly path: string;
  /** 1-based. */
  readonly line: number;
  /** 1-based, in characters. */
  readonly column: number;
  /** The match's length, in characters. */
  readonly length: number;
  /** The whole line the match is on. */
  readonly text: string;
}

/** Every match in one file, in the order they were found. */
export interface FileMatches {
  readonly path: string;
  readonly matches: readonly StoreMatch[];
}

/** What a search came back with. */
export interface StoreSearchResult {
  readonly files: readonly FileMatches[];
  /** Whether a ceiling cut the search short — the Search view's "more" state. */
  readonly limitHit: boolean;
}

/**
 * As much of VS Code's `ITextQuery` as a store search uses.
 *
 * Structural rather than imported: this module must not depend on the workbench,
 * and these five fields are the whole of what the endpoint can honour.
 */
export interface TextQueryLike {
  readonly contentPattern: {
    readonly pattern: string;
    readonly isRegExp?: boolean;
    readonly isCaseSensitive?: boolean;
    readonly isWordMatch?: boolean;
  };
  /**
   * Globs from the "files to include" box, as VS Code's `{glob: true}` map.
   *
   * The values are `unknown` because VS Code's own are: a glob may map to a
   * *sibling clause* ("exclude `x.js` when `x.ts` is beside it"), which needs the
   * directory listing this search does not have. Only `true` counts — see
   * [`activeGlobs`].
   */
  readonly includePattern?: Readonly<Record<string, unknown>>;
  /** Globs from "files to exclude", plus the configured `search.exclude`. */
  readonly excludePattern?: Readonly<Record<string, unknown>>;
  readonly maxResults?: number;
}

/** The IDE's own ceiling, when the query names none. */
export const DEFAULT_MAX_RESULTS = 2000;

/**
 * The globs a `{glob: true}` map turns on.
 *
 * Only a literal `true`. A glob mapped to a **sibling clause** is a conditional
 * rule that needs the file's siblings to evaluate, and this search sees one path
 * at a time: honouring it approximately would either hide files the user can see
 * in the explorer or show files they asked to hide, and both are worse than
 * leaving a rule this search cannot evaluate unapplied.
 */
export function activeGlobs(patterns: Readonly<Record<string, unknown>> | undefined): string[] {
  return Object.entries(patterns ?? {})
    .filter(([, on]) => on === true)
    .map(([glob]) => glob.trim())
    .filter((glob) => glob !== "");
}

/**
 * The request one query becomes.
 *
 * The include glob is only sent when there is **exactly one**: the endpoint
 * narrows by a single glob, and sending the first of several would silently drop
 * the files the others named. With several, the server searches everything and
 * [`selectedByGlobs`] applies them here — fewer bytes on the wire is not worth a
 * wrong answer.
 */
export function searchRequest(query: TextQueryLike): SearchFilesRequest {
  const includes = activeGlobs(query.includePattern);
  return {
    pattern: query.contentPattern.pattern,
    regex: query.contentPattern.isRegExp ?? false,
    case_sensitive: query.contentPattern.isCaseSensitive ?? false,
    whole_word: query.contentPattern.isWordMatch ?? false,
    glob: includes.length === 1 ? includes[0] : null,
    dir: null,
    max_results: query.maxResults ?? DEFAULT_MAX_RESULTS,
  };
}

/**
 * Whether `path` is one of the files `globs` names. No globs means every file.
 *
 * The rule is the server's, spelled the same way (`sc-files`' `glob_matches`): a
 * pattern with no `/` matches the file's **name** anywhere in the tree, one with
 * a `/` matches the whole store-relative path, `**` spans directories and `*`
 * does not. Two rules that disagreed would make a search's answer depend on how
 * many patterns were typed.
 */
export function selectedByGlobs(path: string, globs: readonly string[]): boolean {
  if (globs.length === 0) return true;
  return globs.some((glob) => globMatches(glob, path));
}

/** Whether one glob selects `path`. */
export function globMatches(glob: string, path: string): boolean {
  const pattern = glob.trim();
  if (pattern === "") return true;
  if (!pattern.includes("/")) {
    const name = path.split("/").pop() ?? path;
    return matchSegment(pattern, name);
  }
  return matchSegments(split(pattern), split(path));
}

/** A path's non-empty segments. */
function split(path: string): string[] {
  return path.split("/").filter((segment) => segment !== "");
}

/** Match segment-by-segment, with `**` spanning any number of them. */
function matchSegments(pattern: string[], segments: string[]): boolean {
  if (pattern.length === 0) return segments.length === 0;
  const [first, ...rest] = pattern;
  if (first === "**") {
    for (let skip = 0; skip <= segments.length; skip += 1) {
      if (matchSegments(rest, segments.slice(skip))) return true;
    }
    return false;
  }
  if (segments.length === 0) return false;
  return matchSegment(first, segments[0]) && matchSegments(rest, segments.slice(1));
}

/** Match one segment: `*` any run of characters, `?` exactly one. */
function matchSegment(pattern: string, segment: string): boolean {
  if (pattern.length === 0) return segment.length === 0;
  const head = pattern[0];
  if (head === "*") {
    for (let skip = 0; skip <= segment.length; skip += 1) {
      if (matchSegment(pattern.slice(1), segment.slice(skip))) return true;
    }
    return false;
  }
  if (segment.length === 0) return false;
  if (head === "?" || head === segment[0]) {
    return matchSegment(pattern.slice(1), segment.slice(1));
  }
  return false;
}

/**
 * Group matches by file, applying the query's globs to what came back.
 *
 * Excludes are applied here in every case: "files to exclude" and the
 * configured `search.exclude` are the user's, and the endpoint takes one glob,
 * which the includes have already claimed.
 */
export function groupByFile(
  matches: readonly StoreMatch[],
  query: TextQueryLike,
): readonly FileMatches[] {
  const includes = activeGlobs(query.includePattern);
  const excludes = activeGlobs(query.excludePattern);
  const files = new Map<string, StoreMatch[]>();
  for (const match of matches) {
    if (!selectedByGlobs(match.path, includes)) continue;
    if (excludes.some((glob) => globMatches(glob, match.path))) continue;
    const existing = files.get(match.path);
    if (existing === undefined) {
      files.set(match.path, [match]);
    } else {
      existing.push(match);
    }
  }
  return [...files.entries()].map(([path, matches]) => ({ path, matches }));
}

/**
 * Run one query against `store`, in one request.
 *
 * `limitHit` is carried through rather than dropped: the Search view says "more
 * results are available" with it, and a view that claimed 2000 matches were all
 * of them would be wrong exactly when the answer mattered.
 */
export async function searchStore(
  api: ApiClient,
  store: string,
  query: TextQueryLike,
): Promise<StoreSearchResult> {
  const found = await api.searchFiles(store, searchRequest(query));
  return {
    files: groupByFile(found.matches, query),
    limitHit: found.truncated,
  };
}
