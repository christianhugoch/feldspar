/**
 * The chat model against a stubbed socket (§11.4): the four properties the
 * transcript has to hold whatever the server sends, plus the two the reopened
 * history depends on.
 *
 * No browser and no server: `ChatSession` takes a `SocketLike`, so a test *is*
 * the other end of the socket.
 */

import { describe, expect, it } from "vitest";

import {
  AGENT_CHAT_ROUTE,
  ChatSession,
  agentChatUrl,
  defaultControlValues,
  emptyChat,
  applyEvent,
  applyUserMessage,
  compactionLabel,
  conclusionLabel,
  normalizeControls,
  splitCodeBlocks,
  transcriptFromRun,
  type Entry,
  type ServerEvent,
  type SocketLike,
} from "./agentChat";

/** The other end of the socket: what was sent, and a way to send back. */
class StubSocket implements SocketLike {
  sent: string[] = [];
  closed = false;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  send(data: string): void {
    this.sent.push(data);
  }
  close(): void {
    this.closed = true;
  }
  /** Deliver one server event to the session. */
  deliver(event: ServerEvent): void {
    this.onmessage?.({ data: JSON.stringify(event) });
  }
  /** What was sent, parsed. */
  messages(): { type: string; [key: string]: unknown }[] {
    return this.sent.map((text) => JSON.parse(text));
  }
}

/** A session over a fresh stub, already opened. */
function session(agent = "librarian", initial?: { runId?: string | null; entries?: Entry[] }) {
  const socket = new StubSocket();
  const chat = new ChatSession(socket, agent, () => {}, initial);
  socket.onopen?.();
  return { socket, chat };
}

describe("where the socket is", () => {
  it("is the same origin as the page, on the route the server mounts", () => {
    expect(agentChatUrl({ protocol: "http:", host: "localhost:3032" })).toBe(
      `ws://localhost:3032${AGENT_CHAT_ROUTE}`,
    );
  });

  it("uses wss: wherever the page itself is over TLS", () => {
    expect(agentChatUrl({ protocol: "https:", host: "saltcorn.example.com" })).toBe(
      `wss://saltcorn.example.com${AGENT_CHAT_ROUTE}`,
    );
  });
});

describe("a turn, as it arrives", () => {
  it("appends deltas in order into one assistant entry", () => {
    let state = applyUserMessage(emptyChat(), "how many books?");
    for (const delta of ["There ", "is ", "one."]) {
      state = applyEvent(state, { type: "text", delta });
    }
    expect(state.entries).toEqual([
      { kind: "user", text: "how many books?" },
      { kind: "assistant", text: "There is one.", reasoning: "" },
    ]);
    expect(state.running).toBe(true);
  });

  it("keeps reasoning out of the answer", () => {
    // Reasoning is the model's notes, not its reply; rendering it as the answer
    // would show working as conclusion.
    let state = applyEvent(emptyChat(), { type: "reasoning", delta: "checking the table" });
    state = applyEvent(state, { type: "text", delta: "One." });
    expect(state.entries).toEqual([
      { kind: "assistant", text: "One.", reasoning: "checking the table" },
    ]);
  });

  it("pairs a tool call with its result, by id", () => {
    let state = applyUserMessage(emptyChat(), "how many?");
    state = applyEvent(state, { type: "text", delta: "Let me look." });
    state = applyEvent(state, {
      type: "tool_call",
      id: "call_1",
      name: "query_books",
      arguments: { limit: 5 },
    });
    // The result is not there yet: that is what renders as a tool still running.
    expect(state.entries[2]).toEqual({
      kind: "tool",
      id: "call_1",
      name: "query_books",
      args: { limit: 5 },
      result: null,
      isError: false,
    });

    state = applyEvent(state, {
      type: "tool_result",
      id: "call_1",
      name: "query_books",
      content: '[{"title":"Dune"}]',
      is_error: false,
    });
    expect(state.entries[2]).toMatchObject({ result: '[{"title":"Dune"}]', isError: false });

    // Text after a tool starts a **new** assistant entry, so the turn reads as
    // it happened rather than as one paragraph with a tool inside it.
    state = applyEvent(state, { type: "text", delta: "There is one." });
    expect(state.entries).toHaveLength(4);
    expect(state.entries[3]).toEqual({
      kind: "assistant",
      text: "There is one.",
      reasoning: "",
    });
  });

  it("marks a failed tool as failed without ending the turn", () => {
    let state = applyEvent(emptyChat(), {
      type: "tool_call",
      id: "c1",
      name: "query_books",
      arguments: {},
    });
    state = applyEvent(state, {
      type: "tool_result",
      id: "c1",
      name: "query_books",
      content: "error: no table named `bookz`",
      is_error: true,
    });
    expect(state.entries[0]).toMatchObject({ isError: true });
    expect(state.running).toBe(false); // no turn was started by these alone
  });

  it("ends on done, keeping the run it produced", () => {
    let state = applyUserMessage(emptyChat(), "hi");
    state = applyEvent(state, { type: "text", delta: "hello" });
    state = applyEvent(state, { type: "done", run: "run-1", state: "done", answer: "hello" });
    expect(state.running).toBe(false);
    expect(state.runId).toBe("run-1");
    expect(state.lastState).toBe("done");
  });
});

describe("a loop that stopped on its own says why", () => {
  it("adds a notice for a stuck or over-budget run, and none for an answer", () => {
    let state = applyUserMessage(emptyChat(), "fix it");
    state = applyEvent(state, {
      type: "done",
      run: "run-9",
      state: "done",
      answer: "",
      conclusion: "stuck",
      reason: "3 malformed tool calls in a row, the last to `edit`",
    });
    expect(state.entries.map((e) => e.kind)).toEqual(["user", "notice"]);
    expect(state.entries[1]).toMatchObject({
      message: expect.stringContaining("3 malformed tool calls"),
    });
    expect(state.running).toBe(false);

    state = applyUserMessage(state, "again");
    state = applyEvent(state, {
      type: "done",
      run: "run-9",
      state: "done",
      answer: "",
      conclusion: "over_budget",
      budget: "wall_time",
    });
    expect(state.entries[3]).toEqual({
      kind: "notice",
      message: "The agent ran out of its wall time budget.",
    });

    const answered = applyEvent(applyUserMessage(emptyChat(), "hi"), {
      type: "done",
      run: "r",
      state: "done",
      answer: "hello",
      conclusion: "answered",
    });
    expect(answered.entries.map((e) => e.kind)).toEqual(["user"]);
  });

  it("rebuilds the notice from a stored run, and labels it for the list", () => {
    const entries = transcriptFromRun({
      messages: [{ role: "user", content: "fix it" }],
      phase: { phase: "done", conclusion: { conclusion: "stuck", reason: "looping" } },
    });
    expect(entries.map((e) => e.kind)).toEqual(["user", "notice"]);
    expect(conclusionLabel({ conclusion: "stuck", reason: "looping" })).toBe("stuck");
    expect(conclusionLabel({ conclusion: "answered" })).toBeNull();
    expect(conclusionLabel(null)).toBeNull();
  });
});

describe("a failure is rendered, not swallowed", () => {
  it("keeps everything that came before the error", () => {
    let state = applyUserMessage(emptyChat(), "go");
    state = applyEvent(state, { type: "text", delta: "Working…" });
    state = applyEvent(state, { type: "error", message: "401 invalid x-api-key" });
    state = applyEvent(state, { type: "done", run: "run-2", state: "failed", answer: "" });
    expect(state.entries.map((e) => e.kind)).toEqual(["user", "assistant", "error"]);
    expect(state.entries[2]).toEqual({ kind: "error", message: "401 invalid x-api-key" });
    // And the composer is released: a failed turn must not leave the page stuck.
    expect(state.running).toBe(false);
    expect(state.lastState).toBe("failed");
  });

  it("an unreadable frame is dropped rather than taking the transcript with it", () => {
    const { socket, chat } = session();
    chat.send("hi");
    socket.onmessage?.({ data: "not json" });
    socket.deliver({ type: "text", delta: "hello" });
    expect(chat.current().entries.map((e) => e.kind)).toEqual(["user", "assistant"]);
  });

  it("releases the composer when the socket closes mid-turn", () => {
    const { socket, chat } = session();
    chat.send("hi");
    socket.onclose?.();
    const state = chat.current();
    expect(state.running).toBe(false);
    expect(state.entries[state.entries.length - 1].kind).toBe("error");
  });
});

describe("the session's side of the protocol", () => {
  it("starts by naming the agent, then sends each message", () => {
    const { socket, chat } = session();
    chat.send("how many books?");
    expect(socket.messages()).toEqual([
      { type: "start", agent: "librarian", run: null },
      { type: "message", text: "how many books?" },
    ]);
  });

  it("names the run it is continuing, so the history reopens a conversation", () => {
    const { socket } = session("librarian", { runId: "run-7" });
    expect(socket.messages()[0]).toEqual({
      type: "start",
      agent: "librarian",
      run: "run-7",
    });
  });

  it("holds a message typed before the socket opened", () => {
    const socket = new StubSocket();
    const chat = new ChatSession(socket, "librarian", () => {});
    chat.send("early");
    // Nothing crossed the wire yet, but the transcript already shows it — the
    // person typed it, and a message that vanished would be worse than one that
    // waits.
    expect(socket.sent).toHaveLength(0);
    expect(chat.current().entries[0]).toEqual({ kind: "user", text: "early" });
    socket.onopen?.();
    expect(socket.messages()).toEqual([
      { type: "start", agent: "librarian", run: null },
      { type: "message", text: "early" },
    ]);
  });

  it("refuses a second message while the agent is answering", () => {
    const { socket, chat } = session();
    chat.send("first");
    chat.send("second");
    expect(socket.messages().filter((m) => m.type === "message")).toHaveLength(1);
    // Once the turn ends, the next message goes.
    socket.deliver({ type: "done", run: "r", state: "done", answer: "" });
    chat.send("second");
    expect(socket.messages().filter((m) => m.type === "message")).toHaveLength(2);
  });

  it("sends nothing for an empty message", () => {
    const { socket, chat } = session();
    chat.send("   ");
    expect(socket.messages().filter((m) => m.type === "message")).toHaveLength(0);
  });

  it("aborts only a turn that is running, and leaves the transcript intact", () => {
    const { socket, chat } = session();
    chat.abort();
    expect(socket.messages().some((m) => m.type === "abort")).toBe(false);

    chat.send("keep going");
    socket.deliver({ type: "tool_call", id: "c1", name: "query_books", arguments: {} });
    chat.abort();
    expect(socket.messages().some((m) => m.type === "abort")).toBe(true);

    // The server ends the run and says so; what had already happened is still
    // on screen.
    socket.deliver({ type: "done", run: "run-3", state: "aborted", answer: "" });
    const state = chat.current();
    expect(state.running).toBe(false);
    expect(state.lastState).toBe("aborted");
    expect(state.entries.map((e) => e.kind)).toEqual(["user", "tool"]);
  });
});

describe("reopening a stored run", () => {
  /** A run's context as `_fd_runs` holds it: the loop's own state. */
  const context = {
    messages: [
      { role: "user", content: "how many books?" },
      {
        role: "assistant",
        content: "Let me look.",
        tool_calls: [{ id: "c1", name: "query_books", arguments: { limit: 5 } }],
      },
      { role: "tool_result", tool_call_id: "c1", name: "query_books", content: '[{"title":"Dune"}]' },
      { role: "assistant", content: "There is one: Dune.", tool_calls: [] },
    ],
    step: 2,
    max_steps: 20,
  };

  it("rebuilds the transcript the socket streamed", () => {
    const entries = transcriptFromRun(context);
    expect(entries.map((e) => e.kind)).toEqual(["user", "assistant", "tool", "assistant"]);
    expect(entries[2]).toMatchObject({
      name: "query_books",
      args: { limit: 5 },
      result: '[{"title":"Dune"}]',
      isError: false,
    });
    expect(entries[3]).toMatchObject({ text: "There is one: Dune." });
  });

  it("shows a failed tool as failed", () => {
    const entries = transcriptFromRun({
      messages: [
        { role: "assistant", content: "", tool_calls: [{ id: "c1", name: "q", arguments: {} }] },
        { role: "tool_result", tool_call_id: "c1", name: "q", content: "error: no table" },
      ],
    });
    // A turn that only called a tool said nothing, so there is no empty bubble.
    expect(entries.map((e) => e.kind)).toEqual(["tool"]);
    expect(entries[0]).toMatchObject({ isError: true });
  });

  it("marks each compaction where it happened, with its summary", () => {
    const entries = transcriptFromRun({
      messages: [
        { role: "user", content: "build it" },
        { role: "assistant", content: "", tool_calls: [{ id: "c1", name: "dump", arguments: {} }] },
        { role: "tool_result", tool_call_id: "c1", name: "dump", content: "x" },
        { role: "assistant", content: "done", tool_calls: [] },
      ],
      context: {
        compactions: [
          {
            step: 2,
            at: 3,
            elided: 1,
            before_tokens: 900,
            after_tokens: 300,
            up_to_index: 1,
            summary: "## Goal\nbuild it",
          },
        ],
      },
    });
    // The transcript is whole, with the marker before the turn it preceded.
    expect(entries.map((e) => e.kind)).toEqual(["user", "tool", "compaction", "assistant"]);
    const marker = entries[2] as Extract<Entry, { kind: "compaction" }>;
    expect(marker).toEqual({
      kind: "compaction",
      elided: 1,
      beforeTokens: 900,
      afterTokens: 300,
      summary: "## Goal\nbuild it",
    });
    expect(compactionLabel(marker)).toBe(
      "Context compacted from 900 to 300 tokens: 1 old tool result cleared, older turns summarised",
    );
  });

  it("appends a live compaction as a marker", () => {
    const state = applyEvent(emptyChat(), {
      type: "compaction",
      step: 4,
      elided: 2,
      before_tokens: 3400,
      after_tokens: 1200,
    });
    expect(state.entries).toEqual([
      { kind: "compaction", elided: 2, beforeTokens: 3400, afterTokens: 1200, summary: null },
    ]);
  });

  it("is empty for a run with nothing readable in it", () => {
    expect(transcriptFromRun(null)).toEqual([]);
    expect(transcriptFromRun({})).toEqual([]);
    expect(transcriptFromRun({ messages: "nonsense" })).toEqual([]);
  });
});

describe("the composer's controls", () => {
  const declared = [
    { kind: "toggle", name: "web", label: "Search the web", default: true },
    {
      kind: "select",
      name: "branch",
      label: "Branch",
      options: [
        { value: "main", label: "main" },
        { value: "next", label: "next" },
      ],
      default: "next",
    },
  ];

  it("takes what a trait declares, and drops what it cannot render", () => {
    const controls = normalizeControls([
      ...declared,
      { kind: "toggle", name: "no-label" },
      { kind: "select", name: "empty", options: [] },
      { kind: "carousel", name: "wat", label: "no" },
      "nonsense",
    ]);
    // One malformed control costs the composer that control, not the toolbar.
    expect(controls.map((c) => c.name)).toEqual(["web", "branch"]);
    expect(normalizeControls(undefined)).toEqual([]);
  });

  it("starts each control where its declaration says", () => {
    expect(defaultControlValues(normalizeControls(declared))).toEqual({
      web: true,
      branch: "next",
    });
  });

  it("keeps a choice already made when the declaration arrives again", () => {
    const controls = normalizeControls(declared);
    const chosen = { web: false, branch: "main" };
    expect(defaultControlValues(controls, chosen)).toEqual(chosen);
    // …but not one the redeclared control no longer offers.
    const narrowed = normalizeControls([
      { kind: "select", name: "branch", options: [{ value: "next", label: "next" }] },
    ]);
    expect(defaultControlValues(narrowed, chosen)).toEqual({ branch: "next" });
  });

  it("sends the controls with the message they were composed with", () => {
    const { socket, chat } = session();
    socket.deliver({ type: "controls", controls: declared });
    chat.setControl("web", false);
    // A control nobody declared is not a control.
    chat.setControl("nonexistent", "x");
    chat.send("what changed?");

    const message = socket.messages().find((m) => m.type === "message");
    expect(message).toEqual({
      type: "message",
      text: "what changed?",
      controls: { web: false, branch: "next" },
    });
  });

  it("says nothing about controls when the agent has none", () => {
    const { socket, chat } = session();
    chat.send("hello");
    expect(socket.messages().find((m) => m.type === "message")).toEqual({
      type: "message",
      text: "hello",
    });
  });

  it("freezes the controls a queued message was composed with", () => {
    // Typed before the socket opened: the frame belongs to the message, so a
    // control changed while it waits does not rewrite what was asked.
    const socket = new StubSocket();
    const chat = new ChatSession(socket, "librarian", () => {});
    socket.deliver({ type: "controls", controls: declared });
    chat.send("first");
    chat.setControl("web", false);
    socket.onopen?.();

    expect(socket.messages()[1]).toEqual({
      type: "message",
      text: "first",
      controls: { web: true, branch: "next" },
    });
  });
});

describe("code in an answer", () => {
  it("separates fenced code from the prose around it", () => {
    expect(
      splitCodeBlocks("Here it is:\n\n```rust\nfn main() {}\n```\n\nThat is all."),
    ).toEqual([
      { kind: "prose", text: "Here it is:" },
      { kind: "code", language: "rust", text: "fn main() {}" },
      { kind: "prose", text: "\nThat is all." },
    ]);
  });

  it("treats a fence that is still open as code", () => {
    // A streaming answer is read while the fence is open; waiting for the close
    // would reformat the paragraph under the reader when the turn ends.
    expect(splitCodeBlocks("wait:\n```\nSELECT 1")).toEqual([
      { kind: "prose", text: "wait:" },
      { kind: "code", language: "", text: "SELECT 1" },
    ]);
  });

  it("leaves an answer with no code as one run of prose", () => {
    expect(splitCodeBlocks("There is one: Dune.")).toEqual([
      { kind: "prose", text: "There is one: Dune." },
    ]);
    expect(splitCodeBlocks("")).toEqual([]);
  });
});
