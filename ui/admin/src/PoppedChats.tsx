// The popped-out chats, drawn over whatever the admin is showing.
//
// Rendered by the shell rather than by a route, because that is the whole
// point: the store (`chatWindows.ts`) outlives the router, so a conversation
// popped out of `/agents/x/chat` stays in the corner while the person goes to
// look at the table they were asking about.
//
// **One list, three modes, no remounting.** Every window — docked, minimized,
// or full screen — is rendered as a child of the same row, keyed the same way,
// and the mode is only ever a change of class. That is not tidiness: each
// window holds a live WebSocket and a transcript that exists nowhere else until
// the turn ends, so a window that changed parent on its way to full screen
// would drop the conversation to get there. Full screen is therefore a
// `position: fixed` window that is still, in the DOM, in the row it left.
//
// A minimized window keeps its body mounted and hidden (`admin.css`) for the
// same reason: minimizing a chat must not be a slower way of closing it.

import { useEffect } from "react";

import {
  closePoppedChat,
  fullChatWindow,
  setPoppedChatMode,
  useChatWindows,
} from "./chatWindows";
import { AgentChat } from "./screens/AgentChat";

export function PoppedChats() {
  const windows = useChatWindows();
  const full = fullChatWindow(windows);

  // Escape gives the screen back, as it does for every other modal thing. It
  // does not close the chat: Escape is for leaving a mode, and closing one of
  // these throws away a conversation.
  const fullKey = full?.key ?? null;
  useEffect(() => {
    if (fullKey === null) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setPoppedChatMode(fullKey, "docked");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [fullKey]);

  if (windows.length === 0) return null;

  return (
    <div className="chat-window-row">
      {full && (
        // Dimming what is behind a full-screen chat, and taking the click that
        // means "I am done with it". Inside the row, so it stacks against the
        // windows rather than against the page (see `admin.css`).
        <div
          className="chat-window-backdrop"
          onClick={() => setPoppedChatMode(full.key, "docked")}
        />
      )}
      {/* Newest first, against the row's `row-reverse`: the two cancel out to
          oldest-on-the-left, and what falls off a screen too narrow for all of
          them is the oldest rather than the one just popped out. Rendering the
          new window at the *front* also means React inserts it rather than
          moving the others — a moved node is a transcript scrolled back to the
          top. */}
      {[...windows].reverse().map((chat) => (
        <section
          key={chat.key}
          className={`chat-window chat-window-${chat.mode}`}
          role="dialog"
          aria-modal={chat.mode === "full"}
          aria-label={`Chat with ${chat.agent}`}
        >
          <AgentChat
            agent={chat.agent}
            initial={{ runId: chat.runId, entries: chat.entries, draft: chat.draft }}
            frame={{
              mode: chat.mode,
              onMinimize: () => setPoppedChatMode(chat.key, "minimized"),
              onRestore: () => setPoppedChatMode(chat.key, "docked"),
              onFullScreen: () => setPoppedChatMode(chat.key, "full"),
              onClose: () => closePoppedChat(chat.key),
            }}
          />
        </section>
      ))}
    </div>
  );
}
