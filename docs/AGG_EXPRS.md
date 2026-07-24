# Aggregations in JavaScript expressions

Design proposals for aggregation over *incoming* keys in formula expressions — for
calculated fields and for ownership formulae. Status: proposal, not yet implemented.

## Context

A formula is a single pure JavaScript expression evaluated in the context of one table and
one row (GOALS § Authorization; `sc-expr`). Outgoing foreign keys are already covered: a
Ⱶ-join path such as `publisherⱵname` reaches a value on the target table, with
optional-chaining null semantics. What is missing is the other direction: *this* row has
children — rows in other tables whose key fields point at it — and we want to aggregate
over them: the number of order lines, the sum of their totals, whether any share row grants
the current user access.

Whatever syntax we pick has to satisfy every consumer of the one shared AST:

- **R1 — it is JavaScript.** The expression must parse under swc and evaluate in the
  sandboxed deno runtime unchanged. No preprocessor, no second grammar.
- **R2 — it is statically analyzable.** At `validate` time we must be able to extract, for
  every aggregation: the child table, the key field followed, the fields read, and the
  restriction. This is needed three times over: (a) the symbolic translator must render a
  correlated subquery for RLS policies and runtime WHERE injection; (b) the reified
  evaluator does no I/O, so the host must know what to prefetch and bind; (c) stored
  calculated fields need a dependency record — *which child-table writes invalidate this
  value* — to generate recomputation triggers.
- **R3 — the restriction is the formula language.** The filter on which child rows count
  should be an expression in the same language, with the child row, the parent row and
  `user` in scope — so it translates to SQL with the machinery we already have, and so
  admins learn one language, not two.
- **R4 — naming the incoming relation is unambiguous** against field names and forward
  Ⱶ-paths, or ambiguity is a load-time error (principle 5, no silent failures).

Running example schema: `orders` has a key field `customer` → `customers`; `order_lines`
has a key field `order` → `orders` plus fields `qty`, `price`, `status`; `shares` has key
fields `document` → `documents` and `shared_with` → `users`.

## The aggregation set

Scalar aggregations: `count`, `sum`, `avg`, `min`, `max`. Boolean quantifiers: `some`,
`every` (named after the JS array methods, translating to `EXISTS` / `NOT EXISTS`).
Extensions that fit the same shapes later: `countDistinct`, `arrayAgg`, `stringAgg`, and an
ordered `latest(rel, r => r.created_at, r => r.status)` — the value of the third arrow for
the child row maximizing the second (Saltcorn 1's Latest aggregation; SQL `ORDER BY … DESC
LIMIT 1` subquery).

These names join the whitelisted-globals rule that already exists in `analyze.rs`: a field
named `sum` shadows the aggregation function, and a formula that then calls `sum(…)` fails
validation with an error naming the collision. (If we ever find that intolerable, the
escape hatch is namespacing — `Agg.sum(…)` — but the flat names read far better and field
names colliding with five short verbs will be rare.)

## Naming the incoming relation

All three proposals need to say "the order_lines rows whose `order` key points at this
row". The natural spelling reuses the join character in mirror image:

```
order_linesⱵorder        // child table Ⱶ key field — "order_lines via order"
```

A forward path starts with a *field of the current table*; an inverse relation starts with
a *table name*. The two can only collide when a field name equals a table name **and** the
rest of the segments resolve both ways — and even then, the syntactic position
disambiguates: an inverse relation is only legal in an aggregation-argument position, where
a forward path (a scalar) is meaningless. Resolution rule: in an aggregation argument, try
inverse resolution first; anywhere else, forward only; if a genuine ambiguity remains
(both readings resolve in the same position), validation fails and names the identifier —
never a silent pick.

Two refinements worth considering:

- **Shorthand**: when exactly one key field of `order_lines` targets the current table and
  no field of the current table is named `order_lines`, allow the bare table name:
  `sum(order_lines, r => r.qty)`. The long form is only *required* when the child table has
  two keys to the parent (`messages` with `from_user` and `to_user`).
- **A distinct character for the inverse hop**: U+A78D LATIN CAPITAL LETTER TURNED H — `Ɥ`
  — is, like Ⱶ, Unicode category Lu and therefore a valid JS identifier character
  (verified against V8). The inverse relation would then be spelled `order_linesⱵorder`
  with Ɥ in place of Ⱶ: `order_linesꞍorder`, leaving Ⱶ exclusively for forward paths.
  A turned H for the turned-around join is a nice mnemonic and makes
  the direction visible in the source, at the cost of a second special character to teach
  and to type. Recommendation: **stay with Ⱶ for both directions** and rely on position +
  the ambiguity error; reserve Ɥ as the disambiguation escape hatch only if practice shows
  collisions are common. (Decide before shipping; retrofitting the character is a
  formula-rewriting migration.)

---

## Proposal A — aggregate a reverse value path

The terse form: the first argument is a single identifier spelling
`childtable Ⱶ keyfield Ⱶ valuefield`, denoting the multiset of that field's values over the
child rows; an optional second argument is a predicate arrow over the child row.

```js
count(order_linesⱵorder)                                   // two segments: just the rows
sum(order_linesⱵorderⱵqty)                                 // three: aggregate this field
sum(order_linesⱵorderⱵqty, r => r.status === "shipped")    // restricted
avg(order_linesⱵorderⱵprice) > 100
```

Analysis: inside an aggregation call, a first-argument identifier is split on Ⱶ and
resolved inverse-first (table, key field on that table targeting the current table, then a
field of that table). Outside an aggregation call the identifier fails validation — the
multiset is not a value.

SQL: `(SELECT coalesce(sum(l.qty), 0) FROM order_lines l WHERE l.order = orders.id AND
l.status = 'shipped')`. Reified path: the host prefetches the child rows, binds them, and a
prelude in the rendered script defines the aggregation functions.

**For**: mirrors the forward joinfield syntax exactly — admins who know `publisherⱵname`
read `sum(order_linesⱵorderⱵqty)` instantly; the common case is the tersest of all three
proposals.

**Against**: the aggregated value must be a bare field — `sum` of `qty * price` is
inexpressible (workarounds like `sum(a) * x` only go so far); `count`'s two-segment path
vs. everyone else's three segments is an asymmetry to teach; and the moment a predicate
appears, child *rows* enter the picture anyway (`r => r.status === …`), so the language
ends up with two notions — paths-as-value-multisets and rows-in-arrows — where one would
do.

## Proposal B — relation identifiers are arrays of child rows *(recommended)*

One concept: `order_linesⱵorder` names, in any aggregation-argument position, the array of
child rows. Aggregations are ordinary functions over it; the value being aggregated is an
arrow over the child row; the restriction is `.filter()` with a predicate arrow — exactly
the JS an admin would guess.

```js
count(order_linesⱵorder)
sum(order_linesⱵorder, r => r.qty * r.price)
sum(order_linesⱵorder.filter(r => r.status === "shipped"), r => r.qty)
avg(order_linesⱵorder.filter(r => r.qty > minimum_qty), r => r.price)   // parent field in scope
some(sharesⱵdocument, s => s.shared_with === user.id)
```

The validated grammar (what the symbolic translator and the prefetch analysis accept) is
small and closed:

```
AggCall  := AggFn '(' Rel (',' Arrow)? ')'
Rel      := RelIdent ( '.filter(' Arrow ')' )*
```

with each `Arrow` body being an ordinary formula expression over one parameter (the child
row) plus everything already in scope — parent fields, `user`, operation flags. Child
fields are member accesses (`r.qty`); a child row's *own* forward joinfields come along for
free, because `r.productⱵname` is just a member access whose property name is a Ⱶ-path,
resolved against the child table's shape and translated as a nested scalar subquery.

Evaluation:

- **Symbolic**: `AggCall` renders as one correlated subquery — `filter` predicates and the
  key correlation land in `WHERE`, the value arrow's body (translated with `r.x` mapped to
  child columns) lands inside the aggregate function. `some`/`every` render as
  `EXISTS` / `NOT EXISTS (… WHERE (pred) IS NOT TRUE)`.
- **Reified**: no grammar knowledge needed at all. The host prefetches the child rows
  (batched per relation, not per row), binds `order_linesⱵorder` as a real JSON array, and
  the script prelude defines `sum`/`avg`/… as real functions with the null semantics
  below. `.filter` is just `Array.prototype.filter`. The fallback evaluator is therefore
  trivially faithful — the same property the crate already leans on for method calls.

**For**: full expressiveness (aggregate any expression of the child row); one concept
instead of two; reads as plain JavaScript; the strict grammar keeps everything translatable
and prefetchable *today*, while leaving a natural loosening later (arbitrary array methods
on relation arrays — `order_linesⱵorder.map(…).sort(…)` — could be admitted for non-stored,
reified-only contexts without any syntax change). Proposal A's terse form can even be kept
as pure sugar: `sum(order_linesⱵorderⱵqty)` ⇒ `sum(order_linesⱵorder, r => r.qty)` —
desugared at lowering time, so downstream phases see only one form.

**Against**: the common case is wordier than A; the translator must pattern-match the
grammar (bounded — one call shape, one chain form) and report anything outside it as
`Untranslatable` with a message naming what to change.

## Proposal C — fully explicit specification

No new identifier resolution at all: the aggregation call names everything with strings
(or an options object, which would mean admitting object literals into the language):

```js
sum("order_lines", "order", r => r.qty, r => r.status === "shipped")
// or
sum({ from: "order_lines", by: "order", value: r => r.qty, where: r => r.status === "shipped" })
```

**For**: zero ambiguity with fields or forward paths; validation errors are easy to word;
nothing new in scope.

**Against**: table and field names as string data escape every tool that understands
identifiers — the admin-UI rename-refactor, syntax highlighting, future autocomplete — and
invite typos that only surface at validate time with worse messages; it reads like an API,
not like a formula; and the options-object variant opens object literals in a language that
deliberately refuses them. This proposal is the fallback if the identifier-based ones prove
untenable, not a contender on style.

---

## Semantics (common to all proposals)

Both evaluators must agree; this table *is* the spec, and the parity tests extend to it.

| aggregation | `null` child values | empty relation | SQL rendering note |
|---|---|---|---|
| `count` | counted (rows, not values) | `0` | `count(*)` |
| `sum` | ignored | `0` | `coalesce(sum(…), 0)` |
| `avg` | ignored | `null` | bare `avg(…)` |
| `min`, `max` | ignored | `null` | bare |
| `some` | predicate `null` → row does not satisfy | `false` | `EXISTS` |
| `every` | predicate `null` → row fails it | `true` | `NOT EXISTS (… WHERE (p) IS NOT TRUE)` |

`sum` of an empty set is `0`, not SQL's `NULL` — the JS-programmer expectation
(`[].reduce((a,b)=>a+b, 0)`), and it avoids the `total * rate` null-poisoning footgun; the
SQL side coalesces. The prelude functions implement exactly these rules so the reified
path matches by construction.

## Consequences

**Ownership formulae.** This is what finally expresses share tables:

```js
owner === user.id
  || some(sharesⱵdocument, s => s.shared_with === user.id && (_read || s.can_write))
```

Under RLS translation the operation flags constant-fold per policy as they already do, and
the quantifier becomes an `EXISTS` correlated subquery inside the policy — no refetch, the
original motivation for RLS mode. One new hazard: a policy on `documents` now *queries*
`shares`; if `shares` has its own RLS policy referencing `documents`, Postgres reports
`infinite recursion detected in policy` at query time. Detect policy-reference cycles at
RLS-enablement time and refuse with a message naming the cycle (principle 5), and document
the standard fix (a `SECURITY DEFINER` helper or exempting the child table from RLS).

**Stored calculated fields.** `Analysis` grows a record per aggregation —
`AggUse { child_table, key_field, value_fields, filter_fields }` — from which recomputation
triggers are generated: an insert/delete on the child, or an update touching the key, value
or filter fields, recomputes the parent row (both old and new parent when the key itself
changes). This is precisely the dependency information the topological sort in
TECHNICAL_DESIGN §6.2 needs. Note GOALS: during RLS policy translation, a stored calculated
field used in an ownership formula is inlined as its *definition* — with aggregations that
means the subquery, so the recursion check above must look through stored fields too.

**Non-stored calculated fields.** Saltcorn 1 restricted joinfields and aggregations to
stored fields. The symbolic translator lifts that: a translatable aggregation renders as a
scalar subquery in the SELECT list at read time. Untranslatable ones fall back to the
reified evaluator with batched prefetch (one child query per relation per page of parent
rows, grouped by key value — never per-row).

**`SchemaShape`.** Needs to answer "which key fields target this table". `KeyShape`
already stores `target_table`/`target_field`, so no new per-field data is required — the
caller must simply *include the candidate child tables* in the shape, and the analyzer
scans their key fields. Add a helper (`SchemaShape::incoming(table) -> [(table, key_field)]`)
rather than a parallel index that could drift.

## Recommendation

Proposal **B**, with Proposal A's three-segment path form kept as lowering-time sugar for
the bare-field case, and the bare-table-name shorthand for unambiguous single-key
relations. Decide the Ⱶ-vs-Ɥ question (one character for both directions vs. a dedicated
inverse character) before first release, since it is a migration to change; the
recommendation here is single-character Ⱶ with hard validation errors on ambiguity.
