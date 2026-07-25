# Saltcorn v2 — Actions & Triggers TODO

Ordered, checkable task list for the fourth milestone after the MVP. Earlier lists are archived
in [docs/TODO-mvp.md](./docs/TODO-mvp.md) (the MVP),
[docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md) (file stores + the React framework),
[docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md) (the `_sc_tables`/`_sc_fields` overlays,
rich types and File fields) and [docs/TODO-post-mvp-3.md](./docs/TODO-post-mvp-3.md) (ownership
formulae, calculated fields and row-level security); scope and rationale remain in
[docs/GOALS.md](./docs/GOALS.md) and [docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md)
(§10.1 and §10.2, which this milestone rewrites as implemented).

**Milestone definition of done:** an admin creates a **trigger** in the admin UI — a name, an
**event**, and one configured **elementary action** — and it fires. Table `insert`/`update`/
`delete` events fire it for any table, subject to an **"only if"** formula over the affected row;
`login`, `startup` and `error` (application *and* system) events fire it with no table involved;
a `none` trigger has no intrinsic event and is run on demand — from the admin UI, and from an
**application's API**, because an application picks the triggers it exposes exactly as it picks
its tables, and the generated TypeScript client gets a typed method for each. In the last phase
**periodic** triggers (`often` every five minutes, plus `hourly`/`daily`/`weekly` with an
admin-configured time) fire on schedule from a scheduler the server owns.

**Elementary actions only.** Workflows and agents are the *other* two trigger bodies (§10.2) and
neither lands here: this milestone builds the event → action half, and does it so that
`TriggerBody::Workflow` slots in beside `TriggerBody::Action` later without reshaping the event
model, the storage or the admin UI. The durable workflow engine (§10.3) is a milestone of its
own, and so is the message bus it runs on.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## Decisions taken up front

These are the choices the phases below assume. They are recorded here because each one is a
place where a different answer would have produced a different plan.

1. **Dispatch is synchronous and after the write commits.** A table event fires once the row
   operation has succeeded, and the request awaits its triggers. There is no durable queue yet
   (that is the workflow milestone's `sc-bus` + run store), and the alternative — spawning a
   detached task — buys latency at the cost of losing events on shutdown and of tests that race.
   A failing action is logged as an application error and does **not** roll back the write
   (it cannot: the write is committed) and does **not** fail the request. Exception: a trigger
   run *directly* (the API/admin "run" path) returns the action's error to its caller, because
   there the action **is** the request.
2. **The row layer is the one choke point.** `sc-api::rows`' create/update/delete are what every
   write goes through — REST, the admin row editor, File-field uploads — so that is where table
   events are emitted. Writes that bypass it (metadata bootstrap, a provider used directly) do
   not fire triggers, which is the correct behaviour for `_sc_*` tables and a documented limit
   for anything else.
3. **The "only if" formula is `sc-expr`, minus the operation flags.** Same language as ownership
   formulae and calculated fields — the row's fields, `user`, Ⱶ-join paths and Ↄ-aggregations —
   evaluated **reified** on the row the event carries. The flags are refused by name: the event
   already *is* the operation, so `_insert` in an insert trigger is a tautology and in a delete
   trigger a lie.
4. **A trigger is cached, like every other created entity** (GOALS): the trigger set is loaded
   once and reloaded on save/delete, so firing an event is a lookup in memory, not a query.
5. **Recursion is bounded, not forbidden.** An action that writes a row fires that table's
   triggers; the event carries a depth, and exceeding the limit (default 5) is an application
   error naming the chain. Forbidding it outright would rule out the common "denormalise into a
   second table" trigger.
6. **Periodic triggers are UTC and single-node.** Times are configured and interpreted in UTC —
   a per-trigger timezone is a real feature but it is not this milestone's, and "the server's
   local time" is a footgun on any deployment that moves. A missed run (server down) fires once
   at startup via a persisted `last_run_at`, and nothing coordinates two nodes; that is the
   durable queue's job.
7. **One scope rule for every formula in a trigger.** Ownership formulae and calculated fields
   established that **bare identifiers name the fields of the row the formula is about**; an
   action's `where` is about the *target* table's rows, not the triggering row, so it keeps that
   rule and the triggering row becomes **ambient**, spelled as an object exactly as `user`
   already is:

   | spelling | meaning |
   |---|---|
   | bare `status` | a field of the row this formula ranges over — the affected row in `only_if`, the target table's row in a `where` |
   | `row.status` | the triggering event's row (`payload` for a non-table event) |
   | `old.status` | the pre-update row on an `update` event; `null` on every other event |
   | `user.email` | the caller, unchanged from ownership formulae |

   So `only_if` on an update is `status === 'done' && old.status !== 'done'`, and an
   `update_rows` `where` on a *different* table is `project === row.id && status === 'draft'`.
   A formula that ranges over **no** table — an `insert_row` value, a `fetch` body — therefore
   has no bare field identifiers at all: it is written `row.title`, `user.email`. The rule in one
   line: *bare is the row the formula is about; dotted is the row that caused it.* `Ⱶ`-joins and
   `Ↄ`-aggregations work off the bare scope as they always have.

   Mechanically this is one generalisation of existing code, not a new language: `sc-expr`'s
   `UserEnv` already inlines exactly one ambient object (`user`) for the symbolic evaluator and
   binds it for the reified one, so it widens to a **set** of ambient objects (`user`, `row`,
   `old`) with the same inlining, the same null semantics (`old.x` on an insert is null, like an
   anonymous `user.x`) and the same validation (an ambient member that is not a field of its
   table is named as an error).

---

## Phase 1 — The `sc-action` crate: actions, events, the run context

A new workspace crate at **layer 6** (design §2), depending on `sc-catalog`, `sc-expr`,
`sc-query`, `sc-types` and `sc-error` — and deliberately **not** on `sc-auth` or `sc-api`: an
event carries the caller as a role plus a JSON user object (exactly what a formula binds), which
is all an action needs and keeps the crate below the API layer that will call into it.

- [ ] Workspace member `crates/sc-action`, added to `Cargo.toml`'s members and
  `[workspace.dependencies]`, with the layer-6 comment the other layers carry.
- [ ] `Event` — the event *model*, split from the trigger that listens for it:
  `EventKind` (`Insert`/`Update`/`Delete`/`None`/`Login`/`Startup`/`Error`/`Often`/`Hourly`/
  `Daily`/`Weekly`) plus an `Event` value carrying `channel: Option<String>` (the table name for
  table events), `row`/`old_row` (`Option<Json>`), `payload: Json` (an API-run trigger's body,
  an error event's `{kind, message, …}`), the caller (`role: u8`, `user: Option<Json>`) and
  `depth: u8`. `EventKind` round-trips through a lowercase string — it is stored in a column and
  posted by the SPA.
- [ ] `Action` trait per §10.1, object-safe and `async_trait`:
  `name()`, `description()`, `config_spec() -> Vec<FormField>` (the admin form is rendered from
  it, exactly as a file-store backend's is) and
  `run(&self, ctx: &mut ActionContext<'_>) -> Result<Json>`. The returned JSON is the action's
  result: the response body of a directly-run trigger today, a workflow step's context
  contribution tomorrow.
- [ ] `ActionContext<'_>` — `&Catalog`, the `&Event`, the trigger's `config: &Attrs`, the
  evaluator (`Option<&Arc<dyn JsEvaluator>>`, absent in contexts that have no engine, and then a
  configuration error rather than a silent skip), and a `Json` context object the action may
  read and write (the seam the workflow engine's run context grows into).
- [ ] `ActionRegistry` — name → `Arc<dyn Action>`, with `builtin_actions()` returning the
  registry Phase 3 fills. An unknown action name is a configuration error naming it and the
  registered alternatives, never a silently skipped trigger.
- [ ] Unit tests: registry lookup and the unknown-name error, `EventKind` string round-trip
  (including an unknown kind refused by name), the depth limit refused at the boundary with the
  chain named, and a hand-rolled test action asserting the `ActionContext` contract.

## Phase 2 — `_sc_triggers`: the trigger model, storage and validation

The trigger is stored metadata with nothing to introspect it from, so its row *is* its
definition — the `_sc_applications` / `_sc_file_stores` pattern, strict reads included (a
missing or wrong-shaped column is an error naming the trigger and the column, not a default).

- [ ] `Trigger` (pure data, `sc-action`): `id: TriggerId` (UUID, §9), `name` (unique — it is
  what an application, an API path and a "run" button reference), `description`, `when:
  EventKind`, `channel: Option<String>`, `only_if: Option<String>`, `action: String`,
  `configuration: Attrs`, `min_role: Option<u8>` (who may run it through an API),
  `attributes: Attrs` (sparse: the periodic timing of Phase 8, `enabled`, `last_run_at`).
- [ ] `_sc_triggers` table + `bootstrap_triggers` (idempotent, §9 columns: `id`, `name`,
  `description`, `attributes`), plus `save_trigger` / `load_trigger` / `load_trigger_by_name` /
  `list_triggers` / `delete_trigger`, each mirroring `file_stores.rs`.
- [ ] **Validation on save**, so a broken trigger is refused at the boundary rather than
  discovered at fire time: the action name resolves in the registry; the configuration validates
  against its `config_spec`; a table event names a channel that is a real table (and a non-table
  event names none); the `only_if` formula parses and validates against that table's shape with
  `user` allowed and the operation flags **refused by name** (decision 3) — the `calc.rs`
  "validate like an ownership formula, minus …" pattern, generalised so there is one validator
  with a scope policy rather than three near-copies.
- [ ] `Triggers` — the cached registry: `Triggers::load(&catalog)` builds it, `reload()` refreshes
  it after a save/delete, and lookup is by `(kind, channel)` for events and by name for direct
  runs. A stored trigger that fails validation at load is **dropped with a reported issue** and
  never fires (fail closed, as an invalid ownership formula grants nothing), exposed for the
  admin UI to surface.
- [ ] **`sc-expr`: ambient objects, generalising `UserEnv`** (decision 7). One inlined ambient
  object becomes a named set — `user`, `row`, `old` — across validation (each object's members
  checked against its table's fields, an unknown one named), the symbolic translator (each
  member inlines as a literal, exactly as `user.x` does today; `old.x` is null when there is no
  old row) and the reified binder (each object bound, or `null`). The `Guc` env is untouched:
  RLS policies have no triggering row, so only `user` has a GUC. Parity tests extended over the
  new objects, since parity is the gate everything downstream stands on.
- [ ] **Move the reified-binding prefetch down into `sc-catalog`** (`prefetch.rs`): the Ⱶ-join
  and Ↄ-relation resolution that `sc-api::ownership` built for the write path is exactly what
  an `only_if` evaluation needs, and `sc-action` sits below `sc-api`. One implementation, two
  callers; `sc-api` keeps its ownership logic and calls the moved helper.
- [ ] Tests: round-trip a trigger through the store (including strict-read refusals for a
  mangled column); each validation refusal by name (unknown action, bad config, table event
  with no channel / a missing table, non-table event with a channel, an `only_if` naming an
  unknown field, an `only_if` using `_insert`); an invalid stored trigger dropped from the
  registry with its issue reported.

## Phase 3 — The elementary actions

Deliberately few (GOALS: "the number of built-in actions should be minimal"); control flow is
the workflow engine's job, not an action's. Every action's configuration value that references
the event is a **formula** in the same `sc-expr` language under decision 7's scope rule — so one
language spans ownership, calculated fields, only-if and action configuration.

- [ ] `insert_row` — target table, and a field→formula map. The formulas range over no table, so
  they read the event ambiently (`row.title`, `user.id`); the computed values go through the
  ordinary `rows` write path so type coercion, File-field validation and the target table's own
  triggers all apply (this is the recursion decision 5 exists for).
- [ ] `update_rows` — target table, a `where` formula, and a field→formula map of assignments.
  Both are written in the target table's scope (`project === row.id`, `count + 1`).
- [ ] `delete_rows` — target table and a `where` formula, same scope.
- [ ] **The `where` semantics, decided once for both:** the predicate **selects** rows — it is
  translated to a SQL `WHERE` when it translates (`UserEnv` inlining the event's row and user as
  literals, so no new SQL construct is needed) and falls back to fetch-then-filter through the
  reified evaluator when it does not, exactly as ownership reads do. The matched rows are then
  written **one at a time through the `rows` layer by primary key** rather than as one bulk
  statement: that is what makes each affected row's own triggers fire with its own row payload
  (decision 2), and it is why an untranslatable `where` costs nothing extra — the rows are
  being fetched either way. A target table with no single-column primary key is refused at save,
  not at fire time.
- [ ] `fetch` — an HTTP request to a configured URL: method (default `POST`), headers, and a
  formula-computed JSON body (defaulting to the event). The **parsed response body is the
  action's result**, which is what earns the name over `webhook` — a directly-run trigger can
  return it to its caller, and a workflow step will put it in the context. A non-2xx response is
  an application error naming the status; the timeout is bounded and configurable.
  **Dependency decision to record:** an HTTP *client* is new to the workspace — `reqwest` with
  `rustls-tls` and `default-features = false`, no OpenSSL, matching §16's pure-Rust TLS posture.
- [ ] `run_js_code` — a JavaScript **code body** (statements, `return`) run in the existing
  `deno_core` isolate with the event's row, `user` and payload in scope, returning JSON. This
  extends `JsEvaluator` with a third method (`run_code`) beside `eval`/`eval_value`, on the same
  thread/watchdog/sandbox machinery. **Bounded on purpose:** no host API, so the code cannot
  read or write the catalog — that is the `sc-code` JS adapter's milestone (§15), and this is
  its seed.
- [ ] Tests: one per action against a real database — rows inserted/updated/deleted with formula
  values; the `where` selecting exactly the intended rows, asserted **twice** over the same case
  (a translatable predicate through SQL and an untranslatable one through the fallback, same
  rows); the scope rule pinned by a test where the target table and the event's table share a
  field name and `status` vs `row.status` select differently; a `fetch` received by a one-shot
  local listener with its response body returned as the action's result (and a non-2xx surfaced
  as an error); `run_js_code` returning a computed value plus a sandbox assertion that it cannot
  reach the host.

## Phase 4 — Table events: insert, update, delete

- [ ] **The emit seam.** `sc-catalog` defines a small `TableEvents` trait (one `async` method
  taking table name, operation, new row, old row and the caller) and a `Catalog` holds an
  optional `Arc<dyn TableEvents>`, installed once at boot with `sc-action`'s dispatcher. This
  inverts the layering cleanly — the catalog knows *that* writes are observable, `sc-action`
  knows *what* observing means — and costs no signature churn on the eight `rows` entry points.
- [ ] **The caller travels with every write.** `CallerContext` (role + user JSON) is built for
  *every* row write, not only for RLS tables, so an event knows who caused it; RLS keeps using
  it for its GUCs, unchanged.
- [ ] Emit from `rows::create_row_ctx` / `update_row_guarded` / `delete_row_guarded` after the
  statement succeeds — the three sites decision 2 names. Update carries **both** rows (the
  action and the only-if see `old_row`), delete carries the row as it was.
- [ ] Dispatch: look up triggers for `(kind, table)`, evaluate `only_if` reified against the row
  (bindings from the Phase 2 prefetch; an evaluator error is an application error and the
  trigger does **not** run — the same fail-closed contract ownership has), then run the action
  with the depth incremented.
- [ ] Integration tests through the real router: an insert trigger writing an audit row; an
  update trigger seeing `old_row`; a delete trigger; an `only_if` that gates firing (fires for
  matching rows, silent for others) including one using a Ⱶ-join path; the depth limit stopping
  a self-feeding trigger with the chain named; a failing action logged without failing the
  request or losing the write.

## Phase 5 — The other events: none, login, startup, error

- [ ] **`none`** — no intrinsic event: runnable by name through `sc-action`'s `run_trigger`,
  which Phase 6 (admin) and Phase 7 (API) both call. The posted body becomes the event payload;
  the action's result is the response.
- [ ] **`login`** — fired from the login handler on a successful authentication (admin login and
  an application's own `login` endpoint), carrying the user.
- [ ] **`startup`** — fired once by `sc-cli`/`sc-server` after the catalog, applications and
  triggers are up, before the listener is announced ready.
- [ ] **`error`** — fired where an `Error` becomes a response, carrying `kind`
  (`application`/`system`, from the existing `ErrorKind` split), the message, and the route/user
  context. **Re-entrancy is guarded**: an error raised while handling an error event does not
  fire another, or a misconfigured trigger becomes an infinite loop at the worst moment. The
  `_sc_errors` log itself (§16) is *not* in this milestone — the event is.
- [ ] Tests: a login trigger recording logins; a startup trigger observed to have run once the
  server is up; an error trigger firing for a forced application error *and* a forced system
  error, with the re-entrancy guard asserted by a trigger whose action itself throws.

## Phase 6 — Admin API and admin SPA

- [ ] Admin endpoints on the existing typed `Endpoint` machinery: `listTriggers`,
  `createTrigger`, `updateTrigger`, `deleteTrigger`, `runTrigger` (admin-only "test this now",
  returning the action's result or its error), and `listActions` (each action's name,
  description and `config_spec`, so the SPA renders the configuration form from the server's
  description rather than a hard-coded one). Regenerate the checked-in TS client (the drift test
  is the gate).
- [ ] Admin SPA: a `Triggers.tsx` list (name, event, channel, action, validity badge) and a
  `TriggerForm.tsx` editor — event picker, table picker shown only for table events, `only_if`
  textarea (monospace, with the server's validation message surfaced inline, as the ownership
  formula card does), action picker, and the action's config form rendered from `config_spec`;
  a **Run** button for a `none` trigger showing the result; the periodic timing inputs land in
  Phase 8. Navigation entry alongside Tables/Applications; `tsc --noEmit` and `vite build` pass.
- [ ] Tests: HTTP round-trip create/list/update/delete; each save refusal surfaced by name with
  nothing stored; `runTrigger` returning the action's result and an action error reported as an
  error (not a 200 with a hidden failure).

## Phase 7 — Applications and APIs pick triggers

- [ ] `Application.triggers: Vec<TriggerRef>` — the app's exposed subset, stored in a new
  `triggers` column on `_sc_applications`. Because the design bans migrations for now, the
  bootstrap gains **additive reconciliation**: a declared column absent from an existing
  `_sc_*` table is created (`Catalog::create_field`), so an existing database keeps working
  without a migration framework and without a hand-edited schema.
- [ ] `RestProvider` projects one endpoint per included trigger —
  `POST {mount}/actions/{name}`, body → event payload, action result → response — carrying the
  trigger's `min_role` as its `AuthRequirement` — defaulting to **admin** when unset, so a
  trigger nobody has thought about the access of is never accidentally public. Endpoint naming
  follows `op_name`, so the generated client gets a typed `runFoo(body)`.
- [ ] Wiring: `app_providers_with` resolves the declared triggers against the registry (an
  undeclared/unknown trigger is a configuration error, exactly as a missing table is);
  `AppMounts` re-projects mounted apps when a trigger changes, as it already does for tables.
- [ ] Application form in the admin SPA gains a trigger picker beside the table picker; the
  generated app client is emitted with the new methods.
- [ ] Tests: an app exposing a `none` trigger, called over HTTP at its mount with a payload and
  returning the action's result; role enforcement on that endpoint; a trigger *not* included in
  the app returning 404; the generated client containing the typed method; the additive
  bootstrap adding the column to a pre-existing `_sc_applications` table.

## Phase 8 — The periodic scheduler

- [ ] Timing configuration in the trigger's attributes, validated on save: `often` (every five
  minutes, no configuration), `hourly` (minute past the hour), `daily` (hh:mm), `weekly`
  (day-of-week + hh:mm) — all UTC (decision 6), refused when out of range.
- [ ] `Scheduler` — one tokio task started at boot, waking on the minute boundary, computing
  which triggers are due from their configuration and last run, and firing them through the same
  dispatch path as every other event. Overlapping runs of one trigger are skipped rather than
  queued (a slow action must not stack up), and a firing error is logged without stopping the
  scheduler.
- [ ] `last_run_at` persisted on the trigger row, so a run missed while the server was down
  fires **once** at startup rather than being lost or fired N times; a fresh trigger's first due
  time is computed from now, not from the epoch.
- [ ] Admin SPA: the timing inputs for each periodic kind, and the last-run time shown in the
  list.
- [ ] Tests: a unit test of the due-time computation over a table of (kind, config, now,
  last_run) cases including day/week rollovers and DST-free UTC arithmetic; an integration test
  driving the scheduler's clock so an `often` and a `daily` trigger fire exactly when they
  should; the catch-up-once-at-startup case; the overlap skip.

## Phase 9 — Documentation

- [ ] `docs/TECHNICAL_DESIGN.md`: §10.1/§10.2 rewritten as implemented (the event model, the
  `Action` trait as built, the storage, the fire path and its choke point, only-if, the
  recursion bound, the scheduler, and what `TriggerBody::Workflow` will need); the crate map in
  §2 gains `sc-action`.
- [ ] A tutorial in the established style (`docs/tutorial-triggers.md`): a table trigger with an
  only-if writing an audit trail, a `none` trigger called from an app's API, and a daily
  periodic trigger — cross-linked from the ownership tutorial, with the hygiene test extended.
- [ ] CHANGELOG entries as each phase lands.

---

## Carried past this milestone

- **Workflows and agents as trigger bodies** (§10.3, §11) — the durable engine, runs, traces,
  suspension and versioning. This milestone's `TriggerBody` is action-only, shaped so they slot
  in beside it.
- **The durable queue and multi-node dispatch** — needs `sc-bus`; until then dispatch is
  in-process and the scheduler is single-node.
- **The `_sc_errors` error log** (§16) — the error *event* lands here, the persisted log does
  not.
- **Host-API JavaScript actions** — `run_js_code` is sandboxed and pure; catalog access belongs
  to `sc-code`'s adapters (§15), as do actions written in Python/Rust/Go.
- **`send_email`** and the system email settings (v1's most-used action) — it needs an SMTP
  dependency and a configuration surface of its own.
- **Validate variants of table events** (v1's `Insert Validate` etc., which can *veto* a write)
  — they need a before-commit hook, which is a different contract from decision 1's
  after-commit dispatch.
- **The event log** (v1's configurable auditing of every event) and per-trigger run history.
- **A per-trigger timezone** for periodic events.

## Explicitly OUT of scope for this milestone

- **Buttons that run actions in views/pages** — there is no view layer yet.
- **Custom user-defined event types and channels beyond table names.**
- **Trigger tags, import/export and cloning.**
- Everything still listed as out of scope in [docs/TODO-mvp.md](./docs/TODO-mvp.md),
  [docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md),
  [docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md) and
  [docs/TODO-post-mvp-3.md](./docs/TODO-post-mvp-3.md)
