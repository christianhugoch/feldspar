# Saltcorn v2 — Workflows

Ordered, checkable task list for the eighteenth milestone after the MVP. Earlier lists are
archived in [docs/TODO-mvp.md](./docs/TODO-mvp.md) (the MVP),
[docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md) (file stores + the React framework),
[docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md) (the `_sc_tables`/`_sc_fields` overlays,
rich types and File fields), [docs/TODO-post-mvp-3.md](./docs/TODO-post-mvp-3.md) (ownership
formulae, calculated fields and row-level security),
[docs/TODO-post-mvp-4.md](./docs/TODO-post-mvp-4.md) (actions and triggers),
[docs/TODO-post-mvp-5.md](./docs/TODO-post-mvp-5.md) (the file-store IDE),
[docs/TODO-post-mvp-6.md](./docs/TODO-post-mvp-6.md) (agents),
[docs/TODO-post-mvp-7.md](./docs/TODO-post-mvp-7.md) (the GraphQL provider),
[docs/TODO-post-mvp-8.md](./docs/TODO-post-mvp-8.md) (REST queries, custom SQL and the
generated client), [docs/TODO-post-mvp-9.md](./docs/TODO-post-mvp-9.md) (table constraints
and indexes), [docs/TODO-post-mvp-10.md](./docs/TODO-post-mvp-10.md) (email),
[docs/TODO-post-mvp-11.md](./docs/TODO-post-mvp-11.md) (tables in code),
[docs/TODO-post-mvp-12.md](./docs/TODO-post-mvp-12.md) (concurrent code bodies),
[docs/TODO-post-mvp-13.md](./docs/TODO-post-mvp-13.md) (modules),
[docs/TODO-post-mvp-14.md](./docs/TODO-post-mvp-14.md) (SQLite),
[docs/TODO-post-mvp-15.md](./docs/TODO-post-mvp-15.md) (modules in-process),
[docs/TODO-post-mvp-16.md](./docs/TODO-post-mvp-16.md) (table providers) and
[docs/TODO-post-mvp-17.md](./docs/TODO-post-mvp-17.md) (writable table providers). Scope and
rationale remain in [docs/GOALS.md](./docs/GOALS.md) and
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md) (**§10.3**, which this milestone
rewrites).

This is the milestone GOALS is most emphatic about:

> ensure that the workflow engine matches modern workflow engines for durability and error
> handling … workflow execution code is a mess in saltcorn v1. This needs to be much cleaner.
> The number of built-in workflow actions should be minimal.

Everything the engine needs beneath it already exists. The event model, the trigger record, the
registry, the fire path and its cascade bound landed with actions and triggers (§10.2); the
`_sc_runs` table, the sans-IO steppable loop and the "persist after every step" discipline
landed with agents (§11.4), with a `RunKind::Workflow` variant nothing has produced yet. What is
missing is the body — a program rather than one action — the engine that advances it durably,
and the screen an admin draws it on.

**Milestone definition of done:** an admin creates a trigger whose body is a **workflow**, draws
it in the browser — steps as nodes, control flow as edges, each step's settings rendered from the
action's own declaration — and saves. An insert on `orders` starts a **run**. The run branches on
the order's value, loops over the order's lines, calls an agent, sends an email, and then
**suspends** waiting for a human to approve it. The server is restarted while it waits. The
approval arrives the next day through a form the admin UI renders from the step's own
declaration, and the run carries on **on the version of the workflow it started with**, even
though the workflow has been edited twice since. A step whose HTTP call fails is **retried three
times with exponential backoff** and then jumps to the workflow's error-handling step, and the
run's **trace** shows the context after every step, with the path taken highlighted on the same
graph the admin drew.

**Not in this milestone:** the end-user-facing presentation of a running workflow — v1's
`WorkflowRoom` chat view and its modal popups (§13's viewpatterns do not exist yet), so a
suspended run is resumed from the admin UI and through the API. Nor the copilot trait that
*builds* workflows (§11.6), nor a `sc-bus` crate: see decisions 5 and 12.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## Decisions taken up front

1. **A workflow is a trigger body, not a new top-level entity.** GOALS says "every workflow is a
   trigger", and §10.2 shaped the record for it: `action` + `configuration` become one variant of
   a `TriggerBody`, and the other is `Workflow`. So a workflow inherits — with no new code — its
   event, its `only_if`, its `min_role`, its enabled flag, its periodic timing, its exposure
   through an application's `POST {mount}/actions/{name}`, and the admin's Run button. The
   alternative (a `_sc_workflows` table with its own event model) would duplicate every one of
   those and then have to keep the two in step.
2. **Versions are rows, and a run pins one.** `_sc_workflow_versions` holds `(workflow_id,
   version, steps, error_policy, created_at)` and is **append-only**: saving an edited workflow
   mints `version + 1` rather than rewriting. A run stores the version it started on and loads
   that one for its whole life, which is what GOALS' "a suspended run can finish with its version
   of the workflow" means. Steps in the trigger's `attributes` would be smaller and would make
   the sentence unimplementable.
3. **Control flow is data, with a formula escape hatch — not a formula string.** v1 spells
   `next_step` as a JavaScript expression over the step names, and it is expressive and
   impossible to draw: a visual editor cannot round-trip an arbitrary expression into edges.
   `Next` is therefore an enum — `Step(name)` · `Branch { arms: [(Formula, name)], otherwise }` ·
   `Formula(f)` · `End` — where the first two are exactly what the canvas draws and edits, and
   the third keeps v1's power for the case that needs it (drawn as one dashed edge to a
   "computed" marker, editable as text, never silently rewritten). All four lower to one
   question the engine asks: *given this context, which step is next?*
4. **The run is a steppable machine, exactly as the agent loop is** (§11.2, decision 8). A
   `WorkflowRun` value owns every decision and performs no IO; a driver asks it what to do,
   does it, and feeds the outcome back. The state *is* what `_sc_runs.context` stores, so
   resuming is a load rather than a reconstruction, and the engine's decisions are testable
   synchronously with no database, no clock and no runtime. `_sc_runs` is one table for both
   kinds, as `RunKind` already promised.
5. **The queue is the runs table, claimed with a lease.** The design says the engine is driven by
   "a durable queue on the bus", and `sc-bus` does not exist. Building one to hold a queue would
   be building the wrong thing first: a durable queue's authority has to be the database anyway,
   or a crashed node loses the runs it was holding. So the runnable set is a query —
   `state IN (running, waiting) AND wake_at <= now AND (lease_until IS NULL OR lease_until <
   now)` — and claiming a run is an `UPDATE … SET lease_until, claimed_by WHERE id = … AND
   lease_until IS NOT DISTINCT FROM <what was read>`, which is correct for two nodes as well as
   one. A `WorkQueue` seam over "wake me when something is runnable" is what the bus will
   implement later (a `NOTIFY`, a Redis subscription); the polling implementation behind it is
   twenty lines and is what ships. **Nothing above the seam knows which it is talking to.**
6. **"Each step runs in one transaction" means the run's advance is one atomic write.** The
   normative sentence in GOALS cannot be honoured literally for every step, and saying so now is
   better than implying it: an HTTP request, an email and an LLM call are not transactional, and
   the row layer's writes are individual statements by design (§6 — a caller-context transaction
   is per statement, because RLS rides on session settings). What the engine *does* guarantee,
   and tests: the context, the cursor, the attempt count and the trace row for a completed step
   commit **together, once**, so a run is never observed half-advanced; and a step is **at least
   once**, so a crash mid-step re-runs that step and no other. Steps SHOULD be idempotent, the
   admin UI says so beside any step that is not, and the deviation is recorded in §10.3 rather
   than left for someone to discover. *(Threading one transaction through a step's row writes is
   a real design change to `CallerContext` and is carried past this milestone.)*
7. **The built-in step set is five kinds**, and the count is a decision GOALS made for us
   ("the number of built-in workflow actions should be minimal"). `Action` (run any registered
   action, which is how `run_js_code`, `send_email`, the row actions and `run_agent` are all
   already workflow steps), `Set` (write formulas into the context — the one thing steps need
   that no action provides), `ForEach` (the explicit loop §10.3 asks for), `Wait` (a durable
   timer) and `UserForm` (suspend for input). Everything else an admin could want is an action,
   and adding an action is not adding a step kind.
8. **The context is ambient, and it is spelled `context`.** A step's formulas — a `Set` value, a
   `Branch` arm, a `ForEach`'s collection, an action's own settings — read the run context
   through a new fieldless `Ambient::Context`, beside `row`, `old`, `user` and `payload`, in
   scope only where there *is* a run. Not bare identifiers: those already mean "the row this
   formula ranges over" (§10.1's `EVENT_SCOPE`), and quietly redefining them inside a workflow
   would make one language mean two things. `ActionContext::context` — the seam §10.1 left for
   exactly this — is what an action reads and writes, and its return value is stored under the
   step's name.
9. **The editor is React Flow (`@xyflow/react` 12, MIT), and we write no canvas.** It is the
   library §12's crate tree already names for the drag-and-drop builder, it is the one every
   comparable product uses, and the surveyed alternatives lose on the same axis: `rete.js` (a
   node *engine* with its own execution model we would have to ignore), `litegraph.js` /
   `Drawflow` (not React, imperative DOM), `elkjs`-plus-SVG by hand (a layout engine is not an
   editor), and the JointJS/GoJS class (commercial). Layout is `@dagrejs/dagre` — synchronous,
   tiny, and enough for a DAG with back-edges. The admin CSP already carries
   `style-src 'unsafe-inline'` for Tabler, which is what React Flow's inline node transforms
   need; **no other relaxation is acceptable**, and a test asserts the policy is unchanged.
10. **The graph model is a tested TypeScript module, the canvas is a dumb renderer.** `steps ⇄
    {nodes, edges}` in both directions, auto-layout, the validation the admin sees before saving,
    and the "which node is this run on" projection all live in `.ts` files with `vitest` tests —
    the repo's established split (`fieldForm.ts`, `constraintForm.ts`, `agentChat.ts`). The
    `.tsx` renders what those return and holds no rules.
11. **A workflow's steps validate on save and again on load**, in one function, exactly as a
    trigger's configuration does: every `Next` names a step that exists, the start step exists,
    every step is reachable, each action resolves and its configuration validates against its own
    `config_spec_for` the trigger's channel, each formula parses and resolves in the scope that
    step will have, and an error-policy handler names a real step. A workflow that fails leaves
    the live set **with its reason kept**, stays listed and editable, and refuses to start a run.
12. **Tests as before.** Rust for `sc-workflow` (the machine synchronously; the driver against a
    fake clock, a fake queue and recording actions), real Postgres for storage, recovery and the
    end-to-end path, `vitest` for the editor's model. No test sleeps for a timer: the clock is a
    parameter, as the scheduler's already is.

---

## The plan

### 1. The model, the storage, and the trigger body

- [x] 1.1 New crate `sc-workflow` at layer 7 (`sc-error`, `sc-types`, `sc-query`, `sc-catalog`,
  `sc-expr`, `sc-action`). Workspace member, `clippy` clean, no new dependency beyond what the
  workspace already builds.
- [x] 1.2 The types of §10.3, revised by decisions 3 and 7: `Workflow { id, version, start,
  steps, error_policy, trace }`, `Step { name, kind, next, error_policy, description }`,
  `StepKind::{ Action { action, configuration }, Set { assignments }, ForEach { over, var, body },
  Wait { until }, UserForm { fields, assign_to, min_role, timeout } }`, `Next`, `ErrorPolicy::{
  Retry { max, backoff }, Handler { step }, Fail }` and `Backoff { initial_ms, factor, max_ms,
  jitter }`. Serde round-trip with a test: the stored JSON is the API shape and the editor's
  shape, and there is no third spelling.
- [x] 1.3 `_sc_workflow_versions` (§9): `id` (uuid pk), `workflow` (the trigger's id),
  `version` (int), `description`, `steps` (json), `attributes`, `created_at`, `created_by`.
  `UNIQUE (workflow, version)`. Append-only — `save_workflow` reads the max version and inserts
  the next; nothing updates a row. Strict reading, as `_sc_triggers` has.
- [x] 1.4 `_sc_run_traces` (§9, created here rather than by the agent milestone that named it):
  `id`, `run`, `seq`, `step`, `started_at`, `finished_at`, `attempt`, `outcome`
  (`ok`|`error`|`suspended`), `error`, `context` (the context **after** the step), `attributes`.
  Written only when the workflow's `trace` flag is on, in the same statement batch as the run's
  advance.
- [x] 1.5 `_sc_runs` gains, through the additive bootstrap: `wake_at` (when this run next wants
  the engine — now, a retry's deadline, a `Wait`'s end, NULL for one that is waiting on a human),
  `lease_until` and `claimed_by` (decision 5), and `subject_version` (the workflow version this
  run is pinned to; NULL for an agent run). `RunState` gains `Waiting` beside `running`, `done`,
  `failed` and `aborted`.
- [x] 1.6 `TriggerBody` in `sc-action`: `Action { action, configuration }` | `Workflow`.
  `_sc_triggers` gains a `body` text column as the discriminator and `action` becomes nullable;
  reading is strict in both directions (a `workflow` body with an action name is as much an error
  as an `action` body without one). Every existing caller of `trigger.action` moves to the enum —
  validation, the live set, dispatch, the admin API, the app-exposed endpoints.
- [x] 1.7 `WorkflowEngine`, a seam in `sc-action` that `TriggerDispatcher` holds and
  `sc-workflow` implements — the `TableEvents` precedent, and for its reason: dispatch is layer 6
  and the engine is layer 7, so the dispatcher cannot name it. One method: *start a run of this
  workflow for this event, and answer what happened*. Absent (a build tool, a unit test) a
  workflow trigger refuses by name rather than silently doing nothing.
- [x] 1.8 `Ambient::Context` in `sc-expr` (decision 8): fieldless like `Payload`, declared by a
  shape only where a run exists, so naming `context` in an ordinary trigger's `only_if` is an
  unknown identifier rather than a null. `workflow_shape(catalog, channel)` in `sc-workflow`
  extends `action_shape` with it, and is the *only* place a step's scope is decided.

### 2. The machine

- [x] 2.1 `WorkflowRun`: the whole resumable state as one serialisable value — `context`, the
  **frame stack** (a `ForEach` needs somewhere to keep its cursor and its collection, and nesting
  means a stack rather than a field), the current step, the attempt count for it, the step budget
  and the accumulated trace sequence. `Serialize`/`Deserialize` round-trip tested; this is what
  `_sc_runs.context` holds.
- [x] 2.2 `next_step()` → `Decision`: `RunAction { step, action, configuration }` ·
  `Evaluate { formulas }` (a `Set`, a `Branch`, a `ForEach`'s collection — everything needing the
  JS evaluator, which the machine does not hold) · `Suspend { until | awaiting_input }` ·
  `Done { context }` · `Failed { step, error }`. Fed back through `step_succeeded(value)`,
  `step_failed(error)`, `evaluated(values)` and `resumed(input)`.
- [x] 2.3 The advance rules, each with a synchronous test: an action's return value is stored in
  the context under the step's name (and a `Set`'s assignments merge into it); `Next` resolves
  through all four variants; `End` and "no next" both finish; a `ForEach` pushes a frame, binds
  the loop variable under `var`, and pops to its own `next` when the collection is exhausted;
  an empty collection runs the body zero times; the step budget (default 1000, configurable per
  workflow) ends a run as `MaxSteps`-like rather than looping forever.
- [x] 2.4 The error rules, likewise: `Retry` counts attempts and asks for a `Suspend` until the
  backoff deadline, then re-runs *the same step*; exhausting `max` falls through to the
  workflow-level policy; `Handler` jumps to the named step with the error in the context under a
  reserved key; `Fail` ends the run as failed with the reason. A per-step policy overrides the
  workflow's, and "no per-step policy" is not "no policy".

### 3. The driver: durability, recovery and the queue

- [x] 3.1 `Driver::advance(run)`: load the pinned workflow version, ask the machine, do the IO
  (run the action through `ActionRegistry` with an `ActionContext` carrying the run context, the
  event, the evaluator, the mailer and the dispatcher; or evaluate the formulas), feed the
  outcome back, and **write once** — context, cursor, attempt, state, `wake_at` and the trace row
  in one batch (decision 6).
- [x] 3.2 `WorkQueue` (decision 5) with the polling implementation: claim due runs under a lease,
  renew it while a step is in flight, and release it on write. A lease that expires is a crashed
  node's run, and the next poll picks it up — which is the recovery path, tested by writing a run
  with a stale lease rather than by killing a process.
- [x] 3.3 `WorkflowEngineTask`: one tokio task started by `serve` (and only by `serve`, as the
  scheduler is), with a bounded number of runs in flight, the clock as a parameter, and a
  shutdown that lets in-flight steps finish. Installed on the dispatcher as the `WorkflowEngine`
  seam.
- [x] 3.4 Starting a run: from an event (the payload, row, old and user land in the run's own
  event record so a resumed run still has them), from the admin's Run button, from the scheduler,
  and from an application's exposed endpoint. The response is the run id and its state — a
  workflow body does not return a value at the end of `run`, it suspends (§10.2), and the caller
  gets something addressable rather than a wait.
- [x] 3.5 Cascade and authority: a step's writes carry the run's chain (the trigger's name plus
  the step's), so `MAX_DEPTH` bounds a workflow that writes a row that starts a workflow exactly
  as it bounds actions today; and a step runs with the authority §10.1 gives an action — admin,
  carrying the event's user.
- [x] 3.6 Failures reach the error log (§16) and the run's `error` column, with the step named.
  A run that fails is a record, not a lost report.

### 4. Suspension: waiting for a time, and waiting for a person

- [ ] 4.1 `Wait { until }`: a formula yielding a duration or an instant; the run's `wake_at` is
  written and the run leaves the queue's reach until then. Restart-safe by construction, and
  tested by moving the clock rather than by waiting.
- [ ] 4.2 `UserForm { fields, assign_to, min_role, timeout }`: the step declares `FormField`s —
  the same "settings as data" vocabulary everything else uses — and the run suspends with
  `wake_at` NULL and the pending form recorded on the run. `resume_run(id, values)` validates the
  values against the declaration (`validate_attrs`), merges them into the context under
  `assign_to`, and hands the run back to the queue. A `timeout` sets `wake_at` so an abandoned
  approval fails or branches instead of waiting forever.
- [ ] 4.3 Who may resume: the run's `min_role` floor, defaulting to admin, checked in the API
  layer — the same rule and the same default a trigger's exposure has.
- [ ] 4.4 `cancel_run` (a running or waiting run becomes `aborted` with a reason) and
  `retry_run` (a failed run resumes at the step that failed, attempt count reset) — the two
  operations an admin looking at a stuck run actually needs.

### 5. The admin API

- [ ] 5.1 `getWorkflow` — the current version's steps, the version number, the validation issues,
  and the version history (number, when, who). `saveWorkflow` — steps in, a new version out,
  refusing an invalid one with the message the editor shows in place. `revertWorkflow` — mint a
  new version whose steps are an old one's, because rewriting history is what append-only says
  no to.
- [ ] 5.2 `listWorkflowRuns` (by workflow, filterable by state, newest first, paged) reusing the
  run summary schema the agent milestone defined, plus `subject_version`, the current step and
  `wake_at`. `getRun` grows the workflow half: the trace rows, the pending form and the pinned
  version.
- [ ] 5.3 `resumeRun`, `cancelRun`, `retryRun` — §4's three, typed, admin-authenticated, each
  answering the run's new state.
- [ ] 5.4 `listActions` already declares every action's `config_spec` and is what the step
  palette and the step inspector render; the one addition is which actions are *usable as a
  workflow step* on a channel-less run, so the palette does not offer a step whose configuration
  cannot be filled in.
- [ ] 5.5 The trigger endpoints carry `TriggerBody`: creating a trigger with a workflow body
  creates version 1 (an empty workflow with one start step, so a new workflow opens on a canvas
  rather than on an error), and `runTrigger` on one answers a run id.

### 6. The visual editor

- [ ] 6.1 `@xyflow/react` and `@dagrejs/dagre` added to `ui/admin`; the vendored CSS imported
  through the bundler (no CDN, no `@import` — the admin theme test's rule); a test asserting the
  admin CSP is unchanged by their arrival.
- [ ] 6.2 `workflowGraph.ts` (decision 10): `stepsToGraph(steps)` → nodes and edges, with a
  branch's arms as labelled edges and a `Next::Formula` as one dashed edge to a computed marker;
  `graphToSteps(nodes, edges)` back again, preserving everything the canvas does not model
  (descriptions, per-step error policies, formula text); `layout(nodes, edges)` over dagre; and
  `validate(steps)` — decision 11's rules, client-side, for the message that appears while the
  admin is still looking at the canvas. `vitest` for each, including a round-trip property over a
  workflow using every step kind.
- [ ] 6.3 `WorkflowEditor.tsx`: the canvas with one node type per step kind (its own icon and
  colour, the step's name, a one-line summary of what it is configured to do), edges drawn and
  deleted by dragging, a palette to drop a new step, undo/redo, auto-layout on demand, and a
  save that mints a version. Deleting a step that others point at is refused with their names,
  not silently repointed.
- [ ] 6.4 The inspector: the selected step's name, description, `Next` (a picker per branch arm
  with the formula beside it), its error policy, and — for an `Action` step — the action picker
  and `SettingsFields` over that action's `config_spec`, which is the same component the trigger
  form uses and is why a plugin's action gets a working step form with no change to this file.
  `Set`, `ForEach`, `Wait` and `UserForm` each get their own small editor; `UserForm`'s is a
  repeated form of `FormField` declarations.
- [ ] 6.5 The trigger form gains "Workflow" beside the actions, and saving one lands on the
  editor. The triggers list shows a workflow's step count and its version, and links to its runs.
- [ ] 6.6 `WorkflowRuns.tsx` and `RunDetail.tsx`: the run list with state, current step, when it
  will wake and who started it; the detail showing the trace as a timeline (step, attempt,
  duration, outcome, the context after it, with the change from the step before highlighted) and
  **the same canvas in read-only mode** with the path taken drawn on it and the current step
  marked — the reuse decision 10's split is what makes cheap.
- [ ] 6.7 A suspended run's pending form, rendered from its declaration by `SettingsFields`, with
  resume, cancel and (on a failed run) retry.

### 7. Tests

- [ ] 7.1 `sc-workflow` unit tests: the machine synchronously (§2's rules, one test each), the
  serde round-trip, and validation's refusals by message.
- [ ] 7.2 The driver against a fake clock, a recording action registry and an in-memory queue:
  retries with backoff, the handler jump, the budget, a `ForEach` over a hundred items, and a
  step that fails on its first attempt and succeeds on its second.
- [ ] 7.3 Real Postgres: version pinning (edit the workflow twice while a run is suspended; the
  run finishes on version 1), recovery (a run row with an expired lease is picked up and finishes
  correctly, and its step runs **once more**, not twice from the start), the append-only
  guarantee, and the trace rows.
- [ ] 7.4 An end-to-end integration test over HTTP: create the trigger, save the workflow, insert
  a row, watch the run suspend, resume it with a form value, and read the finished context and
  its trace — the milestone's definition of done, minus the browser.
- [ ] 7.5 `vitest` for `workflowGraph.ts` and the run-path projection.
- [ ] 7.6 A test that the depth bound still holds when the cascade goes through a workflow.

### 8. Documentation

- [ ] 8.1 `docs/tutorial-workflows.md`: build the order-approval workflow of the definition of
  done from an empty canvas, including what to do when a step is not idempotent.
- [ ] 8.2 §10.3 of `docs/TECHNICAL_DESIGN.md` rewritten to what was built — decision 3's `Next`,
  decision 5's queue and its `WorkQueue` seam, decision 6's honest reading of the transaction
  guarantee, and the step set with the reason it is five.
- [ ] 8.3 The CHANGELOG entry, and the crate tree in §2 (`sc-workflow` loses its "planned"
  status; `ui/admin` gains React Flow).

---

## Carried past this milestone

- **A `sc-bus` crate.** Decision 5 leaves a seam shaped for it: cache invalidation, the queue's
  wake-up and real-time collaboration are one problem, and solving it for the queue alone would
  be solving it in the wrong place.
- **One transaction across a step's row writes** (decision 6), which needs `CallerContext` to
  carry a transaction handle — a change to every write path, and a milestone of its own.
- **`WorkflowRoom`**: the end-user chat presentation of a running workflow, and modal
  interaction pushed over a socket. Both wait on §13's viewpatterns.
- **A `SubWorkflow` step** that starts a child run and waits for it durably — a parent suspended
  on a child is a second suspension reason and a second recovery case.
- **Parallel steps.** The machine's frame stack is the place a fork would live, and deciding
  which steps are independent is the same question the agent loop deferred about concurrent
  tools.
- **The copilot trait that writes workflows** (§11.6), which this milestone's API is the surface
  for.
