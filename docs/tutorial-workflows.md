# Tutorial: Workflows — a program that survives the night

Draw a program on a canvas: branch on the row that started it, loop over its children, stop and
wait for a person to approve it, and carry on the next day — on the version of the program it
started with, even though you have edited it twice since. Restart the server while it waits and
nothing is lost, because a run is a row and not a process.

This continues from [tutorial-triggers.md](tutorial-triggers.md): you have a server started with
`--base-domain localhost` and an admin account. Nothing from that tutorial is required — the
tables below are new — but the trigger model is, because **a workflow is a trigger body**, and
everything you know about events, `only_if` and the Run button applies unchanged.

## The rule in one line

A **trigger** binds one event to one **body**, and a body is either one action or a **workflow**:

> a workflow is a **program** — steps, and control flow between them — that runs one step at a
> time, **durably**: the run's state is written after every step, so it can wait for a day, a
> person, or a restart, and carry on where it stopped.

Three consequences worth holding on to before you draw anything:

- **A run is a row, not a process.** Nothing is holding a future or a timer. A run that is
  waiting is a `_sc_runs` row with a `wake_at` in it (or a NULL, for one waiting on a person),
  and the engine is a query for the rows whose time has come.
- **A run is pinned to the version it started on.** Saving an edited workflow mints a *new*
  version; it never rewrites one. A run suspended since yesterday finishes on yesterday's
  program, which is the only reading of "waiting for approval" that is not a lie.
- **A step runs at least once.** The context, the cursor and the trace commit together, once —
  but a step's *effect* (an HTTP call, an email, a row write) is not in that transaction, so a
  node that dies between the effect and the write re-runs that step when it comes back. There is
  a whole section on this below, because it is the one thing a workflow asks of you.

## What you will build

The order-approval workflow: submitting an order starts a run that reads the order's lines,
counts them, decides whether the order is large enough to need a human, waits — for **three
days**, or for a person, whichever comes first — and then ships or rejects the order and records
what it did.

```
load_lines ──▶ start_count ──▶ per_line ──▶ classify ──┬── context.large ────────▶ approve ──┬── approved ──▶ ship ───┐
                                  │                    │                                     │                       ├──▶ log
                                  └──▶ add_line        └── otherwise ──────────────▶ ship    └── otherwise ──▶ reject ┘
                                       (the loop body)
```

Six of the eight steps are the two kinds you will use most — an **Action** (any registered
action, which is what makes `run_js_code`, `send_email` and the row actions workflow steps) and a
**Set** (write formulas into the run's own memory). The other two are the loop and the wait for a
person.

## Step 1 — Three tables

**Tables → New table name → `orders`**, then add fields:

| Field | SQL type | Nullable |
|---|---|---|
| `customer` | `text` | ✔ |
| `email` | `text` | ✔ |
| `total` | `bigint` | ✔ |
| `status` | `text` | ✔ |

Then `order_lines`:

| Field | Type | Nullable |
|---|---|---|
| `order` | **Key to…** `orders` | ✔ |
| `item` | `text` | ✔ |
| `qty` | `bigint` | ✔ |

And `order_events`, which is where the workflow will record what it decided:

| Field | Type | Nullable |
|---|---|---|
| `order` | **Key to…** `orders` | ✔ |
| `kind` | `text` | ✔ |
| `at` | `bigint` | ✔ |

`at` is a number rather than a timestamp for the reason [tutorial-triggers.md](tutorial-triggers.md)
gives: the formula language has no `new`, so "now" is `Date.now()` — epoch milliseconds, which
sort and compare exactly as well.

## Step 2 — A trigger whose body is a workflow

**Triggers → New trigger**:

| Field | Value |
|---|---|
| Name | `order_approval` |
| Event | `A row is updated` |
| Table | `orders` |
| **Only if** | `status === 'submitted' && old.status !== 'submitted'` |
| **Runs** | `A workflow` |

The **Do** card changes when you choose `A workflow`: there is no action to configure, because
the steps live on a canvas of their own. Press **Create trigger** and you land on the workflow
editor, on **version 1** — an empty canvas with one start step, rather than an error telling you
nothing has been saved yet.

Everything else on the trigger form still means what it meant. The event, the table, the
`only_if`, **Enabled**, the minimum role and the exposure through an application are the
trigger's, and a workflow inherits all of them: they are not re-declared anywhere in the editor.

That `only_if` is doing two jobs, and both are worth having in mind before you draw anything:

- **It gives the run something to read.** An order's *lines* cannot exist before the order does,
  so a workflow started by the order's insert would loop over nothing. Submitting is the moment
  the work is ready, and `status` is how a row says so.
- **It stops the workflow starting itself.** Two of the steps below write to `orders`, and a
  write to `orders` is an update event on `orders` — the very event this trigger is on. The
  condition is about a *change* (`old.status !== 'submitted'`), so the workflow's own write to
  `status` does not match it. A workflow on a table it also writes needs this; a cascade is
  bounded at five deep either way, but "bounded" is not the same as "intended".

## Step 3 — The canvas, and the first two steps

The editor is three things: the **palette** ("Add a step:") along the top of the canvas, the
**canvas** itself, and the **inspector** on the right, which edits whichever step is selected.
Nothing is saved until you press **Save a new version**.

Select the start step the editor made for you. In the inspector, change its **Name** to
`load_lines` and its kind to **Action**, then fill it in:

| Setting | Value |
|---|---|
| Action | `run_js_code` |
| Code | see below |

```js
return await db.order_lines
  .where({ order: row.id })
  .select("id", "item", "qty")
  .orderBy("id")
  .rows();
```

That is the same `db` handle a `run_js_code` trigger gets, and `row` is the order that was
inserted — a run carries the **whole event** that started it, so `row`, `old`, `user` and
`payload` mean the same thing on day two of a suspended run as they did on day one.

**What a step returns is stored under the step's name.** When this step finishes, the run's
context has `load_lines` in it: an array of line rows. Every later step reads it as
`context.load_lines`.

Now add the second step: press **Set** in the palette, and in the inspector name it
`start_count` with one assignment:

| context key | formula |
|---|---|
| `units` | `0` |

Then draw the edge. Drag from the bottom of `load_lines` to the top of `start_count`; the
inspector's **Then** card follows along, and now says *Go to one step → `start_count`*. Edges and
the **Then** card are two views of one thing — the editor reads your program back out of the
edges, which is what makes dragging one an edit rather than a decoration.

Two rules about drawing worth knowing now:

- **Dropping an edge on a step that already has one makes a branch**, whose first arm is the new
  edge and whose *otherwise* is where the step used to go. That is the only reading that keeps
  the program you already had reachable; the empty guard is what validation then asks you to
  fill in.
- **Deleting a step is refused with the names of the steps that point at it.** Silently
  repointing them would invent a program nobody wrote. Renaming one, by contrast, repoints
  everything that named it — including the start step.

## Step 4 — The loop

Press **For each** and configure it:

| Setting | Value |
|---|---|
| Name | `per_line` |
| Over | `context.load_lines` |
| Item name | `line` |
| Body | `add_line` (you are about to make it) |

Then press **Set**, name it `add_line`, and give it one assignment:

| context key | formula |
|---|---|
| `units` | `context.units + context.line.qty` |

Leave `add_line`'s **Then** as *End the run*. Inside a loop body that means *this iteration*
ends, not the run — which is the one thing about the loop worth remembering, and why the loop
needs no "continue" step. Draw `start_count → per_line`, and check that `per_line`'s **Body**
names `add_line`.

The loop **binds the item into the context** under the name you gave it, so the body reads
`context.line`. An empty collection runs the body zero times; a collection that is not a list is
a *failure* rather than an empty loop, because running the body zero times would hide a formula
naming something the run never wrote. Loops nest — the body of a loop may contain another one —
because a run keeps a stack of frames rather than a single cursor.

## Step 5 — The branch

Press **Set** again, name it `classify`, and give it one assignment:

| context key | formula |
|---|---|
| `large` | `row.total > 100 \|\| context.units > 20` |

Draw `per_line → classify`. Then, in `classify`'s **Then** card, choose **Branch on a condition**
and fill in one arm:

| Arm | Value |
|---|---|
| when | `context.large` |
| step | `approve` (in a moment) |
| Otherwise | `ship` (in a moment) |

Arms are tried in order and the first true one wins; *otherwise* is where the run goes when none
matched. A branch with no *otherwise* and nothing true simply finishes, which is the honest
reading of a branch with nothing else to say.

Formulas in a workflow are the same one-expression language as an `only_if` or a field value,
with one addition: **`context`**, the run's own memory. It is `context.units` and not a bare
`units` on purpose — bare identifiers already mean "a field of the row this formula ranges over"
everywhere else in Saltcorn, and quietly redefining them inside a workflow would make one
language mean two things.

## Step 6 — Waiting for a person

Press **User form** and configure it:

| Setting | Value |
|---|---|
| Name | `approve` |
| Answers go to | `approval` |
| Who may answer | `Admin only` |
| Give up after | `1000 * 60 * 60 * 24 * 3` |

and, under **Ask for**, two fields:

| Name | Label | Type | Required |
|---|---|---|---|
| `approved` | Approved? | `bool` | ✔ |
| `note` | Note | `text` | |

Then set `approve`'s **Then** to a branch:

| Arm | Value |
|---|---|
| when | `context.approval.approved` |
| step | `ship` |
| Otherwise | `reject` |

And its **If it fails** card to **Jump to a step → `reject`**.

Three things just happened, and each is a rule rather than a setting:

- **A form suspends the run waiting on a *person*.** With no timeout that means no deadline at
  all — a `wake_at` of NULL on the run's row, which is exactly "no clock will make this
  runnable"; only somebody answering will. Ours has a timeout, so its only clock is the moment
  it gives up.
- **The declaration travels with the run.** What the person is shown a week from now is the form
  the step declared *when the run reached it* — not the workflow's current text. You may edit
  this step tomorrow; the approval already waiting is unaffected.
- **"Give up after" is what makes an abandoned approval a decision.** After three days the run
  wakes with a failure — "nobody answered" — and its **error policy** decides what that means.
  Ours jumps to `reject`. Without a timeout the run waits indefinitely, which is sometimes right
  and is never accidental.

## Step 7 — The two endings, and the record

Press **Action** twice more.

`ship`:

| Setting | Value |
|---|---|
| Action | `update_rows` |
| Table | `orders` |
| Where | `id === row.id` |
| Assignments | `{"status": "'shipped'"}` |

`reject`: the same, with `{"status": "'rejected'"}`.

The **Where** formula ranges over the *target* table — bare `id` is the order being updated — and
`row` is still the event's row, which is how "this order and no other" is written. The
assignments are formulas, so a literal string is quoted twice: `"'shipped'"`, exactly as in the
triggers tutorial.

Then one more **Action**, named `log`, with `run_js_code`:

```js
const already = await db.order_events
  .where({ order: row.id, kind: "decided" })
  .exists();
if (already) return { wrote: false };
await db.order_events.insert({ order: row.id, kind: "decided", at: Date.now() });
return { wrote: true };
```

That body is written the way it is on purpose, and Step 12 is about why.

Draw the last edges — `ship → log`, `reject → log` — and leave `log`'s **Then** as *End the run*.

## Step 8 — Save, and what the editor checks

In the right-hand column, **This workflow** is the program's own settings:

| Setting | Value |
|---|---|
| Starts at | `load_lines` |
| When a step fails and says nothing itself | `Fail the run` |
| Step budget | `1000` |
| **Record a trace** | ✔ **on** |

Turn the trace on. It writes the whole context after every step, which is what the run screen's
timeline draws — worth having while you are learning, and worth thinking about later, because a
trace row carries a copy of the context.

Press **Save a new version**. If anything does not hold together the editor tells you *all* of it
at once, above the canvas: every `next` that names a step that is not there, an unreachable step,
an action whose settings do not validate, a formula that will not resolve. A workflow that fails
these checks is still **stored and still editable** — editing it is the repair — but it will not
start a run.

You are now on **version 2**, and version 1 is still in the **Versions** card beside it. Saving
never rewrites; it appends. That is what makes the next two steps possible.

## Step 9 — Run it

**Tables → orders → Data → Add row**: customer `Acme`, email your own address, total `500`,
status blank. Then **Tables → order_lines → Data** and add two rows pointing at that order, with
quantities of 3 and 4.

Nothing has happened yet — the trigger is on the *update*. Now go back to **Tables → orders →
Data**, edit the order, and set `status` to `submitted`.

Go to **Triggers → `order_approval` → Runs**. There is one run, and it is already **waiting**, at
step `approve`, on version 2: the engine drove it as far as it could go the moment the row was
updated, and that was as far as a person.

The columns are the operational questions: **State**, **Started**, **Step** (where it is),
**Version** (which program it is on), **Wakes** and **Started by**. **Wakes** is three days out —
that is the form's *timeout*, and the only clock this run has. Take the timeout off the step and
the column is blank, because a form with no deadline is a run no clock will ever make runnable.

## Step 10 — Restart the server

Stop the server. Start it again. Open **Triggers → `order_approval` → Runs**.

The run is still there, still waiting, still at `approve`, still on version 2. Nothing was
recovered, because nothing was lost: the run's whole state — the context, the loop frames, the
cursor, the attempt count — is a JSON document on its row, and resuming is a load rather than a
reconstruction.

## Step 11 — Edit the workflow, then answer the form

While the run waits, go back to the editor and change something visible: give `ship` a second
assignment, say `{"status": "'shipped'", "customer": "'EDITED'"}`. **Save a new version** — you
are on version 3.

Now open the run (**Runs → the run's row**) and look at it before you answer:

- **Waiting for an answer** — the form, rendered from the step's own declaration by the same
  component that renders a trigger's action settings. It says where the answers will go:
  `context.approval`.
- **Where it got to** — the same canvas you drew, read-only, with the path this run took lit up
  and the current step marked. The card's subtitle says *drawn on version 2, the one this run is
  pinned to*, because drawing the path of one program on the picture of another would be a lie.
- **What happened** — the trace as a timeline: one entry per completed step attempt, with what
  that step *changed* in the context rather than the whole document twenty times over. There is
  **one** `add_line` entry for two lines; the last of the "trips people up" entries says why.

Tick **Approved?**, write a note, and press **Answer and carry on**.

The run finishes while you watch — the answers are checked against the declaration you were
shown, merged into the context under `context.approval`, and the run carries on from `approve` to
`ship` to `log`. Then look at **Tables → orders → Data**: the status is `shipped` and the
customer is still `Acme`. **Version 3 did not run.** The run finished on version 2, which is the
program it started on.

## Step 12 — When a step is not idempotent

Here is the guarantee, stated exactly:

> The context, the cursor, the attempt count, the run's state, its `wake_at` and its trace row
> commit **together, once**. A run is never observed half-advanced. But a step's own effect is
> not in that transaction — an HTTP request, an email and (today) a row write are not
> transactional — so **a step runs at least once**: a process that dies between a step's effect
> and its write comes back, is told to run *that* step again, and runs no other.

The window is small and it is real. (One nuance, spelled out in the last "trips people up"
entry: when a loop body is a *single* step, its iterations are serviced in one pass, so a crash
re-runs the iterations that had not been written rather than only the last one.) So: what do you
do about a step whose effect must not happen twice?

**1. Prefer effects that are repeatable.** `ship` sets `status` to `'shipped'` for one order.
Running it twice leaves exactly the same row — that is what "idempotent" means, and most row
updates are, for free. Prefer an update keyed by the event's row over an insert that appends.

**2. Make the step ask whether it has already happened.** That is why `log` is written as a
`run_js_code` body with a check in front of the insert rather than an `insert_row` step: a second
run of it finds the `order_events` row already there and writes nothing. The check and the write
are one database round trip each, but they are *the step's own*, so re-running the step re-runs
the check.

**3. Let the database refuse the duplicate.** A unique constraint on `(order, kind)`
([tutorial-constraints.md](tutorial-constraints.md)) makes the second insert impossible rather
than merely unlikely. Note what happens next, though: the second insert *fails*, and a failing
step goes through its error policy — so pair the constraint with a body that catches it, or with
**If it fails → Jump to a step** pointing at somewhere that treats "already done" as success.

**4. For an effect that leaves the server, carry an idempotency key.** A payment or a webhook
should be sent with a key the far end deduplicates on, derived from something stable about *this*
piece of work — `"order-" + row.id + "-charge"` — and not from the time or a random number. A
retried step then recomputes the same key, and the far end recognises it.

**5. When none of those is possible, put the risky step last** — or at least after everything
that records the decision — and know where the window is. A step you cannot make repeatable is a
step whose repeat you have to be able to detect afterwards.

What the engine will *not* do is silently skip a step it is unsure about. "At least once" is a
promise it keeps; "exactly once" is one nothing that calls the outside world can keep, and saying
so is better than implying otherwise.

## Step 13 — When a step fails

Give any Action step an **If it fails** policy and you have the other half of the engine:

- **Retry, then fall through** — *Attempts*, *First wait (ms)*, *Multiply by*, *Longest wait
  (ms)*. The run suspends until the backoff deadline and then re-runs **the same step**; the
  waits grow, are capped, and are jittered so that a hundred runs that failed on the same outage
  do not retry in lockstep and become the outage's second wave. Exhausting the attempts falls
  through to the workflow's own policy.
- **Jump to a step** — the run goes to the named step with the failure in the context under
  `context.error`, so a handler can read `context.error.message` and record or route it.
- **Fail the run** — it stops, with the step named on the run's row, in the server's error log,
  and raised as an `error` **event**, so an alerting trigger sees a workflow that stopped exactly
  as it sees a request that failed.

A step with no policy of its own uses the workflow's, and the workflow's default is *fail*.

On the run screen, a stuck run has two buttons: **Cancel** (a running or waiting run becomes
`aborted`, with the reason you type, and the queue never picks it up again — it is not `failed`,
because nothing went wrong, somebody decided) and, on a failed run, **Retry from `step`**, which
starts again **at the step that failed** with the attempt count reset, on the pinned version. The
steps before it already had their effects; re-running them is the thing at-least-once is trying
to do less of.

## Things that trip people up

- **An Action step's settings cannot read `context` — today.** A `Set`, a branch guard, a
  `For each`'s collection, a `Wait`'s deadline and a form's timeout are evaluated by the engine
  and see the run; an action's own settings are evaluated by the action, in the same scope a
  trigger's settings have, which is the **event** (`row`, `old`, `user`, `payload`) and not the
  run. `"title": "context.large"` on an `insert_row` step is refused on save, saying
  *unknown identifier `context`*. The way round it is the one Step 3 uses: put the work in a
  `run_js_code` body, which can read `row` and query the database for whatever else it needs.
  (§10.3 records this as the gap it is; decision 8 intends an action's settings to see the run,
  and they do not yet.)
- **A `run_js_code` body sees the event, not the run.** Same reason, same shape: `row`, `old`,
  `user` and `payload` are bound; `context` is not. What the body *returns* lands in the context
  under the step's name, which is how a body hands its answer to the steps after it.
- **`End the run` inside a loop body ends the iteration.** It is the same `Next::End`; which one
  it means is a question the run's frame stack answers. A body that "ends" is a body that has
  finished this item.
- **The loop variable outlives the loop.** `context.line` is still the last item after
  `per_line` has finished, because the item is bound *into the context* and nothing clears it. Do
  not read it after the loop expecting it to be gone; accumulate what you need, as `add_line`
  does.
- **A loop body of one step is serviced in one pass.** The engine keeps working while the next
  thing it is asked about is the step it is already on, and iteration two of a one-step body is
  that same step — so all the iterations share **one** trace entry and **one** write. It does
  not change what the run computes, and for a `Set` body it costs nothing; but a body whose step
  has an *effect* is worth splitting into two steps, because then each iteration is its own
  advance, its own write and its own trace row, and a crash re-runs one item rather than the
  uncommitted tail of the loop.
- **A step budget stops a loop that never leaves.** 1000 steps by default, configurable per
  workflow, counted across the whole run. A run that hits it stops and names the step it stopped
  at — and it refuses to be retried into the same wall.
- **A run started before you saved is not on your new version, and that is the point.** If an
  edit was meant to fix a run that is already waiting, cancel it and start a new one; there is no
  way to move a run onto a version it did not start on, deliberately.
- **Restoring an old version is a new version, not a rewrite.** The **Versions** card's
  **Restore** mints a new version whose steps are an old one's. Append-only is what lets a
  suspended run load version 1 tomorrow.
- **A computed `next` is drawn as one dashed edge to a marker, and cannot be edited by dragging.**
  If you write *A formula names the step* (`context.total > 100 ? 'approve' : 'ship'`), the
  editor cannot know where it goes, so it draws that it does not know — and validation stands
  down about unreachable steps, because marking a good step dead would be worse than saying
  nothing.
- **The engine only runs under `serve`.** A `build-app`, a backup or an admin script that opens
  the same database does not start advancing runs — deliberately. In a process with no engine, a
  workflow trigger refuses by name rather than reporting success for a run nobody started.
- **A trace is off by default.** The run screen says so when there is nothing to draw: turn
  **Record a trace** on in the editor. It is off by default because every row carries a copy of
  the whole context.
- **The trigger still decides *whether* the run starts.** `only_if`, **Enabled** and the minimum
  role are the trigger's, and they are checked before a run exists. A workflow that never starts
  is usually a trigger question, and **Triggers** is where the diagnosis is.
- **Two runs of the same workflow are independent.** Submitting five orders starts five runs,
  each with its own context, its own cursor and its own place in the queue. There is no "the" run
  of a workflow.

## What next

The engine you have just used is also what runs a workflow started from the admin's **Run**
button, from the scheduler (a `daily` trigger whose body is a workflow), and from an
application's exposed `POST /api/actions/{name}` — which answers the **run's id and state**
rather than a result, because a workflow may not have finished by the time the request has to.

From here, [tutorial-agents.md](tutorial-agents.md) is the natural next step: `run_agent` is an
ordinary registered action, so an agent is an ordinary workflow step — its prompt a formula over
the event, as every action's settings are — and "ask the model, then have a human approve what it
suggested" is this tutorial's shape with `load_lines` swapped for the agent.
[tutorial-constraints.md](tutorial-constraints.md) is the other one, for Step 12's third remedy.
