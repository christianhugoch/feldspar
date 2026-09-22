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
 * One of VS Code's `IFolderQuery`s, as much of it as a store search uses.
 *
 * The workbench puts part of what the user asked for **here** rather than on the
 * query: the configured `search.exclude` and `files.exclude` (so this is where
 * `node_modules` is excluded), a `./src` typed into "files to include", and the
 * file list of "search only in open editors". Every glob in it is relative to
 * `folder`.
 */
export interface FolderQueryLike {
  /** The folder searched: `/<store>` for the workspace, or a directory in it. */
  readonly folder: { readonly path: string };
  readonly includePattern?: Readonly<Record<string, unknown>>;
  /** One entry per exclude root; a single-folder workspace has one. */
  readonly excludePattern?: readonly { readonly pattern: Readonly<Record<string, unknown>> }[];
}

/**
 * As much of VS Code's `ITextQuery` as a store search uses.
 *
 * Structural rather than imported: this module must not depend on the workbench,
 * and these fields are the whole of what the endpoint can honour.
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
  /**
   * The folders searched. Absent means the whole store; present and empty means
   * none of it — "only open editors" with no editor open.
   */
  readonly folderQueries?: readonly FolderQueryLike[];
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
 * The includes narrow the search **server-side** whenever the endpoint's one glob
 * can spell them: one as itself, several as a `{a,b}` union — which matters,
 * because VS Code turns a typed `*.ts` into two globs ("any `*.ts` at any
 * depth", and "anything inside a directory named that"), and a search that brought back everything and filtered it here
 * would spend the result ceiling on files nobody asked for. A glob that already
 * has braces, a comma or a `[…]` class cannot go into a union (the server has no
 * classes at all), so then the server searches everything and
 * [`selectedByGlobs`] applies them here — fewer bytes on the wire is not worth a
 * wrong answer.
 *
 * `store` places the folder queries: one folder below the store's root becomes
 * the endpoint's `dir`.
 */
export function searchRequest(query: TextQueryLike, store = ""): SearchFilesRequest {
  return {
    pattern: query.contentPattern.pattern,
    regex: query.contentPattern.isRegExp ?? false,
    case_sensitive: query.contentPattern.isCaseSensitive ?? false,
    whole_word: query.contentPattern.isWordMatch ?? false,
    glob: serverGlob(activeGlobs(query.includePattern)),
    dir: serverDir(query, store),
    max_results: query.maxResults ?? DEFAULT_MAX_RESULTS,
  };
}

/** The includes as the endpoint's one glob, or `null` when it cannot say them. */
function serverGlob(includes: readonly string[]): string | null {
  if (includes.length === 0) return null;
  if (includes.length === 1) return /[[\]]/.test(includes[0]) ? null : includes[0];
  if (includes.some((glob) => /[{},[\]]/.test(glob))) return null;
  return `{${includes.join(",")}}`;
}

/** The sub-tree to search, when the query names exactly one below the root. */
function serverDir(query: TextQueryLike, store: string): string | null {
  const folders = query.folderQueries ?? [];
  if (folders.length !== 1) return null;
  const dir = folderDir(store, folders[0].folder.path);
  return dir === null || dir === "" ? null : dir;
}

/**
 * A folder's store-relative directory: `""` for the store itself, `null` for a
 * folder outside it (which then selects nothing).
 */
export function folderDir(store: string, folderPath: string): string | null {
  const root = `/${store}`;
  const path = folderPath.replace(/\/+$/, "");
  if (path === root) return "";
  return path.startsWith(`${root}/`) ? path.slice(root.length + 1) : null;
}

/**
 * Whether `path` (store-relative) is in one of the query's folders and survives
 * that folder's include and exclude globs, which are relative to the folder.
 */
export function selectedByFolders(
  path: string,
  store: string,
  folders: readonly FolderQueryLike[] | undefined,
): boolean {
  if (folders === undefined) return true;
  return folders.some((folder) => {
    const dir = folderDir(store, folder.folder.path);
    if (dir === null) return false;
    if (dir !== "" && !path.startsWith(`${dir}/`)) return false;
    const relative = dir === "" ? path : path.slice(dir.length + 1);
    if (!selectedByGlobs(relative, activeGlobs(folder.includePattern))) return false;
    const excludes = (folder.excludePattern ?? []).flatMap((e) => activeGlobs(e.pattern));
    return !excludes.some((glob) => excludedBy(glob, relative));
  });
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

/**
 * Whether an exclude glob drops `path`: it names the file, or a directory the
 * file is in. That is what VS Code means by one: the default `search.exclude`
 * entry for `node_modules` hides everything under every `node_modules`, not a
 * file called that.
 */
export function excludedBy(glob: string, path: string): boolean {
  const segments = split(path);
  for (let depth = segments.length; depth > 0; depth -= 1) {
    if (globMatches(glob, segments.slice(0, depth).join("/"))) return true;
  }
  return false;
}

/**
 * Whether one glob selects `path`.
 *
 * `{a,b}` alternatives are expanded first, as the server does. `[…]` classes
 * are this side's alone: they are how VS Code escapes a literal `*` or `[` in a
 * file name it puts in a query (an open editor's path), and those globs are never
 * sent.
 */
export function globMatches(glob: string, path: string): boolean {
  const pattern = glob.trim();
  if (pattern === "") return true;
  return expandBraces(pattern).some((alternative) => {
    if (!alternative.includes("/")) {
      const name = path.split("/").pop() ?? path;
      return matchSegment(alternative, name);
    }
    return matchSegments(split(alternative), split(path));
  });
}

/** Every alternative a glob's `{a,b}` groups spell. A `{` with no `}` is literal. */
function expandBraces(glob: string): string[] {
  const open = glob.indexOf("{");
  if (open < 0) return [glob];
  const close = glob.indexOf("}", open);
  if (close < 0) return [glob];
  const head = glob.slice(0, open);
  const tail = glob.slice(close + 1);
  return glob
    .slice(open + 1, close)
    .split(",")
    .flatMap((choice) => expandBraces(`${head}${choice}${tail}`));
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

/**
 * Match one segment: `*` any run of characters, `?` exactly one, `[abc]` one of
 * those.
 */
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
  const close = head === "[" ? pattern.indexOf("]", 2) : -1;
  if (close > 0) {
    return (
      pattern.slice(1, close).includes(segment[0]) &&
      matchSegment(pattern.slice(close + 1), segment.slice(1))
    );
  }
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
 * which the includes have already claimed. So are the folder queries' globs —
 * `store` is what their folders are placed against.
 */
export function groupByFile(
  matches: readonly StoreMatch[],
  query: TextQueryLike,
  store = "",
): readonly FileMatches[] {
  const includes = activeGlobs(query.includePattern);
  const excludes = activeGlobs(query.excludePattern);
  const files = new Map<string, StoreMatch[]>();
  for (const match of matches) {
    if (!selectedByGlobs(match.path, includes)) continue;
    if (excludes.some((glob) => excludedBy(glob, match.path))) continue;
    if (!selectedByFolders(match.path, store, query.folderQueries)) continue;
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
 * `onFile` hears about every file with matches, before the promise resolves, and
 * it is **not optional in practice**: VS Code's Search view builds its tree from
 * the progress a provider reports, and only takes the completed result's
 * statistics. A provider that answered with `results` alone showed the matches
 * VS Code found itself — in the open editors — and none of the store's.
 *
 * `limitHit` is carried through rather than dropped: the Search view says "more
 * results are available" with it, and a view that claimed 2000 matches were all
 * of them would be wrong exactly when the answer mattered.
 */
export async function searchStore(
  api: ApiClient,
  store: string,
  query: TextQueryLike,
  onFile?: (file: FileMatches) => void,
): Promise<StoreSearchResult> {
  const found = await api.searchFiles(store, searchRequest(query, store));
  const files = groupByFile(found.matches, query, store);
  if (onFile !== undefined) files.forEach(onFile);
  return { files, limitHit: found.truncated };
}
