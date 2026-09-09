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

/** One event the server sends. The six of §11.4, plus `controls` (below). */
export type ServerEvent =
  | { type: "text"; delta: string }
  | { type: "reasoning"; delta: string }
  | { type: "tool_call"; id: string; name: string; arguments: unknown }
  | { type: "tool_result"; id: string; name: string; content: string; is_error: boolean }
  | { type: "done"; run: string | null; state: string; answer: string }
  | { type: "error"; message: string }
  | { type: "controls"; controls: unknown };

/** One control a trait puts in the composer, beside the send button.
 *
 * The composer is the only place a person can say anything to an agent, so it
 * is where a trait's *modes* belong: "search the web for this one", "which
 * branch am I working on", "run it after you write it". A dropdown of a store's
 * directories set once beats the same sentence typed into every message, and a
 * toggle is a thing the model should be told rather than guess.
 *
 * Nothing declares one yet: no built-in trait has a mode. The shape is here —
 * and rendered — because the alternative is a composer whose toolbar row does
 * not exist and has to be invented under a trait that needs it. A control is
 * therefore **data**: a trait declares it the way it declares a tool's schema,
 * the server forwards the declaration in a `controls` event, and the panel
 * renders whatever arrives without knowing which trait sent it — the same rule
 * §11.2 sets for the trait config form.
 *
 * The two kinds are the two the toolbar of every chat interface has: a toggle
 * (a mode that is on or off) and a select (one of several). Both carry a value
 * that travels with the *next* message, which is what makes them a modifier on
 * what the person is about to say rather than a button that does something on
 * its own. A control that acts immediately would be a client→server frame of
 * its own, and is deliberately not invented until a trait asks for one. */
export type ComposerControl =
  | { kind: "toggle"; name: string; label: string; title?: string; default?: boolean }
  | {
      kind: "select";
      name: string;
      label?: string;
      title?: string;
      options: { value: string; label: string }[];
      default?: string;
    };

/** What a control is currently set to, as it travels with a message. */
export type ControlValue = string | boolean;

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
  /** What the agent's traits put in the composer's toolbar. Empty until one
   * declares something, which no built-in trait does yet. */
  controls: ComposerControl[];
  /** What each of those is set to, keyed by control name. */
  controlValues: Record<string, ControlValue>;
};

/** A conversation with nothing in it yet. */
export function emptyChat(): ChatState {
  return {
    entries: [],
    running: false,
    runId: null,
    lastState: null,
    controls: [],
    controlValues: {},
  };
}

/** The controls in a `controls` event, with anything unreadable dropped.
 *
 * A declaration comes off the socket, so it is checked here rather than
 * trusted: a trait that ships a malformed control should cost the composer that
 * one button, not the whole toolbar and not the transcript. */
export function normalizeControls(value: unknown): ComposerControl[] {
  if (!Array.isArray(value)) return [];
  const controls: ComposerControl[] = [];
  for (const raw of value) {
    const control = raw as Partial<ComposerControl> & Record<string, unknown>;
    if (typeof control?.name !== "string" || control.name === "") continue;
    if (control.kind === "toggle") {
      if (typeof control.label !== "string") continue;
      controls.push({
        kind: "toggle",
        name: control.name,
        label: control.label,
        title: typeof control.title === "string" ? control.title : undefined,
        default: control.default === true,
      });
    } else if (control.kind === "select") {
      const options = Array.isArray(control.options)
        ? control.options.filter(
            (option): option is { value: string; label: string } =>
              typeof (option as { value?: unknown })?.value === "string" &&
              typeof (option as { label?: unknown })?.label === "string",
          )
        : [];
      // A select with nothing to select is a dead control, not a narrow one.
      if (options.length === 0) continue;
      controls.push({
        kind: "select",
        name: control.name,
        label: typeof control.label === "string" ? control.label : undefined,
        title: typeof control.title === "string" ? control.title : undefined,
        options,
        default: typeof control.default === "string" ? control.default : undefined,
      });
    }
  }
  return controls;
}

/** What the controls start at, keeping whatever the person had already chosen.
 *
 * The declaration can arrive again mid-conversation (a trait whose options
 * depend on what the last turn did), and a dropdown that reset itself every
 * time would lose a choice made two messages ago. A value is kept only while
 * the control is still offered *and* still accepts it. */
export function defaultControlValues(
  controls: ComposerControl[],
  previous: Record<string, ControlValue> = {},
): Record<string, ControlValue> {
  const values: Record<string, ControlValue> = {};
  for (const control of controls) {
    const had = previous[control.name];
    if (control.kind === "toggle") {
      values[control.name] = typeof had === "boolean" ? had : control.default === true;
    } else {
      const keeps = typeof had === "string" && control.options.some((o) => o.value === had);
      values[control.name] = keeps
        ? had
        : (control.options.find((o) => o.value === control.default)?.value ??
          control.options[0].value);
    }
  }
  return values;
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
    case "controls": {
      const controls = normalizeControls(event.controls);
      return {
        ...state,
        controls,
        controlValues: defaultControlValues(controls, state.controlValues),
      };
    }
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

/** A run of prose, or a fenced code block, in what the agent said. */
export type Block =
  | { kind: "prose"; text: string }
  | { kind: "code"; language: string; text: string };

/** Split an answer on ``` fences.
 *
 * Not a Markdown renderer, and deliberately not the beginning of one: an agent
 * that reads and writes files answers with code, and code set in the body font
 * with its indentation collapsed is the one part of an answer that is unusable
 * rather than merely plain. Everything else — headings, lists, emphasis — is
 * still legible as it was typed, so it stays as typed.
 *
 * An unterminated fence is still a code block: a streaming answer is read while
 * the fence is still open, and waiting for the closing one would reformat the
 * paragraph under the reader every time a turn finished. */
export function splitCodeBlocks(text: string): Block[] {
  const blocks: Block[] = [];
  const fence = /^```([^\n`]*)\n?/gm;
  let at = 0;
  let match: RegExpExecArray | null;
  while ((match = fence.exec(text)) !== null) {
    const prose = text.slice(at, match.index);
    if (prose.trim() !== "") blocks.push({ kind: "prose", text: prose.replace(/\n+$/, "") });
    const start = match.index + match[0].length;
    const close = fence.exec(text);
    const end = close ? close.index : text.length;
    const code = text.slice(start, end).replace(/\n$/, "");
    if (code !== "") blocks.push({ kind: "code", language: match[1].trim(), text: code });
    if (!close) return blocks;
    // A `null` from `exec` has already rewound `lastIndex` to 0, so an
    // unterminated fence must leave the loop rather than search again from the
    // top — which is the same fence, for ever.
    at = close.index + close[0].length;
  }
  const rest = text.slice(at);
  if (rest.trim() !== "") blocks.push({ kind: "prose", text: rest.replace(/\n+$/, "") });
  return blocks;
}

/** The transcript of a **stored** run, rebuilt from `_fd_runs.context`.
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
  /** Message frames composed before the socket opened, sent in order once it
   * does — each already carrying the controls it was composed with. */
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
      for (const frame of this.pending.splice(0)) {
        this.socket.send(frame);
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
    const frame = this.messageFrame(text);
    if (this.open) {
      this.socket.send(frame);
    } else {
      // Frozen now rather than on send: the controls belong to the message that
      // was composed with them, not to whatever they say when the socket opens.
      this.pending.push(frame);
    }
  }

  /** Set one composer control. Takes effect on the *next* message: a control is
   * a modifier on what is about to be said, and a turn already in flight was
   * sent with the values it was sent with. */
  setControl(name: string, value: ControlValue): void {
    if (!this.state.controls.some((control) => control.name === name)) return;
    this.update({
      ...this.state,
      controlValues: { ...this.state.controlValues, [name]: value },
    });
  }

  /** One `message` frame, carrying the controls only when there are any — so an
   * agent with no modes sends exactly the frame it sent before them. */
  private messageFrame(text: string): string {
    const values = this.state.controlValues;
    return JSON.stringify(
      Object.keys(values).length > 0
        ? { type: "message", text, controls: values }
        : { type: "message", text },
    );
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
