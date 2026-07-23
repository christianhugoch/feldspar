# Saltcorn v2 — Ownership Formulae & Row-Level Security TODO

Ordered, checkable task list for the third milestone after the MVP. Earlier lists are archived
in [docs/TODO-mvp.md](./docs/TODO-mvp.md) (the MVP),
[docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md) (file stores + the React framework) and
[docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md) (the `_sc_tables`/`_sc_fields` overlays,
rich types and File fields); scope and rationale remain in [docs/GOALS.md](./docs/GOALS.md)
(the new **Authorization** section is this milestone's charter) and
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md) (§7.3, which this milestone rewrites).

**Milestone definition of done:** an admin writes an **ownership formula** on a table — a
JavaScript expression over the row's fields, the current `user`, the operation flags
(`_read`/`_insert`/`_update`/`_delete`/`_write`) and Ⱶ-joinfields — and a user who does *not*
meet the table's `min_role` can reach exactly the rows the formula grants, through an
application's API, for exactly the operations it grants. On Postgres the admin can flip
`rls_enabled` and the *same* formula is enforced by the database itself as generated RLS
policies, with the runtime checks switched off and joinfields no longer refetched.

The milestone forces the expression question that has been open since the MVP (§18.4,
JavaScript vs CEL): **the answer is JavaScript**, evaluated two ways from one parse —
**reified** (actually run, in a `deno_core` runtime) and **symbolically** (translated to
`sc_query::Expr`, which is what an RLS policy or an injected WHERE clause is). The two
evaluators must agree, and that parity is a tested property, not a hope — see §1.4. CEL loses
because v1 compatibility, calculated fields and the future JS code adapter (§15) all want
JavaScript anyway; one expression language, parsed once, beats two.

One deliberate exclusion, from the user: the GOALS line on **stored calculated fields being
inlined into RLS policies** cannot land at the start of this milestone because calculated
fields do not exist yet. Phase 7 brings calculated fields in as a stretch goal, and the RLS
inlining is its last item — inside this milestone if Phase 7 lands, carried otherwise.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## Phase 1 — The expression crate (`sc-expr`): parse and analyse ✅

A new workspace crate. It owns the ownership-formula language: parsing JavaScript expressions,
discovering what a formula refers to, validating that against a table, and (in later phases)
the two evaluators. It depends on `sc-query` (the translation target) and `sc-error`; it does
**not** depend on `sc-catalog` — the catalog describes tables *to* it through a small
`TableShape` view (field names, key targets, user-field names/types), so the crate stays a
generic library (§1 code guidelines: separate generic crates).

- [x] Workspace member `crates/sc-expr`. Parse with **`swc_ecma_parser`** (parse-only, no
  transforms) into its AST, wrapped in our own `Formula` type holding the source and the
  parsed expression. swc rather than a hand-rolled parser because the reified evaluator is
  real V8 (`deno_core`), so the parser must accept exactly what V8 accepts — two grammars for
  one language is a divergence factory; swc is the parser family Deno itself uses. A parse
  error is an application error naming the position. **Beyond the plan, deliberate:** swc's
  AST never escapes the crate — `Formula::parse` lowers it into `sc-expr`'s own `Ast`
  (`src/ast.rs`) at the boundary. Two reasons: a `Formula` lives on cached catalog entries
  shared across threads, so its tree must be plain owned `Send + Sync` data with no interned
  atoms; and one owned tree is what both evaluators consume (§1.4's parity needs a single
  semantic object). Lowering is also where the language is bounded: a formula is a single
  pure expression, and assignment, `new`, `this`, comma/spread/bitwise/`in`/`**`,
  function/class expressions and block-bodied arrows are refused **by name**; arrows with
  simple parameters are kept so `groups.some(g => …)` stays usable reified
- [x] **The Ⱶ operator is an identifier character, not an operator.** U+2C75 (Latin capital
  letter half H) is Unicode category Lu, so `publisherⱵname` is a *single valid JavaScript
  identifier* — V8 and swc both accept it as-is. That one fact is the design: no
  preprocessing, no syntax extension. The reified path binds a variable literally named
  `publisherⱵname`; the symbolic path splits identifiers on Ⱶ into a join path. Tested
  against swc (`a_half_h_join_path_is_one_identifier`, single and chained); the in-crate V8
  assertion landed with §3 (`a_half_h_identifier_binds_in_v8`)
- [x] Free-variable analysis: walk the AST and collect every unbound identifier (arrow
  parameters bind; `src/analyze.rs`), classified in `validate` into: a field of the table; a
  Ⱶ-join path (split on Ⱶ, resolved link by link through `Key` fields to any depth); `user`;
  the operation flags `_read`, `_insert`, `_update`, `_delete`, `_write`; a **whitelisted JS
  global** (`Math`, `undefined`, `NaN`, … — added beyond the plan because they exist in the
  reified engine and rejecting them would refuse legitimate formulas; fields shadow globals,
  matching the §3 scope binding); or **unknown**, which is a validation error naming the
  identifier and the table
- [x] `Formula::validate(&SchemaShape, table)` — the check the admin API runs on save (§4)
  and the merge runs on load: classifies every free variable, resolves every join path (each
  link must be a `Key` field; the final segment a field on the target), returns the
  `Analysis` later phases plan from (fields, resolved paths, flags, user usage). Member
  access on `user` is checked against the user table's fields when the shape provides them
  (`SchemaShape::user_fields`; `None` = caller doesn't know, membership unchecked; computed
  `user[x]` is flagged, not rejected). The shape grew into `SchemaShape` — a *map* of
  `TableShape`s — because resolving a chained path needs the target tables' fields too
- [x] Unit tests (24, in-module): identifiers incl. Ⱶ single and chained
  (`publisherⱵcountryⱵname` — depth), every operation flag, unknown identifier named in the
  error, a join path through a non-Key field refused (and to a missing target field, and an
  empty Ⱶ-segment), parse errors positioned (line *and* column, multi-line), every lowering
  refusal named, arrow scope binding and un-shadowing, `Formula: Send + Sync` asserted

## Phase 2 — Symbolic evaluation: translation to `sc_query::Expr` ✅

The formula becomes a SQL predicate. This single translator serves both consumers: the
runtime-check path (§5) inlines the current user's values as literals; the RLS path (§6)
renders user values as `current_setting` GUC reads. The operation flags never reach SQL at
all — the translator is invoked *per operation* with the flags folded to constants, so
`_read || owner === userⱵ…` simply constant-folds.

- [x] `translate(&Formula, op: Operation, env: &UserEnv, shape: &SchemaShape, table) ->
  Result<Expr, TranslateError>` (`crates/sc-expr/src/translate.rs`; the shape argument grew
  with §1's `SchemaShape`) over the translatable subset: literals (string/number/bool/`null`;
  integral numbers bind as `Int`), field identifiers → qualified `Expr::Col`,
  `===`/`==`/`!==`/`!=`, ordered comparisons, `&&`/`||`/`!`, parentheses, conditional (`?:` →
  searched `CASE`), `??` → `COALESCE`, arithmetic in value position, member access on `user`.
  Anything else is `TranslateError::Untranslatable` naming the construct, which §5 catches
  (fall back to reified) and §6 surfaces (refuse to enable RLS). **`TranslateError` is a
  two-variant error on purpose**: `Untranslatable` (a property of the formula's shape — fall
  back) vs `Error` (a real mistake, e.g. an unknown identifier — fail), so a typo can never
  ride the fallback path into V8. A bare non-`user` value as a condition is untranslatable
  (its truthiness needs its type); bare `user` and boolean `user.x` are translated
- [x] **Null semantics are specified once, by the translation.** `===`/`==` render as
  `IS NOT DISTINCT FROM` (and `!==`/`!=` as `IS DISTINCT FROM`), because that is JS's
  two-valued equality — `owner === user.id` with a null `owner` is *false*, and its negation
  *true*, in both worlds; `x === null` is `IS NULL`. Ordered comparisons keep SQL semantics
  (null-involving → not granted); JS's `null < 5 === true` coercion is specified *away*, and
  §1.4's normalisation is what makes the reified evaluator match (see Phase 3). Loose `==`
  translates as strict — the coercion table is not part of the formula language. **One
  documented consequence, pinned by test**: an anonymous user's `user.x` is null, so
  `owner === user.id` *matches null-owner rows* for anonymous callers; a formula that must
  not grant anonymously writes `user && …` — bare `user` is object-or-null, so its
  truthiness is exactly the logged-in test (the §8 tutorial calls this out).
  `sc-query` grew `BinOp::IsNotDistinct`/`IsDistinct` — plus `Expr::Subquery` (scalar
  subquery, for the bullet below) and `Expr::Cast` (with a renderer-side type-name guard,
  since the cast type is the one string neither identifier-quoted nor bound), each with
  renderer tests including an injection-shaped type name refused
- [x] Ⱶ-join paths → **correlated scalar subselects**: `publisherⱵname` on `books` becomes
  `(SELECT _sc_j1.name FROM publishers AS _sc_j1 WHERE _sc_j1.id = books.publisher)`, nested
  per link for deeper chains (golden-tested at depth two). Optional-chaining semantics are
  free: a null FK yields no row yields SQL NULL, which grants nothing — exactly the GOALS
  contract. Aliases are `_sc_j<n>` because the `_sc_` prefix is reserved (§9), so an alias
  can never shadow a real table a correlated reference points at
- [x] `UserEnv` with two modes: **`Inline(Option<BTreeMap<String, Value>>)`** — `user.x`
  becomes a literal of the current user's value (always parameterised), `user` (truthiness /
  `=== null`) becomes a boolean constant — and **`Guc { field_types }`** — `user.x` becomes
  `CAST(jsonb_extract_path_text(CAST(current_setting('sc.user', true) AS jsonb), 'x') AS
  <type>)` (the function form rather than `->>`, which the renderer does not spell; the
  cast is skipped for `text`), and `user === null` becomes `current_setting('sc.user',
  true) IS NULL`. A missing GUC is SQL NULL, so the Guc mode **fails closed** by
  construction. The user field→type map rides in the env, not the shape — only Guc mode
  needs types, and only for the fields the formula touches. `USER_GUC = "sc.user"` is the
  crate-level constant §6 will set
- [x] Unit tests (18 on the translator + 3 new renderer tests in `sc-query`): golden SQL per
  construct through a Postgres-flavoured dialect; both `UserEnv` modes over the same
  formulas; flag folding per operation (a `_read ||` formula translating to `TRUE` for
  select and to exactly the ownership clause for writes — the folder only applies
  **value-exact** simplifications, so it is sound in value position too); the null cases
  pinned including the anonymous-null-match corner; every untranslatable construct named;
  unknown identifier asserted to be `Error`, not `Untranslatable`

## Phase 3 — Reified evaluation on `deno_core` ✅

Actually running the formula, for the constructs SQL cannot hold, and as the reference
implementation the symbolic path is tested against.

- [x] `deno_core` dependency (0.408) and a `JsEvaluator` behind a small trait
  (`crates/sc-expr/src/eval.rs`; a deliberate seam: it decouples the formula machinery from
  the engine. rusty_v8 does build on FreeBSD — the ports tree's patches were upstreamed —
  but upstream publishes no prebuilt static lib for it, so FreeBSD means a from-source V8
  build; the trait is also where a lighter engine (boa/quickjs) could slot in if V8's build
  weight ever becomes a problem). A `JsRuntime` is `!Send`, so `DenoEvaluator` owns a
  **dedicated thread** holding the runtime, fed by a channel (async callers await a oneshot)
  — the pattern the code adapters (§15) will reuse. No extensions, no ops, and `globalThis.
  Deno` (deno_core's own plumbing) deleted at setup: the sandbox test asserts `Deno`/`fetch`/
  `require`/`process` are gone at the V8 level **and** that they are refused a layer earlier
  — an identifier outside the formula vocabulary never reaches V8 at all, because the binder
  errors on it. A watchdog thread terminates runaway evaluation via the isolate handle
  (`terminate_execution`, then `cancel_terminate_execution` so the isolate recovers) after a
  timeout (default 250ms); a terminated or throwing formula is an `Err`, and the trait
  contract says **`Err` is deny** — the §5 caller logs it as an application error
- [x] Scope binding: one call (`FormulaCall`) evaluates a formula against a row — each field
  name bound to the row's value, `user` to the user's field object (or `null`), the five
  flags to the operation (through the same fold table as the translator, by construction),
  and each Ⱶ-identifier bound to its **prefetched** join value. The evaluator does not do
  I/O; the caller (§5) fetches join values, because only it has a catalog — and a join value
  the caller *failed* to prefetch is a named error, not a silent `undefined`. Result is
  coerced by JS truthiness (`!!`) to a bool. Values ride into the script as one JSON literal
  (JSON is syntactic JS), never string-concatenated — an injection-shaped value staying a
  value is a test, asserted against the same isolate's globals afterwards
- [x] **Evaluate the normalisation, not the raw source.** The evaluator renders JS *from the
  analysed AST* (`src/normalise.rs`, a pure function unit-tested without V8): ordered
  comparisons **and arithmetic** are null-guarded to `null` via single-evaluation IIFEs (JS
  would coerce `null` to `0`; guarding to `null` rather than `false` keeps even the value
  identical to SQL's), `==`/`!=` render strict, and `user.x` renders as
  `(user === null ? null : user.x)` so an anonymous user reads as null instead of throwing.
  `!`/`&&`/`||`/`?:`/`??` render **natively** — JS's two-valued logic is the spec, and §2's
  translator was adjusted to meet it where the two differ: `!P` translates as
  `P IS DISTINCT FROM TRUE` (SQL `NOT NULL` is `NULL` where JS `!null` is `true`), pinned by
  a golden test and a parity case. Native logic also means untranslatable formulas keep
  ordinary short-circuit behaviour on the fallback path
- [x] **Parity tests, the gate for everything after this phase**
  (`crates/sc-expr/tests/parity.rs`): for every translatable construct, formula × row × user
  matrices (nulls throughout — including the anonymous null-match corner *and* the `user &&`
  guard closing it, `!(pages < 100)` on a null row, null FKs at join depth one and two) both
  reified and via the translated `Expr` executed against real Postgres — each case asserting
  **three ways**: evaluators agree *and* match the expected verdict, so both drifting wrong
  together still fails. GUC mode runs against a real `set_config` session, with the
  missing-GUC case failing closed. Plus 10 evaluator unit tests: sandboxing (both layers),
  timeout termination and isolate recovery, a thrown formula denying, an unbound join value
  named, truthiness coercion, flags per operation, the Ⱶ-in-V8 assertion (closing Phase 1's
  deferred claim), and the untranslatable class (`user.groups.some(g => g === dept)`)
  actually running

## Phase 4 — Storage and the admin surface

- [ ] `ownership_formula` (string) and `rls_enabled` (bool) live in `TableMeta.attributes`
  — GOALS names attributes for `rls_enabled` explicitly, and both are sparse (§9's rule);
  typed accessors on `TableMeta`, no new columns. The merged `Table` exposes
  `ownership: Option<Formula>` parsed at merge time
- [ ] `updateTable` accepts and **validates** the formula via `Formula::validate` — an
  unknown identifier, bad join path or parse error is a 400 naming the problem, nothing
  written. A formula already stored that fails validation at merge time (a field was
  dropped; a restored dump) is a **reported issue** in the §3.2 style (`field_overlay_issues`
  precedent) and **grants nothing** until repaired — fail closed, table stays min_role-only
- [ ] `table_schema()` grows `ownership_formula` and `rls_enabled`; `updateTable` refuses
  `rls_enabled: true` when the backend lacks `DbCapabilities::row_level_security` or the
  formula does not translate (§6 owns making it true *work*)
- [ ] Admin SPA (`ui/admin` `TableDetail.tsx` settings card): a formula textarea with the
  server's validation error surfaced inline, and an RLS toggle rendered only when the
  capability is advertised; the table list marks tables with a formula. Regenerate the TS
  client; `tsc --noEmit` and `vite build` gates as before
- [ ] Tests: round-trip through the HTTP surface; each invalid-formula shape refused with
  nothing written; the merge-time issue reported and failing closed (integration, real
  Postgres)

## Phase 5 — Runtime enforcement (no RLS)

The general path, correct on any backend. The access rule becomes:
**allowed = role meets the operation's `min_role` OR the ownership formula is true** for this
row/user/operation (§7.3 — ownership *extends* access below the role floor, never narrows it).

- [ ] Endpoint auth: a table with an ownership formula relaxes its REST endpoints'
  `AuthRequirement` from `MinRole` to `Public` — the formula decides `user === null`
  (public may own rows if the admin says so), so the gate moves into the handler. Tables
  without a formula keep today's behaviour exactly
- [ ] Reads: callers meeting `min_role_read` get today's unfiltered query; below it, the
  translated predicate (`UserEnv::Inline`, `_read` folded) is ANDed into the select's WHERE.
  An untranslatable formula falls back to fetch-then-filter through the reified evaluator,
  with join values prefetched per §3 (batched, not per-row round-trips)
- [ ] Writes: update and delete inject the predicate into the statement's WHERE (affected
  rows 0 → not found/denied — never a distinguishable "exists but forbidden" probe);
  untranslatable → load the row, evaluate reified, then act. Update checks the formula on
  the existing row (`_update`) **and** the proposed row (WITH CHECK semantics, mirroring §6
  exactly — a user must not move a row out of their own ownership). Insert evaluates against
  the proposed row (`_insert`)
- [ ] The per-File-field endpoints (download/upload) apply the same rule — they are reads
  and writes of the row, and the path-cumulative file `min_role` still applies on top
- [ ] `AppMounts::refresh_table` liveness: saving/clearing a formula re-projects mounted
  apps, as access changes already do
- [ ] Integration tests, three roles against a real application (`sc-server`): the classic
  owner-field formula (`owner === user.id`) granting exactly own rows for read and write
  below the role floor; a Ⱶ formula (`projectⱵowner === user.id`) granting through the join;
  an operation-split formula (`_read || owner === user.id`: anyone reads, owners write); a
  public-granting formula with `user === null`; an at-or-above-floor user unaffected
  throughout; update-out-of-ownership refused; an untranslatable formula (e.g. a method
  call) behaving identically through the reified path

## Phase 6 — Postgres row-level security

The same formula, enforced by the database. Flipping `rls_enabled` swaps the enforcement
mechanism, never the outcome — the phase's tests are §5's scenarios re-run under RLS.

- [ ] **Caller context as GUCs.** Every row operation runs inside a transaction that first
  issues `SET LOCAL sc.role = '<n>'` and (when logged in) `SET LOCAL sc.user = '<json>'` —
  one JSON GUC, matching §2's Guc mode, not one GUC per field. This needs the row paths in
  `sc-api` to run statements through a driver transaction with settings; add the smallest
  seam `sc-db` allows. Admin endpoints set `sc.role = 1`. A path that forgets the GUC sees
  **no rows, not all rows** — fail closed is a property of the policy shape, and there is a
  test proving it
- [ ] Policy generation: enabling RLS emits `ALTER TABLE … ENABLE ROW LEVEL SECURITY`,
  **`FORCE ROW LEVEL SECURITY`** (the server connects as the table's owner, which RLS
  otherwise exempts — without FORCE the policies are decoration), and four policies from §2
  with `UserEnv::Guc` and the flag folded per operation: SELECT/DELETE with USING, INSERT
  with WITH CHECK, UPDATE with USING **and** WITH CHECK. Every policy carries the role floor
  as `current_setting('sc.role', true)::int <= <min_role_op> OR (<formula>)`, so the role
  side of the access rule moves into the database too
- [ ] Lifecycle: policies are dropped and recreated when the formula or the access rules
  change while enabled; disabling drops the policies and the ENABLE/FORCE; a dropped table
  takes its policies with it (nothing to clean). Enabling with an untranslatable or invalid
  formula is refused naming the construct (§4 declared this; here it is enforced against
  the real translator). Formula validation, not trust: the emitted DDL contains no value
  that did not pass through the `Expr` renderer's quoting
- [ ] When `rls_enabled` is set, the §5 runtime checks are **switched off** for that table —
  "you no longer have to check" (GOALS): reads go unfiltered to the database, writes
  unpredicated (a policy violation surfacing as zero affected rows or a `42501`, mapped to
  the same not-found/denied the runtime path produces). Joinfields are not refetched; the
  correlated subselects in the policy do that work
- [ ] Integration tests (Postgres, real application): §5's scenario suite extracted into a
  shared harness and run in both modes — same assertions, enforcement swapped; the
  missing-GUC fail-closed probe; toggling RLS off restores runtime enforcement with no
  policy debris (`pg_policies` empty for the table); a formula edit while enabled
  regenerating policies live

## Phase 7 — Calculated fields (stretch)

In-milestone if capacity allows (the user flagged it *may* land now), carried otherwise.
Expression-defined calculated fields only — code-adapter calculation (§6.2's full vision)
stays out; the dependency machinery it needs is a milestone of its own. Everything here
reuses `sc-expr` as-is: a calculated field is a formula over the same scope minus `user` and
the operation flags.

- [ ] `DataFieldKind::Calc { expression, stored }` in the `_sc_fields` overlay (kind
  discriminant + parameters in attributes, per §3.1's pattern), validated like ownership
  formulas: fields and Ⱶ-paths, no `user`, no flags. Dependencies **between** calculated
  fields on the same table resolved topologically at merge time; a cycle is a reported
  issue naming the fields
- [ ] Non-stored: computed on read — translatable expressions projected into the SELECT as
  SQL; untranslatable evaluated reified per row after fetch. Not writable, refused by name
  on the write path
- [ ] Stored: a real column, recomputed in Rust on this row's insert/update (write path,
  not DB triggers — the recursion-limit trigger design waits for the calc-fields milestone
  proper). Staleness through joinfield changes on the *target* table is documented, not
  chased. Backfill on definition change
- [ ] **The GOALS RLS line, last**: policy translation inlines a stored calculated field's
  *defining expression* wherever the ownership formula references it — the stored value is
  never trusted inside a policy. Enabling RLS with a formula referencing a stored calc
  field whose definition is untranslatable is refused
- [ ] Tests: topological order and cycle reporting; non-stored parity (SQL projection vs
  reified) piggybacking §3's harness; stored recompute and backfill; the RLS-inlining
  behaviour proven by granting via a calc field whose stored value has been tampered with
  directly in SQL — the policy must follow the definition, not the tampered value

## Phase 8 — Documentation

- [ ] `docs/TECHNICAL_DESIGN.md`: §7.3 rewritten as implemented (the access rule, the two
  evaluators, parity as a tested property, fail-closed rules, the GUC scheme, FORCE, the
  enforcement swap); §18.4 resolved — JavaScript, with the reasoning recorded; `sc-expr`
  added to §2's crate map
- [ ] A tutorial in the established style (`docs/tutorial-ownership.md`): a two-user app,
  an owner formula, a Ⱶ formula through a join, then flipping on RLS and watching `psql`
  show the policies — cross-linked from the existing tutorials (the hygiene test enforces
  resolution, `tutorials_are_cross_linked` may need its list extended)
- [ ] CHANGELOG entries as each phase lands

---

## Carried past this milestone

- **A second reified engine behind `JsEvaluator`** (§3) — optional, not required for any
  target platform (V8 builds everywhere GOALS targets, from source on FreeBSD); worth doing
  only if V8's build weight or embed size becomes a cost we care about
- **Code-adapter calculated fields** and the full dependency/trigger design (§6.2) — Phase 7
  is deliberately the expression-only slice
- **Ownership on files and per-view/page rules** (§7.3's "where applicable") — this
  milestone covers table rows and the File-field endpoints that read/write them
- **`OptionsSource::ClientCode`** — still waiting on `ui/form-runtime` (§12)

## Explicitly OUT of scope for this milestone

- **CEL** — the question is closed, not deferred
- **RLS on non-Postgres backends** — `DbCapabilities::row_level_security` gates it; only the
  Postgres driver advertises it
- **Formula-based ACLs beyond tables** (§7.3's ACL-language layer for views, pages, actions)
- **DB-trigger-based stored calculation** and recomputation on joined-table change
- Everything still listed as out of scope in [docs/TODO-mvp.md](./docs/TODO-mvp.md),
  [docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md) and
  [docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md)
