# Saltcorn v2 — GraphQL API TODO

Ordered, checkable task list for the seventh milestone after the MVP. Earlier lists are
archived in [docs/TODO-mvp.md](./docs/TODO-mvp.md) (the MVP),
[docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md) (file stores + the React framework),
[docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md) (the `_sc_tables`/`_sc_fields` overlays,
rich types and File fields), [docs/TODO-post-mvp-3.md](./docs/TODO-post-mvp-3.md) (ownership
formulae, calculated fields and row-level security),
[docs/TODO-post-mvp-4.md](./docs/TODO-post-mvp-4.md) (actions and triggers),
[docs/TODO-post-mvp-5.md](./docs/TODO-post-mvp-5.md) (the file-store IDE) and
[docs/TODO-post-mvp-6.md](./docs/TODO-post-mvp-6.md) (agents); scope and rationale remain in
[docs/GOALS.md](./docs/GOALS.md) and [docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md)
(**§13.4**). The survey behind this milestone's decisions — the Rust and browser libraries
considered, and why each won or lost — is [docs/GRAPHQL_API.md](./docs/GRAPHQL_API.md), which
also holds the normative schema shape. The aggregation semantics it reuses are
[docs/AGG_EXPRS.md](./docs/AGG_EXPRS.md).

**Milestone definition of done:** an admin enables the **GraphQL** provider on an application
beside its REST one, and a caller POSTs

```graphql
query {
  departments(order_by: [{ name: asc }]) {
    name
    manager { email }
    employees(where: { active: { eq: true } }, limit: 5) { name salary }
    employees_aggregate(where: { salary: { lt: 50000 } }) { count avg { salary } }
  }
}
```

and gets it — in **one round trip, one query per level**, with the constrained child count
computed by the database as a correlated subquery, not by fetching employees and counting them.
The same caller sees exactly the rows their role and the tables' ownership formulae permit,
aggregates included; asking for more than the configured depth or cost is refused rather than
served slowly. An admin explores the schema from a screen in the admin UI, and the scaffolded
React app type-checks its queries against a `schema.graphql` the build wrote.

**Not a second data path.** Every read goes through `sc-api`'s row and ownership entry points
and every write through `rows::create_row_ctx` / `update_row_as` / `delete_row_as`, so
coercion, rich-type validation, `File`-field checks, table events and §7.3 apply *because they
are the same code*, not because this provider remembered to do them.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## Decisions taken up front

1. **`async-graphql` 7.2 with `dynamic-schema`.** Our schema is data — an admin or an agent
   creates a table at runtime and the API must have it at the next mount — which rules out
   every macro-driven library (`juniper`) outright. `async_graphql::dynamic` builds a schema
   from runtime values, and brings parsing, validation, variable coercion, introspection,
   `sdl()`, error formatting and depth/complexity limits with it. Pin the **stable 7.2 line**,
   not the 8.0 release candidates. The alternatives and the dependency audit are in
   §1 of [docs/GRAPHQL_API.md](./docs/GRAPHQL_API.md); the short version is that 15 of
   async-graphql's ~21 required crates are already in `Cargo.lock`, and all are pure Rust.
2. **The provider does not aggregate; `sc-expr` does.** A GraphQL aggregate selection lowers to
   the *same* correlated subquery a Ↄ chain translates to (`crates/sc-expr/src/translate.rs`
   `aggregation`), projected as another column of the parent `SELECT`. One implementation of
   "sum coalesces to 0, avg/min/max on no rows are null, null keys are ignored", shared by
   calculated fields, ownership formulae and the wire. This is the whole reason the milestone
   is affordable.
3. **Level-batched execution, not whole-operation compilation.** One `SELECT` for the root
   (columns + calc fields + joinfield subqueries + aggregate subqueries) and one batched
   `SELECT` per child relation per level, through a `DataLoader`. Compiling an entire operation
   into a single `json_agg` statement — PostGraphile's design — is faster and is a milestone of
   its own; it also bypasses the row layer, which is where the rules live. Recorded as carried,
   not as a defect.
4. **One schema per application, not per role.** Hasura compiles a schema per role; we
   authorize at resolve time and refuse with a GraphQL error naming the table. The schema
   describes what the *application* exposes, exactly as its `EndpointSet` does.
5. **A refusal is an error, never a quiet zero.** An aggregate the caller may not see is an
   error; a child table whose ownership formula cannot be translated into a subquery predicate
   is an error naming the table. A count over rows the caller cannot read is a leak, and a
   silent `0` is worse than a refusal because nobody investigates it.
6. **Names are derived, and an underivable name omits the table with a diagnostic.** GraphQL
   names are `/[_A-Za-z][_0-9A-Za-z]*/`; Ⱶ and Ↄ are not among them. Inverse relations are
   `<child>` when unambiguous and `<child>_by_<key>` when not. Nothing is mangled to fit —
   mangling invents collisions.
7. **Nothing new in `package.json`.** The generated GraphQL client is a `fetch` wrapper emitted
   beside the typed REST client, because an application's CSP is `default-src 'self'` and its
   dependencies are the developer's business. `gql.tada` is *configured* against the emitted
   SDL, so the types are TypeScript's own work and there is no codegen step to run.
8. **Tests split as before.** Rust unit tests for name derivation, SDL shape and the query
   lowering (no database); `crates/sc-api/tests/` and `crates/sc-server/tests/` against **real
   Postgres** for everything that touches rows, ownership or RLS; `vitest` for the admin
   explorer's model. Every authorization rule gets a test that would *fail open* if the rule
   were dropped — an aggregate is not tested by asserting a number, but by asserting the number
   a caller who may not see the rows gets.

---

## Phase 1 — The shared aggregation builder (`sc-expr`, `sc-query`)

- [x] **`sc-query`: `row_number() OVER (PARTITION BY … ORDER BY …)`.** `Expr::Agg` has no
      `OVER` and there is no `LATERAL`, so a nested child list cannot be given a per-parent
      `limit` in one round trip today. Add the narrowest thing that does: a window form
      carrying partition keys and an order, rendered by the Postgres dialect, with the function
      name structural (chosen by code) exactly as `Agg::func` is. Unit tests for the rendered
      SQL and for serde round-tripping.
- [x] **`sc-expr`: expose the correlated-aggregate builder.** A public spec — child table, key
      field, parent field, alias, aggregate function, `DISTINCT`, the value expression and an
      optional extra predicate already qualified with the alias — and one function returning the
      `sc_query::Expr` for it, carrying the coalesce/null semantics of AGG_EXPRS.md's table.
- [x] **Refactor the translator onto it.** `Translator::aggregation`'s `scalar_sub`/`bare_agg`
      paths call the new function rather than building their own subquery, so the Ↄ path and
      the GraphQL path cannot drift. The existing `sc-expr` aggregation tests are the proof
      that the refactor changed nothing; add one asserting the two entry points produce the
      identical `Expr` for the same aggregate.
- [x] **`sc-expr`: a public alias-rooted join-path expression.** `join_path_expr` roots at a
      table; a child predicate on `r.managerⱵname` needs the same expression rooted at the
      subquery's alias (`join_value_rooted`, today private). Expose it, since the GraphQL
      filter over a child table needs exactly it.
- [x] Tests: the builder produces the documented SQL for `count`, `count(DISTINCT c)`, `sum`
      (coalesced to 0), `avg`/`min`/`max` (null on no rows), each with and without an extra
      predicate; and the motivating query — employees per department below a salary — renders
      one correlated `count(*)` with the salary bound as a parameter, not inlined.

## Phase 2 — `sc-api::graphql`: names, types, and the empty provider

- [x] Add `async-graphql` (`features = ["dynamic-schema", "dataloader"]`, `default-features =
      false`) to the workspace dependencies with the version pin and the reasoning of decision 1
      written where `Cargo.toml`'s other decisions are.
- [x] **`names.rs`: every GraphQL name derived in one place** — object type per table, row
      fields, `_by_pk`, `_aggregate`, the `BoolExp` / `OrderBy` / `SelectColumn` inputs, and the
      inverse-relation rule of decision 6 (using `SchemaProjection::referencing_fields`). A
      table or field whose name is not a valid GraphQL name is omitted and reported. Pure
      functions, unit-tested, no schema building.
- [x] **Type mapping**: `BasicType` → GraphQL scalars, with custom scalars where GraphQL has
      nothing (`Date`, `UUID`, `JSON`, and a `Decimal`/`BigInt` that does not silently become a
      lossy `Float`). Key fields project as the target table's object type; `File` fields
      project as the string path plus the URL the REST provider already serves the bytes at —
      a GraphQL field must not become a second file-download path.
- [x] **`GraphqlProvider`**, implementing `ApiProvider` beside `RestProvider`: `name()` is
      `"graphql"`, `mount()` defaults to `/graphql`, and `endpoints()` carries the two real
      endpoints — `POST {mount}` (`{query, variables, operationName}` in, GraphQL response out)
      and `GET {mount}/schema.graphql` (the SDL, as `text/plain` through `ApiResponse::file`) —
      so the provider participates in the shared endpoint model and TypeScript generation like
      everything else.
- [x] Build the schema in `project()` from the app's declared tables: object types, input types
      and a `Query` root whose resolvers all return "not implemented" for now. `finish()`
      failing is a **mount failure** naming the table that caused it, never a half-served schema.
- [x] Tests: an SDL **snapshot** for a small fixed schema (a parent, a child with two keys back
      to it, a File field, a table with a non-GraphQL name) — the one test that will catch every
      accidental change to the wire contract in the phases that follow.

## Phase 3 — Reading rows: filters, ordering, paging, joinfields ✅

- [x] **`BoolExp` → `sc_query::Expr`.** Per-type comparison inputs (`eq`/`ne`/`gt`/`gte`/`lt`/
      `lte`/`in`/`nin`/`is_null`, plus `like`/`ilike` for text), `_and`/`_or`/`_not`, and every
      literal coerced against the field's declared type through the row layer's own
      `rows::column_value`, so a Date filter binds a date and not a string. (A `like`/`ilike`
      pattern is bound as text instead: it is a pattern, not a value of the column, and holding
      it to the column's attribute rules would refuse well-formed queries.)
- [x] **`order_by`, `limit`, `offset`** onto `rows::RowQuery`, with `limit` clamped to the
      application's configured cap rather than trusted.
- [x] Root fields resolve through **`ownership::read_rows_as`** — the same entry point the
      agent's `query_table` uses — so role floors, ownership formulae and RLS routing are not
      re-decided here. `_by_pk` is that query with a primary-key equality. (`read_rows_as`
      became a rendering of a new `read_row_values_as`: the GraphQL read needs the values typed
      and needs the extra projections it asked for, which the JSON shape drops.)
- [x] **Outgoing keys are Ⱶ-joins, projected not fetched**: a requested `manager { email }`
      adds `join_path_expr`'s correlated subquery to the same `SELECT`, one column per
      requested leaf. Only the requested leaves — the resolver reads the selection set.
- [x] Tests (real Postgres): filters, ordering and paging return what the equivalent REST call
      plus a hand-written `WHERE` would; a joinfield selection issues **one** query (asserted by
      counting statements, not by timing); a null key yields `null` rather than an error.

## Phase 4 — Child lists, batched ✅

- [x] Inverse-relation list fields resolved through `async_graphql::dataloader::DataLoader`,
      keyed by (relation, the field's arguments, parent key) so siblings collapse into one
      `SELECT … WHERE key IN (…)` per level — never one per parent.
- [x] Per-parent `limit`/`offset` via Phase 1's `row_number()` window; without one, a child
      list is bounded by the application's row cap and says so in the error when it is hit,
      rather than streaming a table into a response. (The window is a `RowQuery::partition`,
      so it is numbered *inside* the read the ownership predicate is ANDed into.)
- [x] The child read applies the **child table's** access rules and ownership, not the parent's.
      A child list field is therefore **nullable**, deviating from Hasura: a non-null one
      would propagate the child's refusal up and null the parent.
- [x] Tests (real Postgres): a query over N parents issues one child query, not N (statement
      count); per-parent `limit` returns the first k children *of each* parent; a child table
      the caller may not read is an error on that field with the parents still returned, which
      is what GraphQL partial results are for.

## Phase 5 — Aggregates, including the constrained child aggregate

- [ ] `X_aggregate` at the root: `count(distinct: Column)`, `sum`/`avg` over numeric fields,
      `min`/`max` over comparable ones, filtered by the same `BoolExp`.
- [ ] **The milestone's motivating case**: `employees_aggregate(where: …)` *inside* a parent
      selection lowers, through Phase 1's builder, to a correlated subquery projected as another
      column of the parent query — no extra round trip, the child predicate folded into the
      subquery's `WHERE`.
- [ ] Multiple aggregates in one selection are multiple columns of the one query; the same
      relation aggregated twice under different `where` arguments gets two subqueries with
      distinct aliases and distinct response keys.
- [ ] Aggregates over a **filtered** parent list are aggregates over that filter — the parent's
      `WHERE` is not silently dropped.
- [ ] Tests (real Postgres): the department/salary query returns the same numbers as a
      hand-written SQL `count(*) FILTER`; `sum` over no rows is `0` and `avg` over no rows is
      `null`, matching AGG_EXPRS.md's semantics table exactly; the whole thing is one statement.

## Phase 6 — Mutations

- [ ] `insert_X(object:)`, `update_X_by_pk(pk_columns:, set:)`, `delete_X_by_pk(…)`, each a thin
      call into `rows::create_row_ctx` / `ownership::update_row_as` / `ownership::delete_row_as`
      with the caller's context — so a rich type's rule, a `File` field's checks and the table's
      triggers all fire exactly as they do for a REST caller.
- [ ] The row layer's refusal is the GraphQL error, with its own message and a machine-readable
      `extensions.code`; a validation failure names the field it was about.
- [ ] Mutations exist only for tables the caller's application exposes for writing; a table
      whose `min_role_write` nobody meets still appears in the schema and refuses at resolve
      time (decision 4).
- [ ] Tests (real Postgres): an insert through GraphQL and an insert through REST produce
      identical rows and identical events; an update that would move a row out of the caller's
      ownership is refused on the *proposed* row as §7.3 requires; a `File` field cannot be set
      to a path outside its store.

## Phase 7 — Authorization, cost, and the refusals

- [ ] **RLS**: the whole read — parent, joinfields and aggregate subqueries — runs inside the
      caller-context transaction, so the child tables' policies apply to the correlated
      subqueries. Test it, on a FORCE'd child table: the count a restricted caller sees counts
      only their own rows.
- [ ] **Non-RLS ownership**: the child's translated ownership predicate is ANDed into the
      aggregate's subquery `WHERE`; an **untranslatable** formula refuses the aggregate with an
      error naming the table (decision 5).
- [ ] **Limits**: `limit_depth` and `limit_complexity` on the schema, a row cap per list field
      and an overall statement budget, all configured per application with defaults that are
      set. Introspection stays on.
- [ ] Aliases, fragments and variables are the *user's* input and must not reach SQL as
      identifiers: response keys come from the operation, column names come from the catalog,
      and the test asserts that a field alias spelling a SQL fragment changes nothing but the
      response key.
- [ ] Tests: a table-level leak test per rule above — for each, a caller who may not read the
      child rows, asserting the *refusal or the reduced count*, never the full one; and a
      too-deep and a too-expensive query, each refused before a statement is issued.

## Phase 8 — Wiring: enabling it, generating for it

- [ ] `app_providers_with` builds a `GraphqlProvider` for `"graphql"` (the `match` in
      `crates/sc-app/src/api.rs` that today knows one name), sharing the evaluator and the
      resolved table set with the REST one; `validate_api_mounts` already refuses a colliding
      mount, and the test that proves REST and GraphQL coexist at `/api` and `/graphql` belongs
      with it.
- [ ] The admin's application form offers the **registered provider names as a select** rather
      than a free-text box, listed by the server so `graphql` is discoverable and a typo is not
      a mount failure discovered later.
- [ ] **The build writes `src/saltcorn/schema.graphql`** from `Schema::sdl()` alongside
      `client.ts` and `hooks.ts`, for applications that enable the provider — and only for
      those.
- [ ] **The generated browser client**: a dependency-free `graphql<TData, TVars>(document,
      variables)` over `fetch` in `src/saltcorn/`, carrying the app's mount, credentials and the
      CSRF handling the REST client already does; plus a `gql.tada` configuration pointed at the
      emitted SDL so a query's result and variables are typed by TypeScript itself with no
      codegen step. The scaffold's `tsc --noEmit` is what makes a stale query a build failure.
- [ ] Tests: `crates/sc-server/tests/` — an application with both providers serves REST and
      GraphQL on one subdomain, a table added afterwards appears in the schema at the next mount
      (the `SchemaObserver` path, no restart), and the scaffolded project type-checks against a
      query using an aggregate field.

## Phase 9 — The explorer in the admin UI

- [ ] A screen per application with a GraphQL provider: query editor, variables, response pane,
      and the schema browsed from introspection — bundled with the admin SPA, because
      `default-src 'self'` forbids the CDN GraphiQL every server ships and vendoring GraphiQL
      into every *application* is a dependency an app did not ask for.
- [ ] It runs as the logged-in admin against the application's own mount, and says so — an
      explorer that quietly holds more authority than the caller being debugged is a trap.
- [ ] Tests: `vitest` over the screen's model (document + variables → request, response →
      panes, an error response rendered as an error), with the socketless parts stubbed.

## Phase 10 — Documentation

- [ ] **§13.4 of the technical design** gains the GraphQL provider: the schema shape, the
      aggregate lowering, the authorization rules and the limits, with
      [docs/GRAPHQL_API.md](./docs/GRAPHQL_API.md) named as the record behind them.
- [ ] A tutorial beside the others (`docs/tutorial-graphql.md`): enable the provider, run the
      motivating query, add a constrained aggregate, and type it in a React app.
- [ ] CHANGELOG entries as the phases land, in this repository's voice: what changed and why it
      is that way, not a list of files.

---

## Carried past this milestone

- **Whole-operation compilation** (decision 3): one statement per *operation* rather than per
  level, via `json_agg`/`jsonb_build_object`. It needs a planner and a JSON-building vocabulary
  in `sc-query`, and it would have to answer for the rules the row layer enforces today.
- **`application/graphql-response+json`.** Content negotiation needs an `Accept` header, and
  `ApiRequest` carries none; adding headers touches every provider. Until then the legacy rule
  applies: `application/json`, 200, errors in the body.
- **Subscriptions.** `Schema::execute_stream` is there, the transport is not — applications have
  no WebSocket, and live queries over the trigger system are a design question, not a wiring one.
- **Persisted queries and an operation allow-list**, the production answer to "an arbitrary query
  language is an arbitrary cost". Depth and complexity limits are this milestone's version.
- **Relay-style cursor connections.** `limit`/`offset` is what the row layer expresses; keyset
  pagination is a separate piece of work in `sc-query` first.
- **Aggregates in `where`** (`having`-style filtering of parents by a child aggregate). The
  builder makes it expressible; the schema shape for it is a design question of its own.
- **The other providers** §13.4 names: gRPC, tRPC, MCP. This milestone's job is to prove the
  `ApiProvider` seam carries a second protocol whose shape is nothing like the first.

## Explicitly OUT of scope for this milestone

- **Replacing REST.** The two providers coexist on one application; nothing about the REST
  projection changes except where a shared rule moves into a shared function.
- **A GraphQL API for the admin surface.** The admin API is a fixed `EndpointSet` with a
  generated typed client, and it works; GraphQL is an *application* API.
- **Custom resolvers authored by a developer** (guest code or SQL behind a GraphQL field). Custom
  routes are still stubbed in the REST provider; when they land they land for both.
- **Federation, `@defer`/`@stream`, and schema stitching.**
- **Per-role schemas** (decision 4).
- Everything still listed as out of scope in [docs/TODO-mvp.md](./docs/TODO-mvp.md),
  [docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md),
  [docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md),
  [docs/TODO-post-mvp-3.md](./docs/TODO-post-mvp-3.md),
  [docs/TODO-post-mvp-4.md](./docs/TODO-post-mvp-4.md),
  [docs/TODO-post-mvp-5.md](./docs/TODO-post-mvp-5.md) and
  [docs/TODO-post-mvp-6.md](./docs/TODO-post-mvp-6.md)
