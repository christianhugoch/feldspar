# Saltcorn v2 — Tables & Fields Overlay Implementation TODO

Ordered, checkable task list for the second milestone after the MVP. Earlier lists are archived
in [docs/TODO-mvp.md](./docs/TODO-mvp.md) (the MVP) and
[docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md) (file stores + the React framework); scope
and rationale remain in [docs/GOALS.md](./docs/GOALS.md) and
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md).

**Milestone definition of done:** the `_sc_tables` / `_sc_fields` overlays (§9) exist, and the
two things they were always the prerequisite for actually work.

1. **A table's access rules are set in the admin UI and enforced on the data.** Today
   `AccessRules` is modelled, consumed by the application REST provider (§13.1) — and always
   `{ read: 1, write: 1 }`, because `Table::from_physical` has nowhere to read anything else
   from. Every app-facing table is therefore admin-only, which makes an application that serves
   anyone but its admin impossible to configure. After this milestone an admin sets a table's
   read and write roles in the table editor, they persist, they survive a catalog reload, and a
   user of that role — no more, no less — can read or write those rows through an application's
   API.
2. **A field can be given a rich type when it is created, and `File` is one of them.** Today
   `createField` accepts `name`/`sql_type`/`nullable`; there are no rich types (§6.1 ships none
   in the MVP), and `DataFieldKind::File` is modelled but persisted nowhere, so a column cannot
   say "I am a path in store `uploads`". After this milestone the field editor offers a type
   list assembled from the registered rich types and field kinds, a `File` field records its
   store, folder and MIME restrictions, a row written to it is validated against them, and an
   application can read that file at the role the field's table allows.

Two consequences of getting here are load-bearing and easy to miss, so they are called out now:
the overlay **overturns the catalog's stated invariant** ("no stored metadata beyond
`information_schema`"), which is what makes zero-setup work — so the merge rules below must keep
a table with no overlay row exactly as usable as it is today. And the tripwire test
`file_field_references_are_inert_until_the_fields_overlay_exists` fails the moment §3 lands;
that is its purpose, and §3.5 is where it is answered.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## Phase 1 — The `_sc_tables` overlay

### 1.1 Model & persistence (`sc-catalog`) ✅

Follows `_sc_file_stores` (`crates/sc-catalog/src/file_stores.rs`) column for column in style:
one column per value every row has, sparse values in `attributes`, **strict reads** (a missing
or ill-typed column is an `Error::invalid` naming the table and the column, never a silent
default). What is different — and is the whole difficulty of this phase — is that this is the
first table that is an *overlay*: the row is not the object. The object exists without it.

- [x] `TableMeta` value type in `sc-catalog` (`crates/sc-catalog/src/table_meta.rs`): id (UUID row identity), `table_name`, optional label and description, `AccessRules`, `Attrs`. Distinct from `Table`, which is the *merged* view the rest of the system sees — collapsing the two would make "no row" inexpressible, which is the state most tables are in
- [x] `_sc_tables` table + bootstrap (`bootstrap_table_meta`), idempotent and safe against a database that has never seen Saltcorn. **Not yet called from the boot path** — nothing reads the rows until §1.2's merge exists, so wiring it into `connect_catalog` belongs there rather than bootstrapping a table no code consults
- [x] `save_table_meta` / `load_table_meta` / `load_table_meta_by_name` / `list_table_meta` / `delete_table_meta`
- [x] `name` is `UNIQUE`: it is the key the merge joins on, and two overlay rows for one table is not a state that can be merged. **The column is `name`, not `table_name`** — §9 requires every system metadata table to have a `name`, and for an overlay the subject's name *is* that column, so inventing a second one would have left `name` meaning nothing. The Rust field stays `table_name`, where the distinction from a *store's* name is worth keeping visible
- [x] Both role columns are `NOT NULL` and an off-scale role is refused rather than clamped, on save and on read alike. Unlike a store's nullable `min_role`, "no floor" is not a state a table's rules can be in; and rounding an out-of-range role to the nearest legal one would silently decide who reaches the data
- [x] A system (`_sc_*`) table may not have an overlay row: refused on save, so the merge never has to decide what to do with one
- [x] Decide what an overlay row for a table that does not exist means. **Resolved as proposed: kept, not deleted, and reported** (`orphan_table_meta`). A dropped-and-recreated table (a restore, a migration run outside Saltcorn) would otherwise silently lose its access rules, and the failure is the confusing kind — the table returns as `AccessRules::default()`, so an application that served it stops working with nothing saying why. Deliberately *not* enforced on save either: requiring the table to exist would make the row unsavable exactly when an admin is repairing one
- [x] Integration tests against a real Postgres (`crates/sc-catalog/tests/table_meta_store.rs`, 8): round-trip including both roles separately, a second row for one table refused with the first left un-widened, an off-scale role rejected with nothing written, a system table and a nameless overlay refused, an orphan surviving `DROP TABLE` and un-orphaning itself when the table returns, ordering, and a delete that leaves the table and its columns where they were

### 1.2 Merging overlay onto introspection (`sc-catalog`) ✅

The rule that keeps the zero-setup promise true, stated once and tested directly.

- [x] Wire `bootstrap_table_meta` into the boot path beside `users`, `_sc_applications` and `_sc_file_stores` (`connect_catalog`) — deferred from §1.1. Not lazy-on-first-save: the merge would then silently do nothing on exactly the databases nobody has configured yet, which is all of them until they are
- [x] `Catalog::reload` loads every `_sc_tables` row after introspection and merges it onto the `Table` built by `Table::from_physical`. **No row → today's behaviour exactly**, including `AccessRules::default()`. The merge loop can only modify entries the introspection loop already created, which is the zero-setup promise in one line of code
- [x] The overlay is only consulted when the database *has* an `_sc_tables` table. Not defensiveness — required: `bootstrap_table_meta` creates it *through* `create_table`, which reloads, so a reload that assumed the table existed could never bootstrap it
- [x] Decide and document the precedence rule. **Resolved as proposed**, stated on `Table::apply_overlay`: the database is the authority on everything it knows (columns, types, nullability, keys); the overlay is the authority on everything it knows (access, label, description, attributes), and the two sets do not intersect. A merge with no contested field has no conflict semantics to get wrong. It is a constraint on what may ever be added to `_sc_tables`, and §1.1's full-column-list test is what enforces it
- [x] `Table` gains `label`, `description` and `overlay: Option<TableMetaId>`. The last is provenance, not configuration: `None` means "nobody has configured this table", and recording the row id means §1.3's save updates the existing row instead of racing to create a second one for the same table
- [x] `AccessRules::default()` stays admin-only, and a deleted overlay reverts to it rather than to the previous value
- [x] System (`_sc_*`) tables are never given overlay rows and never merge one — refused on save (§1.1) *and* ignored in the merge, because a restored dump or a hand-edited database can contain a row the API would not have written
- [x] Saving **or deleting** an overlay reloads the catalog cache, exactly as a schema change does — otherwise an admin sets a role, sees it saved, and the running server keeps serving the old one
- [x] Tests: 6 integration (`crates/sc-catalog/tests/table_meta_merge.rs`) — an unconfigured table compared to `from_physical` **in full** rather than spot-checked, an overlay surviving reload and a fresh catalog, a delete reverting to admin-only, an orphan merging onto nothing and still waiting when its table returns, a system table ignoring a row inserted behind the API, and a database with no overlay table loading — plus 3 unit tests on `apply_overlay` itself

### 1.3 Admin API & SPA — table settings (`sc-api`, `sc-server`, `ui/admin`) ✅

- [x] `table_schema()` grows the merged fields: `label`, `description`, `min_role_read`, `min_role_write` — so `listTables` shows the roles in the list, where an admin scanning for "which of these is public" needs them — plus `configured`, the overlay's *presence*: a table an admin deliberately set to admin-only and one nobody has opened both read `1`/`1`, and only the first has settings to forget
- [x] `updateTable` endpoint (`PUT api/tables/{table}`), admin-only, taking the overlay fields only. Renaming a table is **not** in scope — that is a schema change with references to chase. The handler reuses the table's existing `overlay` row id, without which every edit after the first would hit the one-row-per-table rule and fail
- [x] `deleteTableSettings` (`DELETE api/tables/{table}/settings`) — "forget what I configured", returning the table to the closed default. Not in the original list; it is what gives `delete_table_meta` a caller, and it is also the only way to clean up an orphan, whose table cannot be opened because it is not there
- [x] `listOrphanTableSettings` (`GET api/table-settings/orphans`) — §1.1 decided orphan rows are kept, and a row nobody can see is indistinguishable from a leak
- [x] Roles are a server-answered list (`GET api/roles`), and the admin picks "Public (100)", not `100`
- [x] Table editor UI in `ui/admin/src/screens/TableDetail.tsx`: a settings card above the fields card, with role selects, and a "Forget settings" button that appears only when there is something to forget
- [x] The table list shows read/write roles per table, and flags orphan rows in a banner with a per-row "Forget" action (§1.1)
- [x] Integration tests through the HTTP surface (`crates/sc-server/tests/table_settings_api.rs`, 4): configure → list → edit → forget, an off-scale role refused with nothing written and a 404 leaving no stray row, settings outliving a `DROP TABLE` and being cleaned up by name, and the role list growing when a user of role 40 is created

**Deviation from this section's original wording, deliberate.** The plan said roles should come
through §1.6's `OptionsSource::ServerQuery` machinery. They do not, and the reason is what that
machinery is *for*: it exists so the admin UI can render a form for settings it knows nothing
about — a file-store backend's, a framework's — by resolving a spec it did not write. A table's
settings are not that. There are exactly four of them, the SPA knows all four by name, and the
form is hand-written like every other fixed form in the admin UI. Routing it through a spec
resolver would mean building a spec-driven renderer for a static form, to reuse a mechanism
whose whole purpose is the case where the fields are *unknown*. The pick-list is a plain
`listRoles` endpoint instead. (§3.4's attribute forms **are** the unknown-fields case, and that
is where the spec machinery earns its place.)

**What `listRoles` answers.** *(Superseded by the roles-table work below — kept for the
reasoning.)* When first written, `listRoles` synthesised its answer: the two named ends of the
scale plus every role its users happened to hold, because there was no roles table to read. That
was the right shape for "no stored roles" but the wrong thing once a role has to carry more than
a number — see the follow-on. The SPA still merges a table's *current* value into the choices
even when the server does not list it, so a table configured for a role that has since been
deleted shows that role rather than silently snapping to a neighbour on the next save.

### 1.3a Follow-on: roles are rows in `_sc_roles` ✅

`listRoles`'s synthesised list was a stopgap, and it stopped being enough as soon as the design
called for **role-specific settings**: a setting has to attach to *something*, and a role that
is only an integer inferred from who holds it has nowhere to put one. So a role became a row.

- [x] `_sc_roles` table in `sc-auth` (`crates/sc-auth/src/roles.rs`): id, `role` number (unique — it is the key everything holds), `name` (unique), `description`, `attributes` for the role-specific settings that motivated this. **Not an overlay** (§9): a role does not exist without its row, like an application or a store, so the table is the authoritative list
- [x] `users.role` is a **real foreign key** onto `_sc_roles.role`. This forced the schema layer to actually emit foreign keys: `DataFieldKind::Key` was modelled and introspection *derived* it, but `to_column_def` created a plain column — so a `Key` field round-tripped to `Plain` on the next reload, claiming a relationship the database did not have. `ColumnDef::references` + the DDL `REFERENCES` clause close that loop, naming the target column explicitly because the goals require keys onto non-PK columns
- [x] Bootstrap seeds exactly the two roles the system depends on (admin, public) and no invented middle; **roles before users**, since a foreign key onto a missing table is not a constraint any database accepts. A database predating this keeps its unconstrained `role` column (GOALS: evolve the setup, do not retrofit migrations)
- [x] Both built-ins undeletable, a held role undeletable (naming the holder count, nothing cascades), and `create_user` checks the role exists first so the failure is legible rather than a raw constraint violation
- [x] `listRoles` now reports the rows (`{ role, name, description, builtin }`, replacing `{ role, label }`); `createRole` / `deleteRole` added; a Roles screen lists/adds/deletes; the Users role field is a pick-list over roles that exist; the table-settings selects show each role's name
- [x] Tests: 5 in `crates/sc-auth/tests/roles_store.rs` (bootstrap seeds and does not overwrite, round-trip with attributes, a user cannot hold a missing role, a held role cannot be deleted, the table is hidden), the admin-API role flow over HTTP, and two DDL tests for the emitted `REFERENCES`

### 1.4 Enforcement doing observable work (`sc-api`, `sc-server`) ✅

An access rule that is stored but never refused is not an access rule. This is the phase that
makes the milestone's first claim testable.

- [x] Application REST provider (`crates/sc-api/src/rest.rs`) already maps `access.min_role_read`/`min_role_write` onto its endpoints' `AuthRequirement` — confirmed end to end with real roles, not a unit test on the mapping. The mapping was correct since the MVP; it had nothing to distinguish until the overlay gave a table any rule but `1`/`1`
- [x] **The seam: `AppMounts::refresh_table`.** A mounted app builds its providers from the catalog at mount time, so a table's access change reloads the catalog but does not reach the running app until a restart. `refresh_table` re-projects the providers of every mounted app exposing the changed table, live; `updateTable` and `deleteTableSettings` call it. It keeps the existing framework — an access change alters who may reach the data, not a byte served, so no bundler runs — which is the same "the mount registry is live" rule (§13.2) applied to a table change rather than an app edit
- [x] Admin row endpoints stay admin-only, and the endpoint docs now say why: those roles are the table's *application-facing* access, while `listRows`/`createRow`/… are the admin's own view of the same rows, reached only by role 1 through the SPA — two surfaces onto one set of rows, not an oversight
- [x] Integration tests with three roles against a real application (`crates/sc-server/tests/table_access_enforcement.rs`, 2): a `min_role_read: 80` table readable by role 80 and rejected for role 100; reads and writes opened separately (80 reads while writes stay admin's, then 40 writes while 80 still cannot); the change taking effect on the **already-mounted** app with no restart, rebuild or fresh login; and forgetting the settings re-closing the running app. Plus a unit test that `refresh_table` is a no-op on an admin-only server

---

## Phase 2 — Rich types (`sc-types`)

§6.1's `RichType`, which the MVP shipped none of. The order matters: rich types come **before**
the field overlay, because "what may a field's attributes contain" is a rich type's question and
the overlay is only the place the answer is stored.

### 2.1 The trait and the registry ✅

- [x] `RichType` per §6.1: `name`, `attributes() -> &[FormField]`, `validate(&Value, &Attrs)`, `sql_types()`. `fieldviews()` is **deferred** — §6.3's fieldviews are React components and are out of scope here (see "Explicitly OUT of scope")
- [x] Registry of rich types by name, mirroring `registered_backends` / `registered_frameworks`: a name, a spec, a validator (`registered_rich_types` / `rich_type` / `rich_type_config_spec` in `crates/sc-types/src/rich.rs`). Same reason as there — the admin UI must render a form for a type it knows nothing about, including one arriving later from a plugin. The concrete types are §2.2's job, so `builtin_rich_types` is empty for now and asking for one names what is registered
- [x] `TypeRef::Rich(RichTypeRef)` variant, with `sql_type()`, `name()` and `validate()` delegating. `RichTypeRef` is a name resolved against the registry (`RichTypeRef::resolve`); identity is the name, which keeps `TypeRef` deriving `PartialEq`/`Eq`. Added `TypeRef::validate_with(value, attrs)` as the attribute-carrying form the write path takes in §2.3
- [x] Decide how a rich type resolves *back* from introspection. **Resolved as proposed: it does not.** `TypeRef::from_sql_type` keeps returning a basic type; a column is rich only because the `_sc_fields` overlay says so (§3.2). Guessing "this `text` column is an Email" from the DB is exactly the kind of magic that makes a legacy database behave surprisingly

### 2.2 The initial rich types ✅

Small on purpose: enough to prove the three things the machinery must do — validate a value,
declare typed attributes, and constrain what may be stored.

- [x] `String` over `text` with a `max_length` attribute, optional `options` (a select), and an optional `regex` pattern (anchored to a full match; matched by the lightweight `regex-lite` crate) — proves attributes drive both validation and the editor (`crates/sc-types/src/rich_types.rs`). **Deviation:** the `Email` type below was dropped in favour of the `regex` attribute on `String`, which subsumes email validation and any other pattern; "validation with no attributes" is still exercised by running either type with an empty attribute bag
- [x] `Integer` over `int8` with `min`/`max` — proves numeric attribute validation
- [x] ~~`Email` over `text`~~ — dropped: replaced by `String`'s `regex` attribute (see above)
- [x] Decide whether `File` is a rich type or a field kind. **Resolved as proposed: a kind, not a type.** §6.2 already models it as `DataFieldKind::File`, and it is not a value family — it is a *reference*, like `Key`, whose storage type is `text`. The admin-facing type picker (§3.4) merges kinds and types into one list because that is how an admin thinks, but the model keeps them apart, and `Key` proves the shape already. No `File` rich type is registered
- [x] Unit tests per type: valid and invalid values, attribute validation, `sql_type` round-trip

### 2.3 Validation on the write path (`sc-catalog`, `sc-api`)

- [ ] A row write validates each value against its field's `TypeRef` **and** the field's `Attrs`. Today `validate` takes only a value; the rich path needs the attributes bag beside it
- [ ] The error names the field and what was violated ("`email`: not a valid email address"), because this one is shown to a user of an application, not only to the admin
- [ ] Integration test: an application POST that violates a rich type's rule is a 400 naming the field, and the row is not written

---

## Phase 3 — The `_sc_fields` overlay and File fields

### 3.1 Model & persistence (`sc-catalog`)

- [ ] `FieldMeta` value type: id, `table_name`, `field_name`, optional label/description, `TypeRef` (rich type name, if any), field `kind` + its parameters, `Attrs`
- [ ] `_sc_fields` table + bootstrap; `(table_name, field_name)` is `UNIQUE`
- [ ] `save_field_meta` / `load_field_meta` / `list_field_meta` / `delete_field_meta`, strict reads as in §1.1
- [ ] Decide how the kind is stored. **Proposed: a `kind` text discriminant plus the kind's parameters in `attributes`**, rather than a column per kind's parameters: `Key` and `File` have disjoint parameters and more kinds are expected, so per-kind columns would add a nullable column per kind forever. This is §9's own rule ("a value present for many rows gets its own column; a sparse value goes into `attributes`") applied honestly
- [ ] Integration tests: round-trip a `File` field's store/folder/MIME restrictions; an ill-typed `kind` is rejected by name

### 3.2 Merging fields (`sc-catalog`)

The same precedence rule as §1.2, with one genuinely new case: an overlay may say a column is a
rich type, and the column's SQL type has to be able to hold it.

- [ ] Merge overlay rows onto the fields `Table::from_physical` produced: type (basic → rich), kind (`Plain` → `Key`/`File`), label, attributes
- [ ] A rich type whose `sql_types()` does not include the column's actual type is a **reported inconsistency**, not a silent downgrade and not a hard failure: the table must stay usable and the admin must be told. `text` → `Integer` is exactly what a hand-edited database produces
- [ ] `Key` fields: introspection already derives `Key` from a foreign key. The overlay adds only `summary_field` — it must not be able to invent a `Key` that has no foreign key behind it, or a "reference" would exist that the database does not enforce
- [ ] A row for a column that no longer exists is kept and reported, as in §1.1
- [ ] Tests: a field with no row is unchanged; a `File` row survives a reload; a type/column mismatch is reported and the table still serves rows

### 3.3 Admin API — creating and editing fields (`sc-api`, `sc-server`)

- [ ] `createField`'s input grows beyond `name`/`sql_type`/`nullable`: a `type` (basic type name **or** registered rich type name), a `kind` with its parameters, `attributes`, `label`, `required`, `unique`. `sql_type` is derived from the type, not asked for — asking the admin for both is how they get to disagree
- [ ] Creating a field is now **two writes** — the DDL and the overlay row. Decide the failure semantics. **Proposed: DDL first, then the overlay row; if the overlay write fails, the column exists as a plain column and the error says so.** The reverse order would leave an overlay row describing a column that does not exist, which §3.2 has to tolerate anyway but should not be *manufacturing*
- [ ] `updateField` (`PUT api/tables/{table}/fields/{field}`): overlay-only changes — label, attributes, kind parameters, summary field. Changing a field's *SQL type* is a schema change and is **out of scope**
- [ ] `listFields` returns the merged field, including type, kind and attributes
- [ ] `listFieldTypes` endpoint: the registered rich types and kinds with their `Vec<FormField>` specs, so the SPA renders the attribute form for a type it knows nothing about — the same contract `listFrameworks` and the backend registry already have
- [ ] Integration tests through HTTP: create an `Email` field and a `File` field, read both back merged, reject an unknown type by name

### 3.4 Admin SPA — the field editor (`ui/admin`)

- [ ] The "add field" form's type input becomes a pick-list assembled from `listFieldTypes`: basic types, rich types, and the `Key`/`File` kinds in one list
- [ ] Choosing a type renders its declared `FormField` attribute form beneath — driven entirely by the spec, with no per-type branch in the SPA. If a `File` field needs a per-type React branch, the spec is wrong, and the file store's backend form is the precedent that it need not be
- [ ] `File`: the store is a server-resolved pick-list of connected stores (§1.6's machinery again); folder and MIME restrictions are plain inputs
- [ ] The fields table shows type, kind and the `File` store, so a mis-pointed field is visible without opening it
- [ ] The row editor renders a `File` field as a file picker over the field's store, not a free-text path — the admin-side file browse endpoints already exist and are what this reuses

### 3.5 File fields do real work (`sc-catalog`, `sc-files`, `sc-api`)

The point of the whole milestone's second claim, and where the tripwire is answered.

- [ ] A row written to a `File` field is validated: the path is inside the field's store, under its folder if set, and its MIME type is allowed. An unresolvable store is an error naming the store and the field
- [ ] Re-enable the catalog-level half of the file-store delete check. `file_store_field_references` is written, correct, and inert; with the overlay it can finally scan `_sc_fields` and refuse to delete a store that a field points at
- [ ] Rewrite `file_field_references_are_inert_until_the_fields_overlay_exists` into its successor: the same setup, asserting that the reference **is** now found and the delete refused, naming the field. Delete the old assertion rather than weakening it
- [ ] Deleting a *field* that points at a store, and dropping a table containing one, both leave the bytes alone — the same rule a store delete follows, and for the same reason
- [ ] Integration test: create a `File` field, write a row with a valid path, reject a path outside the folder, reject a disallowed MIME type, refuse to delete the store while the field exists

---

## Phase 4 — Application-facing file access (`sc-api`, `sc-server`)

The smallest thing that makes a `File` field observable from an application, and the first
caller that makes §1.4b's path-cumulative `min_role` enforcement do observable work — a carried
item from the last milestone that nothing then could close, because every file endpoint was
admin-only and role 1 clears every rule.

- [ ] An application's REST provider serves the bytes behind a `File` field: read at the table's `min_role_read`, **and** the file's own path-cumulative `min_role` — the stricter of the two, never the looser
- [ ] Upload at the table's `min_role_write`, into the field's store and folder, with the field's MIME restrictions applied server-side. A client-side accept filter is a convenience, not a control
- [ ] Decide the addressing. **Proposed: by table, row id and field name**, not by raw store path — the row is what the caller is allowed to see, and letting an app name an arbitrary path would make the field's folder restriction advisory
- [ ] Integration test with two roles and a per-directory `min_role`: the same file is served to one and refused to the other, with the table's rule and the directory's rule each decisive in one case

---

## Phase 5 — Documentation

- [ ] `docs/TECHNICAL_DESIGN.md`: §9's overlay description gains the merge and precedence rules actually implemented; §6.1 gains the rich-type registry; §17's "no stored metadata beyond information_schema" line is updated to say what replaced it and why the zero-setup promise still holds
- [ ] A tutorial section, in the style of the existing ones: create a table, set its roles, add a `File` field, upload from an application
- [ ] CHANGELOG entries as each phase lands

---

## Carried past this milestone

- **`OptionsSource::ClientCode`** — options that depend on other values in the form. Still waiting on `ui/form-runtime` (§12), which is the only thing that could evaluate it. §3.4's attribute forms will want it (a folder pick-list that depends on the chosen store) and should degrade to a plain input rather than pull the form runtime in
- **Calculated fields** (§6.2's `Calc`) — the `_sc_fields` overlay is their storage, so this milestone unblocks them, but the dependency graph, the topological sort and the code adapters are a feature area of their own

## Explicitly OUT of scope for this milestone

- **§6.3 fieldviews** — the React component registry, per-type editors, and fieldview config. This milestone renders attribute forms from `FormField` specs and file inputs from the existing file endpoints; a fieldview registry is the next milestone's shape, not a prerequisite
- **Renaming or retyping a column** — both are schema changes with references to chase (§3.3). The overlay is edited freely; the schema is created and, for now, left alone
- **Row-level / ownership authorization** (§7.3) — per-table `min_role` is this milestone; ownership formulas and the CEL-vs-JavaScript question (§18.4) are not
- **Multiple databases** — `DbId::primary()` remains the only database, and the overlay tables key on table name within it
- Everything still listed as out of scope in [docs/TODO-mvp.md](./docs/TODO-mvp.md) and [docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md)
