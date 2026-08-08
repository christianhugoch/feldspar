# Saltcorn v2 — API improvements TODO

Ordered, checkable task list for the eighth milestone after the MVP. Earlier lists are
archived in [docs/TODO-mvp.md](./docs/TODO-mvp.md) (the MVP),
[docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md) (file stores + the React framework),
[docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md) (the `_sc_tables`/`_sc_fields` overlays,
rich types and File fields), [docs/TODO-post-mvp-3.md](./docs/TODO-post-mvp-3.md) (ownership
formulae, calculated fields and row-level security),
[docs/TODO-post-mvp-4.md](./docs/TODO-post-mvp-4.md) (actions and triggers),
[docs/TODO-post-mvp-5.md](./docs/TODO-post-mvp-5.md) (the file-store IDE),
[docs/TODO-post-mvp-6.md](./docs/TODO-post-mvp-6.md) (agents) and
[docs/TODO-post-mvp-7.md](./docs/TODO-post-mvp-7.md) (the GraphQL provider); scope and
rationale remain in [docs/GOALS.md](./docs/GOALS.md) and
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md) (**§13.1/§13.3/§13.4**). This milestone
is the **API** section of GOALS.md as it now stands: everything in it that the GraphQL
milestone did not already do.

**Milestone definition of done:** a caller GETs

```
/api/books?select=title,published,author(name,country)&published=gte.2020-01-01&order=published.desc&limit=20
```

and gets it — **one statement**, the author's columns projected as Ⱶ-join subqueries rather
than fetched, the caller seeing exactly the rows their role and the tables' ownership formulae
permit. The application's admin adds a **custom SQL query** with typed input parameters, picks
its HTTP method by hand, and it appears as a typed method on the app's generated client —
typed from the columns *Postgres itself* reported when the query was prepared. The same query
can be added without a browser: `saltcorn api add-query`, which validates it, saves it and
rewrites the client. Nobody regenerates anything by hand: a table gains a column and the app's
`src/saltcorn/` has it at once, with a button in the admin UI that does it on demand and
**rescaffolds** a project directory that has been emptied. A coding agent opening the project
finds `AGENTS.md` at the root, pointing at `src/saltcorn/README.md` — which says the directory
is maintained by the server and how to add a custom query — beside a `schema.sql` describing
the tables it may write SQL against. And an admin who does not want GraphQL aggregations
leaves the switch off, and they are not in the SDL.

**Not a second data path** — the same standing rule. The REST query string lowers to the
*same* `rows::RowQuery` the GraphQL provider builds and goes through the *same*
`ownership::read_rows_as`. The one genuinely new path is custom SQL, which is raw by
definition; decision 6 is how it is fenced.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## Decisions taken up front

1. **One `RowQuery`, two syntaxes.** A REST list request parses into `rows::RowQuery` and runs
   through `ownership::read_rows_as` — the entry point the GraphQL root field and the agent's
   `query_table` already use. The comparison vocabulary (`eq`/`ne`/`gt`/`gte`/`lt`/`lte`/`in`/
   `is_null`/`like`/`ilike`) is **one** lowering, lifted out of `graphql/args.rs::comparison`
   into `sc-api::filter` and shared, so a Date filter binds a date in both surfaces and there
   is one place that decides what a filter may say. REST is a *syntax* over the read layer, not
   a second reader.
2. **PostgREST's syntax, as a stated subset.** Taken: `select=title,author(name,country)` with
   embeds to any depth through outgoing keys, `alias:column` renaming, `column=op.value`
   filters, `order=column.desc`/`.asc`, `limit`, `offset`. Deliberately **not** taken —
   one-to-many embeds (a second, batched read, which is GraphQL's Phase 4 loader; the goal
   names *join fields*), `!inner` (it changes which parents come back — that is a join, and our
   read is one table plus correlated subqueries), the spread `...` operator, `::` casts, and
   `or=(…)`. Everything not taken is refused **by name**, never ignored: a silently dropped
   filter is rows the caller did not ask for, which is the worst failure this API can have.
3. **A query parameter is part of the endpoint.** `Endpoint` gains `query: Vec<QueryParam>`,
   because the typed client cannot otherwise express `?select=` or a `GET` custom query's
   parameters — and an untyped `fetch` written by hand beside a generated client is exactly
   where drift starts (§13.1). The same field serves both features, which is why it is one
   phase and comes first. `ApiRequest.query` becomes an **ordered `Vec<(String, String)>`**:
   a filter vocabulary needs a repeated key (`?published=gte.2020&published=lt.2024`) and
   today's `HashMap` keeps whichever arrived last — a silent failure hiding in a data structure.
4. **Custom SQL is `Statement::Raw`, and it is the only escape hatch.** `sc-query`'s enum is
   the representation of a query (GOALS: "enum-based representation of an SQL query"), and raw
   text is a hole in it; the goal asks for the hole, so it gets exactly one, named, with the
   rule written on it: `Raw { sql, binds }` is constructed **only** from an admin-authored
   query definition, never from anything a caller sent. Parameters are always bound, never
   interpolated, and the `:name` → dialect-placeholder rewriting skips string literals,
   dollar-quoted bodies, line and block comments, and Postgres's `::` cast — so `x::text` is a
   cast and not a parameter called `text`.
5. **The database types the result; the admin types the parameters.** The admin declares each
   input parameter's type (GOALS requires it); the *result* columns come from preparing the
   statement — a new `DatabaseDriver::describe`, over `tokio_postgres`'s `prepare_typed`, whose
   `Statement::columns()` carries each column's name and type. Those become the endpoint's
   `TypeSchema` and the generated client's return type. The alternative — the admin declares
   the result shape too — is a second source of truth that goes stale the first time anyone
   edits the SQL. It also makes saving a broken query impossible: a query that will not prepare
   is refused at the keyboard, carrying Postgres's own message.
6. **A custom query's authority is its own, and it is stated loudly.** Raw SQL does not go
   through the row layer, so ownership formulae, rich-type coercion, `File`-field rules and
   table events do **not** apply to it. Therefore: every query carries a `min_role` defaulting
   to **admin** (§10.2's trigger rule — an access nobody has thought about must not be the one
   that turns out to be public); it executes inside the caller-context transaction, so an
   RLS-protected table's policies still decide what it can see; and the editor and the tutorial
   say both of these in as many words. A custom query is a hole an admin opens on purpose.
7. **The method is the admin's choice; the read-only guarantee is not.** GOALS: "select HTTP
   method manually per custom SQL query" — so the method is a stored field with no inference
   from the SQL. What is inferred is the *transaction*: a `GET` query runs in a `READ ONLY`
   transaction, so an `UPDATE` behind a `GET` fails loudly rather than mutating something a
   cache or a crawler asked for. Other methods commit.
8. **`ApiConfig` gains a config object.** `ApiConfig { provider, mount, config: Attrs }`,
   validated on save against a spec the provider declares (`ApiProviderInfo::config_spec`) and
   rendered by the admin form's existing `SettingsFields` — exactly how a framework's settings
   already work (§13.2/§13.3), with no provider-specific code in the form. GraphQL's
   aggregation switch and its four limits live there. REST's **custom queries** live in the
   same object but as a typed `queries` array with an editor of its own: a list of records each
   carrying a nested list of parameters is not a settings form, and pretending otherwise would
   distort both.
9. **Generated is generated; the project root is the developer's.** `README.md` and
   `schema.sql` join `client.ts` / `hooks.ts` / `graphql.ts` / `schema.graphql` **inside**
   `src/saltcorn/`, and are rewritten on every emit like everything else there. `AGENTS.md` is
   written at the project **root**, at scaffold time only, and is never overwritten — it is the
   developer's file, coding agents append to it, and clobbering it would destroy their work.
   That is the same boundary §13.3 already draws, applied to two new files.
10. **Regeneration writes the generated files; it does not build.** "If the API definition
    changes, the client code must be updated automatically" is discharged by re-emitting
    `src/saltcorn/**` — fast, no external process, and it cannot fail on a bundler — leaving
    `npm run build` to the build button and the dev server. A re-emit that fails (an unreachable
    store, a `code` app with no client path) is logged and never takes a mounted application
    down.
11. **Tests split as before.** Rust unit tests for the query-string parser, the placeholder
    rewriter, the `select` grammar and the client generator (no database); `crates/sc-api/tests`
    and `crates/sc-server/tests` against **real Postgres** for reads, custom queries and every
    authorization rule; `vitest` for the admin editor's model; and the scaffold's `tsc --noEmit`
    test extended to the new generated files. Every authorization rule gets a test that would
    *fail open* if the rule were dropped.

---

## Phase 1 — Query parameters in the endpoint model (`sc-api`)

- [x] **`QueryParam`** in `endpoint.rs`: name, `ValueType`, `required`, and `repeated` (the one
      that carries `?published=gte.…&published=lt.…`), plus `Endpoint::query(…)`. Serde like
      the rest of the model, so a projected endpoint set still round-trips.
- [x] **`ApiRequest.query` becomes ordered pairs** (`Vec<(String, String)>`) with `get` and
      `all` helpers (`query_get`/`query_all`, so they read clearly on the request itself), and
      `sc-server`'s `parse_query` preserves order and duplicates — and percent-decodes, so what a
      handler reads is what the caller wrote. Every existing caller moved with it, `HandlerCtx`
      included — there is no compatibility shim, and a `HashMap` left anywhere is decision 3's
      silent failure waiting to be reintroduced.
- [x] **TypeScript generation**: an endpoint with query parameters takes a typed options object,
      encoded into the URL by the generated client; an endpoint with none is unchanged (no empty
      options argument appearing on every method). Regenerate the admin's checked-in client and
      keep its drift test green. (No admin endpoint declares one yet, so the checked-in clients
      are byte-identical and the drift test needed no regeneration.)
- [x] Tests: the generated signature and the encoding for a required, an optional and a repeated
      parameter; a value needing escaping survives a round trip; the emitted client type-checks
      (the existing generator type-check test); `parse_query` keeps both values of a repeated key.

## Phase 2 — The REST read query string (`sc-api::rest`)

- [x] **`sc-api::filter`**: the comparison vocabulary lifted out of `graphql/args.rs` and made
      the shared lowering (decision 1). The GraphQL tests that cover it are the proof the move
      changed nothing; one new test asserts the two syntaxes produce the identical `Expr` for
      the same comparison.
- [x] **`rest/query.rs`**: parse `select`, `column=op.value`, `order`, `limit`, `offset` into a
      `RowQuery`. An unknown column, an unknown operator, an unparseable bound or a
      not-taken PostgREST feature is a **400 naming it** (decision 2). `limit` is clamped to the
      application's cap rather than trusted, as the GraphQL provider's is. (An absent one
      *becomes* the cap, as GraphQL's does; the cap is `RestProvider::with_row_cap`, defaulting
      to 500, until Phase 3's provider configuration owns it.)
- [x] **`select` with embeds**: a leaf column projects itself; `author(name,country)` projects
      `sc-expr`'s `join_path_expr` correlated subqueries — one column per requested leaf, nested
      back into `{"author": {"name": …}}` in the response — with `alias:` renaming and nesting
      to any depth. A null key yields a null object, not an error.
- [x] **`ownership::join_guard` on every embed**, the rule Phase 7 of the GraphQL milestone
      landed: a caller who may not read the target table is refused by name rather than handed
      a withheld row one column at a time.
- [x] The list endpoints **declare** these as query parameters (Phase 1), so `listBooks` takes a
      typed options object: `select`, `order`, `limit`, `offset` typed, and filters as an
      explicit `filter?: Record<string, string>` — honest about being a string vocabulary rather
      than pretending to a type it does not have. (`QueryParam` gained a `map` shape for it,
      whose keys are the query-string keys.)
- [x] Tests (real Postgres): the milestone's query and its GraphQL equivalent return the same
      rows; an embed is **one** statement (statement count, not timing); ownership formulae and
      RLS still filter what a filtered/ordered/paged read returns; a filter on an unknown column,
      an unsupported operator and `!inner` are each a 400 naming the thing; a caller who may not
      read the embedded table gets `join_guard`'s refusal; `select` with a column the caller's
      role cannot read does not become a way to read it.

## Phase 3 — The GraphQL aggregation switch, and provider configuration

- [x] **`ApiConfig.config: Attrs`** — the field, its storage in `_sc_applications.apis`, and
      validation on save against the provider's declared spec (an unknown key is refused, as a
      framework's is). (`validate_api_config`, called from `save_application` beside the
      framework's own validation and the mount check; it names the provider *and* the setting.)
- [x] **`ApiProviderInfo::config_spec`** (`Vec<FormField>`), returned by `listApiProviders`, and
      the application form rendering it with `SettingsFields` — no provider-specific code in the
      form, exactly as with frameworks. (REST declares `row_cap`, which now comes from the
      application rather than from `RestProvider::with_row_cap`'s default at the call site.)
- [x] **`GraphqlLimits` comes from configuration** rather than from `project_with`'s defaults at
      the call site, and gains the aggregation switch. Aggregates are **off** unless switched
      on (GOALS: "optional and enabled with a switch"): they are the most expensive thing the
      schema can express, and the expensive thing should be present because someone asked for it.
      (`GraphqlLimits::from_config`/`to_config`; `graphql_provider` takes the whole `ApiConfig`,
      so the SDL a build writes and the schema a mount answers stay one projection.)
- [x] With the switch off, `X_aggregate` and the `_aggregate` fields on child relations are
      **absent from the schema**, so asking for one is the library's own "field not found"
      validation error rather than a silent null or a resolve-time refusal. (The
      `XAggregate`/`XNumericFields`/`XComparableFields` objects and the `XSelectColumn` enum go
      with them — they exist only to answer those fields.)
- [x] Tests: the SDL snapshot in both states (the existing snapshot moves to the switched-on
      one); a document using an aggregate against a switched-off app is refused before a
      statement is issued; the limits round-trip through save/load; the admin form round-trips
      a provider's settings (vitest).

## Phase 4 — Custom SQL queries: the model and the execution

- [x] **`CustomQuery`** (in `sc-api`): name, description, HTTP method, sub-path, SQL, ordered
      typed parameters, `min_role`. Validated on save — a name that is a valid client method
      name and unique within the API, a path that cannot collide with a table's routes, at
      least one statement, and every `:name` in the SQL declared as a parameter (and every
      declared parameter used). (Also: a name no *table endpoint* already holds, a path outside
      `actions`/`login`/`logout`/`whoami`, and no two result columns of one name — which would
      collapse into one JSON property. Stored in the API row's config as a typed `queries`
      array, lifted out of the settings-spec check by `validate_api_config`, which now takes
      the whole `Application` because the path and name rules are about the app's tables.)
- [x] **`Statement::Raw { sql, binds }`** in `sc-query`, rendered by the dialect as-is with its
      binds, with the construction rule of decision 4 written where the variant is declared.
      Unit tests for rendering and serde round-tripping. (Plus `param_types`: `WHERE :q IS NULL`
      has no inferable parameter type, so a query that *described* under the admin's declared
      types would have failed the first time it *ran*. Describing and running derive them from
      one place.)
- [x] **The `:name` rewriter**: named parameters to the dialect's positional placeholders,
      skipping single-quoted literals, dollar-quoted bodies, `--` and `/* */` comments and `::`
      casts. Unit-tested against each of those specifically, because each is a way to corrupt
      an admin's query silently. (`sc_query::rewrite_named_params`; also skips quoted
      identifiers, nests block comments as Postgres does, gives a repeated `:name` one
      placeholder and one bind, and counts statements on the same scan.)
- [x] **`DatabaseDriver::describe(sql, param_types) -> Vec<(String, TypeRef)>`**, implemented by
      `PgDriver` over `prepare_typed`, mapping Postgres's reported column types through
      `BasicType::from_sql_type`. A statement that will not prepare returns Postgres's own error.
      (Returns `Vec<DescribedColumn>` — name plus the backend's own type name — because `sc-db`
      does not know about `TypeRef`; the mapping is `sc-api`'s, exactly as `PhysicalTable`'s
      `sql_type` is mapped by the type layer.)
- [x] **Projection into the endpoint set**: one endpoint per query at `{mount}{path}` with the
      admin's method; parameters as **query parameters** for `GET`/`DELETE` and as a typed body
      for the others; the output `TypeSchema` from `describe`; `AuthRequirement::MinRole` from
      the query's `min_role`. So a custom query gets a typed client method like everything else.
      (`RestProvider::with_queries`, a `Result` because a name the API already projects would
      otherwise panic the registry in a running server.)
- [x] **Execution** in the REST provider: coerce each argument to its declared type (the row
      layer's own `rows::column_value`), bind, run inside the caller-context transaction —
      `READ ONLY` for `GET` (decision 7) — and return the rows as JSON. A database error is an
      Application error carrying Postgres's message (§16). (The coercion is `json_to_value`
      against the declared `ValueType` — there is no table to hold the value against — and
      `sc-catalog` grew `run_in_context_read_only` for the transaction.)
- [x] Tests (real Postgres): a two-parameter query returns the same rows as the SQL run by hand;
      an argument containing `'; drop table …` is a *value*, and the table is still there; a
      missing required argument is a 400 naming it; a wrongly-typed one is refused before the
      statement runs; a `GET` query that writes is refused by the read-only transaction; a
      caller below `min_role` is refused; a query over an RLS-protected table sees only the
      caller's rows; a query that does not prepare cannot be saved.

## Phase 5 — The admin editor and the CLI

- [x] **The application form's custom-query editor**, one list per REST API row: name, method,
      sub-path, `min_role`, SQL, and the parameter list. It validates through the server
      (`describe`) before saving, and shows the resulting column names and types — which is also
      how the admin learns what their client method will return. The authority note of
      decision 6 is on the screen, not only in the docs. (The check is the new admin endpoint
      `describeCustomQuery`, which runs the model's rules *and* `describe` — the whole refusal a
      save would give, in one round trip, without storing anything. The editor is offered on the
      provider's own say-so: `ApiProviderInfo::supports_custom_queries`, so the form still knows
      nothing about which provider REST is. `apiRowsToRequest` now carries the queries back
      explicitly, because `buildConfig` writes the declared spec and nothing else — editing an
      app's mount was quietly deleting its custom queries.)
- [x] **`saltcorn api add-query`**: `--app <subdomain> --api <mount> --name … --method … --path …
      --min-role … --param name:type[,…] --sql <text|@file>`. It connects the database like
      `build-app` does, validates by preparing, saves the application, and **re-emits the app's
      generated client** — the sentence GOALS.md leaves unfinished ("This must update the …"),
      read as the generated client, since that is what a new endpoint invalidates.
      (`sc_app::emit_app_client` is that re-emit, shared so Phase 6's button and the automatic
      path reuse it; `name:type?` declares an optional parameter, and `--param` may carry
      several.)
- [x] `saltcorn api list-queries` and `saltcorn api remove-query` beside it. An add-only command
      is a trap: the first typo would need a browser to fix, which is the situation the command
      exists to avoid.
- [x] Tests: vitest over the editor's model (form state → request, a validation failure → the
      message, the described columns → the preview); an integration test (real Postgres) that
      adds a query through the CLI and asserts the endpoint set and the emitted `client.ts` both
      gained it; a CLI add with invalid SQL exits non-zero, prints Postgres's message, and
      leaves the stored application untouched. (Plus the flag parsing as Rust unit tests, and
      `describeCustomQuery` over HTTP: described without saving, Postgres's message on a broken
      statement, and admin-only.)

## Phase 6 — The generated client directory, and keeping it current

- [x] **`src/saltcorn/README.md`**, generated: this directory is maintained by the Saltcorn
      server, every file in it is overwritten on each build, edits belong outside it — and how
      to add a custom SQL query with the CLI, with the command spelled out. (Spelled for *this*
      application — its subdomain and its REST mount — so it is pasteable rather than a
      template, and carrying decision 6's authority note beside the command that opens the
      hole.)
- [x] **`src/saltcorn/schema.sql`**, generated: the `CREATE TABLE` definitions of the tables the
      application declares, so a coding agent working in the project can write a custom query
      against something real. Rendered by the **driver** (a new `DatabaseDriver::render_ddl`
      over the existing `SchemaChange` renderer) rather than by a second DDL writer in `sc-app`
      that would drift from the one the database actually gets. (`sc_app::app_schema_sql` is the
      join; the header says it describes rather than migrates, which is the mistake a file of
      `CREATE TABLE` invites.)
- [x] **`AGENTS.md` at the project root**, written by the scaffold only (decision 9): what this
      project is, that `src/saltcorn/` is generated and why, a pointer to its README, and how to
      add a custom SQL query.
- [x] **The button**: `updateApplicationClient` (admin API) and a control on the application
      screen. It re-emits the generated runtime, and when the project directory is **empty** it
      scaffolds instead — reusing the scaffold's own emptiness check — reporting which of the
      two it did rather than saying "done" for both. (`sc_app::update_app_client` returns a
      `ClientUpdate` that names which happened; the two generators now take one
      `ProjectContext` rather than six parameters that grew by one per generated file.)
- [x] **Automatically**, on an API-definition change: `AppMounts::refresh_table` (a column added,
      a table changed) and saving an application already reproject the endpoint set in memory;
      they now also re-emit `src/saltcorn/**` for each affected mounted app whose source is
      reachable. Non-fatal and logged (decision 10). (The observer is synchronous and runs
      inside somebody's schema change, so the re-emit is spawned; `updateApplication` awaits its
      own and logs.)
- [x] Tests: the scaffold writes the three new files and the "only `src/saltcorn/` is
      regenerated" boundary test covers them; an edited root `AGENTS.md` survives a re-emit; the
      button rescaffolds an emptied directory and re-emits a populated one; adding a column
      through the admin API rewrites `client.ts` **and** `schema.sql` with no build and no
      restart; the scaffolded project still type-checks, including a call to a custom query.
      (The column's *typed* surface is `hooks.ts` and `schema.sql` — REST's row payloads are
      opaque JSON, so `client.ts` moves when the endpoint set does, which the custom query saved
      through `updateApplication` asserts in the same file.)

## Phase 7 — Documentation

- [x] **§13.1** gains query parameters in the endpoint model and their generation; **§13.4**
      gains the REST query string (with the taken/not-taken subset of decision 2), custom SQL
      queries and their authority rule, and per-provider configuration; **§13.3** gains the
      generated-directory contract and `AGENTS.md`.
- [x] A tutorial beside the others (`docs/tutorial-rest-queries.md`): select with embeds, filter
      and page a table, then add a custom SQL query from the admin UI and again from the CLI,
      and call both from the typed client. (Plus what it refuses and why by name, the rules a
      caller who is not an admin meets, and the generated directory that keeps up — the
      tutorial cross-link chain now ends here, and two hygiene tests hold the design document
      and the tutorial to what was built.)
- [x] CHANGELOG entries as the phases land, in this repository's voice: what changed and why it
      is that way, not a list of files.

---

## Carried past this milestone

- **One-to-many embeds in `select`** (`select=departments(name,employees(name))`). The read is
  the batched second query the GraphQL loader already does; what is undecided is the REST shape
  for paging and ordering *within* an embed, and inventing one badly is worse than not having it.
- **The rest of PostgREST's grammar**: `!inner`, the spread operator, `::` casts, `or=(…)`,
  full-text operators, `Prefer:` headers (`ApiRequest` still carries no headers — the same
  blocker the GraphQL milestone recorded for content negotiation).
- **Custom routes authored as guest code.** `HandlerRef::GuestCode` stays stubbed: this
  milestone does the SQL half of GOALS' "custom routes", which is the half the new API items
  name. The endpoint projection built here is what the code half will reuse.
- **`Statement::Raw` for a second driver.** Postgres is the only driver; a raw statement is
  dialect-specific by construction, and the rule (the driver renders and binds it) is what
  carries forward.
- **Prepared-statement caching for custom queries.** Each call prepares; the fix is a cache
  keyed by the query's identity, and it wants a measurement first.
- **Regenerating more than `src/saltcorn/`** for `code`-framework apps: they get their
  `client.ts` and nothing else, because everything else in them is the admin's.

## Explicitly OUT of scope for this milestone

- **Filter parity with GraphQL over relations** — no filtering a parent by a child aggregate,
  no `having`. `sc-expr` can express it; the query-string spelling is a design question of its
  own, and it is already carried on the GraphQL side.
- **Custom queries that are not one statement**: no multi-statement bodies, no DDL, no
  `CALL`/`DO`. A migration is not an API endpoint.
- **A second file-download or upload path** through a custom query.
- **gRPC, tRPC and MCP providers** (§13.4), and per-role schemas.
- Everything still listed as out of scope in [docs/TODO-mvp.md](./docs/TODO-mvp.md),
  [docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md),
  [docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md),
  [docs/TODO-post-mvp-3.md](./docs/TODO-post-mvp-3.md),
  [docs/TODO-post-mvp-4.md](./docs/TODO-post-mvp-4.md),
  [docs/TODO-post-mvp-5.md](./docs/TODO-post-mvp-5.md),
  [docs/TODO-post-mvp-6.md](./docs/TODO-post-mvp-6.md) and
  [docs/TODO-post-mvp-7.md](./docs/TODO-post-mvp-7.md)
