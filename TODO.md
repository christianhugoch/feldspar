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

- [x] A throwaway binary in `/tmp` (not a workspace member): `deno_runtime` 0.263 + `deno_core`
      0.408, one worker with node compat on, `ByonmNpmResolver` over a modules root this repo's
      installer built, running `module-host.mjs` unmodified and answering one `load` for
      `@saltcorn/mqtt`. This is the whole risk of the milestone in one file.
- [x] Measure and record, in this list: clean build wall time and peak RSS of the link; the
      binary's size before and after; a worker's start-to-first-`load` latency with and without
      a snapshot; and the process RSS with two modules loaded, against the ~70 MB the sidecar
      measures.
- [x] Confirm the four wirings the spike needs are as small as they look:
      `NodeRequireLoader` (five methods, four with usable defaults),
      `NodeExtInitServices { node_require_loader, node_resolver, pkg_json_resolver, sys }`,
      a `ModuleLoader`, and `sys_traits`' `RealSys` for `ExtNodeSys`.
- [x] Confirm the exit handler: a module calling `process.exit(3)` closes its worker and leaves
      the spike's process alive. Use the `echo-module` fixture's `echo_exit`, which exists for
      exactly this.
- [x] **Gate.** Write the go/no-go here with the numbers next to it. A "no" is a legitimate
      outcome and the rest of this file is then archived unstarted, with the measurements kept —
      they are the answer to the next person who asks this question.

### What was built

`/home/tomn/spike-deno/` on this machine — outside the repository and not a workspace member,
per the bullet above. Not in `/tmp`, which here is a 16 GB tmpfs: a `deno_runtime` target
directory is 3–6 GB and tmpfs pages are RAM, which on the machine described in the root
`Cargo.toml`'s profile note is how a build closes the terminal window rather than failing.

Four things live there and all of them are throwaway:

| | |
|---|---|
| `modules-root/` | built by **this repo's own `Installer`** (`--install-links`, the `@saltcorn/*` overrides, the stub packages), holding `@saltcorn/mqtt` 0.2.0, `@saltcorn/proxmox` 0.2.2 and the `echo-module` fixture, with `module-host.mjs` copied in as the server writes it |
| `spike/` | `deno_runtime` 0.263, no snapshot — 423 lines of `main.rs` |
| `spike-snapshot/` | the same, plus the §6 `build.rs` snapshot |
| `control/` | `deno_core` 0.408 and nothing else: what `sc-expr`'s `eval` feature already links, so the deltas below are against **what the server already pays** rather than against zero |

The spike keeps `module-host.mjs`'s transport as well as its text: a pair of `std::io::pipe`s
are handed to the worker as its stdio and a reader thread sits on the far end, which is the
sidecar's newline-JSON protocol with the process taken out. Nothing in the host script was
edited.

### It works, and the manifests are byte-identical

`@saltcorn/mqtt`, `@saltcorn/proxmox` and the `echo-module` fixture all load in-process, and
their manifests — the actions, the `configFields`, the flattened `configuration_workflow`, the
`unsupported` census — are **byte-identical** to what `node module-host.mjs` answers for the
same three `load` requests. `Module._load`, `createRequire`, the three tiers of stubs,
`Workflow`, `Form` and `interpolate` all behave as they do under node; `require("async-mqtt")`
resolves out of the npm-built `node_modules` through `ByonmNpmResolver`.

Two differences, both cosmetic and both worth knowing:

- A `run` of `mqtt_publish` against an unconfigured module fails with the **same message** under
  both, but the stack differs: Deno gives CommonJS frames `file://` URLs where node gives bare
  paths, and there is no `processTicksAndRejections` frame.
- `process.version` reports Deno's node-compatibility version (`v26.3.0`) rather than the
  installed node's (`v22.22.0`).

And the milestone's headline claim holds already: the spike loads and runs both modules with
**no `node` on `PATH` at all** (`env -i PATH=/tmp/nonode`, a directory containing one symlink to
`timeout`).

### The four wirings

Smaller than they look, and none of them needed a workaround:

| wiring | cost |
|---|---|
| `ModuleLoader` | **22 lines**: `FsModuleLoader` plus one branch, because a `node:` specifier is its own canonical form and `deno_core`'s module map answers it from the `lazy_loaded_esm` registry the `deno_node` extension registered. `module-host.mjs`'s four `import`s need nothing else. |
| `NodeRequireLoader` | **40 lines**, three methods implemented. `ensure_read_permission` is handed the module's own `PermissionsContainer` on every read, which is exactly where phase 3 goes. |
| `NodeExtInitServices` + `ByonmNpmResolver` | **29 lines**, including the `NodeResolver` and `PackageJsonResolver` it needs. `root_node_modules_dir` is the modules root's own `node_modules` and nothing else was configured. |
| `RealSys` for `ExtNodeSys` | one line. |

Two things the list did not predict and phase 1 must know:

- **`WorkerOptions::bootstrap.has_node_modules_dir` must be `true`**, or `require` looks for a
  Deno npm cache that does not exist.
- **The `transpile` feature is mandatory without a snapshot.** `deno_runtime`'s extension
  sources are TypeScript; with no snapshot to hold the transpiled form, the first worker dies
  with `SyntaxError: Unexpected token ':'` in `ext:deno_bundle_runtime/bundle.ts`. With a
  snapshot, the residual `lazy_loaded_*` sources the snapshot did not consume have to be
  transpiled **by the build script** before they are embedded, or the first `node:` import is a
  `SyntaxError` in `node:console`.

### `process.exit(3)`

Confirmed, with the `echo-module` fixture's `echo_exit`, and it confirms more than the bullet
asked for. Both modules are loaded and answering; then `echo_exit` is fired and a `ping` to the
co-resident `@saltcorn/mqtt` is fired straight after it, with the host's stdin deliberately held
**open** so that the module's own exit is the only thing that can end the run:

```
[   59.4 ms] {"id":1,...}   @saltcorn-test/echo loaded
[  105.3 ms] {"id":4,"ok":true,"value":{"pong":true,"node":"v26.3.0"}}   the co-resident call
requests sent / replies seen:    4 / 3      the exiting call is lost, and only it
op_exit ran before the last step: true
exit code the module asked for:  Some(3)    the module's own code, not an EOF's 0
the spike's own process reached the end of main(), so it is alive.
```

So: the exiting call is lost, its co-resident's call is answered, and the server does not exit —
which is the sidecar's behaviour, kept. **But the mechanism is not the one §4 names, and §4
should be corrected before phase 1 builds on it:**

- §4 is right that `deno_os`'s `op_exit` fires only when no handler is installed, and right that
  `deno_node`'s `process.exit` goes through `Deno.exit` and therefore through it.
- The handler that calls `workerClose()` is installed by `deno_runtime`'s **`WebWorker`**
  bootstrap. A bare `WebWorker` cannot simply be driven with `execute_main_module` +
  `run_event_loop`: it panics `coding error: either js is polling or the worker is terminated`,
  because it expects the worker-host plumbing (the message-polling loop) around it. Phase 1's
  pool has to supply that, or use the other door.
- The other door, which is what the spike used, is `deno_os::WatcherExitHandle`: put one in the
  worker's `OpState` and `op_exit` calls `terminate_execution` on that isolate and sets a
  `WatcherExited` marker instead of calling `std::process::exit`. Two lines, and it works on a
  `MainWorker`.
- **The catch, and it is the one thing here that would have been discovered in production:**
  `terminate_execution` only throws out of *running* JavaScript. A module that exits while the
  worker's event loop is parked on an idle resource leaves the loop parked — the spike hung for
  120 s the first time. `WatcherExited` in `OpState` is the host's evidence that the exit
  happened, and the host, not V8, is what must then drop the worker. Phase 1's
  restart-and-replay path needs that poll.

### The numbers

Measured on this machine (12 cores, 31 GB), release profile, medians of three runs. "Sidecar"
is the current `node module-host.mjs` doing the same two loads.

**Start-to-first-`load`** — spawn (or worker construction) to the first reply on stdout:

| | worker construction | host script evaluated | first `load` answered |
|---|---|---|---|
| `node` sidecar (today) | — | — | **54 ms** |
| Deno worker, **no snapshot** | 159 ms | 204 ms | **255 ms** |
| Deno worker, **with a snapshot** | **9.7 ms** | 56 ms | **105 ms** |

The snapshot is worth 150 ms of every worker start and it is not optional in the way §6
suggested it might be: the difference between 9.7 ms and 159 ms is the difference between a
worker restart being invisible and being a hiccup, and the restart path is on the
`process.exit()` road above.

**RSS**, same runs:

| | process baseline | + worker, no modules | + two modules loaded |
|---|---|---|---|
| `node` sidecar (today) | — | — | **67.2 MB** (whole process) |
| Deno worker, no snapshot | 12.2 MB | 65.3 MB | **83.9 MB** |
| Deno worker, with a snapshot | 12.2 MB | 60.0 MB | **79.5 MB** |
| `deno_core` control | 4.8 MB | 25.8 MB (one bare isolate) | 57.5 MB (eight isolates) |

**This is the measurement that does not come back the way §1 predicted, and the list should say
so rather than quietly keep the old table.** §1 has a Deno worker with node compatibility on at
12.4 MB first and 7.4 MB marginal, and concludes the sidecar's ~70 MB and its second V8 are what
the change buys back. Measured here, a snapshot-backed worker with two real modules on it costs
**67 MB of RSS inside the server** (79.5 − 12.2) against the sidecar's **67 MB in its own
process**. Gross, that is a wash.

The saving is real but it is *net*, and it is smaller: the control says the **first** V8 in a
process costs ~21 MB (4.8 → 25.8 MB) and each isolate after it ~4.5 MB, and the server already
pays that first 21 MB for `sc-expr`'s code isolates. So the honest figure is **~45 MB of RSS and
one process**, not ~50 MB and a second V8. (§1's marginal-cost table was measured on Deno
*workers inside an already-running `deno`*, where the runtime's snapshot is already resident and
shared; a worker that is the first thing in its process does not get that.)

**Build cost**, clean, `CARGO_INCREMENTAL=0`, under `scripts/cargo-guarded.sh`:

| | crates | wall | peak toolchain RSS (sum / largest process) | binary (stripped) |
|---|---|---|---|---|
| `deno_core` control | 561 in the lock file | **30 s** | 2.4 GB / 0.78 GB | 66.6 MB (**47.4 MB**) |
| + `deno_runtime`, no snapshot | 824 | **257 s** | 6.1 GB / 1.04 GB | 164.6 MB (**118.2 MB**) |
| + `deno_runtime`, with snapshot | 824 | **302 s** | 6.9 GB / 1.89 GB | 172.1 MB (**125.2 MB**) |

So `deno_runtime` costs **+263 crates in the lock file, +4.5 minutes of clean build, ~2.5× the
build's peak memory, and +78 MB of stripped binary** over what the workspace already links.
(The debug profile with the workspace's own `debug = 0` budget builds the same tree in 84 s at
4.0 GB, so this is not the ~110-test-binary link problem the root `Cargo.toml` warns about — it
is one more large link, not a hundred.)

**A cost §5 does not list, and should:** `deno_cache`, `deno_kv`, `deno_node_sqlite` and
`deno_webstorage` all require `rusqlite/session`, which turns on
`libsqlite3-sys/preupdate_hook` → `buildtime_bindgen`. **`libclang` becomes a build requirement
of the Saltcorn server**, along with the C headers bindgen needs, and there is no feature knob
to turn it off — the same "every extension is unconditional" that §5 already records about the
Rust side is true of the C toolchain. This machine has no clang at all; the spike borrowed
`libclang.so` out of PyPI's `libclang` wheel and gcc's own freestanding headers to get a
measurement. A deployment would install `libclang-dev`. It belongs in `README.md`'s requirements
table (phase 5) and in CI.

### Gate: **go**, with two amendments to the specification above

The risk this phase existed to retire is retired: the JavaScript half is portable, the wirings
are ninety lines, the modules load and run in-process with no `node` anywhere, and a module that
calls `process.exit()` costs its own worker and nothing else.

But the *reason* stated in §1 has to change, because the memory arithmetic did not survive
contact:

1. **§1's table is wrong for this use and should be replaced by the one above.** Merging the
   pools is still the wrong answer, and every word of §1's coupling argument still holds — a
   module is long-lived state, a code isolate is disposable, and the JS-slice watchdog stops the
   isolate and everything on it. That argument is sufficient on its own and it is what should
   carry §1. What should not be repeated is "what is saved is ~70 MB and a second V8": what is
   saved is ~45 MB and a second *process*, and the honest case for this milestone is **the
   `node` runtime requirement and the permission model of §2**, not memory.
2. **§6's "the first version of this may go without a snapshot" is withdrawn.** Without one a
   worker start costs 159 ms and 5 MB more, and the `transpile` feature has to be on anyway, so
   the snapshot's `build.rs` is the cheaper of the two roads, not the later one. Phase 1 takes
   the snapshot with it.

   **Reversed by phase 2, and the numbers are not why.** V8 shares one read-only heap per
   process and the first isolate built decides it, so a snapshot-backed worker built after one
   of `sc-expr`'s bare `deno_core` isolates aborts the process. The spike could not see this
   because it linked `deno_runtime` and nothing else. See phase 2's own notes below; §6's
   original bargain — start lazily and pay the 150 ms once — is what stands.

The costs to accept, stated plainly so nobody is surprised in phase 5: **+78 MB of stripped
binary, +4.5 minutes of clean build, +263 crates, and `libclang` as a new build-time
requirement.** If any one of those is unacceptable, this is the point to stop — the sidecar
works, and the four bullets above are the answer to the next person who asks.

## Phase 1 — The runtime (`sc-module`, behind a feature)

- [x] `deno-host` feature on `sc-module`, off by default, turned on by `sc-server`. The crate
      must still build and test without it, because that is what keeps the dependency out of
      every other crate's test link.
- [x] `sc_module::deno`: the worker pool. One thread per worker, a current-thread tokio runtime
      and a Deno worker on each, `DEFAULT_CODE_WORKERS`-shaped sizing with its own admin
      setting, and a module pinned to a worker so its `require` cache and its module-level state
      live in one place — which is what the single sidecar gives it today.
- [x] `NodeRequireLoader`: `load_text_file_lossy`, `ensure_read_permission` against the module's
      own `PermissionsContainer`, and the two `is_maybe_cjs` questions answered from
      `package.json`'s `type` via `PackageJsonResolver`.
- [x] `ByonmNpmResolver` over `<modules root>/node_modules`, plus the `NodeResolver` and
      `PackageJsonResolver` it needs.
- [x] The exit handler (specification §4), and the restart-and-replay path from
      `sc_module::host` moved onto it: a dead worker fails its in-flight calls by name and the
      next call gets a fresh worker that replays every load.
- [x] The four bounds a module call is under, matching what `CodeRuntime` already does rather
      than inventing a second vocabulary: the wall clock (`DEFAULT_CALL_TIMEOUT`, 120 s, and it
      stays 120 s — a Proxmox snapshot is not fast), a JS slice enforced with
      `terminate_execution`, a heap limit, and the worker restart as the backstop. A module that
      spins forever must cost its own worker and no one else's.

### What was built, and three things the specification had wrong

`crates/sc-module/src/deno/` — the pool (`mod.rs`), one worker's life (`worker.rs`) and the four
wirings (`wiring.rs`) — plus `crates/sc-module/build.rs` for the snapshot and
`crates/sc-module/src/bounds.rs` for the four bounds. `sc-server` turns the feature on and adds
`--module-workers <n>`, defaulting to one. `ModuleServices` still holds the sidecar: switching
it over is phase 2's first two bullets, and this phase deliberately stops short of them so that
"the runtime changed" and "the protocol changed" are two bisectable commits.

The transport is still the pipe, unedited on the JavaScript side: `module-host.mjs`'s
portability is what phase 0 proved, and a phase that changed the runtime *and* the protocol
could not say which half broke. In-process that costs two helper threads per worker — a
blocking reader and a blocking writer on the two pipe ends — so that the thread which must drain
the pipe is never the thread blocked on writing to it. Both go with the pipe in phase 2.

**1. `deno_core` does not report a V8 termination through `run_event_loop`.** The list assumed a
JS slice enforced with `terminate_execution` would surface as an error from the event loop, the
way it surfaces to `sc_expr`'s worker. It does not: `do_js_event_loop_tick` sees
`is_execution_terminating`, returns `Ok(false)` — "no ops" — and the loop goes back to sleep with
a dead isolate under it, forever. This is the same shape as phase 0's finding about
`WatcherExited` and has the same answer: **the watchdog's own flag is the evidence, and the host,
not V8, ends the worker.** The worker thread reads both on one 50 ms tick.

**2. A module's JS slice is 10 s, not the code pool's 1 s, and it is measured differently.**

- *Differently*, because a code body is entered and left by the worker that runs it, so its
  slice can be armed around the call. A module host is never "entered" — its JavaScript runs
  whenever a socket says so. What is observable instead is the worker **thread**, which comes
  round its loop every 50 ms unless a poll of the event loop has entered JavaScript and not come
  back. So the thread stamps the watchdog each time round, and a stamp older than the slice means
  uninterrupted JavaScript and nothing else.
- *Ten seconds*, because of `require`. A module's `load` synchronously pulls a whole npm
  dependency tree through V8's parser — `async-mqtt` and what it brings is tens of thousands of
  lines with no `await` anywhere in it. A one-second slice would not be a watchdog; it would be
  a rule that large modules may not be installed. Ten seconds is still an order of magnitude
  inside the 120 s wall clock, so a runaway is stopped by the slice rather than waited out by
  the caller.

**3. A runaway must be written as one.** The `echo-module` fixture's runaway action *computes*
rather than looping on nothing: `for(;;){}` is exactly the shape V8 is free to compile with no
interrupt check in it, so a test asserting that an empty loop is stoppable would be asserting
something V8 does not promise — while a module that hangs a server hangs it by computing.

### What it cost this workspace, measured

§5 said the build memory "must be measured, not assumed". Measured:

- **The lock file goes 561 → 1005 packages (+444)**, against the +263 phase 0's bare-`deno_core`
  control predicted — the control shared none of this workspace's own tree, so its delta was the
  smaller one. §5's "+356" and the gate's "+263" are both estimates of this number; this is the
  number.
- **+10.6 MB per test binary, +5 %.** One `sc-server` integration test binary is 212.6 MB with
  the feature off and 223.2 MB with it on (debug, `line-tables-only`). Far less than phase 0's
  +78 MB stripped, and for a reason that expires: nothing in `sc-server` *calls* the pool yet, so
  the linker drops most of `deno_runtime`, and V8 was already there for `sc-expr`. Phase 2 makes
  the call real and this number will grow toward phase 0's.
- **The 47-way link burst needs headroom.** `cargo test -p sc-server --no-run` under
  `scripts/cargo-guarded.sh`'s **default** `SC_BUILD_MEM_HIGH=10G` and default `-j` made no
  progress in 40 minutes on this machine: 48 concurrent links, each now mapping a much larger
  set of rlibs, sat in continuous reclaim. With `-j 4` (or a raised `MemoryHigh`) it completes.
  That is the root `Cargo.toml`'s profile note coming true again, and it belongs in README §10
  in phase 5.

## Phase 2 — The host protocol becomes a function call

- [x] `module-host.mjs` keeps its shape but loses its transport: the `load` / `run` / `unload`
      handlers become an exported entry point the Rust side calls, and the newline-JSON framing,
      the stdout rebinding and the `readline` loop go. The three tiers of `@saltcorn/*` stubs,
      the `Module._load` patch, `Workflow`, `Form`, `interpolate` and the manifest shape are
      untouched — that code is proven portable (see the measurements above) and this milestone
      has no business editing it.
- [x] `ModuleHost` keeps its public surface — `load`, `run`, `unload`, `manifest` — so
      `ModuleServices`, `ModuleAction`, `sc-server`'s five endpoints and the four-step reload
      do not know the runtime changed. The `host.rs` module doc is rewritten; nothing that calls
      it is.
- [x] `sc-expr`: `__scInvoke`'s wrapper shadows the node globals as parameters (specification
      §1a). Worth doing even though code bodies do not run on the module pool's isolates — it
      costs one signature and makes `process` in a code body a `ReferenceError`-shaped mistake
      rather than a silently working one if the pools are ever brought closer.
- [x] Test (`sc-expr`): a code body naming `process`, `require` or `Buffer` sees `undefined`,
      and one naming `db` still works.
- [x] `console.log` from a module goes to `sc-log` directly rather than to a forwarded stderr,
      tagged with the module's name.
- [x] Delete the child-process path. Prototype status, no compatibility shim, no `--node-host`
      escape hatch: two runtimes for the same thing is two things to debug.

### What was built, and the measurement that undid phase 1's snapshot

`module-host.mjs` ends in `globalThis.__scModuleHost(id, request)` and nothing else; the Rust
side calls it through `deno_core::scope!` with the request handed over by `serde_v8`, and the
answer comes back through two native functions installed on the isolate's global before the
script is evaluated. `ModuleHost` is now a façade over `DenoModuleHost` (and, on a build without
`deno-host`, a refusal that names the missing feature); `ModuleServices::install` takes
`--module-workers` and sizes the pool with it. The pipes, their two helper threads and
`tokio::process` are gone from the host.

**Native functions rather than ops, which was not a preference.** An op is reached from
JavaScript through `Deno.core.ops`, and `deno_runtime`'s worker bootstrap ends by calling
`removeImportedOps()` — which deletes every entry there that is not on its own allow-list. A
module host's ops are gone before its main module runs. `v8::Function::new` plus a `v8::Global`
is smaller anyway, and `JsRuntime::op_state_from(scope)` gets the callback back to the worker's
channel.

**`run_event_loop` returning `Ok(())` stopped being an ending.** Over the pipe it could not
happen — a `readline` on stdin is a resource — so phase 1 treated it as one. With no transport
there is nothing to hold the loop open between calls, and "the event loop has nothing to do" is
what an idle module host *is*. The branch is taken out of the `select!` until the next call into
the isolate gives it something to drive; leaving it in is a spin at the speed of the scheduler.

**And the snapshot phase 1 built has to go, which reverses the phase 0 gate's second
amendment.** The measurement behind it stands — 9.7 ms of worker construction with a snapshot
against 159 ms without — but the snapshot cannot be used in *this* process, for a reason the
spike could not show because the spike linked `deno_runtime` and nothing else:

- **V8 shares one read-only heap across every isolate in a process, and the first isolate built
  decides it.** A `deno_runtime` worker built from a custom startup snapshot *after* a bare
  `deno_core` isolate exists aborts the process inside V8's own deserializer
  (`vector.h:415: libc++ Hardening assertion __n < size() failed`, SIGABRT). `sc-expr`'s code
  isolates are bare `deno_core` with no snapshot, so in a server the two pools cannot both have
  their way. Phase 1 could not see it because nothing called the pool; phase 2 saw it as
  `cargo test -p sc-server --test modules_api` aborting on the first test that loads a module.
- **The reverse order works and is not a fix.** A custom snapshot's read-only heap *is* the
  embedded one's, so a bare isolate built afterwards is content — but both pools are lazy by
  design (a deployment with no modules never builds a module isolate; one with no code bodies
  never builds a code one), so "the module pool goes first" is an invariant nothing could keep.
- So `crates/sc-module/build.rs` is deleted, `startup_snapshot` is `None`, and a worker starts
  from V8's embedded snapshot, transpiling `deno_runtime`'s TypeScript extension sources as it
  goes — which is why the `transpile` feature is still not optional. The cost is ~150 ms and
  ~5 MB on the first module call after a server start, once per worker, on a path that already
  waits on npm and somebody else's network. Measured here as the `deno_host` suite going from
  2.6 s to 6.8 s, which is eight worker starts.

The one thing the phase 4 bullet about "existing suites pass unchanged" has to be told: they do,
apart from the skip condition. `tests/host.rs` and `tests/module_actions.rs` skipped without
`node && npm` and now skip without `npm` alone, because that is the behaviour change this
milestone exists to make.

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
