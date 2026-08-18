# Saltcorn v2 — Concurrent code bodies: many runs per isolate

Ordered, checkable task list for the twelfth milestone after the MVP. Earlier lists are
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
and indexes), [docs/TODO-post-mvp-10.md](./docs/TODO-post-mvp-10.md) (email) and
[docs/TODO-post-mvp-11.md](./docs/TODO-post-mvp-11.md) (tables in code). Scope and rationale
remain in [docs/GOALS.md](./docs/GOALS.md) and
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md).

The previous milestone gave `run_js_code` tables. It gave them **synchronously**, and that
decision — right for getting the seam built, and honestly documented as a decision — bought
correctness with the one currency the server cannot spend: concurrency. A host call blocks its
isolate thread (`crates/sc-expr/src/code.rs`, `host_call`'s `handle.block_on`) for the whole
round trip to the database, and a worker checks out one job at a time. So the number of
`run_js_code` requests this server can serve **at once** is `DEFAULT_CODE_WORKERS`. Two.

Everything else about the design is already asynchronous and already scales: the query itself is
awaited on the caller's runtime, the row layer is async throughout, the connection pool bounds
itself. The blocking is *only* in the guest seam, and it costs a thread per concurrent run —
which is exactly the thing a server must not spend per request.

Nor is it only a throughput ceiling. A code body's write raises table events, so a body whose
`.insert()` fires a second `run_js_code` trigger needs a *second* worker while still holding its
own. With two workers, two such requests hold both, the nested runs never get one, and all four
fail at the wall clock. The pool size is not a tuning knob there; it is `1 + max nesting depth ×
concurrency`, which is not a number anyone can pick.

This milestone makes `db` **awaitable**, so that a run costs a pending promise rather than a
thread, and one isolate serves hundreds of runs at once.

**Milestone definition of done:** with the default two isolates, 500 concurrent requests each
firing a `run_js_code` trigger that reads and writes are served concurrently — the fake-host
integration test observes hundreds of host calls in flight at once, and the wall clock is the
database's, not the pool's. A trigger whose write fires a second `run_js_code` trigger completes
on a **one-worker** pool. A body that loops without yielding still fails, alone where that can be
told, and says which trigger did it. And the body reads the way its author expects:

```js
const overdue = await db.invoices
  .where({ paid: false, due: { lt: payload.today } })
  .orderBy("due")
  .limit(50)
  .rows();

for await (const inv of db.customers.where({ active: true }).iter()) { … }

const [chased, owed] = await Promise.all([
  db.reminders.insert(overdue.map((i) => ({ invoice: i.id }))),
  db.invoices.where({ paid: false }).sum("amount"),
]);
```

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## The specification

### 1. The guest API becomes asynchronous

Every terminal returns a Promise: `.rows()`, `.row()`, the scalars, `.insert()`, `.update()`,
`.delete()`, `.sql()`. `.iter()` becomes an **async generator**, walked with `for await`. The
body itself is wrapped in an `async function`, so `await` at its top level is legal, and
`__scRun`'s current refusal of a returned Promise inverts: a returned Promise is what a body now
answers with, and it is awaited.

The chain stays pure and synchronous — `db.invoices.where(…).orderBy(…)` builds a plan and
touches nothing — so only the terminals change. That is what keeps the diff to the prelude small
and the surface recognisable.

### 2. A forgotten `await` must not be silent

This is the tax the change levies, and it is paid here rather than by every trigger author.
`JSON.stringify(promise)` is `{}`, `if (promise)` is true, and `for (const r of promise)` is a
bare `TypeError` — three ways for a missing `await` to look like a wrong answer instead of a
mistake. So a terminal answers a `DbPromise` (a `Promise` subclass) whose `toJSON`,
`Symbol.toPrimitive` and `Symbol.iterator` all throw one named error: *this database call was
not awaited — write `await db.invoices.rows()`*. It costs nothing when the body is right.

### 3. Many runs on one isolate

A run stops being "what the isolate is doing" and becomes an **entry in a table**, keyed by a
token minted per run:

- `RunState` moves from a single `OpState` slot to a `HashMap<RunToken, RunState>`. The host,
  the deadline, the call budget and the bindings are per entry, exactly as they are per run now;
  what goes away is `handle`, because nothing blocks any more.
- The token is a random 128 bits, bound as a `const` in the run's own function scope. Not an
  index: two runs on one isolate may carry different authority (`db.asUser()` delegates to *this*
  event's caller), and a body must not be able to reach another's host by writing `1`.
- The op no longer blocks. `op_sc_db` becomes an ordinary `async` op that awaits
  `BridgeHost::call` — which already only sends on a channel and awaits a oneshot, so its future
  needs no reactor of its own and is driven by the isolate's own event loop.
- A run's result is delivered by a **completion op** (`__scDone` / `__scFail`) rather than by
  `execute_script`'s return value, because with runs resident that return value is a pending
  promise and the event loop, not the call, is what finishes a run. A syntax error still surfaces
  synchronously from the call, and still should.
- The worker thread becomes a current-thread tokio runtime driving `run_event_loop`, accepting
  new jobs while the resident ones are in flight.

### 4. The two clocks

Today one watchdog enforces one run's JS time by terminating the isolate. With runs multiplexed
that instrument is too blunt to keep pointed the same way: terminating the isolate stops
everyone. So the two things it currently does are separated.

- **The wall clock** (`timeout_ms`, per run) needs no termination at all. A run past its deadline
  is refused its next host call, and the caller stops waiting `CALLER_GRACE` later — both already
  written, both already correct, and between them they cover every run that is *waiting* rather
  than *running*. Which, for an I/O-bound body, is all of them.
- **The JS slice** is new and is what the watchdog now enforces: how long a body may run
  **without yielding**. Small (default 1 s, never more than the run's remaining wall clock),
  because a body that computes for a second between two queries is already pathological. A
  `while (true) {}` is caught in a second rather than in five.

Attribution: the isolate records the running run as each one starts and as each resumes (a cheap
sync mark from the guest at the point a host call returns), so a slice overrun blames the body
that actually overran it and names its trigger.

The co-residents are the honest part. Termination takes them with it, and they must **not** be
silently re-run: a body that has already inserted rows is not idempotent, and re-executing it is
a worse failure than the one being handled. So a run that has made **zero** host calls is
re-queued (provably no side effects yet); every other resident fails with its own named error
saying another body on the same isolate did not yield. Rare, loud, and never a duplicated write.

### 5. Admission, and what the ceiling becomes afterwards

Concurrency is no longer free-of-threads-but-free-of-everything: each resident run holds its
scope, its bindings and up to a 1000-row read in the V8 heap. So a worker admits at most
`max_inflight` runs (default 256; two workers → 512) and the rest queue exactly as they do now,
with queue time still inside the deadline. The isolate gets a heap limit and a near-heap-limit
callback, so pressure refuses new admissions instead of aborting the process.

Past that the ceiling is the **database connection pool**, which is the right place for it and
already bounds itself — a fact worth stating in §10.1, because it is the answer to "how many
`run_js_code` requests can this server serve" once this milestone lands.

---

## Decisions taken up front

1. **Async, not a source transform.** Rewriting the body with swc to insert `await` at every
   terminal was considered and rejected: aliasing (`const f = db.books.rows; f()`) makes it
   unsound, and a surface that is *sometimes* magic is worse than one that is honestly async.
2. **The pool stays small.** More isolates buy CPU parallelism, which is not what is short; this
   milestone buys concurrency. `DEFAULT_CODE_WORKERS` stays 2.
3. **The seam does not change.** `CodeHost` still takes one JSON plan and answers one JSON value.
   Nothing in `sc-api`'s host, the plan language, authority, the row cap or the cascade guard is
   touched by this milestone — §15's other guest languages inherit the same plans.
4. **No backwards compatibility for the synchronous spelling.** Prototype status: existing bodies,
   tests, docs and the editor's `.d.ts` are updated to `await`, and the sync form is gone rather
   than deprecated.
5. **`fetch` and timers stay out.** This milestone builds the plumbing that would make them
   possible; the decision not to have them in the sandbox is unchanged and unrelated.

---

## Phase 1 — The awaitable guest API (`sc-expr`)

- [x] `op_sc_db` becomes an `async` op awaiting the host directly; `host_call`'s
      `handle.block_on` and `RunState::handle` go away. Still one run per isolate at this phase,
      so the change is the API shape and nothing else — and is reviewable on its own.
- [x] `DB_PRELUDE`: `__scDbCall` returns a Promise; every terminal awaits it; `iterate` becomes
      `async function*`. The chain builders stay synchronous and untouched.
- [x] `build_code_script` wraps the body in an `async function`; `__scRun` awaits the result
      instead of refusing a Promise (`code.rs`'s current refusal inverts).
- [x] `DbPromise` with throwing `toJSON` / `Symbol.toPrimitive` / `Symbol.iterator`, so a
      forgotten `await` is one named error rather than `{}`, `true`, or a bare `TypeError`.
- [x] The worker drives `run_event_loop` after `execute_script` so a single run's promises
      actually settle; the watchdog is paused across the whole event-loop wait as it is across a
      host call today. **Deviation:** the watchdog is left *armed* across the event-loop wait
      and paused only by the op, exactly as it is today — pausing it wholesale would leave a
      `while (true)` after an `await` unbounded, losing the worker rather than the run. The
      run's wall clock now bounds the whole event-loop wait instead, which covers the case that
      motivated the bullet (a body suspended on a promise that never settles).
- [x] Update every existing code test in `sc-expr` and `sc-api` to `await`, which is also the
      check that the surface reads the way §1 claims.

## Phase 2 — Many runs per isolate

- [ ] `RunState` becomes a table keyed by a 128-bit random `RunToken`, bound as a `const` in the
      run's scope; `op_sc_db` takes the token and looks the run up. An unknown token is a named
      error, not a panic.
- [ ] Completion ops `__scDone` / `__scFail`, and the run's oneshot moves into the table entry:
      `execute_script` starts a run, the event loop finishes it. Errors carry `e.stack` so the
      message an admin sees is no worse than today's.
- [ ] The worker becomes `Builder::new_current_thread()` + `block_on`: a loop that admits jobs
      while pumping the event loop, and that parks on the job channel when nothing is resident
      (never a busy poll).
- [ ] Per-worker job channels with a least-inflight dispatcher (an `AtomicUsize` per worker)
      replacing the shared `Mutex<Receiver>`, so admission is load-aware and needs no MPMC
      dependency.
- [ ] `max_inflight` per worker (default 256), with the overflow queued exactly as now.
- [ ] Caller side: `CodeRuntime::run`'s serving loop keeps in-flight host calls in a
      `FuturesUnordered` instead of awaiting each inline, so a body's `Promise.all([…])` issues
      its queries **in parallel**. Borrowed host, no spawn, no `'static` requirement.
- [ ] Test: on a **one-worker** pool, 200 runs against a fake host that sleeps 50 ms each
      complete in ~one round trip's order of magnitude, and the host records ≥ 100 calls in
      flight at once.
- [ ] Test: runs do not leak into each other — 50 concurrent runs with distinct bindings each
      assert their own values, and one run exhausting its call budget leaves the others' budgets
      untouched.
- [ ] Test: a code body whose `.insert()` fires a second `run_js_code` trigger completes on a
      one-worker pool. This is the deadlock the milestone removes, and it deserves a test that
      would have failed before it.

## Phase 3 — The two clocks and the runaway

- [ ] Split the budgets: the per-run wall clock keeps its two existing enforcement points
      (deadline check in the op, `CALLER_GRACE` in `CodeRuntime::run`) and stops driving the
      watchdog; the watchdog enforces the **JS slice** (`DEFAULT_JS_SLICE`, 1 s, clamped to the
      run's remaining wall clock).
- [ ] Attribution: the isolate tracks the currently-running token — set when a run starts and by
      a cheap sync mark where a host call returns — so a slice overrun names the guilty body.
- [ ] On termination: `cancel_terminate_execution`, fail the guilty run with the slice error,
      re-queue only residents with **zero** host calls, fail the rest with their own named error.
      Drop the run table's entries either way, so nothing outlives the isolate that held it.
- [ ] V8 heap limit via `create_params` plus a near-heap-limit callback that stops admitting
      rather than aborting the process.
- [ ] Tests: `while (true) {}` fails with the slice error and names the trigger; a co-resident
      that had made no host call still completes; one that had made a host call fails with the
      co-resident error rather than being re-run (asserted by counting the fake host's writes).

## Phase 4 — The hot path

- [ ] Compile `DB_PRELUDE` **once per isolate** as a factory (`__scMakeDb(token)`) rather than
      splicing it into every run's script. The per-run `db` object is still fresh — decision 5 of
      the previous milestone (a body that assigns to `db` poisons nothing) is preserved by the
      factory, not by recompilation.
- [ ] Cache the compiled body per isolate, keyed by a content hash the Rust side sends: a run is
      then `__scInvoke(token, key, bindings)`, with the source travelling only on a miss. A
      trigger firing 1000 times compiles once.
- [ ] Benchmark the three of them together (`cargo bench` or a timed test): runs/second on one
      isolate against a fake host, before and after, recorded in the CHANGELOG.

## Phase 5 — Configuration, documentation and the definition of done

- [ ] `DenoEvaluator::with_max_inflight` beside `with_code_workers`, and — the gap this milestone
      also closes — an actual config path: `sc-server` builds the evaluator with
      `DenoEvaluator::new()` and nothing reads either knob today.
- [~] `docs/TECHNICAL_DESIGN.md` §10.1: "The API is **synchronous**" becomes its opposite, with
      the `for await` spelling, the two clocks, the admission bound, and the sentence naming the
      connection pool as the ceiling that remains. *(Done in phase 1: the synchronous claim, the
      `for await` spelling and every example. Still to do here: the two clocks, the admission
      bound and the connection-pool ceiling, none of which exist yet.)*
- [x] `ui/admin/src/codeTypes.ts`: terminals answer `Promise<…>`, `iter()` answers
      `AsyncIterableIterator<Row>`, and the doc comments lose "Synchronous — there are no promises
      in the sandbox". Monaco's own diagnostics then catch a forgotten `await` in the editor,
      which is where it is cheapest to catch.
- [x] `run_js_code`'s doc comment and `docs/tutorial-triggers.md`: every example gains its
      `await`, and the "the code is **synchronous**" bullet is replaced by what a body now has to
      know — await your queries, `Promise.all` is real parallelism, and a body that computes for a
      second without yielding is the one shape the runtime will refuse.
- [ ] The end-to-end test the milestone is defined by: 500 concurrent trigger fires on the
      default pool, all served, none timing out.
- [ ] CHANGELOG.

---

## Explicitly OUT of scope for this milestone

- **`fetch`, timers, or any second host surface.** Decision 5. The sandbox gains an event loop
  here and exactly no new capability.
- **Transactions across statements** (`db.transaction(fn)`). Still its own milestone, and
  multiplexed runs make the case for it no easier: a held transaction across arbitrary guest
  code is a lock held for the run's whole deadline, now with hundreds of runs resident.
- **Preemptive scheduling.** One isolate runs one body's JavaScript at a time; this milestone
  interleaves runs at their `await` points and bounds the slice between them. A body that wants
  CPU parallelism is a body that wants a different tool.
- **Growing or shrinking the pool at runtime.** The pool stays fixed and small (decision 2);
  admission control, not elasticity, is what bounds occupancy.
- **`db` in a formula.** Unchanged: the formula isolate stays pure and synchronous, and the
  deadlock that would follow from giving it a host call is the reason `CodeRuntime` exists.
- **The other guest languages** (§15's Python, Rust, Go adapters). The seam is untouched by
  design (decision 3), which is the point — they inherit this concurrency without inheriting any
  of its plumbing.
