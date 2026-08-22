# Saltcorn v2 — Modules in-process: v1 plugins on the Deno runtime

Ordered, checkable task list for the fifteenth milestone after the MVP. Earlier lists are
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
[docs/TODO-post-mvp-13.md](./docs/TODO-post-mvp-13.md) (modules) and
[docs/TODO-post-mvp-14.md](./docs/TODO-post-mvp-14.md) (SQLite). Scope and rationale remain in
[docs/GOALS.md](./docs/GOALS.md) and [docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md).

A module runs in a `node` child process. That was the right call when it was made — §15.1 says
so, and says why: `CodeRuntime` is a bare V8 with four ops and no module loader, and a v1 plugin
is a CommonJS package whose dependencies *are* the point of it, so running one there "is not a
shim but an implementation of Node". §15.1 also names the thing that already is that
implementation, in the same sentence: **`deno_runtime`, and why it is not `deno_core`**. This
milestone takes that sentence up. A module stops being a second process and becomes a worker
thread in this one, on the same V8 the rest of the server already links.

**What was measured before this list was written**, because the whole milestone turns on whether
a real v1 plugin survives Deno's node compatibility layer and none of it is worth starting if it
does not. The *unmodified* `module-host.mjs` was run under `deno run --node-modules-dir=manual`
against a modules root built by this repo's own installer recipe (`--install-links`, the
`@saltcorn/*` overrides, the stub packages):

- `@saltcorn/mqtt` loads and answers a manifest **byte-identical** to the one `node` answers —
  the actions, the `configFields`, the flattened `configuration_workflow`, the `eventTypes`
  census, all of it.
- `@saltcorn/proxmox` (the one that requires `node-fetch` without depending on it) loads the
  same under both.
- The `Module._load` patch that answers every `@saltcorn/*` require works: Deno's `node:module`
  polyfill exposes it, and `createRequire(import.meta.url)` resolves out of the modules root's
  `node_modules` exactly as node's does.
- The dependency that is the *reason* for a sidecar reaches a real socket: `async-mqtt`
  connecting to a closed port fails `ECONNREFUSED` under both, and connecting to a live broker
  **published a real message** under both.
- A `run` request against `mqtt_publish` produced the same error, with the same stack shape,
  under both.

So the JavaScript half is portable as it stands. What this milestone is actually about is the
Rust half: hosting that JavaScript in-process instead of behind a pipe.

**Milestone definition of done:** a server with `@saltcorn/mqtt` installed has **no `node`
process**, `node` is not on its PATH, and a trigger wired to `mqtt_publish` publishes. The
module's worker is denied the filesystem and allowed one host:port, and the Modules tab says so.
A module that calls `process.exit()` loses its in-flight calls and nothing else — the server
does not exit.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## The specification

### 1. Not the `CodeRuntime` isolates — a second pool beside them

"Into the Deno threads" is the obvious reading of this change, and the obvious objection to
keeping two pools is that isolates are expensive. Measured on this machine, they are not, and the
numbers are what settle the question rather than an argument about sandboxing:

| | first | **marginal** |
|---|---|---|
| Bare `deno_core` isolate (what `CodeRuntime` runs today) | 18.6 MB | **2.5 MB** |
| Deno worker, node compatibility on, idle | 12.4 MB | **7.4 MB** |
| Deno worker with `async-mqtt` required | 29 MB | **~17 MB** |
| The `node` sidecar, two modules loaded | — | **~70 MB** |

(RSS, idle, measured by spawning 0–32 of each. The first row is a `deno_core` 0.408 binary
against this workspace's own prebuilt V8; the middle two are Deno workers, which are
`deno_runtime`'s `WebWorker` — the same object a module thread would be. `DEFAULT_MAX_HEAP` is a
ceiling, not a reservation, so V8 grows into it lazily and these are floors.)

Read across: a code-body isolate costs **2.5 MB**, and turning node compatibility on makes it
cost **7.4 MB** — three times as much, for a surface no code body may use. Merging the pools
therefore does not save the module pool's memory, it *spreads the module pool's cost across every
code worker*. Two code isolates and one module worker is ~22 MB; the same work merged into two
node-capable workers is ~24 MB. Merging is memory-neutral at best and worse as the code pool
grows, so there is nothing to buy with the coupling it would cost.

**The coupling it would cost.** A module is long-lived state and a code-body isolate is
disposable by design. `@saltcorn/mqtt` holds a module-level `client` — a socket with
`reconnectPeriod: 1000` and live `connect`/`message` callbacks that must survive between calls —
while the JS-slice watchdog is, in `sc_expr::code`'s own words, "the only instrument that stops
JavaScript, and a blunt one, because it stops the isolate and everything resident on it". At
`DEFAULT_CODE_WORKERS = 2`, one runaway `while(true)` in a trigger would have a coin-flip chance
of terminating the broker subscription. Today that chance is zero, and stopping a runaway body is
a routine handled event rather than an incident. Pinning modules to a dedicated isolate inside a
merged pool avoids it by rebuilding two pools with extra steps.

The bounds differ by an order of magnitude too — `DEFAULT_CODE_TIMEOUT` is 5 s over a 1 s slice,
against a module's 120 s because a Proxmox snapshot is slow — and `DEFAULT_MAX_HEAP` is 256 MB of
shared admission budget that a module buffering MQTT messages would eat, stopping code-body
admission for a reason no admin could see.

So: **same process, different pool**, and the module pool defaults to **one** worker, because
that is what the sidecar already is — one process holding every module, not one per module. What
is saved is the process: ~70 MB and a second V8, heap and GC, against ~17 MB for the worker that
replaces it.

What *is* worth sharing is a level down: one Deno-capable runtime type, one extension set and one
snapshot, with two pools and two sets of bounds over it. That is roughly what `CodeRuntime` and
the formula isolate already are to each other.

The per-call cost of the pipe is the least of it and this list should not pretend otherwise: 2000
round trips through the newline-JSON protocol complete in well under a second, and a module
action is somebody else's network, next to which a JSON serialise and a pipe write do not
register.

### 1a. The node globals are shadowed, not deleted

Node compatibility is on for the module pool's isolates, and `process`, `Deno`, `Buffer` and
`require` are on their `globalThis`. Code bodies do not run there — but the fence is worth
stating exactly, because "fenced off" can mean two things and only one of them is available.

A **sound** fence would be a separate V8 context per tier: its own global object and its own
intrinsics. V8 has them; `deno_core` 0.408 does not expose them — `JsRealm` is `pub(crate)` and
`main_context()` is the only public accessor. Deletion is not a substitute (shared
`Object.prototype`, `Function("return globalThis")()`, closure references retained by loaded
module code), so nothing here should be described as a boundary.

It does not need to be one. A trigger's code body and a module install are behind the **same
admin check**, and installing a module already runs arbitrary code as the server — so an admin
who could write `require("node:fs")` in a code body already has that capability by other means.
The fence is hygiene, not privilege, and hygiene is had cheaply: `__scInvoke`'s wrapper already
controls the scope a body is compiled in, so the node globals are **shadowed as parameters** of
that wrapper (`async function body(process, Deno, Buffer, require, module, exports, __dirname,
global)`). Lexical, no deletion, no effect on module code, escapable through `globalThis` but not
by accident — which is the whole of what is wanted.

### 2. The sandbox the sidecar does not have

§15.1 ends: "Installing a module runs arbitrary code as the server … There is no sandbox." That
sentence is a consequence of `node`, which has no permission model to ask for. Deno does:
`deno_permissions::PermissionsContainer` is a per-worker argument, so a module's worker can be
given a net allow-list and denied the filesystem, the environment and subprocesses — the module
that declares it talks to one broker gets one broker.

Two honesty constraints on that claim, both of which must reach the screen:

- **`npm install` is still unsandboxed.** Install scripts run as the server, before any worker
  exists. This milestone does not change that half and must not imply it did.
- **A permission set is only as good as what defaults to it.** A module installed with no
  declared permissions gets the *closed* set, and widening it is an admin action on the Modules
  tab with the module's own reason next to it — not a flag that quietly defaults open.

### 3. `node_modules` stays exactly where it is (byonm)

npm remains the installer, unchanged: `--install-links`, the `@saltcorn/*` overrides, the stub
packages, the name recovered by diffing `dependencies`. All three findings from the modules
milestone still hold and none of them is about the runtime.

What resolves a `require("async-mqtt")` at run time is
`deno_resolver::npm::ByonmNpmResolver` — "bring your own node_modules", the mode Deno uses
against a directory somebody else installed. No Deno npm cache, no lockfile, no registry client
inside the server, and the modules root on disk after this milestone is the same directory it
was before. That is what keeps the change to one layer.

### 4. `process.exit()` must close a worker, not the server

Today a module that calls `process.exit()` takes the sidecar down, its co-residents' in-flight
calls are failed by name, and the next call starts a fresh process that replays every load. That
behaviour is a *feature* of being a separate process and it is the thing most easily lost by
moving in-process — `deno_os`'s `op_exit` calls `std::process::exit`, which in this server is
the server.

It is not lost, because Deno already solved it for its own workers: `deno_os` exposes
`setExitHandler`, `op_exit` fires only when no handler is installed, and `deno_runtime`'s worker
bootstrap installs one that calls `workerClose()` (`js/99_main.js`). `deno_node`'s
`process.exit` polyfill goes through `Deno.exit` and therefore through that handler, and its own
comment says so. So a module host worker installs an exit handler, and the restart-and-replay
logic that `sc_module::host` already has is kept verbatim — it just restarts a thread instead of
a process.

### 4a. `functions`, and the fifth host surface

A v1 plugin supplies `functions` as well as `actions`, and v1 makes them "available to formulas
and code actions" (`docs/Saltcorn1_description.md`). Nothing is loaded from that key today, and a
code body cannot call one. Three shapes exist in real plugins and the design has to carry all
three:

```js
// @saltcorn/nominatim-geocode — declared, async, over a module-level geocoder
functions: { geocode_lat: { run: async (query) => …, isAsync: true,
                            arguments: [{ name: "query", type: "Object" }] } }
// @saltcorn/markdown — a bare *synchronous* function over a module-level markdown-it
functions: { md_to_html: (m) => md.render(m || "") }
// @saltcorn/large-language-model — a function of the module's own configuration
functions: (config) => ({ llm_generate: { run: async (prompt, opts) => …, isAsync: true } })
```

**The isolate split is not what makes this a hop — module state is.** Every one of those closes
over something built at load time: a `Nominatim`, a `markdown-it`, the module's configuration. A
module is loaded **once**, with one configuration, holding one set of state; code bodies run on a
**pool**. So the function has to execute on the isolate where its module was loaded no matter how
the pools are arranged — merging them would mean loading the module once per code worker, which
is N geocoders, N configurations and N MQTT connections. The hop is inherent to modules being
singletons, and splitting the pools neither creates it nor makes it more expensive.

**So it is the fifth host surface, and the seam is the one already used four times.** `db`,
`fetch`, `fs` and `trigger` are each a trait taking one JSON plan and answering one JSON value,
each with its own budget, each bound only when a host is present. `functions` is a
`ModuleFnHost` of exactly that shape:

```json
{ "module": "@saltcorn/nominatim-geocode", "function": "geocode_lat",
  "args": [ { "street": "…", "city": "…" } ], "timeout_ms": 4750 }
```

`trigger` is the precedent to copy rather than a new problem: a code body **already** reaches
module code across a boundary today, because an admin can wire a trigger to a module action and
`trigger("…")` runs it — over a pipe, into another process. In-process that call gets cheaper,
not harder.

**What v1's own vocabulary gives for free.** `isAsync` says which functions v1 itself treated as
awaitable, and `arguments: [{ name, type }]` is the same v1 field vocabulary `sc_module::spec`
already translates — so a function's signature can be surfaced in the code editor's generated
types (§12.2) without inventing anything.

**Four limits, to be written down rather than discovered:**

- **In a code body they become awaitable.** `md_to_html` is synchronous in v1 and cannot be over
  a seam. In a body that is `await modfn.md_to_html(x)`, and `SETUP`'s `DbPromise` already exists
  to make a forgotten `await` a named error rather than `[object Promise]`. It is still a v1
  behaviour difference and the Modules tab should say so. In a **formula** they stay synchronous,
  by §4b.
- **Arguments and results are JSON.** A function taking a callback, or returning a stream, does
  not cross — `llm_text_to_speech` returns one when `opts.stream` is set. That path fails with a
  sentence naming why, not with a mangled value.
- **N+1 is the caller's to avoid.** A body looping a module function over a thousand rows is a
  thousand round trips; the call budget that already bounds `db` for exactly this reason bounds
  this too.

### 4b. Synchronous functions in a formula: prefetched, exactly as a Ⱶ-join is

A formula is synchronous, pure, op-less and bounded at 250 ms, and `@saltcorn/markdown`'s
`md_to_html` is a synchronous function that v1 makes available to formulas. Those two facts look
like a contradiction and are not, because **this problem is already solved once in this
codebase** and the second answer is the same as the first.

A Ⱶ-join is I/O. The evaluator does none. The way that is reconciled today is that the join path
is a *static* fact of the syntax, collected at parse time by `analyze`, resolved by the caller
before the formula runs (`sc_catalog::prefetch_bindings`, driven by `analysis.join_paths`), and
bound into `FormulaCall::row` as an ordinary scope entry — the evaluator sees a value and never
knows there was a query. `FormulaCall::row` documents it in those words: "including one entry per
Ⱶ-join identifier the formula uses … since the evaluator does no I/O".

A module function call is the same shape of problem. `md_to_html(notes)` is a static fact of the
syntax; `analyze` already walks `Ast::Call`. So it is collected into the analysis beside
`join_paths`, `prefetch_bindings` resolves it against the module pool, and the result is bound as
a scope entry with the call rewritten to name it. **The formula isolate stays exactly as pure as
it is today** — no ops, no module surface, no node compatibility, no 250 ms budget spent waiting
on anybody.

`isAsync` does not enter into it: the I/O happens outside the evaluator either way, so a declared
-async function hoists exactly as a declared-sync one does. What decides whether a call may be
hoisted is *syntax*:

- **Hoistable**: arguments that are columns, literals, Ⱶ-join identifiers, or pure expressions
  over those. A call inside a conditional hoists too — both branches are evaluated, which is
  wasted work for a pure function and never a wrong answer.
- **Not hoistable**: a call inside a lambda (`items.map(md_to_html)`), where the arity is not
  known until the formula runs, and arguments that depend on the formula's own computation.
  **Refused on save**, with a sentence naming the call and why — never silently evaluated to
  something else.

**Ownership formulas refuse module functions outright**, hoistable or not. `JsEvaluator`'s
contract is that `Err` is deny — fail closed — so an ownership rule calling `geocode_lat` turns a
Nominatim outage into "nobody may read anything", and every row read waits on a third party.
Calculated fields, `only_if` and the other formula uses take the hoist; the one that decides
authorization does not.

### 5. What the dependency costs, stated plainly

`deno_runtime` is not a small addition and the phase 0 gate below exists because of it.
Measured against this workspace:

- **+356 crates.** The workspace's dependency tree is 403 crates; `deno_runtime`'s is 634; the
  union is 759.
- **There are no feature knobs to trim it.** `deno_runtime`'s only features are `docsrs`,
  `exclude_runtime_main_js`, `hmr`, `snapshot` and `transpile`. Every extension is
  unconditional, so a Saltcorn module that publishes MQTT links WebGPU (`wgpu`, `naga`), FFI,
  N-API, `deno_kv`, `deno_cron`, the image and canvas stack, the WebSocket and HTTP servers and
  a REPL line editor (`rustyline`).
- **Version lock.** `deno_runtime` 0.263 requires `deno_core ^0.408`, which is this workspace's
  current pin — they line up today. But every `deno_runtime` release bumps `deno_core`, so from
  this milestone on `sc-expr` and `sc-module` are pinned together and upgrade together.
- **Build memory.** The profile note in the root `Cargo.toml` already records that ~110 test
  binaries each statically linking V8 pushed this machine into `systemd-oomd`. This adds to the
  same link. The mitigation is that the dependency is `sc-module`'s alone and behind a feature,
  so only the binary and `sc-module`'s own tests pay it — but it must be measured, not assumed.

### 6. The startup a snapshot pays for

Without a build-time snapshot, a Deno worker evaluates the whole runtime's JavaScript from
source at start. `deno_runtime`'s `snapshot` feature exists for this and takes a `build.rs`;
`WorkerOptions::startup_snapshot` takes the result. The first version of this may go without one
— the pool is started lazily and a deployment with no modules never starts it, which is the same
bargain the sidecar already makes — but the cost must be measured and written down, because "the
first module call after a restart takes two seconds" is the kind of thing that is discovered by
a user rather than by a test.

---

## Phase 0 — The spike, and the gate

This phase is allowed to end in "no". Nothing in phases 1–5 is worth starting if the numbers
here come back wrong, and the sidecar is a working system with no defect driving this change.

- [ ] A throwaway binary in `/tmp` (not a workspace member): `deno_runtime` 0.263 + `deno_core`
      0.408, one worker with node compat on, `ByonmNpmResolver` over a modules root this repo's
      installer built, running `module-host.mjs` unmodified and answering one `load` for
      `@saltcorn/mqtt`. This is the whole risk of the milestone in one file.
- [ ] Measure and record, in this list: clean build wall time and peak RSS of the link; the
      binary's size before and after; a worker's start-to-first-`load` latency with and without
      a snapshot; and the process RSS with two modules loaded, against the ~70 MB the sidecar
      measures.
- [ ] Confirm the four wirings the spike needs are as small as they look:
      `NodeRequireLoader` (five methods, four with usable defaults),
      `NodeExtInitServices { node_require_loader, node_resolver, pkg_json_resolver, sys }`,
      a `ModuleLoader`, and `sys_traits`' `RealSys` for `ExtNodeSys`.
- [ ] Confirm the exit handler: a module calling `process.exit(3)` closes its worker and leaves
      the spike's process alive. Use the `echo-module` fixture's `echo_exit`, which exists for
      exactly this.
- [ ] **Gate.** Write the go/no-go here with the numbers next to it. A "no" is a legitimate
      outcome and the rest of this file is then archived unstarted, with the measurements kept —
      they are the answer to the next person who asks this question.

## Phase 1 — The runtime (`sc-module`, behind a feature)

- [ ] `deno-host` feature on `sc-module`, off by default, turned on by `sc-server`. The crate
      must still build and test without it, because that is what keeps the dependency out of
      every other crate's test link.
- [ ] `sc_module::deno`: the worker pool. One thread per worker, a current-thread tokio runtime
      and a Deno worker on each, `DEFAULT_CODE_WORKERS`-shaped sizing with its own admin
      setting, and a module pinned to a worker so its `require` cache and its module-level state
      live in one place — which is what the single sidecar gives it today.
- [ ] `NodeRequireLoader`: `load_text_file_lossy`, `ensure_read_permission` against the module's
      own `PermissionsContainer`, and the two `is_maybe_cjs` questions answered from
      `package.json`'s `type` via `PackageJsonResolver`.
- [ ] `ByonmNpmResolver` over `<modules root>/node_modules`, plus the `NodeResolver` and
      `PackageJsonResolver` it needs.
- [ ] The exit handler (specification §4), and the restart-and-replay path from
      `sc_module::host` moved onto it: a dead worker fails its in-flight calls by name and the
      next call gets a fresh worker that replays every load.
- [ ] The four bounds a module call is under, matching what `CodeRuntime` already does rather
      than inventing a second vocabulary: the wall clock (`DEFAULT_CALL_TIMEOUT`, 120 s, and it
      stays 120 s — a Proxmox snapshot is not fast), a JS slice enforced with
      `terminate_execution`, a heap limit, and the worker restart as the backstop. A module that
      spins forever must cost its own worker and no one else's.

## Phase 2 — The host protocol becomes a function call

- [ ] `module-host.mjs` keeps its shape but loses its transport: the `load` / `run` / `unload`
      handlers become an exported entry point the Rust side calls, and the newline-JSON framing,
      the stdout rebinding and the `readline` loop go. The three tiers of `@saltcorn/*` stubs,
      the `Module._load` patch, `Workflow`, `Form`, `interpolate` and the manifest shape are
      untouched — that code is proven portable (see the measurements above) and this milestone
      has no business editing it.
- [ ] `ModuleHost` keeps its public surface — `load`, `run`, `unload`, `manifest` — so
      `ModuleServices`, `ModuleAction`, `sc-server`'s five endpoints and the four-step reload
      do not know the runtime changed. The `host.rs` module doc is rewritten; nothing that calls
      it is.
- [ ] `sc-expr`: `__scInvoke`'s wrapper shadows the node globals as parameters (specification
      §1a). Worth doing even though code bodies do not run on the module pool's isolates — it
      costs one signature and makes `process` in a code body a `ReferenceError`-shaped mistake
      rather than a silently working one if the pools are ever brought closer.
- [ ] Test (`sc-expr`): a code body naming `process`, `require` or `Buffer` sees `undefined`,
      and one naming `db` still works.
- [ ] `console.log` from a module goes to `sc-log` directly rather than to a forwarded stderr,
      tagged with the module's name.
- [ ] Delete the child-process path. Prototype status, no compatibility shim, no `--node-host`
      escape hatch: two runtimes for the same thing is two things to debug.

## Phase 2a — `functions`: what a module supplies besides actions

- [ ] `module-host.mjs`: `functions` joins `supportedKeys`, resolved the three ways v1 allows
      (bare function, `{ run, isAsync, description, arguments }`, and a function of the module's
      configuration), and reported in the manifest with its declared signature.
- [ ] `ModuleFnHost` in `sc-expr` beside `TriggerHost`: one plan in, one JSON value out, its own
      call budget, bound only when a host is present.
- [ ] The prelude binds it under a reserved name, resolved per module so two modules may each
      supply `geocode_lat` without one shadowing the other.
- [ ] `sc-api` implements the host over `ModuleServices`, routing to the worker the named module
      is loaded on — which is where its state is.
- [ ] The generated code-editor types carry each function's `arguments` and `description`, so a
      body's author sees the signature (§12.2).
- [ ] `sc-expr::analyze`: module-function calls collected beside `join_paths`, with the
      hoistable/not-hoistable classification of §4b decided there, at parse time.
- [ ] `sc_catalog::prefetch_bindings` resolves them against the module pool and binds the results
      into `FormulaCall::row`, with the call rewritten to name the binding. The evaluator gains
      nothing — no op, no surface, no node compatibility.
- [ ] `Formula::validate` refuses, on save: a call that cannot be hoisted (naming it and why),
      and any module function at all in an **ownership** formula (naming the fail-closed reason).
- [ ] Test (`sc-expr` + `sc-catalog`): `md_to_html(notes)` in a calculated field producing the
      module's own output with the formula isolate still op-less; the same call inside a
      `.map()` refused on save; a module function in an ownership formula refused on save; and a
      hoisted call whose module is not loaded failing the formula rather than answering `null`.
- [ ] The three refusals of specification §4a are refusals with sentences: a function used in a
      **formula**, a non-JSON argument, and a result that will not serialise.
- [ ] Test: `@saltcorn/markdown`'s `md_to_html` and `@saltcorn/nominatim-geocode`'s
      `geocode_lat` (against a stub HTTP server) called from a `run_js_code` body; a module
      function that closes over the module's configuration seeing the configured value; two
      modules supplying the same function name; and a body that forgets the `await` getting the
      `DbPromise` error rather than a promise in a string.

## Phase 3 — Permissions

- [ ] `_sc_modules` gains the module's permission set: a net allow-list, a read/write path
      allow-list, and the environment variables it may see. Closed by default (specification
      §2).
- [ ] `PermissionsContainer` per module, built from that row, handed to the module's worker.
- [ ] `ui/admin` Modules tab: the permissions a module has, editable, with the sentence about
      `npm install` still running as the server kept and kept accurate.
- [ ] A denied permission is an error that names what was denied and what to allow, not an
      `EACCES` from inside somebody's dependency.

## Phase 4 — Tests

- [ ] `sc-module`'s existing suites (`host.rs`, `install.rs`, `module_actions.rs`,
      `module_store.rs`) pass unchanged against the new runtime. They are the specification of
      what a module host does and they should not need editing; anything that does need editing
      is a behaviour change that has to be justified here.
- [ ] A test that no `node` process exists while a module runs, and that the server starts and
      serves a module with `node` removed from `PATH` (npm is still needed to *install*).
- [ ] `echo_exit`: the worker dies, its in-flight call is failed by name, the co-resident
      module's call is answered, the next call succeeds against a replayed worker, and the
      server is still up.
- [ ] A runaway module (a `while(true)`) is stopped by the JS slice, costs its own worker, and
      does not delay a code body on the `CodeRuntime` pool — the two pools are independent and a
      test should say so.
- [ ] A permission test: a module denied the filesystem cannot read the modules root; a module
      allowed one host cannot reach a second.
- [ ] An integration test against a real broker for `@saltcorn/mqtt` (the spike proves this
      works; the suite should keep it working), skipped when no broker is configured.

## Phase 5 — Documentation

- [ ] `docs/TECHNICAL_DESIGN.md` §15.1: rewritten. The section currently argues *for* the
      sidecar and names `deno_runtime` as the thing it is not; both halves change, and the
      paragraph that says there is no sandbox is replaced by one that says precisely what is
      sandboxed and what is not.
- [ ] `docs/tutorial-modules.md`: `node` is no longer a runtime requirement, npm still is, and
      the permissions screen.
- [ ] `README.md`: the requirements table.
- [ ] CHANGELOG entry.

---

## Explicitly OUT of scope for this milestone

- **Modules on the `CodeRuntime` isolates.** Specification §1. A user's code body does not get
  `require`, and no measurement changes that.
- **Replacing the `@saltcorn/*` stub tier with the real API.** `Table`, `File`, `User`,
  `getState` still throw, still naming themselves. That is the `CodeHost` seam in the other
  direction and it is its own milestone — §15.1 already says so, and this milestone deliberately
  does not touch `module-host.mjs`'s stub table so the two changes stay separable.
- **The entity types this version still does not load.** `viewtemplates`, `table_providers`
  and `eventTypes` are still censused and still not loaded (`functions` are no longer among
  them — phase 2a). Nothing about the runtime was what
  stopped them.
- **Sandboxing `npm install`.** Install scripts still run as the server. Specification §2 says
  so on the screen rather than quietly.
- **Native (N-API) addons.** Deno supports N-API and a module with a `.node` addon may well
  work, but nothing in the two real modules exercises it, so it is untested and undertaken by
  nobody. If one turns up it is a bug report with a name on it, not a supported path.
- **Deno as a *language* for modules.** A module is still a v1 CommonJS npm package. TypeScript
  sources, `deno.json`, JSR specifiers and `npm:` imports are not a thing a module may use.
