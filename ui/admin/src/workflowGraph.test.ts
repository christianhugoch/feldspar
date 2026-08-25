/**
 * The workflow editor's model (§10.3, decision 10).
 *
 * The thing worth pinning down is the **round trip**: an admin opens a workflow
 * on a canvas, moves one node, and saves. If the trip through nodes and edges
 * loses a description, a retry's backoff or a formula's text, the save is a
 * silent edit of a program nobody asked to change — and nothing in the build
 * would notice. So the property here is over a workflow using every step kind
 * and every `Next` variant, mirroring the Rust fixture (`every_kind`) that the
 * serde round-trip is asserted over, because those two round trips are the two
 * halves of "the stored JSON is the editor's shape".
 *
 * The rest is what the admin sees before they can save (`validate`), and what
 * the run detail screen draws on the same canvas (`runPath`).
 */

import { describe, expect, it } from "vitest";

import {
  computedNodeId,
  graphToSteps,
  layout,
  newStep,
  referencesTo,
  replaceStep,
  runPath,
  stepSummary,
  stepsToGraph,
  uniqueStepName,
  validate,
  type Step,
  type Workflow,
} from "./workflowGraph";

/**
 * A workflow using **every** step kind and every `Next` variant — the mirror of
 * `sc_workflow`'s `every_kind()` fixture, so the two round trips are asserted
 * over the same program.
 */
function everyKind(): Workflow {
  const steps: Step[] = [
    {
      name: "fetch",
      description: "ask the supplier",
      kind: {
        type: "action",
        action: "fetch",
        configuration: { url: "https://example.com" },
      },
      next: { type: "step", step: "total" },
      error_policy: {
        type: "retry",
        max: 3,
        backoff: { initial_ms: 1000, factor: 2, max_ms: 60000, jitter: true },
      },
    },
    {
      name: "total",
      kind: { type: "set", assignments: [{ target: "total", formula: "context.fetch.amount" }] },
      next: {
        type: "branch",
        arms: [{ when: "context.total > 100", step: "approve" }],
        otherwise: "lines",
      },
    },
    {
      name: "approve",
      kind: {
        type: "user_form",
        fields: [
          { name: "approved", type: "bool", required: true },
          { name: "note", type: "text" },
        ],
        assign_to: "approval",
        min_role: 40,
        timeout: "86400000",
      },
      next: { type: "step", step: "lines" },
    },
    {
      name: "lines",
      kind: { type: "for_each", over: "context.fetch.lines", var: "line", body: "pause" },
      next: { type: "formula", formula: "context.total > 0 ? 'pause' : null" },
    },
    {
      name: "pause",
      kind: { type: "wait", until: "1000" },
      next: { type: "end" },
      error_policy: { type: "handler", step: "fetch" },
    },
  ];
  return {
    id: "00000000-0000-0000-0000-000000000001",
    version: 3,
    start: "fetch",
    steps,
    error_policy: { type: "handler", step: "pause" },
    trace: true,
    max_steps: 1000,
  };
}

describe("steps ⇄ graph", () => {
  it("round-trips a workflow using every step kind and every next", () => {
    const workflow = everyKind();
    const back = graphToSteps(stepsToGraph(workflow), workflow);
    expect(back).toEqual(workflow);
  });

  it("keeps what the canvas does not model", () => {
    // The three things a canvas has nowhere to draw and every reason to lose:
    // an admin's description, a retry's backoff, and a computed `next`'s source.
    const workflow = everyKind();
    const back = graphToSteps(stepsToGraph(workflow), workflow);
    expect(back.steps[0].description).toBe("ask the supplier");
    expect(back.steps[0].error_policy).toEqual(workflow.steps[0].error_policy);
    expect(back.steps[3].next).toEqual({
      type: "formula",
      formula: "context.total > 0 ? 'pause' : null",
    });
    // …and the flags that are the workflow's rather than any step's.
    expect(back.trace).toBe(true);
    expect(back.max_steps).toBe(1000);
    expect(back.error_policy).toEqual({ type: "handler", step: "pause" });
  });

  it("draws one edge per branch arm, labelled with its guard", () => {
    const { edges } = stepsToGraph(everyKind());
    const fromTotal = edges.filter((e) => e.source === "total");
    expect(fromTotal.map((e) => [e.target, e.label])).toEqual([
      ["approve", "context.total > 100"],
      ["lines", "otherwise"],
    ]);
    expect(fromTotal[0].data).toEqual({
      role: "arm",
      index: 0,
      when: "context.total > 100",
    });
  });

  it("draws a computed next as one dashed edge to a marker, not as edges to steps", () => {
    const { nodes, edges } = stepsToGraph(everyKind());
    const marker = nodes.find((n) => n.id === computedNodeId("lines"));
    expect(marker?.type).toBe("computed");
    // The marker carries no step, so nothing about it can be saved back as one.
    expect(marker?.data.step).toBeUndefined();
    const computed = edges.filter((e) => e.source === "lines" && e.data.role === "formula");
    expect(computed).toHaveLength(1);
    expect(computed[0].dashed).toBe(true);
    expect(computed[0].target).toBe(computedNodeId("lines"));
    // `pause` is named *inside* the formula and must not be drawn as an edge —
    // which step a formula reaches is the run's answer, not the editor's.
    expect(edges.some((e) => e.source === "lines" && e.target === "pause" && e.data.role === "next"))
      .toBe(false);
  });

  it("draws a loop's body and a step's error handler, and reads them back", () => {
    const workflow = everyKind();
    const { edges } = stepsToGraph(workflow);
    expect(edges.find((e) => e.source === "lines" && e.data.role === "body")?.target).toBe("pause");
    const handler = edges.find((e) => e.data.role === "handler");
    expect([handler?.source, handler?.target, handler?.dashed]).toEqual(["pause", "fetch", true]);

    // Cutting the body edge is an edit, not a no-op: the body is emptied, and
    // `validate` is what tells the admin so.
    const graph = stepsToGraph(workflow);
    const cut = { ...graph, edges: graph.edges.filter((e) => e.data.role !== "body") };
    const back = graphToSteps(cut, workflow);
    const lines = back.steps.find((s) => s.name === "lines");
    expect(lines?.kind).toEqual({ type: "for_each", over: "context.fetch.lines", var: "line", body: "" });
    expect(validate(back).map((i) => i.problem)).toContain("`lines`: the loop has no body.");
  });

  it("reads a dragged edge back as the control flow it draws", () => {
    const workflow = everyKind();
    const graph = stepsToGraph(workflow);
    // The admin drags `pause`'s (absent) next onto `total`.
    const edges = [
      ...graph.edges,
      {
        id: "pause→next→total",
        source: "pause",
        target: "total",
        label: "",
        dashed: false,
        data: { role: "next" as const },
      },
    ];
    const back = graphToSteps({ ...graph, edges }, workflow);
    expect(back.steps.find((s) => s.name === "pause")?.next).toEqual({
      type: "step",
      step: "total",
    });
  });

  it("keeps the start step, and moves it when the one it named is gone", () => {
    const workflow = everyKind();
    const graph = stepsToGraph(workflow);
    expect(graphToSteps(graph, workflow).start).toBe("fetch");

    const withoutStart = {
      ...graph,
      nodes: graph.nodes.filter((n) => n.id !== "fetch"),
    };
    expect(graphToSteps(withoutStart, workflow).start).toBe("total");
  });

  it("summarises a step in one line, per kind", () => {
    const workflow = everyKind();
    expect(workflow.steps.map(stepSummary)).toEqual([
      "fetch",
      "total",
      "2 fields → approval",
      "line of context.fetch.lines",
      "until 1000",
    ]);
  });
});

describe("layout", () => {
  it("places every node without overlapping, top to bottom", () => {
    const graph = layout(stepsToGraph(everyKind()));
    const placed = new Map(graph.nodes.map((n) => [n.id, n.position]));
    // Every node got a position…
    expect(placed.size).toBe(graph.nodes.length);
    // …the start is above what follows it…
    expect(placed.get("fetch")!.y).toBeLessThan(placed.get("total")!.y);
    // …and no two nodes share one.
    const spots = new Set(graph.nodes.map((n) => `${n.position.x},${n.position.y}`));
    expect(spots.size).toBe(graph.nodes.length);
  });

  it("survives an edge to a step that does not exist", () => {
    // Which is exactly the state an admin is in mid-edit, and dagre would
    // otherwise invent a node for the missing target and place a phantom.
    const workflow = everyKind();
    const broken = replaceStep(workflow, "pause", {
      ...workflow.steps[4],
      next: { type: "step", step: "gone" },
    });
    const graph = layout(stepsToGraph(broken));
    expect(graph.nodes.some((n) => n.id === "gone")).toBe(false);
  });
});

describe("validate", () => {
  it("passes a workflow that is right", () => {
    expect(validate(everyKind(), ["fetch"])).toEqual([]);
  });

  it("reports every problem, not the first", () => {
    const workflow: Workflow = {
      id: "x",
      version: 1,
      start: "nowhere",
      steps: [
        { name: "a", kind: { type: "action", action: "" }, next: { type: "step", step: "gone" } },
        { name: "b", kind: { type: "wait", until: "" }, next: { type: "end" } },
      ],
    };
    const problems = validate(workflow).map((i) => i.problem);
    expect(problems).toContain("The start step `nowhere` does not exist.");
    expect(problems).toContain("`a` continues to `gone`, which does not exist.");
    expect(problems).toContain("`a`: no action is chosen.");
    expect(problems).toContain("`b`: no deadline to wait until.");
    // Three broken steps are three markers on three nodes: each issue names the
    // step it is about, so the canvas can mark it.
    expect(validate(workflow).filter((i) => i.step === "a")).toHaveLength(2);
  });

  it("names a step nothing reaches", () => {
    const workflow: Workflow = {
      id: "x",
      version: 1,
      start: "a",
      steps: [
        { name: "a", kind: { type: "set", assignments: [] }, next: { type: "end" } },
        { name: "orphan", kind: { type: "set", assignments: [] }, next: { type: "end" } },
      ],
    };
    expect(validate(workflow)).toEqual([
      { step: "orphan", problem: "Nothing reaches `orphan`." },
    ]);
  });

  it("stands down about reachability when a computed next is reachable", () => {
    // Which step a formula goes to is the run's answer; marking a good step dead
    // because the browser cannot read JavaScript is worse than saying nothing.
    const workflow: Workflow = {
      id: "x",
      version: 1,
      start: "a",
      steps: [
        {
          name: "a",
          kind: { type: "set", assignments: [] },
          next: { type: "formula", formula: "'b'" },
        },
        { name: "b", kind: { type: "set", assignments: [] }, next: { type: "end" } },
      ],
    };
    expect(validate(workflow)).toEqual([]);
  });

  it("checks an action name against the palette it was offered from", () => {
    const workflow = everyKind();
    expect(validate(workflow, ["send_email"]).map((i) => i.problem)).toContain(
      "`fetch`: there is no action named `fetch`.",
    );
    // With no list, the browser says nothing — the server holds the registry.
    expect(validate(workflow)).toEqual([]);
  });

  it("names a handler that does not exist, per step and for the workflow", () => {
    const workflow = everyKind();
    const broken: Workflow = {
      ...workflow,
      error_policy: { type: "handler", step: "gone" },
      steps: workflow.steps.map((s) =>
        s.name === "pause" ? { ...s, error_policy: { type: "handler" as const, step: "also_gone" } } : s,
      ),
    };
    const problems = validate(broken).map((i) => i.problem);
    expect(problems).toContain(
      "The workflow handles errors with `gone`, which does not exist.",
    );
    expect(problems).toContain(
      "`pause` handles errors with `also_gone`, which does not exist.",
    );
  });
});

describe("editing", () => {
  it("names a dropped step so it is something a next can point at", () => {
    expect(uniqueStepName("action", [])).toBe("action");
    expect(uniqueStepName("action", ["action"])).toBe("action_2");
    expect(uniqueStepName("action", ["action", "action_2"])).toBe("action_3");
  });

  it("opens a new step empty and ending the run", () => {
    expect(newStep("for_each", "loop")).toEqual({
      name: "loop",
      kind: { type: "for_each", over: "", var: "item", body: "" },
      next: { type: "end" },
    });
  });

  it("names the steps that point at one, which is what refuses a delete", () => {
    const workflow = everyKind();
    // `pause` is a loop's body, a computed next's *unreadable* target, and an
    // error handler's source — the body and the workflow policy are what count.
    expect(referencesTo(workflow, "pause").sort()).toEqual(["lines"]);
    expect(referencesTo(workflow, "lines").sort()).toEqual(["approve", "total"]);
    expect(referencesTo(workflow, "fetch")).toEqual(["pause"]);
  });

  it("repoints everything at a renamed step, including the start", () => {
    const workflow = everyKind();
    const renamed = replaceStep(workflow, "fetch", {
      ...workflow.steps[0],
      name: "ask_supplier",
    });
    expect(renamed.start).toBe("ask_supplier");
    // The step that handled its errors by jumping to it follows the rename…
    expect(renamed.steps.find((s) => s.name === "pause")?.error_policy).toEqual({
      type: "handler",
      step: "ask_supplier",
    });
    // …and nothing still points at the old name.
    expect(validate(renamed)).toEqual([]);
  });
});

describe("runPath", () => {
  const workflow = everyKind();
  const graph = stepsToGraph(workflow);

  it("marks the nodes a run has been on and the edges it took", () => {
    const path = runPath(
      graph,
      [
        { seq: 1, step: "fetch", outcome: "ok" },
        { seq: 2, step: "total", outcome: "ok" },
        { seq: 3, step: "approve", outcome: "suspended" },
      ],
      "approve",
    );
    expect(path.visited).toEqual(["fetch", "total", "approve"]);
    expect(path.current).toBe("approve");
    expect(path.failed).toBeNull();
    // The branch arm that was taken, and not the `otherwise` beside it.
    expect(path.edges).toContain("total→arm0→approve");
    expect(path.edges).not.toContain("total→otherwise→lines");
  });

  it("draws a retry as one node and no self-loop", () => {
    // The workflow has no edge from `fetch` to itself; a retried step must not
    // invent one.
    const path = runPath(graph, [
      { seq: 1, step: "fetch", outcome: "error" },
      { seq: 2, step: "fetch", outcome: "ok" },
    ]);
    expect(path.visited).toEqual(["fetch"]);
    expect(path.edges).toEqual([]);
    expect(path.failed).toBe("fetch");
  });

  it("lights the error handler's edge only on the failure that took it", () => {
    const path = runPath(graph, [
      { seq: 1, step: "pause", outcome: "error" },
      { seq: 2, step: "fetch", outcome: "ok" },
    ]);
    expect(path.edges).toEqual(["pause→handler→fetch"]);
    expect(path.failed).toBe("pause");
  });

  it("falls back to the last step it traced when nothing says where it is", () => {
    const path = runPath(graph, [{ seq: 1, step: "fetch", outcome: "ok" }], null);
    expect(path.current).toBe("fetch");
  });

  it("includes a current step the trace has not reached yet", () => {
    // Tracing is off on most workflows, so the current step is often all there
    // is — and the canvas still has to mark it.
    const path = runPath(graph, [], "total");
    expect(path.visited).toEqual(["total"]);
    expect(path.current).toBe("total");
  });
});
