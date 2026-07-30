/**
 * What the IDE knows about the language server *before* it has one (design
 * §12.1, phase 4): where its socket is, and what to say when there is not going
 * to be one.
 *
 * Split from `languageClient.ts` for the same reason `storeFiles.ts` is split
 * from `fileSystemProvider.ts`: these are decisions, not plumbing, and a decision
 * that can be tested without booting a workbench should be.
 */

/** The part of `window.location` the socket URL is derived from. */
export interface Origin {
  readonly protocol: string;
  readonly host: string;
}

/**
 * The socket URL for a store's language server.
 *
 * Same origin as the page — which is what the IDE's `connect-src 'self'` allows,
 * and what carries the session cookie the route authenticates with — and `wss:`
 * wherever the page itself is served over TLS, because a plain-`ws:` socket from
 * an `https:` page is blocked as mixed content.
 *
 * The path is `sc-server`'s `LSP_ROUTE`; `ide_language_server.rs` asserts the two
 * spellings agree, because nothing else would notice them diverging.
 */
export function languageServerUrl(origin: Origin, store: string): string {
  const scheme = origin.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${origin.host}/ide/lsp/${encodeURIComponent(store)}`;
}

/**
 * What to tell the admin when the socket closes before the client ever started.
 *
 * The server's refusals are sentences written to be read — "`assets` has no local
 * path, so it can be edited but not type-checked" — and they arrive in the close
 * frame, so the reason is shown exactly as it was sent. A socket that closed with
 * *no* reason is a different event (a proxy, a restart, a network); it gets a
 * sentence of its own rather than an empty notification.
 */
export function noSemanticsMessage(store: string, reason: string): string {
  const given = reason.trim();
  return given === ""
    ? `No TypeScript semantics for ${store}: the language server is not available.`
    : given;
}
