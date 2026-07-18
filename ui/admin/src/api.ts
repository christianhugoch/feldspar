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
 * Upload a file's bytes to a store.
 *
 * **The one admin operation not in the generated client**, and deliberately so.
 * The endpoint model is JSON-only — a `TypeSchema` has no bytes shape — so a raw
 * binary body cannot be described by it, and the server serves this from a route
 * outside the typed `EndpointSet` (`POST /upload/{store}/{*path}`). Everything
 * else in this SPA goes through the generated client; this is the exception, so
 * it lives here beside the other hand-written browser concerns rather than being
 * scattered into a screen.
 *
 * `writeFile` remains the typed path for small text files (the editor uses it);
 * this is for arbitrary bytes at arbitrary size.
 */
export async function uploadFile(
  store: string,
  path: string,
  file: File,
): Promise<void> {
  // Each segment is encoded separately: the path is a `{*path}` capture, so its
  // slashes are structural and must survive, while any other special character
  // in a filename must not.
  const encodedPath = path
    .split("/")
    .map(encodeURIComponent)
    .join("/");
  const res = await browserFetch(
    `/upload/${encodeURIComponent(store)}/${encodedPath}`,
    { method: "POST", body: file },
  );
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`uploadFile failed: ${res.status}${text ? `: ${text}` : ""}`);
  }
}

/**
 * The HTTP status embedded in an error thrown by the generated client, if any.
 * The client throws `Error("<name> failed: <status>[: <server message>]")`, so
 * we recover the code to distinguish, e.g., a 401 (bad credentials) from a real
 * network failure. The status is matched wherever it sits, since a server
 * message may follow it.
 */
export function errorStatus(err: unknown): number | null {
  if (err instanceof Error) {
    const match = err.message.match(/failed: (\d+)/);
    if (match) return Number(match[1]);
  }
  return null;
}

/**
 * The server's own message from an error the generated client threw, if it
 * carried one (the `: <server message>` the client appends after the status) —
 * otherwise the raw error message. This is what surfaces, e.g., a failed build's
 * bundler diagnostics to the admin.
 */
export function errorMessage(err: unknown, fallback: string): string {
  if (err instanceof Error) {
    const match = err.message.match(/failed: \d+: ([\s\S]+)$/);
    if (match) return match[1];
  }
  return fallback;
}
