// Observe: watching a stream's elements arrive, live (TODO "Streams", task
// 7.3, §9).
//
// The one admin screen in this tree whose content is not a read. There is no
// "the elements" to `GET` — an element is not stored (§4) — so this is a socket
// that is handed each element as it arrives, and everything on the screen
// follows from that:
//
//   - **Pause and Clear are client-side only.** Pausing the server would mean
//     either dropping the elements or queueing them, and §7 says which of those
//     a stream does — so Pause stops the *rendering* and says how many arrived
//     while it was held, rather than pretending the flow waited.
//   - **The history says what it is.** The socket replays the last hundred
//     envelopes *this process* saw, and those rows are labelled "since this
//     server started", which is the whole truth: there is no element table and
//     there is not going to be one.
//   - **A lag is shown, never hidden.** A consumer that cannot keep up is the
//     consumer's problem (§7), and a gap this screen names beats a gap it
//     silently draws.
//
// The rendering follows the declared element type, which is what declaring one
// is for: a table of the declared keys for `json`, a text tail for `text`, and
// a hex head for `binary` — never bytes set in a body font.

import { useEffect, useMemo, useRef, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import { IconArrowLeft, IconPlayerStop, IconTrash } from "../icons";
import { PageBody, PageHeader, StatusBadge } from "../layout";
import {
  applyFrame,
  cellText,
  counterNotes,
  elementCells,
  elementColumns,
  elementText,
  elementTypeSummary,
  emptyObserve,
  extraKeys,
  formatCount,
  formatTime,
  hexHead,
  parseFrame,
  statusLabel,
  streamObserveUrl,
  type ElementType,
  type Envelope,
  type ObserveState,
} from "../streams";

export function StreamObserve({ streamId }: { streamId: string }) {
  const [state, setState] = useState<ObserveState>(emptyObserve());
  const [name, setName] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [paused, setPaused] = useState(false);
  /** Elements that arrived while paused — counted, because the flow did not
   * wait and saying nothing would be claiming it did. */
  const [held, setHeld] = useState(0);
  /** The newest envelope at the moment Clear was pressed: everything from it
   * down is hidden. A *mark* rather than a count, because the tail is bounded
   * and a count would go stale the moment the far end started falling off —
   * and a mark that falls off simply stops matching, which shows everything
   * again, which is the right answer for a tail that has turned over
   * completely since it was cleared. */
  const [clearMark, setClearMark] = useState<Envelope | null>(null);

  // The row, for the heading and the "disabled" case. One read: a definition
  // does not move while a socket is open, and what does move arrives on it.
  useEffect(() => {
    let cancelled = false;
    void api
      .getStream(streamId)
      .then((stream) => {
        if (cancelled) return;
        setName(stream.name);
        setEnabled(stream.enabled);
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setState((s) => ({ ...s, error: errorMessage(err, "That stream no longer exists.") }));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [streamId]);

  // `paused` is read inside the socket's message handler, which is installed
  // once; a ref is what lets the handler see the current value without the
  // socket being torn down and reconnected on every pause.
  const pausedRef = useRef(paused);
  pausedRef.current = paused;

  useEffect(() => {
    const socket = new WebSocket(streamObserveUrl(window.location, streamId));
    socket.onmessage = (event: MessageEvent) => {
      const frame = parseFrame(typeof event.data === "string" ? event.data : "");
      if (!frame) return;
      if (frame.type === "element" && pausedRef.current) {
        setHeld((n) => n + 1);
        return;
      }
      setState((current) => applyFrame(current, frame));
    };
    socket.onerror = () => {
      setState((s) => ({ ...s, error: "The connection to the server failed." }));
    };
    socket.onclose = (event: CloseEvent) => {
      // The server closes with a *reason* rather than a status once the
      // handshake has succeeded — "this stream is not running on this server"
      // — and that sentence is the answer to what the screen is showing
      // nothing for.
      setState((s) => ({
        ...s,
        error: event.reason || (s.ready ? "The connection closed." : null),
      }));
    };
    return () => socket.close();
  }, [streamId]);

  const badge = statusLabel(state.status, enabled);
  const notes = counterNotes(state.counters);
  const tail = useMemo(() => {
    if (!clearMark) return state.tail;
    const cut = state.tail.indexOf(clearMark);
    return cut === -1 ? state.tail : state.tail.slice(0, cut);
  }, [state.tail, clearMark]);
  /** Where the replay starts in the *whole* tail — the visible one is a prefix
   * of it, so the index means the same thing in both. */
  const historyFrom = state.tail.length - state.replayed;

  return (
    <>
      <PageHeader
        pretitle="Stream"
        title={name || "Observe"}
        actions={
          <div className="btn-list">
            <Button
              variant={paused ? "primary" : "outline-secondary"}
              onClick={() => {
                setPaused((p) => !p);
                setHeld(0);
              }}
            >
              <IconPlayerStop className="icon-2" />
              {paused ? "Resume" : "Pause"}
            </Button>
            <Button
              variant="outline-secondary"
              onClick={() => {
                setClearMark(state.tail[0] ?? null);
                setHeld(0);
              }}
            >
              <IconTrash className="icon-2" />
              Clear
            </Button>
            <Button variant="outline-secondary" href="#/streams">
              <IconArrowLeft className="icon-2" />
              Streams
            </Button>
          </div>
        }
      />
      <PageBody>
        {state.error && <Alert variant="danger">{state.error}</Alert>}

        <div className="card mb-3">
          <div className="card-body d-flex flex-wrap gap-3 align-items-center">
            <StatusBadge tone={badge.tone} title={badge.title}>
              {badge.label}
            </StatusBadge>
            <span className="text-muted">
              {elementTypeSummary(state.elementType)}
            </span>
            <span className="text-muted">
              {formatCount(state.counters.elements)} elements since this server started
            </span>
            {notes.length > 0 && <span className="text-warning">{notes.join(" · ")}</span>}
          </div>
        </div>

        {/* §7: told, never hidden. */}
        {state.lagged > 0 && (
          <Alert variant="warning">
            This screen fell behind and lost {formatCount(state.lagged)} elements. Nothing
            back-pressures a stream: a consumer that cannot keep up is skipped rather than made to
            wait, and the elements that were skipped are gone.
          </Alert>
        )}
        {paused && (
          <Alert variant="info">
            Paused — {formatCount(held)} elements have arrived and are not shown. Pausing stops
            this screen, not the stream.
          </Alert>
        )}

        {state.ready && tail.length === 0 && !state.error && (
          <Alert variant="info">
            Nothing yet. Elements appear here as they arrive; there is no history beyond what this
            server has seen since it started, because an element is not stored.
          </Alert>
        )}

        {tail.length > 0 && (
          <div className="card">
            <ElementTail
              tail={tail}
              type={state.elementType}
              isHistory={(index) => index >= historyFrom}
            />
          </div>
        )}
      </PageBody>
    </>
  );
}

/** The tail, rendered against the declared element type. */
function ElementTail({
  tail,
  type,
  isHistory,
}: {
  tail: Envelope[];
  type: ElementType | null;
  isHistory: (index: number) => boolean;
}) {
  const columns = elementColumns(type);
  return (
    <Table hover responsive className="card-table table-vcenter">
      <thead>
        <tr>
          <th>Received</th>
          {type?.kind === "json" ? (
            columns.map((column) => <th key={column}>{column}</th>)
          ) : (
            <th>Value</th>
          )}
          <th>Source</th>
        </tr>
      </thead>
      <tbody>
        {tail.map((envelope, index) => (
          <tr key={`${envelope.received_at}-${index}`} className={isHistory(index) ? "text-muted" : undefined}>
            <td className="text-nowrap">
              {formatTime(envelope.received_at)}
              {/* The whole truth about what this row is (§9). */}
              {isHistory(index) && (
                <div className="small">since this server started</div>
              )}
            </td>
            <ValueCells envelope={envelope} type={type} columns={columns} />
            <td className="small font-monospace text-muted">
              {envelope.source === undefined ? "—" : cellText(envelope.source)}
            </td>
          </tr>
        ))}
      </tbody>
    </Table>
  );
}

/** One element's value, in as many cells as its type asks for. */
function ValueCells({
  envelope,
  type,
  columns,
}: {
  envelope: Envelope;
  type: ElementType | null;
  columns: string[];
}) {
  if (type?.kind === "json") {
    const extra = extraKeys(envelope, type);
    return (
      <>
        {elementCells(envelope, type).map((cell, index) => (
          <td key={columns[index]} className="font-monospace small">
            {cell}
            {/* Undeclared keys are carried through rather than dropped (§4);
                naming them on the first column is what tells an admin there is
                something to add to the declaration. */}
            {index === 0 && extra.length > 0 && (
              <div className="text-muted">+ {extra.join(", ")}</div>
            )}
          </td>
        ))}
      </>
    );
  }
  if (type?.kind === "binary") {
    const value = typeof envelope.value === "string" ? envelope.value : "";
    const head = hexHead(value);
    return (
      <td className="font-monospace small">
        {head.length === null ? "—" : `${head.hex}  (${formatCount(head.length)} bytes)`}
      </td>
    );
  }
  return <td className="font-monospace small text-break">{elementText(envelope)}</td>;
}
