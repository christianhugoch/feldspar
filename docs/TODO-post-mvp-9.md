# Saltcorn v2 — Table constraints and indexes TODO

Ordered, checkable task list for the ninth milestone after the MVP. Earlier lists are
archived in [docs/TODO-mvp.md](./TODO-mvp.md) (the MVP),
[docs/TODO-post-mvp-1.md](./TODO-post-mvp-1.md) (file stores + the React framework),
[docs/TODO-post-mvp-2.md](./TODO-post-mvp-2.md) (the `_sc_tables`/`_sc_fields` overlays,
rich types and File fields), [docs/TODO-post-mvp-3.md](./TODO-post-mvp-3.md) (ownership
formulae, calculated fields and row-level security),
[docs/TODO-post-mvp-4.md](./TODO-post-mvp-4.md) (actions and triggers),
[docs/TODO-post-mvp-5.md](./TODO-post-mvp-5.md) (the file-store IDE),
[docs/TODO-post-mvp-6.md](./TODO-post-mvp-6.md) (agents),
[docs/TODO-post-mvp-7.md](./TODO-post-mvp-7.md) (the GraphQL provider) and
[docs/TODO-post-mvp-8.md](./TODO-post-mvp-8.md) (REST queries, custom SQL and the
generated client); scope and rationale remain in [docs/GOALS.md](./GOALS.md) and
[docs/TECHNICAL_DESIGN.md](./TECHNICAL_DESIGN.md).

This milestone brings Saltcorn 1's **table constraints** across: the four things its
Constraints screen can add to a table, and nothing it cannot.

**Milestone definition of done:** an admin opens a table, adds a **jointly-unique**
constraint over two fields with a message in their own words, and a second row with the
same pair is refused with *that message* rather than with `duplicate key value violates
unique constraint "sc_uq_…"`. They add an **index** on a field, and a **full-text search**
index over the table's text columns. They write a **row constraint** —
`salary > 0 && departmentⱵname !== 'Ops'` — and the *database* refuses a row that
breaks it, whichever route the write came in by: the admin UI, the REST API, a trigger's
action, a CSV import, or `psql`. Every one of the four is visible in the admin UI, deletable
from it, and readable back from the database after a restart with no stored copy anywhere to
drift from it.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## Decisions taken up front

1. **A constraint lives in the database, not in an overlay row.** Every other `_sc_*` table
   is an *overlay* that adds what introspection cannot yield (§9's rule: nothing there may
   restate a fact the database already knows). A unique constraint, an index and a row
   constraint are facts the database knows perfectly well — so they are read back by
   [`introspect`](crates/sc-db-postgres/src/introspect.rs) like the primary key and the
   foreign keys, and `Table::constraints` is a merged view of the live schema. The
   consequence is the point: a constraint added by hand in `psql` shows up in the admin UI,
   a restored dump keeps its constraints without a metadata table to restore beside it, and
   there is no state where the two disagree.
2. **What the database cannot hold, a comment holds.** Two things are Saltcorn's and not
   Postgres's: the **error message** an admin writes for a violated constraint, and the
   **formula source** a row constraint was generated from (a `plpgsql` body is not a
   formula, and re-deriving one from the other is not a thing anybody should attempt). Both
   ride in `COMMENT ON CONSTRAINT` / `COMMENT ON TRIGGER` as a small JSON object, which is
   the one place a comment is *the* natural store: it is attached to the object, it is
   dropped with it, `pg_dump` carries it, and it cannot outlive what it describes. A comment
   that is absent or is not our JSON leaves an unnamed constraint that still works — the
   same "a legacy database just works" rule the overlays keep.
3. **A row constraint is a trigger, because a `CHECK` cannot ask a question about another
   table.** Saltcorn 1 emits a `CHECK` when the formula has no join fields and *silently
   enforces nothing* when it has (its `tryCatchInTransaction` swallows the failure, leaving
   the constraint enforced only in the application's own JS path). Since GOALS requires join
   fields and aggregations in constraint formulae, and principle 5 forbids the silent half,
   every row constraint here is one deferrable `CONSTRAINT TRIGGER … AFTER INSERT OR UPDATE`
   over a `plpgsql` function that evaluates the *same* `sc_query::Expr` the ownership
   translator produces — with `RAISE EXCEPTION` carrying the admin's message. One
   implementation, whatever the formula says.
4. **A deferrable `CONSTRAINT TRIGGER`, which means `AFTER`.** The choice that matters is
   *deferrable*, for the reason the foreign keys already carry it (§13.1): an ordinary write
   is checked at the statement exactly as it would be, and a caller holding a transaction —
   a CSV import whose rows point at each other — may `SET CONSTRAINTS ALL DEFERRED` and have
   the joins and aggregations the formula reads evaluated when the whole file is in. Only a
   constraint trigger can be deferred, and a constraint trigger is `AFTER` by definition; the
   row being written is read from `NEW` either way, so nothing else about the check changes.
5. **Structured DDL where the shape is structural; generated SQL where it is an
   expression.** `SchemaChange` grows `AddUniqueConstraint`, `DropConstraint`,
   `CreateIndex`, `DropIndex` and `SetComment` — the module's own doc comment has always
   said "indexes, constraints and alterations arrive as the design demands them", and this
   is the demand. The row constraint's trigger is generated SQL through
   `SchemaStep::Sql`, exactly as the RLS policies are and for the identical reason: it
   carries an arbitrary boolean expression, which is what `SchemaChange` deliberately does
   not model.
6. **The message reaches the user or the constraint has failed.** A unique violation
   surfaces from Postgres naming the constraint; `sc-api::rows` maps it, through the
   table's constraint list, to the admin's own message. A row constraint's message is
   already in the `RAISE`, so it needs no mapping — but both are checked by a test that
   writes a bad row through the ordinary row layer and asserts the admin's words come back.
7. **The formula's vocabulary is the ownership formula's, minus what a trigger cannot
   see.** Fields, Ⱶ-join fields, Ↄ-aggregations, the whole expression language — but
   **not** `user` and **not** the operation flags (`_insert`, `_update`, …). A trigger has
   no request and no session, so `user` would be null in the database and true in the admin
   UI's preview: two answers to one question. Both are refused **by name** at save time.
8. **Full-text search is an index over the table's text fields, not a field.** Saltcorn 1
   spells it as an `Index` on a pseudo-field called `_fts`; here it is its own constraint
   kind, because "index on a column" and "GIN index on a `to_tsvector` of every text column"
   are different things and calling the second the first is how the first grew a special
   case. The language is the admin's choice from Postgres's own `pg_ts_config` list.
9. **Constraints are named by what they are.** `sc_uq_<table>_<cols>`, `sc_ix_<table>_<col>`,
   `sc_fts_<table>`, `sc_ck_<table>_<name>` — derived, so adding the same unique constraint
   twice is refused by the name it already has rather than silently creating a second one.
   A row constraint additionally takes a short **name** from the admin, because it is the
   one kind whose identity cannot be derived from its fields, and because the name is what
   appears in the error when the message is left blank. Every generated name is truncated
   to Postgres's 63 bytes with a hash suffix, so two long field names cannot collide.
10. **Dropping a field that a constraint names is refused by name.** Postgres would drop
    the unique constraint and the index silently with the column, and would leave a row
    constraint's trigger to fail at the next write with a `plpgsql` error nobody can act
    on. The schema editor already refuses a drop that another table's key needs; this is
    the same refusal for the same reason.

---

## Phase 1 — The physical model: introspection and DDL

- [x] **`sc-db`**: `PhysicalConstraint { name, kind, comment }` on `PhysicalTable`, with
      `PhysicalConstraintKind::{Unique { columns }, Index { columns, expression, method },
      RowTrigger}` — the three shapes the backend has for the four things an admin can add.
- [x] **`SchemaChange`** gains `AddUniqueConstraint`, `DropConstraint`, `CreateIndex`,
      `DropIndex` and `SetComment { target, comment }` (decision 5), with `CommentTarget`
      naming a constraint, an index or a trigger.
- [x] **`sc-db-postgres::ddl`** renders all five, with the comment's text escaped as a SQL
      string literal and `NULL` for a comment being removed.
- [x] **`sc-db-postgres::introspect`** reads unique constraints (`pg_constraint.contype =
      'u'`), indexes that no constraint owns (`pg_index`, minus `conindid`), and
      non-internal triggers, each with `obj_description`.
- [x] Tests: the rendering of each new change (unit); an introspection round trip against
      real Postgres — create each kind by hand, read it back, drop it, read it back again.

## Phase 2 — The Saltcorn model: `TableConstraint`

- [x] **`sc-catalog::constraint`**: `TableConstraint { name, kind, error_message }` with
      `ConstraintKind::{Unique { fields }, Index { field }, FullTextSearch { language },
      Formula { formula }}`, the comment metadata it round-trips through (decision 2), and
      the naming rules of decision 9.
- [x] **`Table::constraints`**, filled by `from_physical` — so every consumer of a `Table`
      sees them, and a `psql`-made constraint is one of them.
- [x] **The DDL for each kind**: `constraint_steps(dialect, projection, table, constraint)`,
      returning the `SchemaStep`s that create it and the ones that drop it. The row
      constraint's trigger function is generated here beside `rls.rs`'s policies and for
      the same reason.
- [x] **The full-text expression**: `to_tsvector('<language>', coalesce(f1,'') || ' ' || …)`
      over the table's text fields, in one place, because the index and any later search
      must be the same expression or the index is not used.
- [x] Tests: the generated SQL for all four kinds (unit, no database); metadata round-trips
      through a comment; a name longer than 63 bytes is truncated *and* stays unique.

## Phase 3 — Applying them: the schema editor and the write path

- [x] **`schema_edit::Operation::{AddConstraint, DropConstraint}`**, validated against the
      projected schema like everything else in a batch: the fields exist, a formula parses
      and validates, `user` and the operation flags are refused by name (decision 7), and a
      name already taken is refused as taken.
- [x] **`drop_field` refuses a field a constraint names** (decision 10), and `DropTable`
      does not have to, since the constraints go with the table.
- [x] **`sc-api::rows` maps a constraint violation to the admin's message** (decision 6),
      through the table's constraint list, on insert and on update alike.
- [x] Tests (real Postgres): each kind is created, enforced and dropped through the schema
      editor; a jointly-unique violation comes back as the admin's message; a row
      constraint over an aggregation refuses the eleventh row and allows the tenth; a
      formula naming `user` is refused at save; dropping a constrained field is refused by
      name; the constraint survives a catalog reload because it was read back, not stored.

## Phase 4 — The admin API and the UI

- [x] **`listConstraints` / `createConstraint` / `deleteConstraint`** on
      `/api/tables/:table/constraints`, admin-only, with the regenerated `client.ts` for
      both bundles.
- [x] **A Constraints card on the table screen** — the list with what each one is, an add
      form per kind (jointly-unique tick boxes, the index field picker, the language picker,
      the formula box with its error message), and a delete button. Saltcorn 1's Constraints
      link is a separate screen; here it is a card on the table page, because that page is
      already where a table's shape is edited and a second screen would be a second place to
      look.
- [x] Tests: vitest over the form model (the four kinds → the request bodies, a validation
      message, the summary each kind renders as); the endpoints over HTTP (admin-only, the
      three verbs, and a constraint appearing in `listConstraints` after it is created).

## Phase 5 — Backup, and documentation

- [x] **The backup carries a table's constraints**, and the restore adds them after the rows
      (the order `pg_dump` uses). A restore that rebuilt the columns and not the rules would
      hand back a table accepting what the original refused, silently — and constraints are in
      no `_sc_*` table for the backup to have picked up incidentally.
- [x] **§5.1 and §9 of the technical design** gain the constraint model, the trigger rule and
      the comment metadata; a tutorial joins the others
      ([docs/tutorial-constraints.md](./tutorial-constraints.md)), and two hygiene tests
      hold both to what was built.
- [x] CHANGELOG entry in this repository's voice: what changed and why it is that way.

---

## Explicitly OUT of scope for this milestone

- **`CHECK` constraints as a first-class kind.** A formula with no join field could be a
  `CHECK`, and Postgres would enforce it a little more cheaply — but then a formula's
  enforcement mechanism would depend on its *text*, and adding a join field to a working
  constraint would silently change what it is. One mechanism.
- **Exclusion constraints, partial and expression indexes, multi-column indexes.** All are
  reachable from `psql` and will be *shown* by introspection; none has an admin-UI shape
  yet, and inventing four forms nobody asked for is how the field editor got big.
- **Using the full-text index from a search view.** The index is created and the expression
  is shared; the query side belongs to whatever search surface comes next.
- **Constraint formulae that call `fetch` or user JS.** The translator refuses what it
  cannot render, and a trigger cannot call out.
- Everything still listed as out of scope in the eight earlier lists.
