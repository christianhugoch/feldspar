# Saltcorn v2 — Writable table providers

Ordered, checkable task list for the seventeenth milestone after the MVP. Earlier lists are
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
[docs/TODO-post-mvp-14.md](./docs/TODO-post-mvp-14.md) (SQLite),
[docs/TODO-post-mvp-15.md](./docs/TODO-post-mvp-15.md) (modules in-process) and
[docs/TODO-post-mvp-16.md](./docs/TODO-post-mvp-16.md) (table providers). Scope and rationale
remain in [docs/GOALS.md](./docs/GOALS.md) and
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md).

The last milestone ended with a sentence, and this one is that sentence:

> `write()` fails with a sentence naming the provider and saying that writable providers are
> not implemented yet.

v1's `get_table(cfg)` may answer three more methods beside `getRows`:

```js
get_table: (cfg) => ({
  getRows:    async (where, opts)  => [...],
  insertRow:  async (rec, user)    => newId,   //  ⎫
  updateRow:  async (rec, id, user) => {},     //  ⎬ this milestone
  deleteRows: async (where, user)  => {},      //  ⎭
})
```

`@saltcorn/postgres-tables` is the reference implementation of all three: it answers them when
its `read_only` flag is off and omits them when it is on, which is the whole of v1's
writability model — **a capability of the configuration, not of the provider**, and known only
by calling `get_table`.

**Milestone definition of done:** a table whose provider is a remote PostgreSQL table is
edited from the admin UI's row editor — a row added, a field changed, a row deleted — and the
change is visible in the remote database. Nothing above `sc-catalog::provider` knows the table
is not in a database, and a provider configured read-only refuses each of the three with a
sentence naming itself.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## The specification

### 0. `pg` under Deno, and what `@saltcorn/postgres-tables` can and cannot be here

This milestone was gated on a question with an empirical answer: **does the npm `pg` module run
on a `deno_runtime` worker?** It does. A fixture requiring `pg`, loaded on a module worker in
this process and granted one socket, opens a connection and returns rows — see
`crates/sc-module/tests/pg_provider.rs`. Nothing was shimmed and no branch of `pg` was avoided.

What *cannot* be run here is `@saltcorn/postgres-tables` itself, and the reason is not `pg`:
its first line is `require("@saltcorn/data/db")`, which pulls v1's entire `@saltcorn/data`
package into the worker. That package loads far enough to fail inside itself — the observed
error is `isNode is not a function`, a CommonJS interop difference in v1's own module graph
that has nothing to do with table providers. A v1 plugin that is a *thin* wrapper over an npm
library (`@saltcorn/rss`) loads here; one that is a client of v1's own internals does not, and
no amount of table-provider work changes that.

So `@saltcorn/postgres-tables` is this milestone's **specification** — the three method
signatures, the `read_only` flag that withholds them, and the `where`/`options` pair honoured in
SQL — and the test fixture is a self-contained provider written against `pg` directly, doing
what it does for the same reasons.

- [x] 0.1 A `pg`-backed fixture module (`tests/fixtures/pg-module`), modelled on
  `@saltcorn/postgres-tables`: a `configuration_workflow` asking for the connection and the
  table, `fields(cfg)` reporting the columns, and a `get_table(cfg)` whose four methods are
  SQL against the remote database, with `read_only` omitting the writing three.

### 1. Writability is a property of the configuration

v1 asks `get_table(cfg)` and looks at what came back. Here the same question is asked once per
catalog reload, beside the one that asks for the columns, and the answer is carried on the
table:

| | |
|---|---|
| `TableSource::Provider { module, provider, writes }` | which of the three the provider answers |
| `ProvidedWrites { insert, update, delete }` | three booleans; `NONE` for a read-only one |

Asked at reload rather than at write time for the reason the columns are: the admin UI has to
know *before* it draws a button whether there is anything behind it, and a screen that offers
"Add row" on a feed is a screen that lies. The write path checks again on the far side anyway,
because a module can be reconfigured between a reload and a write.

- [x] 1.1 `ProvidedWrites` in `sc-catalog`, the third field of `TableSource::Provider`, and
  `Table::provided_writes()`. `Table::provided` takes it.
- [x] 1.2 `TableProviderHost::writes(module, provider, table, config)`, and
  `Catalog::reload` asking it beside `provided_fields` — a provider that cannot be reached
  answers `NONE`, which is the same fail-closed rule a broken ownership formula gets.

### 2. `Statement` → v1's three calls

The read path interprets a `Select` over JSON rows because a provider may ignore the `where`
object. The write path has the opposite shape: **v1's write methods are not a query language**,
they address one row by its primary key, so the translation is a *narrowing* and every
narrowing that cannot be made is refused by name.

| Statement | v1 call | How the address is found |
|---|---|---|
| `INSERT` | `insertRow(rec)` per row | — |
| `UPDATE` | `updateRow(rec, id)` per row | the filter is run as a `SELECT` first, and its rows' primary keys are the ids |
| `DELETE` | `deleteRows(where)` | the filter is run as a `SELECT` first; the `where` handed over is `{ pk: { in: [ids] } }` |

Reading first is not an optimisation to skip: `RETURNING` is on every write this system issues
(`rows.rs` asks for `*` on all three), v1's `updateRow` returns nothing and its `insertRow`
returns only an id, so the row that comes back is read either way. `DELETE` must read *before*
it deletes, since afterwards there is nothing to read.

- [x] 2.1 `ProvidedTableProvider::write` dispatching the three, and the `RETURNING` rows read
  back through the same `rows()`/`run_select_over` pair a `SELECT` uses.
- [x] 2.2 What it refuses, by name: a table whose provider declares no primary key (there is
  no way to address a row); a non-literal expression in a `SET` or in a `VALUES` (there is no
  database to evaluate it in); a write the provider does not answer, naming which of the three
  and saying that the provider's configuration decides it.
- [x] 2.3 `insertRow`'s answer is v1's: the new primary key, or nothing. Nothing is not an
  error — a provider whose key is generated remotely may not know it — and the row that comes
  back is then the record as written, so `RETURNING *` still answers.

### 3. The module half

- [x] 3.1 Four ops on the host script — `provider_writes`, `provider_insert`, `provider_update`,
  `provider_delete` — each calling `get_table(cfg, table)` afresh, as `provider_rows` does and
  for its reason.
- [x] 3.2 The same four on `DenoModuleHost`/`ModuleHost`, routed to the worker the module is
  loaded on.
- [x] 3.3 `ModuleTableProviders` implements the four new trait methods, re-checking the
  provider's name on this side exactly as the read methods do.

### 4. The admin API and the UI

- [x] 4.1 `table_json`'s `provider` object gains `writes: { insert, update, delete }`.
- [x] 4.2 The table page's data strip reads it: "Edit" rather than "View" when anything is
  writable, the CSV import tile offered when `insert` is, and the row editor's own controls
  (add, save, delete) each gated on their own capability.
- [x] 4.3 The row editor refuses nothing it can do and offers nothing it cannot, and the
  server's own sentence is what a refusal says.

### 5. Tests and documentation

- [x] 5.1 `sc-catalog`: a fake `TableProviderHost` recording what it was asked, proving the
  three translations, the read-back of `RETURNING`, and each refusal in §2.2.
- [x] 5.2 `sc-module`: the echo fixture gains a writable in-memory provider, and the Deno host
  test asserts the four ops.
- [x] 5.3 `crates/sc-module/tests/pg_provider.rs`: the `pg` fixture against a real database
  from the test harness — the milestone's definition of done, minus the browser — including a
  read-only configuration refusing all three.
- [x] 5.4 `docs/tutorial-table-providers.md`, the design doc's §8.3, and the CHANGELOG.
