// The admin SPA's API access, built on the generated typed client (`client.ts`).
//
// `createClient` accepts a custom `fetch`, so we inject the two things a browser
// session needs that the generated client is deliberately agnostic about:
//
//   - `credentials: "same-origin"` so the `sc_session` cookie rides along, and
//   - the CSRF double-submit header (`x-csrf-token`) echoing the `sc_csrf`
//     cookie on mutating requests (see `sc-server`'s security module).
//
// Keeping this out of the generated client means the generator stays a pure
// contract emitter and the browser concerns live in one small, hand-written spot.

import { createClient, type ApiClient } from "./client";

/** Header the server expects the CSRF cookie echoed in (matches `CSRF_HEADER`). */
const CSRF_HEADER = "x-csrf-token";
/** Non-`HttpOnly` cookie the server hands the SPA (matches `CSRF_COOKIE`). */
const CSRF_COOKIE = "sc_csrf";

/** Read a cookie value by name from `document.cookie`, or `null` if absent. */
function readCookie(name: string): string | null {
  const prefix = `${name}=`;
  for (const part of document.cookie.split("; ")) {
    if (part.startsWith(prefix)) return part.slice(prefix.length);
  }
  return null;
}

/** A `fetch` that carries the session cookie and the CSRF header. */
const browserFetch: typeof fetch = (input, init = {}) => {
  const headers = new Headers(init.headers);
  const csrf = readCookie(CSRF_COOKIE);
  if (csrf) headers.set(CSRF_HEADER, csrf);
  return fetch(input, { ...init, headers, credentials: "same-origin" });
};

/** The shared, browser-ready API client for the whole admin SPA. */
export const api: ApiClient = createClient({ fetch: browserFetch });

/**
 * The HTTP status embedded in an error thrown by the generated client, if any.
 * The client throws `Error("<name> failed: <status>")`, so we recover the code
 * to distinguish, e.g., a 401 (bad credentials) from a real network failure.
 */
export function errorStatus(err: unknown): number | null {
  if (err instanceof Error) {
    const match = err.message.match(/failed: (\d+)$/);
    if (match) return Number(match[1]);
  }
  return null;
}
