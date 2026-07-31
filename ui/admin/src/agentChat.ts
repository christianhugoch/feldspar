// The chat model: the socket protocol of §11.4, and the transcript it builds.
//
// Deliberately **not** a React component. The screen renders what is here, and
// what is here is a reducer plus a thin wrapper over one socket — so the part
// that has to be right (deltas appended in order, a tool call paired with its
// result, an error rendered rather than swallowed, an abort leaving the
// transcript intact) is testable without a browser or a server, which is what
// `agentChat.test.ts` does against a stubbed socket.
//
// The protocol constant below is the same string `sc-server` mounts the route
// on; a Rust test asserts the two spellings agree, because a page connecting to
// the wrong path looks exactly like an agent that never answers.

/** The route the server serves the chat socket on (matches `AGENT_CHAT_ROUTE`). */
export const AGENT_CHAT_ROUTE = "/admin/agent-chat";

/** Where the chat socket is, for a page served from `location`.
 *
 * `wss:` wherever the page itself is over TLS: a `ws:` socket opened from an
 * `https:` page is mixed content and the browser blocks it, which would present
 * as an agent that never answers on exactly the deployments that are configured
 * correctly. */
export function agentChatUrl(location: { protocol: string; host: string }): string {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}${AGENT_CHAT_ROUTE}`;
}

/** One event the server sends. The six of §11.4, and nothing else. */
export type ServerEvent =
  | { type: "text"; delta: string }
  | { type: "reasoning"; delta: string }
  | { type: "tool_call"; id: string; name: string; arguments: unknown }
  | { type: "tool_result"; id: string; name: string; content: string; is_error: boolean }
  | { type: "done"; run: string | null; state: string; answer: string }
  | { type: "error"; message: string };

/** One entry in the transcript as the panel renders it. */
export type Entry =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string; reasoning: string }
  | {
      kind: "tool";
      id: string;
      name: string;
      args: unknown;
      /** `null` until the result arrives — which is what renders as "running". */
      result: string | null;
      isError: boolean;
    }
  /** A failure, in the transcript where it happened. A chat window that
   * silently stops is unfixable by the person watching it (§11.4). */
  | { kind: "error"; message: string };

/** The whole of what the panel draws. */
export type ChatState = {
  entries: Entry[];
  /** Whether a turn is in flight: what disables the composer and shows Stop. */
  running: boolean;
  /** The run this conversation is, once the server has said. */
  runId: string | null;
  /** How the last turn ended, for the line under the transcript. */
  lastState: string | null;
};

/** A conversation with nothing in it yet. */
export function emptyChat(): ChatState {
  return { entries: [], running: false, runId: null, lastState: null };
}

/** Add what the person just said, and mark the turn as running.
 *
 * Separate from `applyEvent` because it is the one entry the *client* knows
 * about before the server does: the server echoes nothing back, so a transcript
 * that only grew from events would show an answer to a question nobody asked. */
export function applyUserMessage(state: ChatState, text: string): ChatState {
  return {
    ...state,
    entries: [...state.entries, { kind: "user", text }],
    running: true,
    lastState: null,
  };
}

/** Fold one server event into the transcript. */
export function applyEvent(state: ChatState, event: ServerEvent): ChatState {
  switch (event.type) {
    case "text":
      return { ...state, entries: appendText(state.entries, event.delta, "text") };
    case "reasoning":
      return { ...state, entries: appendText(state.entries, event.delta, "reasoning") };
    case "tool_call":
      return {
        ...state,
        entries: [
          ...state.entries,
          {
            kind: "tool",
            id: event.id,
            name: event.name,
            args: event.arguments,
            result: null,
            isError: false,
          },
        ],
      };
    case "tool_result":
      return {
        ...state,
        entries: state.entries.map((entry) =>
          entry.kind === "tool" && entry.id === event.id && entry.result === null
            ? { ...entry, result: event.content, isError: event.is_error }
            : entry,
        ),
      };
    case "error":
      // Appended, never replacing: a failure after two paragraphs and a tool
      // call is read alongside them, not instead of them.
      return { ...state, entries: [...state.entries, { kind: "error", message: event.message }] };
    case "done":
      return {
        ...state,
        running: false,
        runId: event.run ?? state.runId,
        lastState: event.state,
      };
  }
}

/** Append a delta to the assistant entry being written, starting one if the
 * last thing in the transcript is not one.
 *
 * Starting a new entry after a tool is what makes a turn read as it happened:
 * "let me look", the tool, then the answer — rather than one paragraph with a
 * tool call spliced into the middle of it. */
function appendText(entries: Entry[], delta: string, into: "text" | "reasoning"): Entry[] {
  const last = entries[entries.length - 1];
  if (last?.kind === "assistant") {
    const updated: Entry = { ...last, [into]: last[into] + delta };
    return [...entries.slice(0, -1), updated];
  }
  return [
    ...entries,
    {
      kind: "assistant",
      text: into === "text" ? delta : "",
      reasoning: into === "reasoning" ? delta : "",
    },
  ];
}

/** Parse one frame off the socket, or `null` if it is not an event we know.
 *
 * An unreadable frame is dropped rather than thrown: the socket is a live
 * conversation, and a parse failure in the middle of one must not take the
 * transcript with it. */
export function parseEvent(data: string): ServerEvent | null {
  try {
    const value: unknown = JSON.parse(data);
    if (value && typeof value === "object" && typeof (value as ServerEvent).type === "string") {
      return value as ServerEvent;
    }
  } catch {
    // fall through
  }
  return null;
}

/** The transcript of a **stored** run, rebuilt from `_sc_runs.context`.
 *
 * This is how the history reopens a conversation: the run's context is the
 * loop's own state (§11.2), whose `messages` are the same `LlmMessage`s the
 * socket streamed. Reading them here rather than asking the server for a second
 * rendering is what keeps one description of what a transcript is. */
export function transcriptFromRun(context: unknown): Entry[] {
  const messages = (context as { messages?: unknown })?.messages;
  if (!Array.isArray(messages)) return [];
  const entries: Entry[] = [];
  for (const raw of messages) {
    const message = raw as {
      role?: string;
      content?: string;
      tool_calls?: { id: string; name: string; arguments: unknown }[];
      tool_call_id?: string;
      name?: string;
    };
    if (message.role === "user") {
      entries.push({ kind: "user", text: message.content ?? "" });
    } else if (message.role === "assistant") {
      // A turn that was only tool calls said nothing, and an empty bubble would
      // be putting words in the model's mouth.
      if ((message.content ?? "") !== "") {
        entries.push({ kind: "assistant", text: message.content ?? "", reasoning: "" });
      }
      for (const call of message.tool_calls ?? []) {
        entries.push({
          kind: "tool",
          id: call.id,
          name: call.name,
          args: call.arguments,
          result: null,
          isError: false,
        });
      }
    } else if (message.role === "tool_result") {
      const call = entries.find(
        (entry) => entry.kind === "tool" && entry.id === message.tool_call_id,
      );
      if (call && call.kind === "tool") {
        call.result = message.content ?? "";
        // The loop writes a failed tool's error as its result, prefixed so the
        // model can tell (§11.2). The same prefix is what tells the panel.
        call.isError = (message.content ?? "").startsWith("error: ");
      }
    }
  }
  return entries;
}

/** The bit of `WebSocket` this model uses — so a test can be a plain object. */
export interface SocketLike {
  send(data: string): void;
  close(): void;
  onopen: (() => void) | null;
  onmessage: ((event: { data: string }) => void) | null;
  onclose: (() => void) | null;
  onerror: (() => void) | null;
}

/** One chat: a socket, the transcript it is building, and the three things a
 * person can do to it. */
export class ChatSession {
  private state: ChatState = emptyChat();
  private open = false;
  /** Messages typed before the socket opened, sent in order once it does. */
  private pending: string[] = [];

  constructor(
    private socket: SocketLike,
    private agent: string,
    private onChange: (state: ChatState) => void,
    initial?: { runId?: string | null; entries?: Entry[] },
  ) {
    if (initial?.entries?.length) {
      this.state = { ...this.state, entries: initial.entries };
    }
    if (initial?.runId) {
      this.state = { ...this.state, runId: initial.runId };
    }
    socket.onopen = () => {
      this.open = true;
      // `start` names the agent and, when reopening a conversation, the run to
      // carry on — which is what makes the history's Continue button possible.
      this.socket.send(
        JSON.stringify({ type: "start", agent: this.agent, run: this.state.runId }),
      );
      for (const text of this.pending.splice(0)) {
        this.socket.send(JSON.stringify({ type: "message", text }));
      }
    };
    socket.onmessage = (event) => {
      const parsed = parseEvent(event.data);
      if (parsed) this.update(applyEvent(this.state, parsed));
    };
    socket.onerror = () => {
      this.update(
        applyEvent(this.state, {
          type: "error",
          message: "The connection to the server failed.",
        }),
      );
    };
    socket.onclose = () => {
      this.open = false;
      // A socket that closed mid-turn would otherwise leave the composer
      // disabled for ever, waiting for a `done` that cannot arrive.
      if (this.state.running) {
        this.update({
          ...applyEvent(this.state, {
            type: "error",
            message: "The connection closed before the agent finished.",
          }),
          running: false,
        });
      }
    };
  }

  /** The transcript as it stands. */
  current(): ChatState {
    return this.state;
  }

  /** Say something. Refused while a turn is running, as the loop itself refuses
   * it: a message spliced into a history the model is already answering is how a
   * question gets answered before it was asked. */
  send(text: string): void {
    if (this.state.running || text.trim() === "") return;
    this.update(applyUserMessage(this.state, text));
    if (this.open) {
      this.socket.send(JSON.stringify({ type: "message", text }));
    } else {
      this.pending.push(text);
    }
  }

  /** Stop the turn. The transcript stays exactly as it is — the server ends the
   * run and answers with `done`, which is what clears `running`. */
  abort(): void {
    if (!this.state.running || !this.open) return;
    this.socket.send(JSON.stringify({ type: "abort" }));
  }

  /** Close the socket (leaving the page). */
  close(): void {
    this.socket.close();
  }

  private update(state: ChatState): void {
    this.state = state;
    this.onChange(state);
  }
}
