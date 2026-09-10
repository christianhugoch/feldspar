/**
 * One conversation with a Saltcorn **agent**, over the admin chat socket
 * (§11.4).
 *
 * The same protocol the admin UI's chat panel speaks — `ui/admin/src/agentChat.ts`
 * is its other client — and deliberately a *different* model of it. The panel
 * folds events into a transcript it renders itself; here the transcript is VS
 * Code's, so what is needed is not a reducer but a turn: send one message, hand
 * every event to a sink as it arrives, and settle when the server says `done`.
 *
 * The socket is same-origin and authenticated by the same session cookie the
 * IDE's own page load was, so there is no credential here and nothing to
 * configure: a page that could open `/ide/` can open this.
 *
 * The **run** is what makes this a conversation rather than a series of
 * questions. The server answers the first `done` with a run id; every later turn
 * on this object starts from it, so a socket that drops mid-conversation costs a
 * reconnection and nothing else — the next `start` names the run and the agent
 * carries on with the history it already has.
 */

/** The route the server serves the chat socket on (matches `AGENT_CHAT_ROUTE`). */
export const AGENT_CHAT_ROUTE = "/admin/agent-chat";

/** Where the chat socket is, for a page served from `location`.
 *
 * `wss:` wherever the page itself is over TLS: a `ws:` socket opened from an
 * `https:` page is mixed content and the browser blocks it, which would present
 * as an agent that never answers on exactly the deployments that are configured
 * correctly. */
export function agentChatUrl(location: {
  protocol: string;
  host: string;
}): string {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}${AGENT_CHAT_ROUTE}`;
}

/** One event the server sends (§11.4). */
export type ServerEvent =
  | { type: "text"; delta: string }
  | { type: "reasoning"; delta: string }
  | { type: "tool_call"; id: string; name: string; arguments: unknown }
  | {
      type: "tool_result";
      id: string;
      name: string;
      content: string;
      is_error: boolean;
    }
  | { type: "done"; run: string | null; state: string; answer: string }
  | { type: "error"; message: string }
  | { type: "controls"; controls: unknown };

/** Parse one frame off the socket, or `null` if it is not an event we know.
 *
 * An unreadable frame is dropped rather than thrown: the socket is a live
 * conversation, and a parse failure in the middle of one must not take the turn
 * with it. */
export function parseEvent(data: string): ServerEvent | null {
  try {
    const value: unknown = JSON.parse(data);
    if (
      value &&
      typeof value === "object" &&
      typeof (value as ServerEvent).type === "string"
    ) {
      return value as ServerEvent;
    }
  } catch {
    // fall through
  }
  return null;
}

/** The bit of `WebSocket` a conversation uses — so a test can be a plain object. */
export interface SocketLike {
  send(data: string): void;
  close(): void;
  onopen: (() => void) | null;
  onmessage: ((event: { data: string }) => void) | null;
  onclose: (() => void) | null;
  onerror: (() => void) | null;
}

/** How a turn ended: the run it belongs to, and the state the server reported.
 *
 * `state` is the server's own word — `done`, `failed`, `aborted` — or, when the
 * socket closed before it said anything, `disconnected`. It is reported rather
 * than thrown: a turn that failed has usually already streamed an `error` event
 * into the answer, and a caller re-throwing would render the failure twice. */
export interface TurnOutcome {
  readonly run: string | null;
  readonly state: string;
}

/** What a turn's events are handed to as they arrive. */
export type EventSink = (event: ServerEvent) => void;

/** Open a real socket to the chat route, resolved once it is open.
 *
 * Rejecting on `onerror` matters: a server with no agents installed refuses the
 * upgrade, and a caller waiting on a promise that never settles is a chat
 * composer that spins for ever.
 */
export function connectAgentChat(url: string): Promise<SocketLike> {
  return new Promise((resolve, reject) => {
    // A browser `WebSocket` *is* a `SocketLike` — it sends, it closes, and it
    // has the four handlers — but its handler signatures are the DOM's, which
    // take an event this code never reads.
    const socket = new WebSocket(url) as unknown as SocketLike;
    socket.onopen = () => resolve(socket);
    socket.onerror = () =>
      reject(new Error(`The chat socket at ${url} could not be opened.`));
  });
}

/** A cancellation source shaped like VS Code's, so a test needs no VS Code. */
export interface CancellationLike {
  readonly isCancellationRequested: boolean;
  onCancellationRequested(listener: () => void): { dispose(): void };
}

/**
 * A conversation with one agent: a socket that is opened when it is first
 * needed, and the run it is building.
 */
export class AgentConversation {
  private socket: SocketLike | null = null;
  private opening: Promise<SocketLike> | null = null;
  private run: string | null = null;

  constructor(
    /** The agent's name — what `_fd_runs.subject` holds. */
    readonly agent: string,
    private readonly connect: () => Promise<SocketLike>,
  ) {}

  /** The run this conversation has become, once the server has said. */
  get runId(): string | null {
    return this.run;
  }

  /**
   * Say `text` to the agent and stream what comes back into `sink`.
   *
   * One turn at a time: the server refuses a message spliced into a history it
   * is already answering, so a caller that overlaps them gets the same refusal
   * the admin panel's disabled composer prevents. VS Code does not overlap
   * requests within one chat session, which is why this does not queue.
   */
  async ask(
    text: string,
    sink: EventSink,
    token?: CancellationLike,
  ): Promise<TurnOutcome> {
    const socket = await this.open();
    return await new Promise<TurnOutcome>((resolve) => {
      let settled = false;
      const finish = (outcome: TurnOutcome) => {
        if (settled) return;
        settled = true;
        subscription?.dispose();
        resolve(outcome);
      };
      socket.onmessage = (event) => {
        const parsed = parseEvent(event.data);
        if (parsed == null) return;
        if (parsed.type === "done") {
          this.run = parsed.run ?? this.run;
          finish({ run: this.run, state: parsed.state });
          return;
        }
        sink(parsed);
      };
      socket.onclose = () => {
        this.forget(socket);
        // A socket that closed mid-turn would otherwise leave the caller waiting
        // for a `done` that cannot arrive.
        sink({
          type: "error",
          message: "The connection closed before the agent finished.",
        });
        finish({ run: this.run, state: "disconnected" });
      };
      socket.onerror = () => {
        sink({
          type: "error",
          message: "The connection to the server failed.",
        });
      };
      // Registered before the message is sent, so a cancellation that arrives
      // while the model is still thinking is not one this turn missed.
      const subscription = token?.onCancellationRequested(() => {
        socket.send(JSON.stringify({ type: "abort" }));
      });
      socket.send(JSON.stringify({ type: "message", text }));
    });
  }

  /** Close the socket, keeping the run — the next turn reconnects to it. */
  close(): void {
    const socket = this.socket;
    this.forget(socket);
    socket?.close();
  }

  /**
   * The open socket, opening one if there is none.
   *
   * `start` is sent here rather than by the caller because it is what binds a
   * *socket* to this agent and this run, and a reconnection has to send it again
   * — which is the whole reason a dropped connection is survivable.
   */
  private open(): Promise<SocketLike> {
    if (this.socket != null) return Promise.resolve(this.socket);
    this.opening ??= this.connect()
      .then((socket) => {
        socket.send(
          JSON.stringify({ type: "start", agent: this.agent, run: this.run }),
        );
        this.socket = socket;
        this.opening = null;
        return socket;
      })
      .catch((err: unknown) => {
        this.opening = null;
        throw err;
      });
    return this.opening;
  }

  private forget(socket: SocketLike | null): void {
    if (this.socket === socket) this.socket = null;
  }
}
