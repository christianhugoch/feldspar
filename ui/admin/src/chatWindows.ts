// Popped-out chats: which conversations are floating over the admin, and what
// state each of their windows is in.
//
// A chat is the one screen someone wants to keep *while* doing something else —
// asking an agent about a table and then going to look at that table is the
// normal shape of the work, and a chat that lives only at `/agents/x/chat`
// makes that a choice between the two. So a chat can be popped out of the page
// into an overlay, exactly as Gmail pops a reply out of the thread: it leaves
// the page, it settles in the bottom-right corner, and the rest of the
// application navigates underneath it.
//
// The windows therefore cannot live in the routed screen — that is the thing
// being navigated away from. They live here, in a store the shell subscribes
// to, so a popped-out chat outlives every route change (and nothing else: a
// reload is a new session, and a socket does not survive one anyway).
//
// Deliberately a store of plain data with pure transitions, and not React state
// threaded through a context: the interesting half — one full-screen window at
// a time, a run that is already open being raised rather than duplicated, the
// ceiling on how many can be open — is then testable without a browser, which
// is the same split `agentChat.ts` makes against the socket.

import { useSyncExternalStore } from "react";

import type { Entry } from "./agentChat";

/** How a popped-out chat is showing.
 *
 * The three Gmail has, for the three things someone is doing with it: reading
 * it beside the page (`docked`), keeping it for later without its transcript in
 * the way (`minimized`), and giving it the screen because the answer got long
 * (`full`). */
export type ChatWindowMode = "docked" | "minimized" | "full";

/** One popped-out chat. */
export type ChatWindow = {
  /** This window's identity, and nothing else's — not the run's.
   *
   * A window is popped out before it has a run (a fresh chat has no id until
   * its first turn finishes), so the key has to exist without one. It is also
   * what React keys the list on, which is what keeps a mode change a change of
   * class rather than a remount — and a remount would drop the socket. */
  key: string;
  /** The agent being talked to, by name (what the socket's `start` carries). */
  agent: string;
  /** The run to carry on, if the chat had one when it was popped out. */
  runId: string | null;
  /** The transcript so far, so the window opens showing what was already said. */
  entries: Entry[];
  mode: ChatWindowMode;
};

/** How many chats may be popped out at once.
 *
 * Not a technical limit: four docked windows are wider than most screens, and
 * the row would either wrap over the page or scroll sideways — both of which
 * are worse than being told the corner is full. */
export const MAX_CHAT_WINDOWS = 3;

/** What a chat hands over when it is popped out of the page. */
export type ChatWindowSeed = {
  key: string;
  agent: string;
  runId: string | null;
  entries: Entry[];
};

/**
 * Pop a chat out, or raise the window that is already showing that run.
 *
 * Reopening a conversation that is already popped out would give two live
 * sockets writing to one run — two halves of a conversation in two windows —
 * so an open run is raised (un-minimized) instead. A chat with no run yet has
 * nothing to collide with: it is a fresh conversation every time.
 *
 * At the ceiling the list is returned unchanged. The button that calls this is
 * disabled there and says why; this is the guard behind it, not the message.
 */
export function openChatWindow(windows: ChatWindow[], seed: ChatWindowSeed): ChatWindow[] {
  if (seed.runId !== null) {
    const already = windows.some((window) => window.runId === seed.runId);
    if (already) {
      return windows.map((window) =>
        window.runId === seed.runId && window.mode === "minimized"
          ? { ...window, mode: "docked" }
          : window,
      );
    }
  }
  if (windows.length >= MAX_CHAT_WINDOWS) return windows;
  return [...windows, { ...seed, mode: "docked" }];
}

/**
 * Set one window's mode.
 *
 * Full screen is exclusive: it is a centred modal over a dimmed page, and two
 * of them would be one modal hiding another. Making the others fall back to
 * docked — rather than refusing — keeps the button meaning the same thing in
 * every window.
 */
export function setChatWindowMode(
  windows: ChatWindow[],
  key: string,
  mode: ChatWindowMode,
): ChatWindow[] {
  return windows.map((window) => {
    if (window.key === key) return { ...window, mode };
    if (mode === "full" && window.mode === "full") return { ...window, mode: "docked" };
    return window;
  });
}

/** Close one window. */
export function closeChatWindow(windows: ChatWindow[], key: string): ChatWindow[] {
  return windows.filter((window) => window.key !== key);
}

/** The window that has the screen, if one has it. */
export function fullChatWindow(windows: ChatWindow[]): ChatWindow | null {
  return windows.find((window) => window.mode === "full") ?? null;
}

/* ------------------------------------------------------------------ *
 * The store the shell renders from.
 * ------------------------------------------------------------------ */

let windows: ChatWindow[] = [];
let counter = 0;
const listeners = new Set<() => void>();

function publish(next: ChatWindow[]): void {
  if (next === windows) return;
  windows = next;
  for (const listener of listeners) listener();
}

/** Every popped-out chat, oldest first — the order they sit in along the bottom. */
export function chatWindows(): ChatWindow[] {
  return windows;
}

/** Watch the list. Returns the unsubscribe. */
export function subscribeChatWindows(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Whether there is still room in the corner. */
export function canPopOutChat(): boolean {
  return windows.length < MAX_CHAT_WINDOWS;
}

/** Pop a chat out of the page and into the corner. */
export function popOutChat(chat: { agent: string; runId: string | null; entries: Entry[] }): void {
  counter += 1;
  publish(openChatWindow(windows, { key: `chat-window-${counter}`, ...chat }));
}

/** Minimize, expand, or bring a window back to the corner. */
export function setPoppedChatMode(key: string, mode: ChatWindowMode): void {
  publish(setChatWindowMode(windows, key, mode));
}

/** Close a popped-out chat. */
export function closePoppedChat(key: string): void {
  publish(closeChatWindow(windows, key));
}

/** Subscribe a component to the popped-out chats. */
export function useChatWindows(): ChatWindow[] {
  return useSyncExternalStore(subscribeChatWindows, chatWindows, chatWindows);
}

/** Empty the corner. Tests only — the shell never closes them all at once. */
export function resetChatWindows(): void {
  counter = 0;
  publish([]);
}
