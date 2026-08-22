# Saltcorn v2 — SQLite: a second database backend

Ordered, checkable task list for the fourteenth milestone after the MVP. Earlier lists are
archived in [docs/TODO-mvp.md](./TODO-mvp.md) (the MVP),
[docs/TODO-post-mvp-1.md](./TODO-post-mvp-1.md) (file stores + the React framework),
[docs/TODO-post-mvp-2.md](./TODO-post-mvp-2.md) (the `_sc_tables`/`_sc_fields` overlays,
rich types and File fields), [docs/TODO-post-mvp-3.md](./TODO-post-mvp-3.md) (ownership
formulae, calculated fields and row-level security),
[docs/TODO-post-mvp-4.md](./TODO-post-mvp-4.md) (actions and triggers),
[docs/TODO-post-mvp-5.md](./TODO-post-mvp-5.md) (the file-store IDE),
[docs/TODO-post-mvp-6.md](./TODO-post-mvp-6.md) (agents),
[docs/TODO-post-mvp-7.md](./TODO-post-mvp-7.md) (the GraphQL provider),
[docs/TODO-post-mvp-8.md](./TODO-post-mvp-8.md) (REST queries, custom SQL and the
generated client), [docs/TODO-post-mvp-9.md](./TODO-post-mvp-9.md) (table constraints
and indexes), [docs/TODO-post-mvp-10.md](./TODO-post-mvp-10.md) (email),
[docs/TODO-post-mvp-11.md](./TODO-post-mvp-11.md) (tables in code),
[docs/TODO-post-mvp-12.md](./TODO-post-mvp-12.md) (concurrent code bodies) and
[docs/TODO-post-mvp-13.md](./TODO-post-mvp-13.md) (modules). Scope and rationale remain in
[docs/GOALS.md](./GOALS.md) and [docs/TECHNICAL_DESIGN.md](./TECHNICAL_DESIGN.md).

`DatabaseDriver` has been a trait with one implementation since the first week. A trait with one
implementation is a guess about what varies, and every guess in this workspace has been settled
by writing the second thing: the second file-store backend is what made `connect_from_def` a
registry, the second API provider is what made a mount a projection. This milestone writes the
second database.

**Why SQLite specifically.** Postgres is what a deployment runs. SQLite is what a laptop, a
Raspberry Pi, a demo and a one-file backup run — no server, no role, no `createdb`, and the
whole database is a file somebody can copy. It is also the backend that tests the trait
hardest, because it is *not* Postgres-shaped: five storage classes instead of a type system, an
`ALTER TABLE` that can barely alter anything, no `COMMENT ON`, no policies and no
`LISTEN`/`NOTIFY`. Anything the layers above quietly assumed about Postgres shows up here as a
failure rather than as a subtlety.

**Milestone definition of done:** `saltcorn serve --sqlite ./app.sqlite` on a machine with no
Postgres at all brings up a whole installation — the file is created, every `_sc_*` table is
bootstrapped, an admin signs up, makes a table with a key that numbers itself, and the rows are
still there after a restart. And an admin on a Postgres deployment opens Tables → Connections,
chooses **SQLite file**, picks a `.sqlite` file out of one of the file stores, and that file's
tables are in the tables list beside everything else, read and written like any other table.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## The specification

### 1. The declared type is the type

SQLite stores five things — null, integer, real, text, blob — and a column has only an
*affinity*. Eleven `Value` kinds do not survive that: a timestamp and a uuid are both text.

What closes the gap is the one thing SQLite keeps verbatim and reports back: the **declared
type name**. So the DDL emits `sc-types`' own names (`int8`, `jsonb`, `timestamptz`, `bytea`)
rather than translating them to SQLite's five, and `PRAGMA table_info` /
`sqlite3_column_decltype` hand them back for introspection and for decoding. A file written by
something else says `INTEGER`, `VARCHAR(64)`, `DATETIME`, `BLOB`, and those are read too.

Text encodings are fixed and sortable: a timestamp is `YYYY-MM-DDTHH:MM:SS.mmmZ`, always UTC and
always the same width, so lexicographic order is chronological order. Reading is deliberately
more lenient than writing, because a foreign file holds `2024-05-06 12:00:00` and a unix time.

### 2. What `ALTER TABLE` cannot do, a rebuild does

SQLite cannot alter a column, add or drop a primary key, or add a `UNIQUE` column. The two
halves of "make this field the key" — `SetPrimaryKey` and `SetColumnGenerator`, which is how a
table gets a key at all, since none is invented — are therefore applied by the documented table
rebuild: create the new shape, copy the rows, drop the old, rename, put the indexes and triggers
back. Inside a savepoint, with `PRAGMA defer_foreign_keys` and a `foreign_key_check` before it
is released.

`render_ddl` refuses a rebuild instead of emitting a guess: it is written from the table's
*current* columns, which the change does not carry.

### 3. The other three gaps

- **No `COMMENT ON`**: a constraint's error message and a row constraint's formula ride in an
  object's comment (§5.1), so the driver keeps them in `_sc_object_comments` — written by the
  same `SetComment` change, read back by introspection.
- **No SQLSTATE**: SQLite reports an extended result code, and everything above reads the
  Postgres spelling. The driver translates the constraint codes (`23505`, `23502`, `23503`,
  `23514`) so an admin's own message for a violated rule is shown on either backend.
- **No policies, no notify**: the capabilities say so, and the layers above already branch on
  them. A `false` that is honest is the whole reason `DbCapabilities` exists.

### 4. Two ways in

- **Primary**: `sqlite = "…"` in a `saltcorn.toml` environment, or `--sqlite PATH`, or
  `SALTCORN_SQLITE`. Exclusive with the Postgres parameters — a section naming both is refused,
  because nothing can choose between two databases for the operator.
- **Secondary**: a `_sc_db_connections` row whose `backend` is `sqlite`, naming a **file store
  and a path inside it** rather than a filesystem path. A SQLite database is a file, the stores
  are where Saltcorn's files are, and a bare path would let anyone with that screen open any
  file the server process can read.

---

## Phase 0 — The dialect seam

- [x] `sc-query`: `SqlDialect::binary_op`, defaulted to `default_bin_op`. SQLite has no `ILIKE`
      (its `LIKE` is already case-insensitive for ASCII), and one operator is not a reason to
      copy the whole renderer.

## Phase 1 — The driver (`sc-db-sqlite`)

- [x] The crate, on `rusqlite` with the amalgamation bundled: the features this driver needs are
      recent (`RETURNING` 3.35, `->` 3.38, `IS DISTINCT FROM` and right/full joins 3.39) and a
      distribution's libsqlite3 is whatever it froze.
- [x] `SqliteDialect`: double-quoted identifiers, `?n` placeholders (numbered, so a re-used
      named parameter binds once), `ILIKE` → `LIKE`.
- [x] `value`: the `Value` ⇄ storage-class mapping, through the declared type (phase 1 of the
      specification above).
- [x] `ddl`: every `SchemaChange`, the rebuild for the two SQLite cannot express, the comment
      side table, and the translation of the defaults Saltcorn generates (`gen_random_uuid()`
      → a randomblob uuid, `now()` → this driver's own timestamp format).
- [x] `introspect`: `sqlite_master` + the `pragma_*` table-valued functions — columns, keys,
      foreign keys (a `REFERENCES t` with no column resolved to `t`'s key), unique constraints,
      indexes, triggers, comments. `INTEGER PRIMARY KEY` is reported as `Identity`.
- [x] `exec`: rendering, binding, decoding, and the SQLSTATE translation.
- [x] `pool` + `transaction`: a connection per transaction, WAL, `busy_timeout`, foreign keys
      **on** (SQLite defaults them off), and every blocking call on tokio's blocking pool.
- [x] `SqliteDriver`: `open` (creates the file), `open_existing` (refuses to invent one),
      `open_in_memory`, and the `DatabaseDriver` impl.
- [x] Tests (`crates/sc-db-sqlite`): the dialect and the value mapping as unit tests; and
      against a real database — CRUD with `RETURNING`, every value kind round-tripping,
      timestamps sorting chronologically, introspection, an enforced foreign key, a unique
      violation with its SQLSTATE, comments, the rebuild that adds a key to a table that has
      rows and indexes, columns added and dropped, transactions (commit, rollback, abandoned),
      `describe`, a file created and reopened, and concurrent queries.

## Phase 2 — SQLite as the primary database

- [x] `sc-config-file`: the `sqlite` key on an environment, and `Environment::check` refusing a
      section that names two databases.
- [x] `sc-cli`: `--sqlite PATH` / `SALTCORN_SQLITE`, `DbConfig::connect` returning
      `Arc<dyn DatabaseDriver>` rather than a `PgDriver`, and a `target()` that names the file.
- [x] Test (`sc-cli/tests/sqlite_primary.rs`): the whole boot path against a file — every
      `_sc_*` table bootstrapped, a table created with a key that numbers itself, a row written
      and read back through the catalog, and still there after a reconnect.
- [x] Test (`sc-cli/tests/config_file.rs`): an environment that names a file, one that names
      both kinds (refused), and `--database-url` outranking the file's `sqlite`.

## Phase 2a — What the row layer assumed about Postgres

Three statements above layer 2 were Postgres text sent unconditionally, and each is now asked of
the driver instead — which is what the second backend was for.

- [x] `Transaction::set_read_only` and `Transaction::defer_constraints`, implemented by both
      drivers (`SET TRANSACTION READ ONLY` / `SET CONSTRAINTS ALL DEFERRED`; `PRAGMA query_only`
      / `PRAGMA defer_foreign_keys`). The SQLite transaction clears `query_only` when it hands
      its connection back, or the next writer on that connection would fail.
- [x] `DbCapabilities::identity_sequences`: a CSV import winds the sequence behind an identity
      key past the rows it just wrote, and a backend that numbers a key from the table itself
      (SQLite's rowid) has no sequence to wind and must not be sent the statement that would.
- [x] The caller-context GUCs are set only where the backend advertises row-level security —
      they exist for the policies, and a backend with none has nothing to hand them to.
- [x] Test (`sc-api/tests/sqlite_rows.rs`): a CSV that carries its own keys imported into a
      SQLite table and the next insert not colliding, a re-import updating, and a
      caller-context read — including a read-only one that refuses a write and leaves its
      connection usable.

## Phase 3 — SQLite as a secondary connection

- [x] `_sc_db_connections` gains `backend`, `file_store` and `file_path`; `DbConnectionDef` and
      its validation branch on the backend; `dial` takes the catalog, because a SQLite
      connection is a path inside a file store.
- [x] `sc-api`/`sc-server`: the three new fields on the wire, the body mapping, and the
      redaction discipline unchanged.
- [x] `ui/admin`: a **Kind** chooser on the connect dialog, and — for SQLite — a file store
      select plus a browser over that store's files, with the path still typeable. The kind is
      fixed once saved: changing it would repoint every table stamped with that name.
- [x] Test (`sc-catalog/tests/sqlite_connections.rs`): a file in a store connected, its tables
      stamped with the connection, a write reaching the file, disconnecting leaving the file
      alone — and the two failures an admin can cause (a store that is not connected, a file
      that is not there) refused with a sentence naming which.
- [x] Test (`sc-server/tests/db_connection_admin_api.rs`): the same through the admin API,
      including Test-before-save and a body naming a file store nobody defined.
- [x] Test (`ui/admin/src/dbConnection.test.ts`): the form's rules for the SQLite half.

## Phase 4 — Documentation

- [x] `docs/TECHNICAL_DESIGN.md` §5.2: the backend, what it solves and what it does not
      advertise; §5.0: a connection names a backend.
- [x] `README.md`: `--sqlite` in the connection table, and what is genuinely absent.
- [x] CHANGELOG entry.

---

## Explicitly OUT of scope for this milestone

- **Row constraints and full-text indexes on SQLite.** Both are generated as Postgres SQL — a
  PL/pgSQL trigger and a `to_tsvector` expression — and translating them is a milestone about
  the constraint layer, not about a driver. They fail at the database with the database's own
  message rather than being silently skipped.
- **Row-level security on SQLite.** There is nothing to enforce it with; the capability says so
  and the layers above already branch.
- **A `pg_dump`-shaped backup of a SQLite database.** The backup module's CSV path works; the
  Postgres-specific parts of restore (`pg_get_serial_sequence`) do not, and a file-copy backup
  is a better answer for a file database than teaching the current one a second dialect.
- **Migrating an installation from Postgres to SQLite, or back.** Two backends, no converter.
- **MySQL, or any third backend.** The trait now has two implementations, which is what the
  milestone was for.
