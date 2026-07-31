// The chat panel: a transcript, a composer, a stop button, and the agent's run
// history (§11.4).
//
// Everything about *what* the transcript is lives in `agentChat.ts` — the
// socket, the event fold, the rebuild of a stored run — and is tested there
// against a stub. This file is the rendering and the three buttons, which is
// the split that lets the interesting half be tested without a browser.
//
// Two decisions visible on screen:
//
//   - **A tool call is a collapsible entry**, naming the tool, holding its
//     arguments and its result. Collapsed by default because a transcript is
//     read for what the agent *said*; expandable because when it goes wrong,
//     what it did is the only thing that explains it.
//   - **An old run reopens read-only.** It is a record of what happened, and a
//     composer under it would invite an edit to history. Continuing one is a
//     deliberate act — the Continue button, which reconnects the socket to that
//     run.

import { useCallback, useEffect, useRef, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Form from "react-bootstrap/Form";
import Spinner from "react-bootstrap/Spinner";

import { api, errorMessage } from "../api";
import type { ListRunsResponse } from "../client";
import {
  ChatSession,
  agentChatUrl,
  emptyChat,
  transcriptFromRun,
  type ChatState,
  type Entry,
  type SocketLike,
} from "../agentChat";
import { navigate } from "../App";
import { IconArrowLeft } from "../icons";
import { PageBody, PageHeader, StatusBadge, type Tone } from "../layout";

type RunItem = ListRunsResponse[number];

/** How a finished run reads in the history, and in the line under a transcript. */
function stateTone(state: string): Tone {
  switch (state) {
    case "done":
      return "green";
    case "failed":
      return "red";
    case "aborted":
      return "yellow";
    default:
      return "blue";
  }
}

/** JSON as a person reads it — a tool's arguments, or a result that happens to
 * be JSON. Anything that is not JSON is shown exactly as it arrived. */
function pretty(value: unknown): string {
  if (typeof value === "string") {
    try {
      return JSON.stringify(JSON.parse(value), null, 2);
    } catch {
      return value;
    }
  }
  return JSON.stringify(value, null, 2) ?? "";
}

export function AgentChat({ agent }: { agent: string }) {
  const [chat, setChat] = useState<ChatState>(emptyChat());
  const [runs, setRuns] = useState<RunItem[]>([]);
  const [viewing, setViewing] = useState<{ run: RunItem; entries: Entry[] } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const session = useRef<ChatSession | null>(null);
  const bottom = useRef<HTMLDivElement | null>(null);

  const loadRuns = useCallback(async () => {
    try {
      setRuns(await api.listRuns(agent));
    } catch (err) {
      setError(errorMessage(err, "Could not load this agent's history."));
    }
  }, [agent]);

  /** Open a socket for this agent, optionally continuing `run`. */
  const connect = useCallback(
    (run?: { id: string; entries: Entry[] }) => {
      session.current?.close();
      // A browser `WebSocket` *is* a `SocketLike` — it sends, it closes, and it
      // calls the four handlers. TypeScript will not agree, because its handler
      // signatures are typed against the DOM's own event objects and a narrower
      // parameter is not assignable under `strictFunctionTypes`. The cast is
      // here rather than in the model, so what the model demands stays the
      // smallest thing a test can be.
      const socket = new WebSocket(agentChatUrl(window.location)) as unknown as SocketLike;
      session.current = new ChatSession(socket, agent, setChat, {
        runId: run?.id ?? null,
        entries: run?.entries ?? [],
      });
      setChat(session.current.current());
    },
    [agent],
  );

  useEffect(() => {
    void loadRuns();
    connect();
    return () => {
      session.current?.close();
      session.current = null;
    };
  }, [agent, connect, loadRuns]);

  // A turn that ended produced or changed a run, so the history is stale.
  const running = chat.running;
  useEffect(() => {
    if (!running) void loadRuns();
  }, [running, loadRuns]);

  useEffect(() => {
    bottom.current?.scrollIntoView({ block: "end" });
  }, [chat.entries]);

  const send = () => {
    const text = draft;
    setDraft("");
    session.current?.send(text);
  };

  const openRun = async (run: RunItem) => {
    setError(null);
    try {
      const whole = await api.getRun(run.id);
      setViewing({ run, entries: transcriptFromRun(whole.context) });
    } catch (err) {
      setError(errorMessage(err, "Could not open that conversation."));
    }
  };

  const continueRun = () => {
    if (!viewing) return;
    connect({ id: viewing.run.id, entries: viewing.entries });
    setViewing(null);
  };

  const removeRun = async (run: RunItem) => {
    if (!window.confirm("Delete this conversation? It is the only record of what happened.")) {
      return;
    }
    try {
      await api.deleteRun(run.id);
      if (viewing?.run.id === run.id) setViewing(null);
      await loadRuns();
    } catch (err) {
      setError(errorMessage(err, "Could not delete that conversation."));
    }
  };

  const entries = viewing ? viewing.entries : chat.entries;

  return (
    <>
      <PageHeader
        pretitle="Agents"
        title={agent}
        actions={
          <>
            <Button variant="outline-secondary" onClick={() => navigate("/agents")}>
              <IconArrowLeft className="icon-2" />
              Back
            </Button>
            <Button
              variant="outline-secondary"
              onClick={() => {
                setViewing(null);
                connect();
              }}
            >
              New conversation
            </Button>
          </>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}

        <div className="row">
          <div className="col-lg-8">
            <Card className="mb-3">
              <Card.Body>
                {entries.length === 0 && (
                  <p className="text-muted mb-0">
                    Ask this agent something. Everything it does — every table it reads, every
                    trigger it runs — happens as you.
                  </p>
                )}
                {entries.map((entry, i) => (
                  <TranscriptEntry key={i} entry={entry} />
                ))}
                {chat.running && !viewing && (
                  <div className="text-muted small d-flex align-items-center gap-2">
                    <Spinner animation="border" size="sm" role="status" />
                    Thinking…
                  </div>
                )}
                <div ref={bottom} />
              </Card.Body>

              {viewing ? (
                <Card.Footer className="d-flex align-items-center justify-content-between">
                  <span className="text-muted small">
                    A past conversation, shown as it happened.{" "}
                    <StatusBadge tone={stateTone(viewing.run.state)} title={viewing.run.error ?? undefined}>
                      {viewing.run.state}
                    </StatusBadge>
                  </span>
                  <Button size="sm" onClick={continueRun}>
                    Continue this conversation
                  </Button>
                </Card.Footer>
              ) : (
                <Card.Footer>
                  <Form
                    onSubmit={(e) => {
                      e.preventDefault();
                      send();
                    }}
                  >
                    <Form.Control
                      as="textarea"
                      rows={3}
                      value={draft}
                      placeholder="Ask the agent…"
                      disabled={chat.running}
                      onChange={(e) => setDraft(e.target.value)}
                      // Enter sends, Shift+Enter is a newline — the convention
                      // every chat box in the world has taught.
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && !e.shiftKey) {
                          e.preventDefault();
                          send();
                        }
                      }}
                    />
                    <div className="btn-list mt-2">
                      <Button type="submit" disabled={chat.running || draft.trim() === ""}>
                        Send
                      </Button>
                      <Button
                        variant="outline-danger"
                        disabled={!chat.running}
                        onClick={() => session.current?.abort()}
                      >
                        Stop
                      </Button>
                      {chat.lastState && chat.lastState !== "done" && (
                        <span className="align-self-center">
                          <StatusBadge tone={stateTone(chat.lastState)}>
                            {chat.lastState}
                          </StatusBadge>
                        </span>
                      )}
                    </div>
                  </Form>
                </Card.Footer>
              )}
            </Card>
          </div>

          <div className="col-lg-4">
            <Card>
              <Card.Header>History</Card.Header>
              <div className="list-group list-group-flush">
                {runs.length === 0 && (
                  <div className="list-group-item text-muted">
                    No conversations yet.
                  </div>
                )}
                {runs.map((run) => (
                  <div
                    key={run.id}
                    className={
                      run.id === (viewing?.run.id ?? chat.runId)
                        ? "list-group-item active"
                        : "list-group-item"
                    }
                  >
                    <div className="d-flex justify-content-between align-items-start gap-2">
                      <button
                        type="button"
                        className="btn btn-link p-0 text-start text-break"
                        onClick={() => void openRun(run)}
                      >
                        {run.description || "(no first message)"}
                      </button>
                      <button
                        type="button"
                        className="btn btn-link p-0 text-danger"
                        aria-label="Delete this conversation"
                        onClick={() => void removeRun(run)}
                      >
                        ×
                      </button>
                    </div>
                    <div className="text-muted small mt-1">
                      <StatusBadge tone={stateTone(run.state)} title={run.error ?? undefined}>
                        {run.state}
                      </StatusBadge>{" "}
                      {new Date(run.created_at).toLocaleString()}
                    </div>
                  </div>
                ))}
              </div>
            </Card>
          </div>
        </div>
      </PageBody>
    </>
  );
}

/** One entry of the transcript. */
function TranscriptEntry({ entry }: { entry: Entry }) {
  const [open, setOpen] = useState(false);

  if (entry.kind === "user") {
    return (
      <div className="mb-3">
        <div className="text-muted small">You</div>
        <div className="text-break" style={{ whiteSpace: "pre-wrap" }}>
          {entry.text}
        </div>
      </div>
    );
  }
  if (entry.kind === "assistant") {
    return (
      <div className="mb-3">
        <div className="text-muted small">Agent</div>
        {entry.reasoning && (
          <details className="text-muted small mb-1">
            <summary>Reasoning</summary>
            <div style={{ whiteSpace: "pre-wrap" }}>{entry.reasoning}</div>
          </details>
        )}
        <div className="text-break" style={{ whiteSpace: "pre-wrap" }}>
          {entry.text}
        </div>
      </div>
    );
  }
  if (entry.kind === "error") {
    // In the transcript, where it happened: a chat that silently stopped would
    // be unfixable by the person watching it.
    return (
      <Alert variant="danger" className="py-2">
        <div className="text-break small mb-0">{entry.message}</div>
      </Alert>
    );
  }
  return (
    <div className="mb-3">
      <button
        type="button"
        className="btn btn-sm btn-outline-secondary"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        {entry.isError ? "⚠ " : ""}
        {entry.name}
        {entry.result === null ? " — running…" : ""}
      </button>
      {open && (
        <div className="mt-2">
          <div className="text-muted small">Arguments</div>
          <pre className="small text-break">{pretty(entry.args)}</pre>
          <div className="text-muted small">Result</div>
          <pre className={`small text-break${entry.isError ? " text-danger" : ""}`}>
            {entry.result === null ? "(still running)" : pretty(entry.result)}
          </pre>
        </div>
      )}
    </div>
  );
}
