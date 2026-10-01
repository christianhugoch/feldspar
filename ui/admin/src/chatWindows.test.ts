/**
 * The popped-out chats (`chatWindows.ts`): the rules a corner full of windows
 * has to keep, none of which need a browser to state.
 *
 * The interesting ones are the two that are about *losing a conversation*: a
 * run must not be opened twice (two live sockets writing one run), and the
 * store must never rebuild the list in a way that would make React remount a
 * window — which is why every transition here returns windows with the same
 * keys in the same order.
 */

import { beforeEach, describe, expect, it } from "vitest";

import type { Entry } from "./agentChat";
import {
  MAX_CHAT_WINDOWS,
  canPopOutChat,
  chatWindows,
  closeChatWindow,
  closePoppedChat,
  fullChatWindow,
  openChatWindow,
  popOutChat,
  resetChatWindows,
  setChatWindowMode,
  setPoppedChatMode,
  subscribeChatWindows,
  type ChatWindow,
} from "./chatWindows";

const said: Entry[] = [{ kind: "user", text: "hello" }];

/** A window, as `openChatWindow` would have made it. */
function window(key: string, runId: string | null, mode: ChatWindow["mode"] = "docked"): ChatWindow {
  return { key, agent: "librarian", runId, entries: [], draft: "", mode };
}

describe("opening a window", () => {
  it("pops a chat out with its transcript, docked", () => {
    const windows = openChatWindow([], {
      key: "w1",
      agent: "librarian",
      runId: "run-1",
      entries: said,
    });
    expect(windows).toHaveLength(1);
    expect(windows[0]).toMatchObject({ key: "w1", agent: "librarian", runId: "run-1" });
    expect(windows[0].mode).toBe("docked");
    expect(windows[0].entries).toEqual(said);
  });

  it("keeps the order it was opened in", () => {
    let windows = openChatWindow([], { key: "w1", agent: "a", runId: null, entries: [] });
    windows = openChatWindow(windows, { key: "w2", agent: "b", runId: null, entries: [] });
    expect(windows.map((w) => w.key)).toEqual(["w1", "w2"]);
  });

  it("raises the window a run is already open in rather than opening a second", () => {
    const windows = openChatWindow([window("w1", "run-1", "minimized")], {
      key: "w2",
      agent: "librarian",
      runId: "run-1",
      entries: said,
    });
    expect(windows).toHaveLength(1);
    expect(windows[0].key).toBe("w1");
    expect(windows[0].mode).toBe("docked");
  });

  it("opens a second window for a chat that has no run yet", () => {
    // Two fresh conversations with the same agent are two conversations; only
    // a *run* is the thing that cannot be in two windows.
    const windows = openChatWindow([window("w1", null)], {
      key: "w2",
      agent: "librarian",
      runId: null,
      entries: [],
    });
    expect(windows.map((w) => w.key)).toEqual(["w1", "w2"]);
  });

  it("refuses past the ceiling", () => {
    let windows: ChatWindow[] = [];
    for (let i = 0; i < MAX_CHAT_WINDOWS; i += 1) {
      windows = openChatWindow(windows, { key: `w${i}`, agent: "a", runId: null, entries: [] });
    }
    const full = openChatWindow(windows, { key: "one-more", agent: "a", runId: null, entries: [] });
    expect(full).toBe(windows);
  });
});

describe("modes", () => {
  it("minimizes and restores one window, leaving the others alone", () => {
    const windows = [window("w1", null), window("w2", null)];
    const minimized = setChatWindowMode(windows, "w1", "minimized");
    expect(minimized.map((w) => w.mode)).toEqual(["minimized", "docked"]);
    expect(setChatWindowMode(minimized, "w1", "docked").map((w) => w.mode)).toEqual([
      "docked",
      "docked",
    ]);
  });

  it("gives the screen to one window at a time", () => {
    const windows = setChatWindowMode([window("w1", null), window("w2", null)], "w1", "full");
    const next = setChatWindowMode(windows, "w2", "full");
    expect(next.map((w) => w.mode)).toEqual(["docked", "full"]);
    expect(fullChatWindow(next)?.key).toBe("w2");
  });

  it("keeps every window's identity and place, so nothing remounts", () => {
    const windows = [window("w1", null), window("w2", null), window("w3", null)];
    const next = setChatWindowMode(windows, "w2", "full");
    expect(next.map((w) => w.key)).toEqual(["w1", "w2", "w3"]);
  });

  it("has no full-screen window until one is asked for", () => {
    expect(fullChatWindow([window("w1", null), window("w2", null, "minimized")])).toBeNull();
  });
});

describe("closing", () => {
  it("removes just that window", () => {
    const windows = [window("w1", null), window("w2", null)];
    expect(closeChatWindow(windows, "w1").map((w) => w.key)).toEqual(["w2"]);
  });
});

describe("the store the shell renders from", () => {
  beforeEach(() => resetChatWindows());

  it("tells its subscribers, and stops when they leave", () => {
    let changes = 0;
    const unsubscribe = subscribeChatWindows(() => {
      changes += 1;
    });

    popOutChat({ agent: "librarian", runId: null, entries: said });
    expect(changes).toBe(1);
    expect(chatWindows()).toHaveLength(1);

    const key = chatWindows()[0].key;
    setPoppedChatMode(key, "full");
    expect(changes).toBe(2);
    expect(fullChatWindow(chatWindows())?.key).toBe(key);

    unsubscribe();
    closePoppedChat(key);
    expect(changes).toBe(2);
    expect(chatWindows()).toHaveLength(0);
  });

  it("carries the unsent message in the entry box into the window", () => {
    popOutChat({ agent: "librarian", runId: null, entries: said, draft: "half a questi" });
    expect(chatWindows()[0].draft).toBe("half a questi");
    popOutChat({ agent: "librarian", runId: null, entries: [] });
    expect(chatWindows()[1].draft).toBe("");
  });

  it("gives each window a key of its own", () => {
    popOutChat({ agent: "librarian", runId: null, entries: [] });
    popOutChat({ agent: "librarian", runId: null, entries: [] });
    const keys = chatWindows().map((w) => w.key);
    expect(new Set(keys).size).toBe(2);
  });

  it("reports when the corner is full, which is what disables the button", () => {
    for (let i = 0; i < MAX_CHAT_WINDOWS; i += 1) {
      expect(canPopOutChat()).toBe(true);
      popOutChat({ agent: "librarian", runId: null, entries: [] });
    }
    expect(canPopOutChat()).toBe(false);

    // A refused pop-out must not even notify: `useSyncExternalStore` re-reads
    // the same array, and a store that published anyway would re-render the
    // shell on every click of a dead button.
    let changes = 0;
    const unsubscribe = subscribeChatWindows(() => {
      changes += 1;
    });
    popOutChat({ agent: "librarian", runId: null, entries: [] });
    expect(changes).toBe(0);
    expect(chatWindows()).toHaveLength(MAX_CHAT_WINDOWS);
    unsubscribe();
  });
});
