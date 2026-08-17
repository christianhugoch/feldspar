# Saltcorn v2 — Tables in code: `db` in `run_js_code`

Ordered, checkable task list for the eleventh milestone after the MVP. Earlier lists are
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
and indexes) and [docs/TODO-post-mvp-10.md](./docs/TODO-post-mvp-10.md) (email). Scope and
rationale remain in [docs/GOALS.md](./docs/GOALS.md) and
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md).

This milestone gives `run_js_code` **tables**. Today the action is deliberately pure: no host
API, so a code body can compute over the event and nothing else. That bound was the right one
to ship with and is the wrong one to keep — the escape hatch that cannot read a row is an
escape hatch for arithmetic. What lands here is the seam §15's `sc-code` adapters were always
going to need, arrived at from the one guest language the server already runs.

**Milestone definition of done:** an admin writes a trigger whose `run_js_code` body reads,
joins, aggregates and writes —

```js
const overdue = db.invoices
  .where({ paid: false, due: { lt: payload.today } })
  .select("id", "amount", "customerⱵemail", { chased: "remindersↃinvoice.length" })
  .orderBy("due")
  .limit(50)
  .rows();

for (const inv of overdue) {
  db.reminders.insert({ invoice: inv.id, sent_to: inv.customerⱵemail });
}
return { chased: overdue.length, owed: db.invoices.where({ paid: false }).sum("amount") };
```

— presses **Run**, and the rows are there. The same body written with `db.asUser()` reads
exactly what the person who caused the event may read and no row more, refusing a write their
ownership formula does not grant. Every `.insert()` fires the table's own triggers, is bounded
by the cascade depth guard, and a body that forgets a `.limit()` on a million-row table gets a
named error rather than an isolate that dies.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## The specification

### 1. What is bound

One binding is added to the `run_js_code` scope, beside `row`, `old`, `user` and `payload`:

```js
db.table("invoices")   // the general form — any table name
db.invoices            // sugar: a Proxy over the same call
```

`db` exists **only** in a code body. A formula — an ownership formula, an `only_if`, a
calculated field, a `{{ }}` token — evaluates in the pure isolate it always did, where
`typeof db === "undefined"` and there are no ops at all.

### 2. Reading

Chain methods are pure and return a new builder; **terminals execute**. The chain mirrors
[`sc_query::Select`](./crates/sc-query/src/statement.rs) field for field — `filter`,
`columns`, `order`, `limit`, `offset`, `group`, `having` — and the terminals reuse the names
the Ↄ-aggregation chains already have in the formula language.

```js
const rows = db.books
  .where({ author: "Woolf", pages: { gt: 200 } })
  .select("id", "title", "publisherⱵname")
  .orderBy("published", "desc")
  .limit(10)
  .offset(20)
  .rows();
```

| chain method | meaning |
| --- | --- |
| `.where(cond)` | restrict; repeated calls **AND** |
| `.select(...cols)` | projections: field names, Ⱶ-paths, and `{ alias: "formula" }` objects |
| `.orderBy(field, dir?)` | `"asc"` (default) or `"desc"`; repeated calls append keys |
| `.limit(n)` / `.offset(n)` | the bound |
| `.groupBy(...fields)` | grouping (phase 6) |
| `.asUser()` / `.asAdmin()` | authority (§5) |

| terminal | result |
| --- | --- |
| `.rows()` | array of row objects |
| `.first()` | one row object or `null` (`LIMIT 1`) |
| `.get(pk)` | the row with that primary key, or `null` |
| `.count()` | number |
| `.sum(f)` `.avg(f)` `.min(f)` `.max(f)` | value or `null`; `f` is a field name or a formula |
| `.exists()` | boolean |
| `.insert(v)` `.update(v)` `.delete()` | writes (§4) |

### 3. The two spellings of a filter, and formula projections

`where` takes **either** the object DSL every other surface already speaks — the REST query
string, the GraphQL `where`, the agent tools — **or** a formula string, which is what the
`update_rows` and `delete_rows` actions take:

```js
.where({ status: "draft", pages: { gte: 100 }, id: { in: [1, 2, 3] } })
.where('status === "draft" && ordersↃcustomer.length > 3')
```

Both lower to one [`sc_query::Expr`](./crates/sc-query/src/expr.rs) through the one
vocabulary in [`sc_api::filter`](./crates/sc-api/src/filter.rs), so `eq`, `is_null`,
`like` and the rest mean in a code body exactly what they mean in a URL. The object form
grows `and`, `or` and `not` keys **in that shared module**, so the REST, GraphQL and agent
filters gain them at the same moment and by the same code:

```js
.where({ or: [ { status: "draft" }, { and: [ { status: "sent" }, { paid: false } ] } ] })
```

A projection may be a formula, which is where joins and child aggregations enter a select:

```js
db.customers.select(
  "id", "name",
  { city:  "addressⱵcity" },                       // Ⱶ  → correlated scalar subquery
  { spend: "ordersↃcustomer.sum(o => o.total)" },  // Ↄ  → correlated aggregate subquery
  { net:   "price * (1 - discount)" },             // an ordinary expression
).rows();
```

Each is an [`sc_expr::Formula`](./crates/sc-expr/src/formula.rs): parsed by `Formula::parse`,
validated against the catalog's `SchemaShape`, translated by `translate_value`, and projected
as a `RowQuery::extra` column — the same path a GraphQL `manager { email }` and a
non-stored calculated field already take. **There is one expression language**, and this is
it; a formula the translator refuses (one that needs the JS evaluator) is an error naming it
and saying to compute it in the code body instead, which costs the author nothing because the
code body is JavaScript.

`.sum("qty * price")`, `.orderBy("customerⱵname")` and a Ⱶ-path in a `where` key resolve the
same way.

### 4. Writing

```js
const created = db.books.insert({ title: "Orlando", author: "Woolf" });   // → the written row
const many    = db.books.insert([ { … }, { … } ]);                        // → array of rows
const upd     = db.books.where({ author: "Woolf" }).update({ shelf: 3 }); // → { updated: 2, ids: [3, 7] }
const del     = db.books.where({ id: 7 }).delete();                       // → { deleted: 1, ids: [7] }
```

- `.update()` or `.delete()` **with no `.where()` throws**. A whole table rewritten or emptied
  is not something an omitted call should be able to cause — §10.1 refuses exactly this on the
  `update_rows` and `delete_rows` actions, at save time, and the reason does not change when
  the caller is a code body.
- `{ updated, ids }` / `{ deleted, ids }` is the shape those two actions already return.
- Writes go through the row layer (`rows::create_row_ctx` / `update_row_ctx` /
  `delete_row_ctx`), so they are coerced against their columns, validated, File-field-checked,
  and **observed by triggers**. A write from a code body is an event like any other: it
  carries this trigger's chain, so `Event::firing`'s cascade bound applies and a body that
  writes the table that fired it is stopped where any other action would be.
- A bulk update or delete resolves its matched rows first and then writes them **one at a
  time through the row layer**, exactly as `update_rows` does — the events are the point.

### 5. Authority: admin by default, `asUser()` to delegate

**By default a code body's reads and writes are the admin's**, carrying the event's user.
This is the rule `rows_scope.rs` already states for `insert_row`/`update_rows`/`delete_rows`:
a trigger is server-side configuration, and an audit row the caller may not insert is the
archetype of what a trigger exists to write. Concretely: a `CallerContext` at `ROLE_ADMIN`
— which clears every RLS policy's role floor, as the admin API's own row editor does — with
the event's user still attached, so a policy that reads `user` sees who caused it.

**`asUser()` delegates to the caller instead**, and is available on the handle, on a table and
on a query; it sets one field of the plan, so where it appears in the chain does not matter:

```js
db.asUser().invoices.where({ paid: false }).rows();   // the whole handle
db.invoices.asUser().where({ paid: false }).rows();   // one table
db.invoices.where({ paid: false }).asUser().rows();   // one query
db.invoices.asAdmin().insert({ … });                  // the default, said out loud
```

Under `asUser()` every operation goes through
[`sc_api::ownership`](./crates/sc-api/src/ownership.rs)'s
`read_rows_as` / `aggregate_values_as` / `insert_row_as` / `update_row_as` / `delete_row_as`
at the event's own role and user — §7.3's rule, the same functions the agent tools use, with
no second implementation of "meets the floor OR the formula grants it" to be subtly wrong.
Which means, without this milestone writing any of it:

- a read is narrowed to the rows the ownership formula grants, translated into the `WHERE`
  where it can be and evaluated row by row where it cannot;
- an update is checked **twice** — on the row as it is and on the row as it would become — so
  it cannot move a row out of the caller's own ownership;
- a row the formula withholds is the same **not found** an absent row gets, so a delegated
  read cannot become a way to probe which rows exist;
- an RLS-enforced table is read and written inside a caller-context transaction and the
  database's own policies decide.

The event's caller is what it delegates to, and events differ: a table event or a
directly-run trigger carries the user who caused it; a **scheduled** or **startup** trigger
carries nobody, so `asUser()` there reads as the public role. That is the honest answer to
"on whose behalf" rather than an error — and it is why `asAdmin()` is the default.

A denial is an `Error::auth` thrown into the code body with its own message, catchable like
any other, so a body may try a delegated write and fall back.

One asymmetry to state rather than hide: a delegated **aggregate** over a table whose
ownership formula the translator refuses is an error (`aggregate_guard` will not fall back to
the evaluator — an aggregate over rows it cannot filter would silently count rows the caller
may not see). The message says to read the rows and aggregate in the code body.

### 6. Results, errors and bounds

- **Rows are the REST wire shape** — `sc_api::convert::value_to_json` — so a row means the
  same thing in `db.books.rows()` as it does over HTTP: a Decimal is exact, a Date is ISO.
- **Nothing reaches SQL as text.** A chain builds a plain plan object; the host resolves every
  table, column and join path through the catalog and lowers to a `Statement` whose literals
  are parameterised on render. There is **no raw-SQL escape hatch** in `db` (an admin who
  wants one has §13.4's custom SQL queries, which are governed).
- **The API is synchronous.** Nothing in the sandbox is awaitable today and nothing here
  changes that: `db.books.rows()` returns rows, not a Promise. A body that returns a Promise
  is still refused rather than stringified.

Three bounds, each with its own named error:

| bound | default | why |
| --- | --- | --- |
| rows per read | 1000 | a read is materialised into the isolate; an unbounded `.rows()` on a large table is an OOM, not a slow query. The error says to add `.limit()`. |
| host calls per run | 200 | an accidental N+1 loop must not hammer the database quietly. |
| wall clock per run | 5 s, `timeout_ms` on the action, hard max 60 s | a trigger runs inside the request or the write that fired it, so an unbounded body is an unbounded hold on that caller — the bound `fetch` already keeps, for the same reason. |

**No transactions in this milestone.** Each statement autocommits, as every action's writes do
today; a body that fails half way leaves the writes it already made, and its events have
already gone out. `db.transaction(fn)` is a later addition (the row layer's
`Executor::Transaction` is the seam it will use) and is out of scope here — see the end.

### 7. The host plan (the language-neutral seam)

The fluent surface is JavaScript; what crosses into Rust is one plain JSON object per
terminal, which is what makes this the seam §15's other adapters implement rather than a
JavaScript feature:

```json
{
  "op": "select",
  "table": "invoices",
  "authority": "admin",
  "where": { "paid": false, "due": { "lt": "2026-08-17" } },
  "select": [ "id", "amount", "customerⱵemail",
              { "alias": "chased", "formula": "remindersↃinvoice.length" } ],
  "order":  [ { "field": "due", "dir": "asc" } ],
  "limit": 50,
  "offset": 0
}
```

`op` is `select` | `aggregate` | `insert` | `update` | `delete`; `where` is either the object
DSL or `{ "formula": "…" }`; `aggregate` carries `[{ "alias", "fn", "arg" }]` and `group`
carries the grouping fields (phase 6); `values` carries an insert's row(s) or an update's
assignments. Every terminal is one plan and one round trip.

---

## Decisions taken up front

1. **A code body gets its own runtime, separate from the formula isolate.** The evaluator
   today is one V8 isolate on one thread serving every ownership check in the process, with
   no ops and a 250 ms watchdog. Giving *that* isolate a blocking host call would put every
   authorization decision on the server behind whatever a trigger's code is doing — and
   worse, it would **deadlock** the moment a delegated read's ownership formula needs the JS
   evaluator, because the thread waiting for the host call is the thread the formula would
   have to run on. So `CodeRuntime` is a small pool of isolates of its own (one op, its own
   watchdog, its own longer timeout) and `DenoEvaluator`'s stays pure. This is not
   scaffolding for this milestone: it is the runtime §15's JS adapter needs.
2. **The host call is synchronous, and blocks a code thread.** The op blocks its own isolate
   thread on the host's reply (`Handle::block_on`, legal because a code thread is not a
   tokio runtime thread) rather than making the guest API awaitable. Two consequences, both
   wanted: the guest language stays plain synchronous JavaScript, which is what an admin
   writing five lines in a form expects; and the thread that is blocked is one of a small
   pool nothing else depends on.
3. **`sc-expr` does not learn what a table is.** It sits below `sc-catalog` and `sc-api`, and
   it stays there: the runtime knows only a `CodeHost` trait taking JSON and returning JSON.
   The table knowledge — catalog lookups, formula translation, the row layer, the §7.3 rule —
   lives in `sc-api`, where all of it already is.
4. **The fluent surface is written in JavaScript, not generated from Rust.** The builder, the
   plan objects and the terminals are a prelude; Rust sees plans. Adding `.orderBy` later
   touches no Rust, and the same plans serve Python when §15 gets there.
5. **The prelude is built per run and cannot be poisoned.** Runs share an isolate, so a body
   that assigns to a global is visible to the next one. `db` is therefore constructed fresh
   inside each run's function scope, and the op handle and run wrapper are installed as
   non-writable, non-configurable globals. (Tampering could never *escalate* — the host
   re-validates every plan against the catalog and the authority — but a body that breaks the
   next trigger's `db` would be a bug nobody could find.)
6. **Admin by default, `asUser()` to delegate** — §5 above. Stated as a decision because the
   alternative is defensible and rejected: running as the event's user by default would make a
   trigger unable to write the audit row it exists to write, and would make the authority of a
   trigger depend on who happened to touch a row.

---

## Phase 1 — The code runtime (`sc-expr`)

- [x] **The `CodeHost` seam**: `#[async_trait] pub trait CodeHost { async fn call(&self,
      request: Json) -> Result<Json>; }` in `sc-expr`, and `CodeCall` gains
      `host: Option<Arc<dyn CodeHost>>`, a wall-clock `deadline` and the call budget. A
      `CodeCall` with no host is exactly today's pure body.
- [x] **`CodeRuntime`**: a pool of isolate threads (default 2, configurable), each built with
      one op `op_sc_db` and its own watchdog, fed by a job channel with a worker checkout.
      `JsEvaluator::run_code` dispatches here; `eval`/`eval_value` keep the existing pure
      isolate untouched. (Built lazily on the first code body: a process that never runs one
      should not pay for two more isolates to find that out.)
- [x] **The op**: blocks on `Handle::block_on(host.call(req))` with the handle captured when
      the job was submitted; **disarms the watchdog for the duration of the call** so a slow
      query is never reported as "your code timed out", and checks the run's wall-clock
      deadline and call budget on entry, returning a named error into JS when either is spent.
- [x] **Non-poisonable globals**: the op handle and the run wrapper installed with
      `writable: false, configurable: false`; the prelude emitted inside the per-run function.
      The fluent `db` builder itself (§2–§5's chain, terminals and `asUser()`) is that
      prelude, written in JavaScript and lowering to §7's plans — the host validates them
      from phase 2.
- [x] Tests: two code bodies run concurrently on the pool; a body that spins is terminated and
      the isolate recovers; a body that sleeps in the host does *not* count against the JS
      watchdog but does against the deadline; the **formula** isolate still has no `Deno`, no
      ops and no `db`.
- [x] *Not on the list, found on the way*: `deno_core` **aborts the process** if V8 posts a
      delayed task against an isolate built outside a tokio runtime context. Both engines now
      build theirs inside one and drop the guard immediately (which is also what leaves a code
      thread free to `block_on`); the formula evaluator had this latent since it was written.

## Phase 2 — The host: reads (`sc-api`)

- [ ] **`sc_api::code_host`**: `TableHost { catalog, authority, chain, limits }` implementing
      `CodeHost`, plus `Authority { Admin, User }` and `HostLimits { max_rows, max_calls }`.
- [ ] **The plan type**: `Plan` (serde) with the §7 shape, and one validation pass that
      resolves the table via `catalog.require`, every named column against the table, every
      Ⱶ-path through `ownership::join_guard`, and refuses anything else by name.
- [ ] **One `where` lowering**: move the object-DSL walk out of
      `sc-core-traits::table::{where_expr, required_where, condition_expr}` into
      `sc_api::filter` beside the comparison vocabulary it already calls, add the `and` /
      `or` / `not` combinators there, and have the agent traits call the moved function.
      (Their tests come along and must still pass unchanged.)
- [ ] **Formulas in a plan**: `where: {formula}` and `{alias, formula}` projections parsed by
      `Formula::parse`, validated against `catalog.schema_shape()` with the table's row as the
      bare scope, translated by `translate_value`; a `TranslateError::Untranslatable` becomes
      the "compute it in your code body" message naming the formula.
- [ ] **The read terminals** against the row layer at admin authority: `select` →
      `rows::list_row_values` through a `RowQuery`; `aggregate` → `rows::aggregate_values`;
      `.get(pk)` → the single-pk read; `.exists()` → a bounded select.
- [ ] **The bounds enforced here**, not in JS: `max_rows` clamps and errors rather than
      truncating silently, `max_calls` counted per run.
- [ ] Tests (unit): plan → `Statement` for a join projection, a Ↄ-aggregate projection, each
      filter operator, the combinators, order/limit/offset; and a named refusal for each of
      unknown table, unknown column, unjoinable table, malformed plan, exceeded row cap.

## Phase 3 — The host: writes

- [ ] `insert` (one row or many) → `rows::create_row_ctx`, returning the written row(s).
- [ ] `update` / `delete` → matched rows resolved first, then `rows::update_row_ctx` /
      `delete_row_ctx` per row, returning `{ updated | deleted, ids }`.
- [ ] A write plan with no `where` is refused in the host as well as in the prelude — the
      prelude is a convenience, the host is the rule.
- [ ] The event's user and this trigger's **chain** ride on every write, so cascades are
      bounded exactly as an action's are.
- [ ] Tests: an insert fires the table's own trigger; a body writing its own table hits the
      cascade bound with the chain in the message; an unfiltered update is refused; a coerced
      value reaches the column typed (a date string binds a date).

## Phase 4 — `asUser()`

- [ ] **The event's caller as a `User`**: `event.user` is JSON and `ownership::*_as` wants an
      `sc_auth::User` — one helper (`User::from_json`, beside `from_row`) reading `id` and
      `extra`, with `event.role` as the role, and `None` for an event with no caller.
- [ ] **Routing**: `Authority::User` sends every operation through `ownership::read_rows_as`,
      `aggregate_values_as`, `insert_row_as`, `update_row_as`, `delete_row_as` at that role
      and user; `Authority::Admin` keeps the phase 2/3 path.
- [ ] **Bulk writes under delegation** resolve their ids through the *same* delegated read, so
      a row the caller cannot see is never a row they can update by predicate.
- [ ] **The prelude**: `.asUser()` / `.asAdmin()` on the handle, on a table and on a query,
      all setting `authority`.
- [ ] The delegated-aggregate refusal (untranslatable ownership formula) carries the message
      from §5, not a bare `Err`.
- [ ] Tests (integration, real Postgres): a sub-floor user's delegated read returns only the
      rows their ownership formula grants while the same body at admin authority returns all
      of them; a delegated insert outside the formula throws and writes nothing; a delegated
      update that would move a row out of the caller's ownership is refused; a delegated read
      of an RLS table sees what the policies allow; a scheduled trigger's `asUser()` reads as
      public. **And the deadlock regression test**: a delegated read whose ownership formula
      is untranslatable (so the *formula* isolate must run inside the host call) completes.

## Phase 5 — Wiring, configuration and documentation

- [ ] `run_js_code` builds a `TableHost` from `ctx.catalog`, the event and `ctx.chain`, binds
      `db`, and gains a `timeout_ms` config field (default 5000, max 60000, validated at save
      time as `fetch`'s is).
- [ ] The action's doc comment stops saying there is no host API, and says what there is.
- [ ] `docs/TECHNICAL_DESIGN.md`: §10.1 gains the `db` specification above; §15 gains the
      `CodeHost` seam as what an adapter implements.
- [ ] `docs/tutorial-triggers.md` gains a "reading and writing tables from code" section with
      the milestone's own example, including `asUser()`.
- [ ] CHANGELOG entry.
- [ ] Tests (integration): the milestone's definition-of-done body, run through the admin's
      Run button endpoint, against a real database.

## Phase 6 — Grouped aggregation (optional; the only part that changes `RowQuery`)

- [ ] `RowQuery` gains `group: Vec<Expr>` and `having: Option<Expr>`, rendered by the existing
      `Select`; `rows::aggregate_grouped` returns one row per group.
- [ ] `.groupBy(...).aggregate({ n: "count()", total: "sum(price * qty)" }).rows()`.
- [ ] The ungrouped terminals (`.count()`, `.sum(f)`, …) become sugar for the same path.
- [ ] Tests: grouped counts and sums with a filter and an ordering; a group key that is a
      Ⱶ-path; the delegated case refuses for the same reason an ungrouped aggregate does.

---

## Explicitly OUT of scope for this milestone

- **Transactions across statements** (`db.transaction(fn)`). The row layer has the seam
  (`Executor::Transaction`); holding one open across arbitrary guest code, with locks held
  for the run's whole deadline, is its own decision and its own milestone.
- **Raw SQL from a code body.** §13.4's custom SQL queries are the governed way to write SQL;
  `db` stays closed.
- **Schema changes from code** — create table, add field, drop anything. The catalog's schema
  editor is an admin surface with its own rules; a code body gets rows.
- **Streaming or cursors.** A read is materialised and bounded by `max_rows`; a body that
  needs a million rows is a body that needs a different tool.
- **An awaitable guest API**, and with it `fetch` or timers inside a code body. The sandbox
  gains exactly one host surface here, and it is tables.
- **The other guest languages** (§15's Python, Rust, Go adapters) — this milestone builds the
  seam they will implement and nothing more.
- **`db` in a formula.** Ownership formulas, `only_if`, calculated fields and `{{ }}` tokens
  keep the pure isolate; a formula that could query is a formula that could be slow on every
  row of every read.
