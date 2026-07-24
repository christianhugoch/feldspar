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

In the function-based proposals (A–C) these names join the whitelisted-globals rule that
already exists in `analyze.rs`: a field
named `sum` shadows the aggregation function, and a formula that then calls `sum(…)` fails
validation with an error naming the collision. (If we ever find that intolerable, the
escape hatch is namespacing — `Agg.sum(…)` — but the flat names read far better and field
names colliding with five short verbs will be rare.)

## Naming the incoming relation

Every expression-level proposal (all but E, which has no expression syntax) needs to say
"the order_lines rows whose `order` key points at this row". The chosen spelling uses the
**Claudian antisigma**. Ⱶ, the forward-join character, is one of the three Claudian
letters; the antisigma `Ↄ` is another — U+2183 ROMAN NUMERAL REVERSED ONE HUNDRED, the
codepoint used for the Claudian antisigma, which is (like Ⱶ) Unicode category Lu and
therefore a valid JavaScript identifier character, verified against V8:

```
order_linesↃorder        // child table Ↄ key field — "order_lines via order"
```

The reversed C marks the reversed join. Forward paths keep Ⱶ exclusively, so the direction
is visible in the source and the resolver never guesses: an identifier containing Ↄ is an
inverse relation, one containing only Ⱶ is a forward path. R4 is thereby satisfied by
construction — no position-dependent resolution, no ambiguity rules; the only requirement
is that Ↄ joins Ⱶ as a character refused in table and field names. In a three-segment
value path (proposal A) the antisigma marks only the direction-reversing hop; the final
segment is field selection on the child row, which keeps Ⱶ: `order_linesↃorderⱵqty` —
"order_lines by order, take qty".

Two refinements considered:

- **Shorthand**: when exactly one key field of `order_lines` targets the current table,
  allow the bare table name: `sum(order_lines, r => r.qty)`. The long form is only
  *required* when the child table has two keys to the parent (`messages` with `from_user`
  and `to_user`). With the antisigma this costs the self-announcing spelling, so the
  resolver must fall back to position (a bare table name is only legal in an
  aggregation-argument position, and a field of the same name wins) — the shorthand is
  optional sugar, droppable if that rule proves confusing.
- **Reusing Ⱶ for both directions** (rejected): tersest, but requires position-dependent
  resolution — inverse-first inside aggregation arguments — plus hard validation errors
  whenever a field name coincides with a child table name and both readings resolve. A
  dedicated turned-H (`Ɥ`, U+A78D) was also considered; the antisigma wins on Claudian
  pedigree and on visual distance from Ⱶ.

---

## Proposal A — aggregate a reverse value path

The terse form: the first argument is a single identifier spelling
`childtable Ↄ keyfield Ⱶ valuefield`, denoting the multiset of that field's values over the
child rows; an optional second argument is a predicate arrow over the child row.

```js
count(order_linesↃorder)                                   // two segments: just the rows
sum(order_linesↃorderⱵqty)                                 // three: aggregate this field
sum(order_linesↃorderⱵqty, r => r.status === "shipped")    // restricted
avg(order_linesↃorderⱵprice) > 100
```

Analysis: an identifier containing Ↄ is split on Ↄ and then Ⱶ, and resolved as child
table, key field on that table targeting the current table, then a field of that table.
Outside an aggregation-argument position the identifier fails validation — the multiset is
not a value.

SQL: `(SELECT coalesce(sum(l.qty), 0) FROM order_lines l WHERE l.order = orders.id AND
l.status = 'shipped')`. Reified path: the host prefetches the child rows, binds them, and a
prelude in the rendered script defines the aggregation functions.

**For**: mirrors the forward joinfield syntax exactly — admins who know `publisherⱵname`
read `sum(order_linesↃorderⱵqty)` instantly; the common case is the tersest of all three
proposals.

**Against**: the aggregated value must be a bare field — `sum` of `qty * price` is
inexpressible (workarounds like `sum(a) * x` only go so far); `count`'s two-segment path
vs. everyone else's three segments is an asymmetry to teach; and the moment a predicate
appears, child *rows* enter the picture anyway (`r => r.status === …`), so the language
ends up with two notions — paths-as-value-multisets and rows-in-arrows — where one would
do.

## Proposal B — relation identifiers are arrays of child rows

One concept: `order_linesↃorder` names, in any aggregation-argument position, the array of
child rows. Aggregations are ordinary functions over it; the value being aggregated is an
arrow over the child row; the restriction is `.filter()` with a predicate arrow — exactly
the JS an admin would guess.

```js
count(order_linesↃorder)
sum(order_linesↃorder, r => r.qty * r.price)
sum(order_linesↃorder.filter(r => r.status === "shipped"), r => r.qty)
avg(order_linesↃorder.filter(r => r.qty > minimum_qty), r => r.price)   // parent field in scope
some(sharesↃdocument, s => s.shared_with === user.id)
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
  (batched per relation, not per row), binds `order_linesↃorder` as a real JSON array, and
  the script prelude defines `sum`/`avg`/… as real functions with the null semantics
  below. `.filter` is just `Array.prototype.filter`. The fallback evaluator is therefore
  trivially faithful — the same property the crate already leans on for method calls.

**For**: full expressiveness (aggregate any expression of the child row); one concept
instead of two; reads as plain JavaScript; the strict grammar keeps everything translatable
and prefetchable *today*, while leaving a natural loosening later (arbitrary array methods
on relation arrays — `order_linesↃorder.map(…).sort(…)` — could be admitted for non-stored,
reified-only contexts without any syntax change). Proposal A's terse form can even be kept
as pure sugar: `sum(order_linesↃorderⱵqty)` ⇒ `sum(order_linesↃorder, r => r.qty)` —
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

The remaining proposals drop the premise the first three share — that an aggregation is a
call to a function named after it.

## Proposal D — aggregation as property access

No aggregation functions at all: a relation identifier is an array of child rows, a static
member access with a child *field* name plucks that field into an array of values, and the
aggregate is a **property read** on the result. The expression is a left-to-right pipeline:

```js
order_linesↃorder.length                                       // count — native, for free
order_linesↃorder.qty.sum
order_linesↃorder.filter(r => r.status === "shipped").qty.sum
order_linesↃorder.map(r => r.qty * r.price).sum                // expression values need .map
sharesↃdocument.some(s => s.shared_with === user.id)           // native array method
```

The validated grammar:

```
AggExpr := Rel ( '.' field | '.map(' Arrow ')' )? '.' aggprop
         | Rel '.some(' Arrow ')' | Rel '.every(' Arrow ')' | Rel '.length'
Rel     := RelIdent ( '.filter(' Arrow ')' )*
aggprop := sum | avg | min | max | count
```

`filter`, `some`, `every`, `map` and `length` are JavaScript's own; only the four
aggregate properties are invented. On the reified path relations are bound wrapped in a
prelude `Proxy` (or a subclassed Array with getters): plucking is a property trap,
`sum`/`avg`/`min`/`max` are getters implementing the semantics table, and the native
methods just work. Symbolically the whole chain renders as one correlated subquery, same
as B.

**For**: reads as a data pipeline, arguably the most fluent of all the options; no free
functions competing with field names in the top-level scope; `count`, `some` and `every`
come out as *actual* JavaScript rather than lookalikes.

**Against**: a property read that computes an aggregate is magic — an admin who knows JS
cannot predict `.sum` from JS knowledge, and `Array.prototype` has no such property, so
the reified path needs the heaviest prelude machinery of any proposal (Proxy traps rather
than plain functions). Field names collide with the invented properties and the array
methods — a child field named `sum`, `map` or `length` is unreachable by pluck and must be
a validation error naming the clash. And the moment the value is an expression, `.map(…)`
reintroduces the arrow, at which point B said the same thing with less mechanism.

## Proposal E — declarative aggregation fields (no expression syntax at all)

Move aggregation out of the expression language entirely. An **Aggregation** is a kind of
calculated field, configured structurally in the admin UI — no formula syntax involved:

- child table and key field (dropdowns, populated from the catalog's incoming keys);
- the aggregate operation (dropdown from the aggregation set);
- a value formula — an ordinary formula validated against the *child* table;
- an optional restriction formula, likewise over the child table.

Expressions then reference the aggregation by its field name like any other field, and the
expression language never learns about aggregation:

```js
// order_total: Aggregation { from: order_lines by order, sum, value: qty * price,
//                            where: status === "shipped" }
order_total > 1000

// shared_with_me: Aggregation { from: shares by document, some,
//                               where: shared_with === user.id }
owner === user.id || shared_with_me         // the ownership formula
```

Correlation to the parent is implicit in the key; when a restriction needs a *parent*
value, the child's own forward Ⱶ-path back through the key already expresses it with no
new mechanism: `qty > orderⱵminimum_qty`. A restriction that references `user` (the share
example) makes the field's value viewer-dependent, so such a field must be non-stored —
enforce that at save time. In RLS translation an aggregation field inlines as its
definition — the same rule GOALS already states for stored calculated fields.

**For**: zero new expression syntax — no new identifier class, no grammar for the
translator to pattern-match, and requirement R4 (relation naming) evaporates because the
child table and key are dropdown selections, not spellings. The most discoverable option
by far for non-programmer admins, and the aggregation is defined once and reusable across
formulas and views. Static analyzability is by construction: the config *is* the
`AggUse` record.

**Against**: friction — a one-off aggregation in a single formula now requires creating a
field first, and heavily-aggregating tables grow a long field list; reading a formula
means chasing the indirection to the field definition. Composition still works
(`order_total / line_count` is a formula over two aggregation fields) but is wordy.

Note that E is **complementary, not competing**: it can coexist with any of A–D, and its
admin UI could even be a builder that *generates* the expression form rather than a
parallel representation — one semantics, two entry points.

## Proposal F — native array methods only

The purist version of D: relations are arrays and **only** JavaScript's own
`Array.prototype` is available — nothing invented at all:

```js
order_linesↃorder.length
order_linesↃorder.filter(r => r.status === "shipped").length
sharesↃdocument.some(s => s.shared_with === user.id)
order_linesↃorder.map(r => r.qty).reduce((a, b) => a + b, 0)          // sum
order_linesↃorder.map(r => r.qty).reduce((a, b) => Math.max(a, b), -Infinity)  // max
```

**For**: nothing to teach, whitelist or shadow — it is exactly the JavaScript it looks
like, and the reified path is plain arrays with no prelude at all.

**Against**: `sum`, `min`, `max` and especially `avg` via `reduce` are hostile to the
non-programmer admin this product serves; the symbolic translator would have to recognise
*idioms* — specific `reduce` lambdas — which is a brittle allowlist with unreadable
"untranslatable" errors one character outside it; and JS coercion diverges from the
semantics table (`null + 3` is `3` in a reduce, while SQL `sum` ignores nulls), so the
two evaluators disagree in exactly the code the admin wrote by hand. Rejected as a primary
design — but its readable subset (`.length`, `.filter`, `.some`, `.every`) is worth
admitting inside whichever proposal wins, and B and D already do.

## Proposal G — a curated method set (F, evolved)

Keep F's shape — relations are arrays, everything is a left-to-right method chain, no free
functions in scope — but *curate* the method set instead of inheriting `Array.prototype`
wholesale. Subtract what has no straightforward SQL rendering, `reduce` above all; add as
**methods** the few aggregates JavaScript lacks: `sum`, `min`, `max`, `avg`, `distinct`.
Every aggregate is still a call (none of D's computed-property magic), and none is a free
function competing with field names in the top-level scope (B's shadowing rule becomes
unnecessary).

Each added method takes an **optional selector argument** saying what to aggregate: either
a **constant string literal** naming a child field, or an **arrow** over the child row.
With no selector, the method aggregates the values it is called on (the output of a
`map`). This makes the common case short without losing the general one:

```js
order_linesↃorder.length
order_linesↃorder.sum("qty")                                     // common case: field name
order_linesↃorder.filter(r => r.status === "shipped").sum("qty")
order_linesↃorder.sum(r => r.qty * r.price)                      // arrow for expressions
order_linesↃorder.avg("price") > 100
order_linesↃorder.distinct("product").length                     // count distinct
order_linesↃorder.map(r => r.qty).sum()                          // selector-less still works
sharesↃdocument.some(s => s.shared_with === user.id)
sharesↃdocument.map(s => s.shared_with).includes(user.id)        // membership: = ANY (…)
tagsↃpost.map(t => t.name).join(", ")                            // string_agg
```

The string form must be a compile-time **constant** — a plain string literal, not a
template, concatenation or variable — for two reasons: validation must resolve it to a
child field at save time (R2), and anything dynamic would blur the line between a field
*name* and the run-time *value* of a String field, an ambiguity the language must not
admit. A string that names no field of the child table is a validation error naming the
table. The native methods keep their native signatures untouched — `join(sep)` stays
exactly `Array.prototype.join`, so value selection for it goes through `map` — the
selector belongs only to the methods we invented.

The method set:

| kept (native JS) | added | removed from F |
|---|---|---|
| `filter(pred)`, `map(arrow)`, `some(pred)`, `every(pred)`, `length`, `includes(x)`, `join(sep)` | `sum(sel?)`, `min(sel?)`, `max(sel?)`, `avg(sel?)`, `distinct(sel?)` — `sel` a constant field-name string or an arrow | `reduce`, `sort`, `reverse`, `slice`, `find`, `findIndex`, `indexOf`, `at`, `flat`, `flatMap`, `concat`, `forEach`, everything else |

The removal criterion is mechanical: out goes anything ordering-dependent or positional
(SQL relations are sets until a terminal `ORDER BY`; `sort`, `slice`, `at`), anything
whose callback is folded state rather than a per-row expression (`reduce`), anything
effectful (`forEach`), and anything returning a row *object* into further computation
(`find`). A curated method outside the set fails validation with a message naming the
alternative ("`reduce` is not available in formulas — use `sum()`").

Translation is uniform because the chain is: `filter` predicates fold into `WHERE`, the
value expression comes from the selector (a field-name string is a column reference, an
arrow translates like any formula arrow) or from a preceding `map`, `distinct` becomes
`DISTINCT`, and the terminal method
picks the SQL form — `length` → `count(*)`, `sum(…)` → `coalesce(sum(…), 0)`,
`some`/`every` → `EXISTS` / `NOT EXISTS`, `includes(x)` → `x = ANY (SELECT …)`,
`join(sep)` → `string_agg(…, sep)` (unordered — accept nondeterministic order, or add an
ordered variant later). The reified path is the lightest of any invented-syntax proposal:
the prelude defines the five added methods on `Array.prototype` inside the isolate (the
sandbox is wholly ours) and everything else is genuinely native. The added methods
implement the semantics table, which repairs F's null-divergence exactly where F broke it
— the hand-written `reduce` is gone, so no user-visible code embeds JS coercion.

**Is this set complete?** Against SQL's aggregate repertoire: `count` is `length`,
`count(DISTINCT …)` is `distinct().length`, `bool_or`/`bool_and` are `some`/`every`,
membership is `includes`, `string_agg` is `join`, and `sum`/`avg`/`min`/`max` are direct.
That covers every aggregation Saltcorn 1's view builder offers except the ordered ones —
Latest chiefly — which need an ordering-aware pair such as `maxBy(sel)`/`minBy(sel)`
(taking the same selector forms)
returning a child *row* (`readingsↃsensor.maxBy("ts").temp` → `ORDER BY … DESC LIMIT
1` subquery); that reintroduces rows-as-results, so it is deliberately deferred rather
than included. Statistical aggregates (`stddev`, `percentile_cont`) are similarly
deferrable additions to the same shape. Verdict: the twelve methods above are reasonably
complete for launch, with `maxBy`/`minBy` the first extension when Latest is missed.

**For**: F's honesty (the chain is real JavaScript, method by method) with B's
expressiveness; the antisigma relation identifier plus curated methods means nothing in
the formula is position-dependent or shadowable; validation errors are per-method and
concrete.

**Against**: the invented methods now have a signature to teach (selector string vs.
arrow vs. selector-less after `map` — three spellings of the same thing, though each is
the obvious one for its case); the constant-string rule needs a good error message when
someone computes a field name; patching `Array.prototype` in the prelude is a (contained,
sandbox-local) global modification; and the curation boundary is a list the docs must
teach, where F could at least say "it's just JavaScript".

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
  || some(sharesↃdocument, s => s.shared_with === user.id && (_read || s.can_write))
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

Relation naming is settled: the Claudian antisigma (`childↃkey`, reserved character,
self-announcing direction) in every expression-level proposal.

The live choice is between the two finalists, **B** (aggregation as free functions:
`sum(order_linesↃorder.filter(p), r => r.qty)`) and **G** (aggregation as curated array
methods: `order_linesↃorder.filter(p).sum("qty")`). They share everything
below the syntax — the antisigma relation identifiers, the semantics table, the `AggUse`
analysis, one-subquery translation, prefetch-and-prelude reified evaluation — so the
decision is genuinely about surface: with the selector argument the two are equally terse
in the common case; B reads like a formula, while G is method-chained JavaScript with no
free functions to shadow and per-method validation errors. A's three-segment path form (`sum(order_linesↃorderⱵqty)`) remains available as
lowering-time sugar under B. The superseded proposals: C (strings fight the language),
D (computed properties are magic; G keeps its pipeline without them), F (raw `reduce`
fails the non-programmer and the translator — G is its repaired form).

**E is worth adopting regardless of the B/G choice** — as the admin-UI entry point that
generates or wraps the expression form, giving dropdown discoverability without a second
semantics.
