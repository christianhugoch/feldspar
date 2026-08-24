# Saltcorn v2 — Table providers: a table whose rows come from a module

Ordered, checkable task list for the sixteenth milestone after the MVP. Earlier lists are
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
[docs/TODO-post-mvp-13.md](./docs/TODO-post-mvp-13.md) (modules),
[docs/TODO-post-mvp-14.md](./docs/TODO-post-mvp-14.md) (SQLite) and
[docs/TODO-post-mvp-15.md](./docs/TODO-post-mvp-15.md) (modules in-process). Scope and
rationale remain in [docs/GOALS.md](./docs/GOALS.md) and
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md).

`sc-catalog`'s `TableProvider` trait has existed since the MVP with exactly one implementation
— `DriverTableProvider`, which forwards a `Select` to a `DatabaseDriver` — and a doc comment
saying "non-trivial providers (RSS/IMAP/search/…) are post-MVP". This milestone is that
sentence: a **second** implementation, whose rows come from JavaScript a module supplies.

The design was settled in v1 and is not reopened here. A v1 plugin exports

```js
table_providers: {
  "RSS feed": {
    configuration_workflow,                       // this provider's own settings
    fields: [{ name: "title", type: "String" }],  // or a function of the configuration
    get_table: (cfg) => ({ getRows: async (where, opts) => [...] }),
  },
}
```

and this milestone loads that key, on the same terms the previous one loaded `actions` and
`functions`: the manifest reports it, the admin configures it, and the call goes to the worker
the module was loaded on. **Read-only.** `get_table` may also return `insertRow`, `updateRow`
and `deleteRows` — `@saltcorn/postgres-tables` does, behind a config flag — and a writable
provided table is the next milestone, not this one.

**Milestone definition of done:** with `@saltcorn/rss` installed and granted one host, an
admin creates a table whose provider is `RSS feed`, types a feed URL into the form the module
declared, and the table's Data tab lists the feed's items with the columns the module named.
Nothing above `sc-catalog::provider` knows the table is not in a database.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## The specification

### 1. Where a provided table comes from

A database table exists because introspection found it. A **provided** table exists because
somebody wrote a row saying so — there is nothing to introspect, and no DDL was ever issued.
That is a real departure from the rule written on `_sc_tables` ("everything here *adds* to what
introspection already yields"), and it is made deliberately and in one place:

> `_sc_tables` holds two kinds of row. Without a provider it is an **overlay**: the table
> exists whether or not the row does, and the row adds access rules, a label, a description.
> With a provider it is a **definition**: the row *is* the table, like `_sc_triggers`' row is
> the trigger, and deleting it deletes the table.

§9 of the design already anticipated this — the `_sc_tables` line reads "access rules,
label/description, attributes, **provided-table defs**".

The definition is three values, and they live in `attributes` rather than in columns because
§9's own rule says so — a value present on a handful of rows out of every table in the database
is sparse, and `ownership_formula` and `rls_enabled` are already there:

| Attribute | Holds |
|---|---|
| `provider_module` | the package name — `@saltcorn/rss` |
| `provider_name` | the provider's own name within it — `RSS feed` |
| `provider_config` | the object the admin filled in, handed to `fields(cfg)` and `get_table(cfg)` |

**The module is stored as well as the provider.** v1 keys `table_providers` globally, so
`provider_name` alone is its whole identity; here a call has to reach *the worker the module
was loaded on*, and two modules may each supply a provider called `Table`. Which module was
meant is the admin's answer at creation time, not a lookup.

- [x] 1.1 `ProvidedTableDef { module, provider, configuration }` in `sc-catalog`, read from and
  written to a `TableMeta`'s attributes. Round-trips.
- [x] 1.2 `TableSource::Provider { module, provider }` beside `TableSource::Database`, and
  `Table::provided(...)` — the constructor `Table::from_physical` is not, because there is no
  physical table.
- [x] 1.3 `Catalog::reload` builds them: after the overlay pass, every `_sc_tables` row
  carrying a provider becomes a table in the map. A name that introspection already produced
  **loses** — the database wins, exactly as the primary wins a name a secondary connection
  offers — and the loss is recorded, not silent.

### 2. The seam: `TableProviderHost`

`sc-catalog` may not name `sc-module` (layer 4 cannot name layer 6), and the shape of the
inversion is already in the crate: `ModuleFnHost` is a trait `sc-expr` declares, `sc-module`
implements and `sc-server` installs on the catalog. This is the same seam for a different
capability.

```rust
#[async_trait]
pub trait TableProviderHost: Send + Sync {
    /// Every provider every loaded module supplies, for the "new table" screen.
    fn providers(&self) -> Vec<TableProviderKind>;
    /// The fields this provider presents for this configuration.
    async fn fields(&self, module: &str, provider: &str, cfg: &Json) -> Result<Vec<DataField>>;
    /// Its rows, as JSON objects, for one v1 `where`/`options` pair.
    async fn rows(&self, module: &str, provider: &str, cfg: &Json,
                  filter: &Json, options: &Json) -> Result<Vec<Json>>;
}
```

- [x] 2.1 The trait, `TableProviderKind { module, provider, config_spec }`, and the slot on the
  `Catalog` (`set_table_providers`, `table_providers`) — `None` in a process with no modules,
  which is what a `sc-catalog` test and a server with nothing installed both are.
- [x] 2.2 `ProvidedTableProvider`: the `TableProvider` implementation. `fields()` are the
  table's; `query()` runs §3; `write()` fails with a sentence naming the provider and saying
  that writable providers are not implemented yet.

### 3. Interpreting a `Select` over JSON rows

v1 does this in JavaScript, in `json_list_to_external_table`: it hands the plugin the `where`
object, and unless the plugin sets `disableFiltering` it *re-filters, re-sorts and re-slices the
answer itself*. The same arrangement is right here and for the same reason — a provider is
allowed to ignore everything it was passed — but the language is this system's `Select`, not
v1's `where` object, so the interpreter is Rust and lives beside the provider.

- [x] 3.1 `run_select_over` in `sc-catalog`: a `Select` and a list of JSON rows in, `Row`s out.
  `WHERE` (the full `Expr` tree this system's reads actually build), `ORDER BY`, `LIMIT`,
  `OFFSET`, the projection list, and un-grouped aggregates — `count(*)` is not an optional
  extra, it is what the tables page shows beside every table's name.
- [x] 3.2 What it **refuses**, by name rather than by wrong answer: a `JOIN`, a `GROUP BY`, a
  `HAVING`, a subquery source, a correlated subquery in an expression. A provided table is not
  in the database, so nothing can join to it; saying so is the whole of the difference between
  a limitation and a bug.
- [x] 3.3 Pushdown: the `Select`'s filter is translated into v1's `where` object when it is
  wholly expressible in it (`{ col: value }`, `{ col: { gt } }`, `in`, `or`), and the
  ordering/limit/offset into v1's `options`, so `@saltcorn/postgres-tables` can do the work in
  the remote database. Anything else is passed as `{}`, and §3.1 runs regardless — a provider
  that ignored the hint still gets the right answer.

### 4. The module half

- [x] 4.1 `module-host.mjs` reads `table_providers` (an object, or a function of the module's
  own configuration, which is v1's `withCfg`). It moves out of the unsupported census.
- [x] 4.2 The manifest gains `table_providers: [{ name, config_fields }]`, where the fields are
  the provider's own `configuration_workflow` flattened exactly as a module's own settings are
  (§5 of the last milestone) and translated by `spec.rs` into `FormField`s.
- [x] 4.3 Two ops, `provider_fields` and `provider_rows`, on the host script and on
  `DenoModuleHost`/`ModuleHost`. Routed to the worker the module is loaded on, for `run` and
  `call`'s reason: `get_table(cfg)` closes over what the module built at load time.
- [x] 4.4 `ModuleTableProviders`, the `TableProviderHost` implementation, and v1's field
  declarations (`{ name, label, type, primary_key, required }`) translated into `DataField`s.
- [x] 4.5 `sc-server` installs it on the catalog beside `ModuleFunctions`, and reloads the
  catalog afterwards — a module that has just been installed, configured or deleted changes
  what a provided table's fields are.

### 5. The admin API and the UI

- [x] 5.1 `listTableProviders` — every provider every loaded module supplies, with the config
  spec the "new table" form renders.
- [x] 5.2 `createProvidedTable` — a name, a module, a provider, and the configuration.
  `updateProvidedTable` — the configuration alone, on the table's settings page, secrets
  merged the way a module's own settings are.
- [x] 5.3 `dropTable` on a provided table forgets the row and issues no DDL; the schema editor
  refuses `add_field`, `alter_field`, `drop_field` and the constraint operations on one, naming
  the provider that decides the columns. `alter_table` (label, description, access rules,
  ownership) is allowed: those are the overlay's, and a provided table has them too.
- [x] 5.4 The New table dialog offers the providers as a third source beside blank and CSV, and
  the table page shows the provider, its configuration form and its issues.

### 6. Tests and documentation

- [x] 6.1 `sc-catalog`: the in-memory `Select` interpreter, the round-trip of a provided-table
  definition through `_sc_tables`, and a fake `TableProviderHost` proving the catalog builds a
  table out of a row and serves its rows through `Catalog::provider`.
- [x] 6.2 `sc-module`: the echo fixture gains a real table provider (its `table_providers` key
  is already there as census filler), and the Deno host test asserts the manifest, the fields
  and the rows.
- [x] 6.3 **`@saltcorn/rss` itself**, installed from the checkout by npm, loaded on a Deno
  worker granted one host, and pointed at a feed served by the test over `127.0.0.1` — the
  milestone's definition of done, minus the browser.
- [x] 6.4 `docs/tutorial-table-providers.md`, the design doc's §8.3, and the CHANGELOG.
