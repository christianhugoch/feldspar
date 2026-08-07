# The GraphQL API provider

The design record for the second `ApiProvider` (§13.4): a GraphQL projection of an
application's tables, beside the REST one. Status: **implemented** — `crates/sc-api/src/graphql`,
built to this record; the plan it was built from is `TODO.md`, what it became is
[TECHNICAL_DESIGN.md](./TECHNICAL_DESIGN.md) §13.4, and the walkthrough is
[tutorial-graphql.md](./tutorial-graphql.md). This file stays as it was written — the decisions
and the alternatives they were taken over — with a note where the implementation went somewhere
else.

## Why GraphQL, stated as a requirement

REST gives a caller the rows of one table. What it cannot give — not without a route per
question — is a *shape* chosen by the caller: this table's rows, with values reached
through its outgoing keys, and **aggregations over its children, constrained by a
predicate on the child**. The motivating query, in full:

> For each department, its name and the number of employees whose salary is below
> 50 000 USD.

Nothing in the REST projection expresses that, and adding it there means inventing a query
language in the query string. GraphQL already is one, and — this is the part that decides
the design — **the aggregation machinery it needs already exists in this repository**:
`sc-expr`'s Ↄ inverse-relation aggregations (docs/AGG_EXPRS.md) translate exactly that
question into one correlated subquery. GraphQL is the dynamic surface over machinery built
for calculated fields and ownership formulae; it is not a second data path.

The bar is the one §13.4 already sets: Hasura / PostGraphile / Supabase.

---

## 1. The Rust side

### The candidates

| Crate | What it is | Verdict |
|---|---|---|
| **`async-graphql`** 7.2.1 (Jan 2026; 8.0.0-rc.5 Apr 2026) | Full server: parse, validate, coerce variables, execute, introspect, format errors. The `dynamic-schema` feature builds a schema from **runtime values** (`dynamic::{Schema, Object, Field, InputObject, Enum, TypeRef, ResolverContext, FieldFuture, FieldValue}`) instead of from proc-macros. | **Chosen.** |
| `juniper` | Code-first, schema declared with macros over Rust types, resolved at compile time. | Rejected: our schema is *data* — an admin or an agent creates a table at runtime and the API must have it at the next mount. A macro schema means generating Rust and recompiling the server. |
| `apollo-compiler` 1.28 / `apollo-parser` (apollo-rs) | Spec-compliant parsing, semantic analysis and validation of schemas and executable documents. Used by Apollo Router. **No executor, no introspection.** | Rejected as the base, kept as the fallback: it is what we would build on if we ever compile a whole operation into one SQL statement (§6) and want to own execution. Choosing it now means writing variable coercion, introspection and the error format ourselves, for no capability we lack. |
| `graphql-parser` | Older parse-only crate, thinly maintained. | Rejected — `apollo-parser` supersedes it. |
| `cynic`, `graphql_client` | Client-side (typed queries against someone else's schema). | Not applicable server-side; noted so the survey is complete. |

### Why `async-graphql`'s dynamic schema is the right shape

`Field::new` takes the resolver as a closure, so a field is an ordinary runtime value:

```rust
pub fn new<N, T, F>(name: N, ty: T, resolver_fn: F) -> Self
where
    N: Into<String>,
    T: Into<TypeRef>,
    F: for<'a> Fn(ResolverContext<'a>) -> FieldFuture<'a> + Send + Sync + 'static,
```

A schema is `Schema::build(query, mutation, subscription)` + `.register(…)` + `.finish()`,
which is precisely how `RestProvider::project` already builds an `EndpointSet` from the
application's declared tables. Everything else follows: `Schema::sdl()` exports the schema
as SDL (which is what the browser-side typing story needs, §2), `Schema::execute` runs an
operation, introspection comes free, and `limit_depth` / `limit_complexity` are builder
options rather than something to invent.

**Prior art**: [Seaography 2.0](https://www.sea-ql.org/blog/2025-10-08-seaography/) (SeaQL,
Oct 2025) generates an async-graphql *dynamic* schema from SeaORM entities — filters,
ordering, pagination, relations, `DataLoader` for N+1 — for the same reason we would:
"avoids the heavy code generation of static approaches". It is the closest analogue that
exists and it validates the approach at scale.

**Dependency weight** is acceptable and consistent with §16: async-graphql's required
dependencies are ~21 pure-Rust crates, of which 15 — `serde`, `serde_json`, `bytes`, `http`,
`indexmap`, `regex`, `thiserror`, `async-trait`, `futures-util`, `base64`, `mime`, `fnv`,
`num-traits`, `pin-project-lite` and `serde_urlencoded` — are **already in `Cargo.lock`**.
The genuinely new ones are its own `async-graphql-{derive,parser,value}` plus `multer`,
`static_assertions_next`, `asynk-strim` and `async-io`. MIT/Apache-2.0, no C dependency, no
OpenSSL.

**Version**: pin the stable **7.2** line. 8.0 is an RC as of April 2026, and this workspace's
posture (see `argon2` in `Cargo.toml`) is not to ship on release candidates.

### What the dynamic API costs

Honest limitations, each with the answer the plan takes:

- **A finished schema is immutable.** Rebuild it, don't mutate it. `AppMounts` already
  implements `SchemaObserver` and re-mounts an application when the catalog changes
  (`crates/sc-server/src/apps.rs:350`), so the GraphQL schema is rebuilt exactly where the
  REST `EndpointSet` is.
- **It is stringly typed.** Type names are `String`s; a typo is a runtime `finish()` error
  rather than a compile error. Answer: one function that derives every name from the
  catalog (`gql_names`), a snapshot test over the emitted SDL, and `finish()` failing the
  mount loudly rather than serving half a schema.
- **Parent values are `FieldValue`s** carrying `Box<dyn Any>`, downcast in child resolvers.
  Answer: exactly one parent type flows through resolvers (a row as `BTreeMap<String,
  Value>`), so there is one downcast to get wrong.
- **One schema per application, not per caller.** Hasura compiles a schema per role; we
  will not. Authorization happens at resolve time and a refusal is a GraphQL error naming
  the table (§5). The schema therefore describes what the *application* exposes, exactly
  as the REST `EndpointSet` does today.
- **No transport headers.** `ApiRequest` carries method, path, query and body but no
  headers, so `Accept` negotiation is unavailable: we serve `application/json` and follow
  the legacy rule of the GraphQL-over-HTTP spec — **200 with an `errors` array**, including
  for validation failures. (`application/graphql-response+json` with its 400-on-invalid rule
  needs `Accept`, and adding headers to `ApiRequest` is a change to every provider. Deferred,
  recorded as carried.)

---

## 2. The browser side

Three separable questions. The constraint that shapes all three: an application's default
CSP is `default-src 'self'` (`CspPolicy::strict`), so **nothing may load from a CDN**, and
the scaffolded project's build is `tsc --noEmit && vite build`, so a client that does not
match the schema must fail the build rather than 404 at runtime.

**Transport.** `graphql-request` (a fetch wrapper, ~1 dependency), `urql` (light, caching,
React hooks, consumes `TypedDocumentNode` natively), Apollo Client (heavy, normalised
cache), Relay (a compiler and a build-time schema; too rigid for a schema that changes when
an admin adds a table). **Decision: generate a dependency-free client** into
`src/saltcorn/` beside the typed REST client — a `graphql<TData, TVars>(document, vars)`
over `fetch` is ~30 lines, adds nothing to `package.json`, and is CSP-proof. `urql` remains
the documented upgrade for an app that wants caching; the generated client is a plain
`fetch` call it can be swapped for.

**Typing.** `gql.tada` computes result and variable types *in TypeScript itself* from a
schema file — no codegen step, types never stale — versus `graphql-code-generator` (a build
step and a config, but the incumbent) and `genql`. Both need the schema on disk. We have
`Schema::sdl()`, so **the build writes `src/saltcorn/schema.graphql`** on every build, the
same way it rewrites `client.ts` and `hooks.ts`, and the scaffold ships a `gql.tada`
configuration pointed at it. A developer who wants codegen instead points it at the same
file. This is the piece that makes the requirement — "arbitrary aggregations, dynamically"
— survive contact with a typed frontend: the aggregate fields are in the SDL, so
`departments { employees_aggregate(where: …) { count } }` type-checks.

**The explorer.** GraphiQL is the standard, but the stock `index.html` every server ships
loads React and GraphiQL from a CDN, which this CSP forbids, and vendoring the `graphiql`
npm package into an application's bundle imposes a heavyweight dependency on every app.
**Decision: the explorer is an admin screen**, not something served on the app's origin —
a query editor, a variables box, a result pane and the schema browsed from introspection,
bundled with the admin SPA (which already vendors its own assets under
`ui/admin/src/vendor`). It talks to the application's mount as the logged-in admin.

> **As built**: the same `default-src 'self'` also forbids the admin page from `fetch`ing its
> own subdomains (`connect-src 'self'`), so the screen posts to an **admin endpoint**,
> `runApplicationGraphql`, which finds the app's mounted provider and calls the very
> `ApiProvider::handle` a request to `staff.example.com/graphql` reaches, with the signed-in
> admin as the caller. Same schema, same limits, same authorization — the endpoint's only
> decision is who is asking.

---

## 3. Where the aggregations come from

This is the load-bearing decision. **The GraphQL provider does not aggregate.** It lowers a
selection into the same correlated subquery `sc-expr` already builds for a Ↄ chain
(`crates/sc-expr/src/translate.rs:767`), and projects it as another column of the parent
`SELECT`:

```graphql
departments {
  name
  employees_aggregate(where: { salary: { lt: 50000 } }) { count }
}
```

becomes one column on the departments query:

```sql
SELECT "departments".*,
       (SELECT count(*) FROM "employees" AS "_sc_g1"
         WHERE "_sc_g1"."department" = "departments"."id"
           AND "_sc_g1"."salary" < $1) AS "employees_aggregate.count"
FROM "departments"
```

which is byte-for-byte the shape `employeesↃdepartment.filter(r => r.salary < 50000).length`
already translates to (the aliases are `_sc_g…` rather than the translator's own `_sc_a…`, so a
GraphQL aggregate and a calculated field in the same statement cannot collide). One query, one implementation of the semantics (`sum` coalesces to 0,
`avg`/`min`/`max` on no rows are null, null keys are ignored, `count(DISTINCT …)`), and
aggregates that behave identically wherever they are asked for — in a calculated field, in
an ownership formula, or over the wire.

Two additions make it reusable rather than copied:

1. **`sc-expr` grows a structural entry point.** Today the correlated-subquery builder is
   private to the translator and reachable only from a parsed JavaScript chain, and
   `Formula` can only be constructed by parsing source. Synthesising JavaScript source from
   GraphQL arguments (`"employeesↃdepartment.filter(r => r.salary < 50000).length"`) would
   work and was considered — the translator parameterises literals, so there is no SQL
   injection — but it makes correctness depend on JS-escaping user data into a source
   string. Instead: expose the subquery builder as a function over an explicit spec
   (child table, key field, parent field, alias, aggregate, value expression, extra
   predicate), and **make the translator call it too**, so the Ↄ path and the GraphQL path
   are one implementation.
2. **`sc-query` grows `row_number() OVER (PARTITION BY … ORDER BY …)`** — the only way to
   give a nested child *list* a per-parent `limit` in one round trip. `Expr::Agg` has no
   `OVER`, and there is no `LATERAL`.

---

## 4. The schema shape

Hasura-flavoured, because that is the stated quality bar and the shape callers already
know, with the deviations noted.

```graphql
type Query {
  departments(where: DepartmentsBoolExp, order_by: [DepartmentsOrderBy!], limit: Int, offset: Int): [Departments!]!
  departments_by_pk(id: BigInt!): Departments
  departments_aggregate(where: DepartmentsBoolExp): DepartmentsAggregate!
}

type Departments {
  id: BigInt!
  name: String!
  # outgoing key — a Ⱶ-join, resolved as a correlated scalar subquery
  manager: Users
  # incoming key — the child rows (nullable: see the deviations)
  employees(where: EmployeesBoolExp, order_by: [EmployeesOrderBy!], limit: Int, offset: Int): [Employees!]
  # incoming key — aggregated, constrained by the child predicate
  employees_aggregate(where: EmployeesBoolExp): EmployeesAggregate!
}

type EmployeesAggregate {
  count(distinct: EmployeesSelectColumn): Int!
  sum: EmployeesNumericFields!
  avg: EmployeesNumericFields!
  min: EmployeesComparableFields!
  max: EmployeesComparableFields!
}

input EmployeesBoolExp {
  _and: [EmployeesBoolExp!]  _or: [EmployeesBoolExp!]  _not: EmployeesBoolExp
  salary: BigIntComparison     # { eq ne gt gte lt lte in nin is_null }
  name: StringComparison       # + { like ilike }
}
```

> **As built**: a database integer is `BigInt`, not GraphQL's 32-bit `Int` (which `count` still
> is), a decimal is a `Decimal` carried as a string, and `avg` has its own
> `<Table>AvgFields` — the average of an integer column is a decimal, not an integer. A `File`
> field is `FileValue { path, url }`, where the URL is the one the REST provider already serves
> the bytes at.

**Deviations from Hasura, deliberately:**

- **No `aggregate` / `nodes` wrapper.** Hasura's `x_aggregate { aggregate { count } nodes { … } }`
  exists because its aggregate field is also the way to page nodes. Ours is not: the sibling
  list field is. `x_aggregate { count }` is one level shallower and reads better.
- **`count(distinct: Column)`** as an argument rather than Hasura's `count(columns: [..], distinct: Bool)`,
  because `count(DISTINCT a, b)` is not what `sc_query::Expr::Agg` spells and a single column
  is what the Ↄ chain's `.distinct(…)` supports.
- **A child list field is nullable**, where Hasura's is not. The child read applies the
  *child* table's access rules, and GraphQL propagates an error on a non-null field up to
  its parent: a caller who may not read `employees` would lose the departments too. The
  refusal belongs on the field that was refused, which is what partial results are for. A
  child list that resolves is still a list of non-null rows (`[Employees!]`), and the root
  list stays `[Departments!]!` — an error there is the whole query's.
- **A child list's `limit`/`offset` are per parent**, which is what one batched read makes
  possible and what `row_number() OVER (PARTITION BY …)` implements. Hasura means the same
  thing; it is stated here because "three each" and "three" are different SQL.
- **Names.** GraphQL names are `/[_A-Za-z][_0-9A-Za-z]*/`, which Ⱶ and Ↄ are not, so an
  inverse relation is `<child>` when the child has exactly one key field pointing here, and
  `<child>_by_<key>` when it has more than one or when `<child>` collides with a field name
  — the same rule AGG_EXPRS.md deferred as sugar, arrived at from the other direction. A
  table or field whose name is not a valid GraphQL name is **omitted with a diagnostic**,
  never mangled: mangling invents collisions, and silence about a missing table is the kind
  of failure principle 5 forbids.

**Mutations** are `insert_departments(object: …)`, `update_departments_by_pk(pk_columns: …,
set: …)` and `delete_departments_by_pk(…)`, each a thin call into `sc-api::rows` — the same
functions the REST provider calls, so coercion, rich-type validation, `File`-field checks,
ownership and table events apply without being re-implemented. A mutation the row layer
refuses is a GraphQL error carrying the row layer's own message.

---

## 5. Authorization

The rule is not new and must not be restated in this provider: **allowed = the caller meets
the operation's `min_role` OR the table's ownership formula grants the row**, with RLS-enabled
tables decided by the database inside a caller-context transaction. The provider's job is to
route every read and write through `sc-api`'s existing entry points (`ownership::read_rows_as`,
`rows::create_row_ctx`, …) rather than to build its own `SELECT`.

Four things are genuinely new, because an aggregate and a projected join are both new ways to
observe rows:

1. **An aggregate must never count a row the caller may not read.** Under RLS the correlated
   subquery runs inside the caller's transaction, so the child table's own policies apply to
   it automatically — that is a property to *test*, not to assume. Without RLS, the child's
   ownership predicate is translated and ANDed into the subquery's `WHERE`.
2. **An untranslatable child ownership formula refuses the aggregate.** A formula that only
   the reified evaluator can decide cannot filter rows inside a subquery; the honest answer
   is an error naming the table, not a count over rows the caller cannot see.
3. **A Ⱶ-join is a read of the table it reaches**, so that table's read rule holds — a key is
   not a way around a floor a list field over the same table enforces. Two options only: the
   caller meets the target's floor, or the target is RLS-enabled and its policies decide inside
   the caller's transaction. A target whose access comes from an ownership **formula** is
   refused by name, translatable or not: the join subquery is built from the schema shape by
   `join_path_expr` and there is no `WHERE` the provider owns to fold the predicate into, so
   answering would hand over a withheld row one column at a time.
4. **Depth and complexity limits are mandatory**, and a nested-relation query is the reason.
   `limit_depth` / `limit_complexity` on the schema builder, plus a per-query row cap on
   every list field and a per-request budget of reads and writes, configured per application
   with defaults that are set rather than absent. An unbounded GraphQL endpoint is a
   denial-of-service surface that REST's fixed routes never were. The first two are validation
   rules, so they refuse **before a statement is issued**; the budget is counted as it is spent,
   because the number of round trips is a property of execution rather than of the document.

Introspection stays **on**: it describes the tables the application already exposes over
REST, and every tool on the browser side needs it.

---

## 6. Execution: one query per level

Resolution is **level-batched**, not row-by-row:

- The root list is one `SELECT` over the parent table, carrying the ordinary columns, the
  calculated ones, a correlated subquery per requested Ⱶ-joinfield (`join_path_expr` already
  builds these), and a correlated subquery per requested aggregate (§3).
- A nested child list is one `SELECT` per relation *per level*, keyed `WHERE key IN (parent
  ids)` through `async_graphql::dataloader::DataLoader`, batched across siblings — the
  N+1 answer Seaography uses and the one the dynamic API is built for.

**Rejected: compiling the whole operation into a single SQL statement** (PostGraphile's and
Hasura's approach, JSON-aggregating each level into the parent). It is the faster design and
it is a milestone of its own — it needs `json_agg`/`jsonb_build_object` in `sc-query`, a
planner, and it bypasses `sc-api::rows`, which is where every rule about coercion, validation
and ownership lives. Level batching gives one query per relation per level, which is bounded
by the *query's* shape rather than by the data's size, and keeps every write and read on the
existing path. Recorded as carried, not as a defect.

---

## 7. Alternatives considered and rejected

- **Extending REST with query-string filters and aggregate parameters** (`?select=…&count=…`),
  PostgREST-style. Cheaper, and it is where a caller who wants one filtered list should go —
  but "arbitrary aggregations over the query, qualified by a constraint on the child" is a
  nested language, and putting a nested language in a query string is inventing GraphQL
  badly. The two providers coexist; REST is not being replaced.
- **A JSON query DSL of our own** over `sc-query::Statement`. We already have the AST, so the
  temptation is real. It loses introspection, every existing client, every existing browser
  tool, and the typed-frontend story — and gains nothing the GraphQL schema does not already
  express.
- **Exposing `sc-query::Statement` directly** to callers. That is a SQL API with extra steps:
  no way to bound it by role, by ownership or by cost.
- **Subscriptions** (`Schema::execute_stream`). The transport has no WebSocket for
  applications — the chat socket is an admin surface — and live queries over the trigger
  system are a design question of their own. Out of scope.
