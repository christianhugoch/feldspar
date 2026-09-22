/**
 * The store's search, as VS Code's search service sees it (design §12.1).
 *
 * The adapter half of `search.ts`, and deliberately nothing more: a query in, an
 * `ISearchComplete` out. It replaces the search-service override's own provider
 * for the `file` scheme — that one walks the tree through the filesystem
 * provider, one request per directory, and this one asks the store (one request,
 * §11.3's `searchFiles`).
 *
 * Registering **replaces** rather than adds: `SearchService` keeps one provider
 * per scheme per kind, so registering ours for `text` is what makes find-in-files
 * use it. File search — the "go to file" quick-open list — is left with the
 * built-in provider, which walks the same filesystem the explorer does and has
 * no endpoint of its own to switch to.
 */

import { ISearchService } from "@codingame/monaco-vscode-api/vscode/vs/workbench/services/search/common/search.service";
import {
  FileMatch,
  OneLineRange,
  SearchProviderType,
  TextSearchMatch,
  type IFileQuery,
  type ISearchComplete,
  type ISearchProgressItem,
  type ISearchResultProvider,
  type ITextQuery,
} from "@codingame/monaco-vscode-api/vscode/vs/workbench/services/search/common/search";
import { Schemas } from "@codingame/monaco-vscode-api/vscode/vs/base/common/network";
import { URI } from "@codingame/monaco-vscode-api/vscode/vs/base/common/uri";
import { getService } from "@codingame/monaco-vscode-api";
import type { IDisposable } from "@codingame/monaco-vscode-api/vscode/vs/base/common/lifecycle";
import type { CancellationToken } from "@codingame/monaco-vscode-api/vscode/vs/base/common/cancellation";

import type { ApiClient } from "./client";
import { searchStore, type FileMatches } from "./search";
import { toUriPath } from "./storeFiles";

/** Nothing found, and nothing to say about it. */
const NOTHING: ISearchComplete = { results: [], messages: [], limitHit: false };

/**
 * Find-in-files over one store, through the search endpoint.
 *
 * `fileSearch` and `clearCache` are the interface's other members: there is no
 * store-side file listing endpoint to answer the first with (quick-open keeps
 * the built-in provider, which this registration does not replace), and there is
 * no cache to clear.
 */
export class StoreSearchProvider implements ISearchResultProvider {
  constructor(
    private readonly store: string,
    private readonly api: ApiClient,
  ) {}

  /**
   * Every file with matches is reported through `onProgress` as well as in the
   * result, and the first is the one that shows: the Search view draws its tree
   * from progress and keeps only the statistics of the result. Without it the
   * view showed the matches VS Code finds itself, in the open editors, and
   * nothing from the rest of the store (`search.ts` says more).
   */
  async textSearch(
    query: ITextQuery,
    onProgress?: (item: ISearchProgressItem) => void,
    token?: CancellationToken,
  ): Promise<ISearchComplete> {
    const results: FileMatch[] = [];
    const found = await searchStore(this.api, this.store, query, (file) => {
      const match = this.fileMatch(file);
      results.push(match);
      // A search the user has replaced is not reported into the new one's view.
      if (token?.isCancellationRequested !== true) onProgress?.(match);
    });
    return { results, messages: [], limitHit: found.limitHit };
  }

  /** One file's matches, as the Search view's tree renders them. */
  private fileMatch(file: FileMatches): FileMatch {
    const match = new FileMatch(URI.file(toUriPath(this.store, file.path)));
    match.results = file.matches.map(
      (hit) =>
        new TextSearchMatch(
          hit.text,
          // VS Code's columns are 0-based where the endpoint's are 1-based, and
          // the end is exclusive — so a match of length `n` at column `c` spans
          // `[c - 1, c - 1 + n)`.
          new OneLineRange(hit.line - 1, hit.column - 1, hit.column - 1 + hit.length),
        ),
    );
    return match;
  }

  async fileSearch(_query: IFileQuery): Promise<ISearchComplete> {
    return NOTHING;
  }

  async getAIName(): Promise<string | undefined> {
    return undefined;
  }

  async clearCache(_cacheKey: string): Promise<void> {}
}

/**
 * Point find-in-files at the store, replacing the provider that walked the tree.
 *
 * Called after `initialize`, because the search service must exist to register
 * with. A failure is reported and swallowed: an editor whose search box is slow
 * is worth having, and one that refused to open because the search could not be
 * re-pointed is not.
 */
export async function registerStoreSearch(
  store: string,
  api: ApiClient,
): Promise<IDisposable | null> {
  try {
    const search = await getService(ISearchService);
    return search.registerSearchResultProvider(
      Schemas.file,
      SearchProviderType.text,
      new StoreSearchProvider(store, api),
    );
  } catch (err) {
    console.error("[saltcorn] find-in-files is falling back to the client-side walk", err);
    return null;
  }
}
