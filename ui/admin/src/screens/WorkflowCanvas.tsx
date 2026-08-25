// The canvas: React Flow rendering whatever `workflowGraph.ts` computed, and
// holding no rules of its own (§10.3, decision 10).
//
// One component, two uses. The editor mounts it with everything on — dragging,
// connecting, selecting — and the run detail screen mounts it **read-only** with
// a `RunPath` marking the nodes and edges a run took. That reuse is what the
// model/renderer split was for: "the same canvas with the path drawn on it" is a
// prop, not a second renderer.
//
// Why React Flow (decision 9): it is the library §12's crate tree already names,
// and the surveyed alternatives lose on the same axis — `rete.js` is a node
// *engine* with an execution model we would have to ignore, `litegraph.js` and
// `Drawflow` are imperative DOM rather than React, and the JointJS/GoJS class is
// commercial. Its inline node transforms are the reason the admin CSP carries
// `style-src 'unsafe-inline'` — which it already did, for Monaco, so nothing was
// relaxed to admit this.

import { useCallback, useMemo } from "react";
import {
  Background,
  Controls,
  Handle,
  MiniMap,
  Position,
  ReactFlow,
  type Connection,
  type Edge,
  type EdgeChange,
  type Node,
  type NodeChange,
  type NodeProps,
} from "@xyflow/react";

import {
  KIND_INFO,
  MARKER_HEIGHT,
  MARKER_WIDTH,
  NODE_HEIGHT,
  NODE_WIDTH,
  type Graph,
  type RunPath,
  type StepKindName,
} from "../workflowGraph";

/** Where each node sits, kept by the editor so a re-render does not undo a drag. */
export type Positions = Record<string, { x: number; y: number }>;

/** What a step's node needs to draw itself. React Flow requires node data to be
 * an index signature, so this is flat values rather than the `Step` itself. */
type StepNodeData = {
  label: string;
  summary: string;
  kind: StepKindName;
  start: boolean;
  /** `for_each` steps get a second source handle for the loop body. */
  loop: boolean;
  /** Something is wrong with this step, per `validate`. */
  problem?: string;
  /** Where a run got to, when one is being drawn over this graph. */
  ran?: "visited" | "current" | "failed";
  [key: string]: unknown;
};

/** A computed marker: the thing a `Next::Formula` points at, carrying the
 * formula so the graph says what it computes without opening the inspector. */
type MarkerNodeData = { summary: string; [key: string]: unknown };

export function WorkflowCanvas({
  graph,
  positions,
  readOnly = false,
  selected,
  issues,
  path,
  onSelect,
  onMove,
  onConnect,
  onDeleteEdges,
}: {
  graph: Graph;
  positions: Positions;
  readOnly?: boolean;
  selected?: string | null;
  /** Problems by step name, so a broken step is marked where it is. */
  issues?: Record<string, string>;
  /** The run to draw over this graph, on the read-only canvas. */
  path?: RunPath | null;
  onSelect?: (id: string | null) => void;
  onMove?: (id: string, at: { x: number; y: number }) => void;
  onConnect?: (source: string, target: string, handle: string | null) => void;
  onDeleteEdges?: (ids: string[]) => void;
}) {
  const nodes: Node[] = useMemo(
    () =>
      graph.nodes.map((node) => {
        const at = positions[node.id] ?? node.position;
        if (node.type === "computed") {
          const data: MarkerNodeData = { summary: node.data.summary };
          return {
            id: node.id,
            type: "computed",
            position: at,
            data,
            draggable: !readOnly,
            selectable: false,
          };
        }
        const kind = (node.data.step?.kind.type ?? "set") as StepKindName;
        const data: StepNodeData = {
          label: node.data.label,
          summary: node.data.summary,
          kind,
          start: node.data.start,
          loop: kind === "for_each",
          problem: issues?.[node.id],
          ran: ranState(node.id, path),
        };
        return {
          id: node.id,
          type: "step",
          position: at,
          data,
          selected: node.id === selected,
          draggable: !readOnly,
          // Not by the Delete key, and deliberately: React Flow deletes a node
          // and its edges in one gesture, which this canvas would have to apply
          // as two separate edits — and a *refused* delete (something points at
          // this step) would still have taken the edges with it. Deleting a step
          // is the inspector's button, which is also where the refusal naming
          // the steps that point at it belongs.
          deletable: false,
        };
      }),
    [graph, positions, readOnly, selected, issues, path],
  );

  const edges: Edge[] = useMemo(
    () =>
      graph.edges.map((e) => {
        const taken = path?.edges.includes(e.id) ?? false;
        return {
          id: e.id,
          source: e.source,
          target: e.target,
          sourceHandle: e.data.role === "body" ? "body" : "next",
          label: e.label,
          // Classes rather than inline `style`, so the look lives in `admin.css`
          // with the rest of the theme and travels with light/dark.
          className: [
            e.dashed ? "wf-edge-dashed" : "",
            taken ? "wf-edge-taken" : "",
            path && !taken ? "wf-edge-untaken" : "",
          ]
            .filter(Boolean)
            .join(" "),
          animated: taken && path?.current === e.target,
          deletable: !readOnly,
        };
      }),
    [graph, path, readOnly],
  );

  const onNodesChange = useCallback(
    (changes: NodeChange[]) => {
      for (const change of changes) {
        if (change.type === "position" && change.position) {
          onMove?.(change.id, change.position);
        } else if (change.type === "select") {
          if (change.selected) onSelect?.(change.id);
        }
      }
    },
    [onMove, onSelect],
  );

  const onEdgesChange = useCallback(
    (changes: EdgeChange[]) => {
      const removed = changes.filter((c) => c.type === "remove").map((c) => c.id);
      if (removed.length > 0) onDeleteEdges?.(removed);
    },
    [onDeleteEdges],
  );

  const connect = useCallback(
    (connection: Connection) => {
      if (connection.source && connection.target) {
        onConnect?.(connection.source, connection.target, connection.sourceHandle ?? null);
      }
    },
    [onConnect],
  );

  return (
    <div className="workflow-canvas">
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={NODE_TYPES}
        onNodesChange={readOnly ? undefined : onNodesChange}
        onEdgesChange={readOnly ? undefined : onEdgesChange}
        onConnect={readOnly ? undefined : connect}
        onPaneClick={() => onSelect?.(null)}
        nodesConnectable={!readOnly}
        nodesDraggable={!readOnly}
        elementsSelectable={!readOnly}
        // The graph is laid out by dagre on demand rather than fitted on every
        // change: a canvas that re-frames itself while an admin is dragging is
        // a canvas that moves under their hand.
        fitView
        proOptions={{ hideAttribution: false }}
      >
        <Background />
        <Controls showInteractive={false} />
        <MiniMap pannable zoomable />
      </ReactFlow>
    </div>
  );
}

/** Whether a run has been on this node, is on it now, or failed on it. */
function ranState(id: string, path?: RunPath | null): StepNodeData["ran"] {
  if (!path) return undefined;
  if (path.failed === id) return "failed";
  if (path.current === id) return "current";
  return path.visited.includes(id) ? "visited" : undefined;
}

/** One step, drawn as a card whose colour is its kind. */
function StepNode({ data, selected }: NodeProps) {
  const step = data as StepNodeData;
  const info = KIND_INFO[step.kind];
  const classes = [
    "wf-node",
    `wf-node-${step.kind}`,
    selected ? "wf-node-selected" : "",
    step.problem ? "wf-node-problem" : "",
    step.ran ? `wf-node-${step.ran}` : "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <div className={classes} style={{ width: NODE_WIDTH, height: NODE_HEIGHT }}>
      <Handle type="target" position={Position.Top} />
      <div className="wf-node-head">
        <span className={`wf-node-kind badge bg-${info.tone}-lt`}>{info.label}</span>
        {step.start && <span className="badge bg-green-lt">start</span>}
        {step.problem && (
          <span className="badge bg-red-lt" title={step.problem}>
            !
          </span>
        )}
      </div>
      <div className="wf-node-name">{step.label}</div>
      <div className="wf-node-summary" title={step.summary}>
        {step.summary}
      </div>
      <Handle type="source" id="next" position={Position.Bottom} />
      {/* A loop's body hangs off the side, so "what this step does per item" and
          "what happens after the loop" are two edges an admin can tell apart. */}
      {step.loop && (
        <Handle type="source" id="body" position={Position.Right} className="wf-handle-body" />
      )}
    </div>
  );
}

/** The marker a computed `next` points at: not a step, and drawn so nobody
 * mistakes it for one. */
function ComputedNode({ data }: NodeProps) {
  const marker = data as MarkerNodeData;
  return (
    <div
      className="wf-marker"
      style={{ width: MARKER_WIDTH, height: MARKER_HEIGHT }}
      title={marker.summary}
    >
      <Handle type="target" position={Position.Top} />
      computed
    </div>
  );
}

const NODE_TYPES = { step: StepNode, computed: ComputedNode };
