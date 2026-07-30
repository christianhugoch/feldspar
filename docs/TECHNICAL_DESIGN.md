# Saltcorn v2 — Technical Design Requirements

This document translates the vision in [GOALS.md](./GOALS.md) into a concrete technical
design: the project (workspace/crate) structure, the major data structures, and the
software architecture. It is a living document and is expected to change as the design is
validated against the MVP.

It is normative where it uses **MUST**/**SHOULD**/**MAY** (RFC 2119); everything else is
guidance and rationale. Rust type sketches are illustrative, not final signatures — they
fix the *shape* of the data, not the exact fields or method names.

---

## 1. Design principles

These are the [GOALS.md](./GOALS.md) code guidelines, restated as the rules this design is
held to:

1. **Simple and clean.** Prefer boring, obvious designs. Clean types over clever ones.
2. **Every line of code is a liability.** Minimise total LOC. Do not add an abstraction,
   config option, or code path without a concrete need.
3. **Abstractions, but not baroque ones.** Extension points are traits with the smallest
   surface that does the job.
4. **Integration-tested first.** Everything is covered by integration tests against a real
   database; unit tests where cheap. Never distort the design purely to increase testability.
5. **No silent failures.** Errors either are handled meaningfully or crash with a clear
   message. No swallowing.
6. **Monorepo.** All first-party code lives in one repo; plugins may live outside.
7. **Separation of concerns.** Generic functionality is factored into independent crates
   with minimal, acyclic dependencies.

Two design invariants that fall out of the goals and pervade everything below:

- **Everything is relational.** Tables, rows, and the universal query language are the
  spine. Workflows, actions, agents, files, and models are all ultimately operations over,
  or triggered by, rows.
- **The database is the source of truth.** All metadata and users live in the primary
  database (in `_sc_*` tables and `users`). The in-memory catalog is a *cache* of that
  truth, kept coherent across processes by the message bus. There is no separate on-disk
  app format; a backup is a database dump plus the file stores.

---

## 2. Workspace and crate structure

Saltcorn v2 is a single Cargo workspace. Crates are layered strictly: a crate may only
depend on crates above it in this list (lower-numbered). This keeps the dependency graph
acyclic and the layering enforceable by `cargo`.

```
saltcorn/
├─ Cargo.toml                     # workspace
├─ crates/
│  ├─ sc-error/                   # 0. error type, Result alias, no-silent-failure helpers
│  ├─ sc-query/                   # 1. universal query language (enum AST) + SQL rendering trait
│  ├─ sc-bus/                     # 1. message bus trait + drivers (pg NOTIFY, in-proc, redis…)
│  ├─ sc-db/                      # 2. DatabaseDriver trait, connection, migrations, tx
│  │   ├─ sc-db-postgres/         #    Postgres driver (MVP)
│  │   └─ sc-db-sqlite/           #    SQLite driver (later; embedded/mobile)
│  ├─ sc-types/                   # 3. type system: RichType, BasicType, attributes, validation
│  ├─ sc-expr/                    # 3. ownership-formula language: parse/analyse/validate,
│  │                              #    symbolic (→ sc-query::Expr) + reified (deno_core) eval
│  ├─ sc-catalog/                 # 4. Catalog, Table, Field, TableProvider trait, cache
│  ├─ sc-auth/                    # 5. User, Role, authz (ACL/RLS), sessions, OAuth2 provider
│  ├─ sc-code/                    # 5. code adapters (JS via a JS engine, Python via CPython)
│  ├─ sc-files/                   # 5. FileStore trait, drivers (local, S3, git), xattr metadata
│  ├─ sc-action/                  # 6. Action trait + registry, Event/Trigger model, `_sc_triggers`
│  │                              #    storage & validation, the live set, dispatch, scheduler
│  ├─ sc-workflow/               # 7. durable workflow engine (steps, runs, traces, recovery)
│  ├─ sc-agent/                   # 7. Agent action + Skill trait + inference loop
│  ├─ sc-model/                   # 7. ModelProvider trait, model instances, inference
│  ├─ sc-fieldview/               # 6. FieldView trait, built-in fieldviews (React components)
│  ├─ sc-viewpattern/             # 8. ViewPattern trait (v1-style views: Show/List/Edit/Filter…)
│  ├─ sc-api/                     # 8. Endpoint model (typed Rust values) + API providers
│  │                              #    (REST/GraphQL/gRPC/tRPC/MCP) + TypeScript consumer gen
│  ├─ sc-app/                     # 8. Application, Framework provider trait, routing/subdomains
│  ├─ sc-core-actions/            # 8. the built-in action set (insert_row, update_rows,
│  │                              #    delete_rows, fetch, run_js_code) — above the row layer,
│  │                              #    because a trigger's write goes *through* it (§10.1)
│  ├─ sc-copilot/                 # 9. copilot agent + AppConstructor stages
│  ├─ sc-server/                  # 9. HTTP server: admin routes, user routes, auth, CSP, sockets
│  └─ sc-cli/                     # 10. `saltcorn` binary (serve, user/app mgmt, backup/restore)
├─ ui/
│  ├─ admin/                      # React + TypeScript + react-bootstrap admin SPA over the
│  │                              #    generated typed API client (table editor, file mgr)
│  ├─ builder/                    # React + Craft.js + react-flow drag-and-drop builder
│  └─ form-runtime/               # React dynamic-form framework (conditional/repeated/dynamic)
├─ plugins/                       # first-party plugins (may be JS or Rust)
└─ tests/                         # cross-crate integration tests (real Postgres)
```

Notes:

- **`sc-db` drivers MUST be Rust** (per GOALS). Every other extension point (table
  providers, types, fieldviews, actions, agents/skills, importers/exporters, model
  providers, view patterns, frameworks, API providers) MAY be implemented in Rust or in a
  guest language via `sc-code`.
- The React apps under `ui/` (including the admin SPA) are built to static bundles and
  **served by `sc-server`**; there is no separate front-end server. A strict CSP is applied,
  and it is satisfied *structurally* by the React build (no inline scripts or handlers) rather
  than by a server-side symbolic-HTML model (see §12).
- `sc-error` sits below everything and defines the single `Error`/`Result` convention so
  principle 5 is mechanically enforced (no `unwrap()` in library code; errors carry
  context).
- **`sc-expr`** is a generic library that depends only on `sc-query` (its translation target)
  and `sc-error` — deliberately *not* on `sc-catalog`, which describes tables to it through a
  small `SchemaShape` view. It owns the ownership-formula and calculated-field language
  (§7.3): one JavaScript expression, parsed once, evaluated two ways — **symbolically** into
  `sc-query::Expr` and **reified** in a V8 (`deno_core`) isolate. The V8 dependency is behind
  an `eval` cargo feature, so `sc-catalog` and everything else below the server link only the
  parse/validate/translate half; the `JsEvaluator` trait (the reified seam) is constructed
  once at server boot.

### 2.1 Extension points (traits) at a glance

The "code entities" from GOALS become a small set of object-safe traits. A **plugin** is a
bundle that registers zero or more implementations of these into the catalog at startup.

| Trait | Crate | Language | Purpose |
|---|---|---|---|
| `DatabaseDriver` | `sc-db` | Rust only | Connect to one database; run queries; manage schema |
| `TableProvider` | `sc-catalog` | any | Present a data source as a virtual table |
| `RichType` | `sc-types` | any | A type known to Saltcorn (attributes + validation) |
| `FieldView` | `sc-fieldview` | any | Display/edit a value of one or more types (React component) |
| `Action` | `sc-action` | any | One elementary step; configurable; reads one event, returns a value |
| `Skill` | `sc-agent` | any | An elementary agent capability (usually an LLM tool) |
| `Importer` / `Exporter` | `sc-catalog` | any | Move table data to/from a format |
| `ModelProvider` | `sc-model` | any | Fit/inspect/apply a predictive model over table data |
| `ViewPattern` | `sc-viewpattern` | any | A v1-style view template over a table |
| `FileStore` | `sc-files` | any | A named directory/object store |
| `ApiProvider` | `sc-api` | any | Expose tables, actions & custom routes over a protocol; emit a typed TS client |
| `Framework` | `sc-app` | any | Own an application's primary UI (React/Next/Svelte/v1); declares its settings for the admin UI |
| `BusDriver` | `sc-bus` | Rust | Publish/subscribe transport for the message bus |
| `CodeAdapter` | `sc-code` | Rust | Host a guest-language interpreter exposing the catalog |

Object-safety and dynamic dispatch (`Box<dyn Trait>`) are the default, because
implementations are chosen at runtime from config and may be provided by guest languages
through a single Rust shim per adapter.

---

## 3. Layered architecture

```
                         ┌───────────────────────────────────────────┐
   HTTP / WS / gRPC ───► │ sc-server  (admin URL + per-app subdomains)│
                         │  · strict CSP · sessions · auth middleware │
                         └───────┬───────────────────────┬───────────┘
                                 │                        │
                    ┌────────────▼─────────┐   ┌──────────▼───────────┐
                    │ Admin UI (React)     │   │ Applications (sc-app)│
                    │  table editor, file  │   │  Framework + N ApiPro│
                    │  mgr, builder        │   │  viders per app      │
                    └────────────┬─────────┘   └──────────┬───────────┘
                                 │                        │
          ┌──────────────────────▼────────────────────────▼──────────────────────┐
          │                       Core services                                    │
          │  sc-workflow · sc-agent · sc-action · sc-model · sc-viewpattern        │
          │  sc-fieldview · sc-copilot · sc-files                                  │
          └──────────────────────┬────────────────────────────────────────────────┘
                                 │
                    ┌────────────▼─────────────┐        ┌──────────────────────┐
                    │  sc-catalog (the Catalog)│◄──────►│ sc-bus (message bus)  │
                    │  Tables, Fields, cache   │  cache │  pg NOTIFY / redis /  │
                    │  + TableProviders        │  inval │  kafka / in-proc      │
                    └────────────┬─────────────┘        └──────────────────────┘
                                 │
                    ┌────────────▼─────────────┐   ┌──────────────────────────┐
                    │ sc-db DatabaseDriver(s)  │   │ sc-code CodeAdapters      │
                    │ Postgres (primary) + …   │   │ JS / Python interpreters  │
                    └────────────┬─────────────┘   └──────────────────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │   sc-query (enum AST)     │
                    └──────────────────────────┘
```

The **Catalog** is the hub. It owns the connected database drivers, the in-memory cache of
tables/fields/config/triggers/etc., and is the object every higher layer is handed to do
its work. It subscribes to the bus for cache-invalidation events and republishes its own
mutations.

---

## 4. The universal query language (`sc-query`)

Per GOALS: a lower-level query representation that is **data, not fluent calls** — an array
of enum values (inspired by SeaQuery but reified). Each `DatabaseDriver` renders it to its
own SQL dialect; `TableProvider`s interpret it directly.

```rust
/// A statement is the top-level query AST. It is a plain data value: serializable,
/// inspectable, and buildable by any language through the code adapters.
pub enum Statement {
    Select(Select),
    Insert(Insert),
    Update(Update),
    Delete(Delete),
}

pub struct Select {
    pub from:    Source,             // table, subquery, or provided table
    pub columns: Vec<Projection>,    // expr AS alias; supports joins' columns
    pub joins:   Vec<Join>,          // inner/left/… ON Expr
    pub filter:  Option<Expr>,       // WHERE
    pub group:   Vec<Expr>,
    pub having:  Option<Expr>,
    pub order:   Vec<OrderBy>,
    pub limit:   Option<u64>,
    pub offset:  Option<u64>,
}

pub enum Expr {
    Col(ColRef),                     // table-qualified column
    Lit(Value),                      // a literal, always parameterised on render
    Param(usize),                    // bind parameter
    Binary { op: BinOp, l: Box<Expr>, r: Box<Expr> },
    Unary  { op: UnOp,  e: Box<Expr> },
    Func   { name: String, args: Vec<Expr> },
    In     { e: Box<Expr>, set: InSet },
    Json   { target: Box<Expr>, path: Vec<JsonStep> },   // JSON is first-class
    Case   { .. },
    // …minimal, extended only as real queries demand it
}

pub enum Value {                     // the row value type used everywhere
    Null, Bool(bool), Int(i64), Float(f64), Text(String),
    Bytes(Vec<u8>), Json(serde_json::Value), Uuid(Uuid),
    Date(..), Time(..), Timestamp(..), Decimal(..),
}
```

Requirements the AST **MUST** support because the goals demand them:

- **Composite primary keys** and **foreign keys to non-primary-key columns** — so `ColRef`
  and join conditions are general `Expr`s, never "the id column".
- **JSON as a built-in** — `Expr::Json` and `Value::Json`, not an add-on type.
- **Literals are always parameterised** on render (SQL-injection-safe by construction; this
  is the query-layer half of the XSS/injection story, the React UI layer is the other half).

Rendering is a trait so each dialect controls quoting, placeholders, JSON operators, upsert
syntax, etc.:

```rust
pub trait SqlDialect {
    fn render(&self, stmt: &Statement) -> Result<(String, Vec<Value>)>; // sql + binds
}
```

---

## 5. Database layer (`sc-db`)

```rust
/// Instantiated once per connected database. MUST be implemented in Rust.
#[async_trait]
pub trait DatabaseDriver: Send + Sync {
    /// Introspect the live schema via information_schema (or equivalent).
    async fn introspect(&self) -> Result<Vec<PhysicalTable>>;

    /// Run a query; literals are already parameterised by sc-query rendering.
    async fn query(&self, stmt: &Statement) -> Result<RowStream>;

    /// Schema management (create/alter/drop table, add/drop column, index…).
    async fn apply_schema(&self, change: &SchemaChange) -> Result<()>;

    /// Transactions: each workflow step and each metadata mutation runs in one.
    async fn begin(&self) -> Result<Box<dyn Transaction>>;

    /// Row-level security support advertisement (drives authz strategy, §7).
    fn capabilities(&self) -> DbCapabilities;

    /// Translate a Postgres-dialect migration to this driver's dialect.
    fn dialect(&self) -> &dyn SqlDialect;
}

pub struct DbCapabilities {
    pub row_level_security: bool,
    pub composite_pk:       bool,
    pub listen_notify:      bool,   // enables the pg-notify bus driver
    pub returning:          bool,
    // …
}
```

Design rules from GOALS:

- **No table discovery step.** As soon as a database is connected, *all* its tables are
  usable via `introspect()`. Metadata is an optional overlay, never a prerequisite (§9).
- **No automatic primary key on create.** `apply_schema` for "create table" does **not**
  invent an `id` column. The user creates key fields explicitly like any other field.
- **Migrations are arrays of Postgres SQL values** translated per-dialect by `dialect()`.
  Per GOALS, we **do not** run schema-changing migrations during early development — we
  evolve the initial setup instead until the metadata schema is stable.

The **primary database** is one connected driver, distinguished by the fact that it hosts
the `_sc_*` metadata tables and the `users` table. Additional databases are connected for
data only. (MVP: single database, same as the primary store.)

---

## 6. Type system (`sc-types`), fields, and fieldviews

### 6.1 Types

```rust
/// A Rich type is one Saltcorn understands: it has typed attributes and validation and a
/// set of fieldviews. A Basic type is any DB type not mapped to a rich type — usable, but
/// only through catch-all fieldviews. The driver maps DB types → rich/basic types.
pub trait RichType: Send + Sync {
    fn name(&self) -> &str;
    fn attributes(&self) -> &[FormField];             // e.g. min/max, select options
    fn validate(&self, v: &Value, attrs: &Attrs) -> Result<()>;
    fn sql_types(&self) -> &[&str];                   // DB types this maps from/to
    fn fieldviews(&self) -> Vec<Box<dyn FieldView>>;  // deferred until §6.3 ships
}
```

MVP shipped **no rich types** — everything was basic — per the milestone. The tables-and-fields
milestone then implemented the trait (minus `fieldviews()`, which waits on §6.3's fieldview
registry) and, with it, the **rich-type registry** (`sc-types::rich`): rich types are registered
by name — `registered_rich_types()`, `rich_type(name)`, `rich_type_config_spec(name)` — mirroring
the file-store-backend and framework registries, and for the same reason: the admin UI must
render an attribute form for a type it knows nothing about, including one a plugin registers
later, so a type is a name plus a `Vec<FormField>` spec plus a validator, resolved at runtime.

`TypeRef` gained a `Rich(RichTypeRef)` variant. A `RichTypeRef` is a **name** resolved against
the registry on use (`RichTypeRef::resolve`); identity is the name, which keeps `TypeRef`
comparable by value. `TypeRef::validate_with(value, attrs)` is the attribute-carrying entry point
the row-write path calls (§2.3 of the milestone): coerce the JSON through the column's storage
type, then let the rich type enforce its configured attributes.

Two rich types ship, chosen to prove the three things the machinery must do — validate a value,
declare typed attributes, and constrain what may be stored:

- **`String`** over `text`, with `max_length`, optional `options` (a select), and an optional
  anchored `regex`. The regex subsumes the originally planned `Email` type (and any other
  pattern), so no dedicated `Email` type exists.
- **`Integer`** over `int8`, with `min`/`max`.

Two boundaries hold the shape steady. **`File` is a field kind, not a rich type**: like `Key`, it
is a *reference* (a store-relative path stored as `text`), not a value family — the admin-facing
type picker merges kinds and types into one list because that is how an admin thinks, but the
model keeps them apart. And **introspection never resolves a column back to a rich type**:
`TypeRef::from_sql_type` always yields a basic type, and a column is rich only because the
`_sc_fields` overlay says so (§9) — guessing "this `text` column is an Email" from the database
is exactly the magic that makes a legacy database behave surprisingly.

### 6.2 Fields — the `BaseField` / `DataField` / `FormField` split

GOALS calls out that v1 conflated DB fields and form fields. v2 separates them explicitly:

```rust
/// Properties shared by every field.
pub struct BaseField {
    pub name:  String,      // valid identifier in SQL and every guest language
    pub label: String,      // human string
    pub type_: TypeRef,     // rich or basic
    pub attributes: Attrs,  // JSON object, type-specific
}

/// A column in a database table.
pub struct DataField {
    pub base:      BaseField,
    pub required:  bool,
    pub unique:    bool,
    pub primary_key: bool,             // may be part of a COMPOSITE pk
    pub calculated: Option<Calc>,      // stored or non-stored calculated field
    pub kind:      DataFieldKind,
}

pub enum DataFieldKind {
    Plain,
    /// Foreign key: holds the value of a referenced field (NOT necessarily the target PK),
    /// with a summary field used as the default label when selecting.
    Key { target_table: TableId, target_field: FieldId, summary_field: Option<FieldId> },
    /// File reference: a relative path within a named file store, optionally restricted to
    /// a folder and/or file types.
    File { store: FileStoreId, folder: Option<String>, mime_allow: Vec<String> },
}

/// A field in a form (may derive from a DataField or be standalone).
pub struct FormField {
    pub base:     BaseField,
    pub fieldview: FieldViewRef,
    pub required: bool,
    pub default:  Option<Json>,        // value used when none is given
    pub visibility: Option<Formula>,   // conditional display on other field values
    pub options_source: OptionsSource, // static | server query | client code
    // …repeat groups & dynamic attributes handled by the form runtime (§12)
}

impl DataField { pub fn to_form_field(&self) -> FormField { /* … */ } }
impl FormField { pub fn to_data_field(&self) -> Option<DataField> { /* … */ } }
```

**`FormField` is also how every configurable extension point declares its settings**, and
there is deliberately **no separate `AttrSpec` type**. A `Framework` (§13.3), `Action`
(§10.1), `Agent` (§11.1), `ModelProvider` (§14.2), `FieldView` (§6.3) and a `RichType`'s
attributes (§6.1) all answer one question — *what should the admin be asked?* — and all answer
it with `Vec<FormField>`. The definition above already allows it: a form field "may derive from
a `DataField` **or be standalone**", and a setting is exactly the standalone case. Giving
settings a parallel type would mean two vocabularies for one question, two things for the admin
UI to render, and two things a guest-language extension must know about; `name`/`label`/`type_`
would be declared twice and drift once. The values a `FormField` describes as settings live in
an `Attrs` bag, so its `default` and `options_source` deal in JSON — the same thing that ends up
in the bag.

Note the level this puts things at: `BaseField.attributes` is an `Attrs`, and what may go in it
is described by that type's `attributes()` — a `Vec<FormField>`. A `FormField` therefore both
carries a `BaseField` and describes what another field's attributes may hold. That is the same
self-description a JSON Schema has, and it is well-founded: the recursion bottoms out at basic
types, which have no attributes.

`BaseField`, `FormField` and `Attrs` live in `sc-types`. `DataField` lives one layer up in
`sc-catalog`, because its `Key`/`File` kinds reference catalog identifiers and it bridges to
`Column`/`ColumnDef` — that is the only part of the split that ever needed layer 4.

**Calculated fields.** Defined either by a simple expression — which may traverse foreign
keys in both directions: outgoing via Ⱶ-joinfields, incoming via the Ↄ aggregation chains
of §7.3 (decided in [AGG_EXPRS.md](./AGG_EXPRS.md), proposal G; calculated-field
expressions use the same aggregation language without `user` and the operation flags) —
or by guest code via a code adapter. Dependency handling per GOALS:

- If **no** calculated field uses a code adapter, calculation is implemented as ordinary
  triggers with a recursion limit.
- If simple expressions are mixed with code-adapter functions, dependencies (both "this
  field depends on…" and "…depends on this field") are collected and **topologically
  sorted**; a cycle is a load-time error (no silent failure).

```rust
pub struct Calc { pub stored: bool, pub source: CalcSource }
pub enum CalcSource { Expr(Formula), Code { adapter: AdapterId, body: String } }
```

### 6.3 Fieldviews

A fieldview displays and optionally edits a value of one or more types. With the admin UI and
applications rendered by React (§12), a fieldview is fundamentally a **React (TypeScript)
component**, described on the Rust side by a small metadata record so the catalog can list,
select, and configure it:

```rust
/// Rust-side descriptor for a fieldview; the actual render/edit UI is a React component
/// referenced by `component` and bundled with the relevant UI (admin SPA, v1-view runtime).
pub struct FieldView {
    pub name: String,
    pub handles: Vec<String>,    // type names this covers, or "*" catch-all
    pub is_edit: bool,
    pub config_spec: Vec<FormField>,
    pub component: ComponentRef, // bundled TS component id
}
```

The earlier symbolic-HTML `Node` model — a `sc-markup` crate producing CSP-safe server HTML
with extracted client JS — has been **dropped**: React satisfies the strict CSP structurally
(no inline handlers) and supplies the interactive components directly. Fieldviews remain
post-MVP; the MVP uses only the catch-all display/parse path over `Value`.

---

## 7. Users, authentication, authorization (`sc-auth`)

### 7.1 Users

Per GOALS, the `users` table lives in the primary database and is deliberately minimal:

- Primary key is **UUID** and is **not deletable**. Code MUST NOT assume any other field
  exists.
- An `email` field exists initially but the admin MAY delete it and substitute another
  identifier field.
- Passwords are stored hashed with a modern KDF (argon2id).
- A `legacy_id` field MAY be added when importing v1 apps (v1 user ids were autoincrement
  integers).
- `role` is an integer **1–100**; 1 = admin (full access), 100 = public (not logged in).
  Admins MAY add arbitrary fields to the user table. `role` is a **foreign key onto
  `_sc_roles`** (§7.4): a role is a row carrying a name and role-specific settings, so
  `users.role` naming a role that does not exist is a state the database rules out.

```rust
pub struct User {
    pub id: Uuid,                       // never deletable
    pub role: u8,                       // 1..=100
    pub extra: BTreeMap<String, Value>, // admin-defined fields (may include email)
}
```

### 7.2 Authentication

- Password + session cookie baseline.
- **Device recognition** ("Google-level"): remember known devices, email the user on a
  new-device login.
- **OAuth2 server option**: Saltcorn can act as an identity provider (`sc-auth` exposes the
  authorization-code/PKCE endpoints when enabled).
- Additional providers (OAuth clients, SSO) via an `AuthMethod` extension.

### 7.3 Authorization

Separate permissions for **read, create, update, delete** (v1 had a coarser model). Per
table (and per File-field endpoint) a minimum role governs each operation; **ownership**
grants row-level access to users who do *not* meet the table-wide minimum role. Ownership is
expressed as an **ownership formula** — a JavaScript expression, stored in the table's
`attributes`, over the row's fields, the current `user`, the operation flags
(`_read`/`_insert`/`_update`/`_delete`/`_write`), Ⱶ-joinfields (below) and Ↄ-aggregations
(below). This is implemented; the paragraphs that follow describe the system as built.

**The access rule.** For an operation on a row:

> **allowed = the caller's role meets the operation's `min_role` OR the ownership formula
> evaluates true** for this row / user / operation.

Ownership *extends* access below the role floor; it never narrows it. A caller who already
meets `min_role` is unaffected by the formula (and by RLS, through a role-floor clause in
every policy). A table with no formula behaves exactly as the plain role model does.

**One language, parsed once, evaluated two ways.** The formula lives in the `sc-expr` crate
(§2). A single parse (via `swc_ecma_parser`, the parser family Deno uses, so the grammar is
exactly V8's) is lowered into `sc-expr`'s own owned AST and evaluated two ways from that one
object:

- **Symbolically** — translated to `sc-query::Expr`, i.e. a SQL predicate. This is what an
  injected `WHERE` clause (runtime checks) and an RLS policy both are.
- **Reified** — actually run, in a V8 (`deno_core`) isolate on a dedicated thread, for the
  constructs SQL cannot hold (`user.groups.some(g => …)`), and as the *reference*
  implementation.

**Parity is a tested property, not a hope.** For every translatable construct, matrices of
formula × row × user are evaluated *both* ways — reified in the isolate, and via the
translated `Expr` executed against real Postgres — and each case asserts three things:
the two evaluators agree *and* both match the expected verdict (so both drifting wrong
together still fails the test). Null handling is specified once, by the translation, and the
reified path is normalised to meet it: `===`/`==` render as `IS NOT DISTINCT FROM` (JS's
two-valued equality — `owner === user.id` on a null `owner` is *false*), ordered comparisons
and arithmetic are null-guarded so JS's `null → 0` coercion cannot diverge from SQL, and
`user.x` on an anonymous (null) user reads as null rather than throwing.

**Fail closed, everywhere.** A stored formula that no longer validates at load time (a field
was dropped, a dump restored) grants nothing and the table stays `min_role`-only, with the
reason reported on the `Table`. An anonymous caller's `user` is null, so a formula that must
not grant anonymously is written `user && …` (bare `user` is object-or-null, so its
truthiness is exactly the logged-in test). Under RLS a missing GUC is SQL NULL, so an
un-set caller context sees no rows by construction. A denial is always shaped identically to
absence — affected-rows 0, mapped to not-found — so "exists but forbidden" is never a probe.

**Enforcement strategy is chosen from `DbCapabilities`**, and the *same formula* produces the
*same verdicts* either way — flipping between them swaps the mechanism, never the outcome
(the RLS tests are the runtime-check scenarios re-run):

- **Runtime checks** (any backend). For reads below the role floor, the translated predicate
  is ANDed into the `SELECT`'s `WHERE`; joinfields ride in as correlated columns projected in
  the same query (zero extra round trips) and are stripped before rows reach the wire. Writes
  inject the predicate into the `UPDATE`/`DELETE` `WHERE` and additionally run a reified check
  on the existing row and, for updates/inserts, on the merged proposed row (WITH CHECK
  semantics — moving a row out of your own ownership is refused). An **untranslatable** formula
  falls back to fetch-then-filter through the reified evaluator.
- **Postgres row-level security** (`DbCapabilities::row_level_security`; opt-in per table via
  `rls_enabled`). `sc-catalog` emits `ENABLE` + **`FORCE ROW LEVEL SECURITY`** and four
  policies — SELECT/DELETE `USING`, INSERT `WITH CHECK`, UPDATE both — from the *same*
  translation under a **GUC** user-env: `user.x` becomes a read of `current_setting('sc.user',
  true)` (a JSON GUC) and the role floor a read of `sc.role`. Every row operation runs inside a
  transaction that `SET LOCAL`s those two GUCs (the value is bound via `set_config`, never
  interpolated); admin endpoints run at `sc.role = 1` so the row viewer works on a FORCE'd
  table. Reads go unfiltered and writes unpredicated to the database — the policy does the
  work, including the joinfield subselects, so nothing is refetched. Enabling RLS is refused at
  save time if the formula does not translate under the GUC env for all four operations, so a
  policy is never emitted for a formula the database cannot honour. `USER_GUC = "sc.user"` and
  the role GUC are wrapped in `NULLIF(…, '')` so an unset-or-empty custom GUC folds to NULL and
  the policy fails closed.
  **Operational requirement:** Saltcorn must **not** connect to Postgres as a superuser (or as a
  role with `BYPASSRLS`). Such a role bypasses row security entirely — `FORCE ROW LEVEL SECURITY`
  covers the table's *owner*, not a superuser — so every policy this emits would be inert while
  looking correct. The database role needs only `LOGIN` and ownership of its own objects; CI
  creates one deliberately rather than using the image's bootstrap superuser, and a deployment
  that gets this wrong has no enforcement to fall back on but the runtime checks above.

**The Ⱶ operator is an identifier character, not an operator.** U+2C75 (Latin capital letter
half H, category Lu) is a valid JavaScript identifier character, so `publisherⱵname` is a
*single* identifier that V8 and swc both accept unchanged — no preprocessing, no syntax
extension. The reified path binds a variable literally named `publisherⱵname`; the symbolic
path splits on Ⱶ into a **join path** rendered as correlated scalar subselects
(`(SELECT _sc_j1.name FROM publishers _sc_j1 WHERE _sc_j1.id = books.publisher)`), nested per
link to any depth, resolving link-by-link through `Key` fields. A null FK yields no row yields
SQL NULL, granting nothing — optional-chaining semantics for free.

**Non-stored calculated fields** (§6.2) reuse this whole machinery over the same scope
**minus `user` and the operation flags**: an expression computed on read, dependency-ordered
so one calc field may read another. A calc-field reference inside an ownership formula is
**inlined** as its defining expression, transitively, before translation — so a calc field is
usable in ownership formulae and in RLS policies alike (an untranslatable inlined definition
refuses RLS, naming the construct). Because a calc field can hold no `user`/flags, inlining can
never smuggle them into a policy. *Stored* calculated fields, and the tamper-safe inlining of a
stored field's *value* into a policy, are deferred to their own milestone.

Beyond roles, GOALS asks for **access-control lists / an ACL language** for views, pages and
actions; that layer above the table/File-field ownership implemented here is future work.

**Formula aggregations — the Claudian antisigma (`Ↄ`).** Formulas aggregate over
*incoming* keys; the design is decided and recorded in [AGG_EXPRS.md](./AGG_EXPRS.md)
(proposal G), and it is available both in ownership formulae and in calculated-field
expressions (§6.2 — the same language minus `user` and the operation flags). A **relation
identifier** spelled with the Claudian antisigma — `order_linesↃorder`, child table Ↄ key
field; U+2183 is category Lu and therefore a valid JavaScript identifier character exactly
like Ⱶ, and like Ⱶ it is refused in table and field names — denotes the array of child
rows whose key points at the current row. Aggregation is a **curated method chain** on it:

```js
order_linesↃorder.filter(r => r.status === "shipped").sum("qty")
sharesↃdocument.some(s => s.shared_with === user.id && (_read || s.can_write))
readingsↃsensor.maxBy("ts").temp
```

Kept native: `filter`, `map`, `some`, `every`, `length`, `includes`, `join`. Invented:
`sum`/`min`/`max`/`avg`/`distinct`, each taking an optional **selector** (a constant
field-name string or an arrow over the child row), and the ordered pair `maxBy`/`minBy`
(selector required; rows with a null key ignored; ties broken by the child's primary key;
member access on the result is optional-chaining by definition). `reduce` and everything
ambient-ordered, positional or effectful is refused by name, with the error naming the
alternative. Null/empty semantics are AGG_EXPRS.md's table, which is the **parity
contract**: symbolically a chain is one correlated subquery (`some`/`every` are
`EXISTS`/`NOT EXISTS` — under RLS, aggregation-based ownership costs no refetch);
reified, a prelude defines the invented methods on the isolate's `Array.prototype` and the
host binds prefetched child rows, batched per relation, never per row. Analysis yields
`AggUse` records (child table, key field, fields read) — the prefetch plan today, and the
recomputation-trigger dependencies for stored calculated fields later. Enabling RLS on a
formula whose aggregation reaches a table whose own policy reaches back is refused with
the cycle named — Postgres would otherwise raise `infinite recursion detected in policy`
at query time.

### 7.4 Roles (`_sc_roles`)

A role is a **row in `_sc_roles`**, not a bare integer. It carries the role number on the fixed
1–100 scale (lower = more privileged), a name shown wherever a role is chosen or displayed, and
`attributes` for role-specific settings (§9's sparse-value rule, so the first such setting needs
no schema change). `users.role` is a foreign key onto it, and so, in intent, is every
`min_role` the access model uses.

`_sc_roles` is **not an overlay** (§9): a role does not exist without its row, exactly as an
application or a file store does not, so the table holds the authoritative list rather than
adding to introspection. Bootstrap seeds exactly the two roles the system itself depends on —
**admin (1)** and **public (100)** — and no invented middle role, because a seeded role nobody
uses is one every admin has to read and decide to delete. Those two are **built in**: neither is
deletable (without admin nobody can administer anything; without public an anonymous request has
no role to be), and a role any user still holds cannot be deleted either — nothing cascades a
user to a different role.

---

## 8. The Catalog and caching (`sc-catalog` + `sc-bus`)

### 8.1 Catalog

```rust
pub struct Catalog {
    primary: Arc<dyn DatabaseDriver>,          // hosts _sc_* and users
    databases: HashMap<DbId, Arc<dyn DatabaseDriver>>,
    cache: RwLock<CatalogCache>,               // tables, fields, config, triggers, apps…
    bus: Arc<dyn BusDriver>,
    code: CodeAdapters,
}

struct CatalogCache {
    tables:   HashMap<TableId, Table>,
    triggers: HashMap<TriggerId, Trigger>,
    config:   ConfigStore,
    apps:     HashMap<AppId, Application>,
    models:   HashMap<ModelId, Model>,
    tags:     HashMap<TagId, Tag>,
    // users, workflow runs and files are NOT cached (see below)
}
```

Per GOALS: **all entities except users, workflow runs, and files are cached in memory** for
performance. When a transaction mutates a cached entity, it publishes a cache-invalidation
message on the bus; every process (including the mutating one, after commit) reloads the
changed entity. Users, runs, and files are read through on each access because they are
high-cardinality and/or change constantly.

### 8.2 Table

```rust
pub struct Table {
    pub id: TableId,
    pub name: String,
    pub database: DbId,
    pub provider: TableProviderRef,       // a DatabaseDriver-backed table, or a virtual one
    pub fields: Vec<DataField>,           // composite PK allowed; may have zero explicit PK
    pub access: AccessRules,              // per-CRUD min role + ownership
    pub attributes: Attrs,
}
```

### 8.3 Table providers

```rust
/// Presents a data source as a table. Interprets the universal query language and returns
/// matching rows. May optionally be materialised into a real table with sync options.
#[async_trait]
pub trait TableProvider: Send + Sync {
    fn fields(&self) -> Vec<DataField>;
    async fn query(&self, select: &Select) -> Result<RowStream>;
    async fn write(&self, change: &RowChange) -> Result<WriteOutcome>; // if writable
    fn materialisation(&self) -> Materialisation;   // None | Snapshot | Synced { … }
}
```

A `DatabaseDriver`-backed table is just the trivial provider. RSS/IMAP/search/etc. are
non-trivial providers. Any provider MAY be materialised into a real table with a sync
policy.

---

## 9. Metadata storage (`_sc_*` tables)

All metadata lives in the primary database. **Any table named `_sc_*` is a system table:
hidden from users.** Every system metadata table MUST have: `name`, `id` (UUID),
`description`, `attributes` (JSON, always an object), plus any other fields. The design rule
(a genuine value judgement per GOALS): **a value present for many rows gets its own column;
a sparse value goes into `attributes`.**

| Table | Holds | Notes |
|---|---|---|
| `_sc_tables` | overlay metadata for tables | access rules, label/description, attributes, provided-table defs; the DB's own tables need no row to be usable (§9.1) |
| `_sc_fields` | overlay metadata for fields | rich type name, field kind (`Key`/`File`) + parameters, label/description, attributes; later calculated-field defs and fieldview defaults (§9.1) |
| `_sc_triggers` | triggers (later: workflows, agents) | **not an overlay** — the row is the trigger's only definition (§10.2): event, channel, `only_if`, action + configuration, `min_role`, and in `attributes` the sparse `enabled` flag and periodic timing. `last_run_at` is the scheduler's own column, never written by a save. Workflows will be **versioned** so a suspended run finishes on its own version |
| `_sc_runs` | workflow & agent runs | current context + state, updated after each step |
| `_sc_run_traces` | per-step context + timing | only when tracing is enabled for that workflow |
| `_sc_errors` | error log | one row per logged error; `kind` = Application \| System (§16); message, source chain, and context (app/route/table/run/step/role); a runtime stream, **not cached** |
| `_sc_config` | configuration | scoped to whole setup or one application; per-key value-type restriction; values stored as JSON |
| `_sc_applications` | applications | framework + its config, subdomain, table/store subset, API config, static dirs, CSP; **not an overlay** — the row is the app's only definition (§13.2), so this table is needed as soon as apps are (MVP) |
| `_sc_models` | model definitions | provider + config fields |
| `_sc_model_instances` | fitted model instances | parameters, hyperparameters, fit metadata |
| `_sc_roles` | roles | **not an overlay** — a role is a row carrying a name and role-specific settings; `users.role` is a foreign key onto it (§7.4). Two built-ins (admin, public) seeded at bootstrap |
| `users` | users | UUID PK (not `_sc_`-prefixed; it is user-facing and extensible) |

**Files have no per-file database row.** Per-file metadata is stored in **xattrs** on disk;
a cross-platform xattr crate is required (Linux/macOS/Windows/FreeBSD). File stores that are
git repositories are recognised as such.

The **overlay** principle for `_sc_tables`/`_sc_fields` is the key to "legacy databases just
work": introspection yields the tables and fields; the overlay only *adds* access rules and
attributes where present. A newly connected database needs zero metadata rows.

The overlay principle does **not** extend to every `_sc_*` table, and the distinction decides
what the MVP can defer. A table exists in the database whether or not `_sc_tables` has a row
for it; an application, a trigger or a model does not exist anywhere but its row. So the
overlay tables can be deferred while their subjects still work (§17), whereas `_sc_applications`
must arrive with applications themselves — there is nothing to introspect an app *from*.

### 9.1 The merge and precedence rules, as implemented

`_sc_tables` and `_sc_fields` now exist, and the rules below are the ones the code enforces
(stated on `Table::apply_overlay` / `Table::apply_field_overlay` in `sc-catalog`).

**Precedence.** The database is the authority on everything it knows — columns, types,
nullability, keys; the overlay is the authority on everything it knows — access rules, label,
description, rich type, field kind, attributes — **and the two sets do not intersect**. A merge
with no contested field has no conflict semantics to get wrong. This is a standing constraint on
what may ever be *added* to the overlay tables, not just a description of today's columns; the
full-column-list tests are what enforce it.

**No row → today's behaviour exactly.** A table with no overlay row comes out of `Catalog::reload`
identical to what `Table::from_physical` built, including admin-only
`AccessRules::default()` — the merge loop can only modify entries the introspection loop already
created, which is the zero-setup promise in one line of code. Deleting an overlay reverts to that
default, never to the previous value. `Table.overlay: Option<TableMetaId>` records provenance:
`None` means "nobody has configured this table", and the id is what lets an edit update the
existing row instead of racing to create a second.

**Keys.** `_sc_tables.name` is `UNIQUE` — it is the key the merge joins on, and it *is* the §9
`name` column (the subject's name; the Rust field stays `table_name`). `_sc_fields` has the
composite `PRIMARY KEY (table_name, name)`, since a field name is unique only within its table;
`id` remains a required, unique row handle (§9 requires `id` present, not that it be the key).

**Strict reads, refused nonsense.** A missing or ill-typed column is an error naming the table
and column, never a silent default. Both role columns are `NOT NULL`, and an off-scale role is
refused — on save and on read alike — rather than clamped, because rounding a role to the nearest
legal one would silently decide who reaches the data. System (`_sc_*`) tables may not have overlay
rows: refused on save *and* ignored in the merge, because a restored dump or hand-edited database
can contain a row the API would not have written.

**Orphans are kept and reported, not deleted.** An overlay row whose table (or column) no longer
exists survives (`orphan_table_meta`, and the field merge's kept rows): a dropped-and-recreated
table — a restore, a migration run outside Saltcorn — would otherwise silently lose its access
rules, and the failure is the confusing kind. Deliberately not enforced on save either: requiring
the subject to exist would make the row unsavable exactly when an admin is repairing one. The
admin API lists orphans and can forget them by name.

**Field-specific rules.** The kind is stored as a text discriminant plus its parameters folded
into `attributes` (lifted back into the structured `DataFieldKind` on read; those keys are
reserved). A rich type whose `sql_types()` does not include the column's actual type — or a type
name no longer registered — is a **reported inconsistency** (`FieldMergeIssue`, surfaced by
`Catalog::field_overlay_issues()`), not a silent downgrade and not a hard failure: the table
stays usable and the admin is told. For `Key` fields the database's target wins whenever it
enforces one — atop an introspected foreign key the overlay adds only `summary_field` and cannot
repoint the reference; on a column with no FK behind it, the overlay supplies the whole
reference, the case the database cannot enforce.

**Liveness.** Saving or deleting an overlay reloads the catalog cache, exactly as a schema change
does, and re-projects the API providers of any mounted application exposing the table
(`AppMounts::refresh_table`, §13.2) — an access or field change reaches a running app with no
restart. The overlay is only consulted when its table exists in the database: bootstrap creates
the overlay tables *through* `create_table`, which reloads, so a reload that assumed them present
could never bootstrap them.

---

## 10. Actions, triggers, workflows (`sc-action`, `sc-workflow`)

*Implemented; this section describes what is built.*

### 10.1 Actions

```rust
/// One elementary step: configurable, and run against one event.
#[async_trait]
pub trait Action: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// The settings this action takes, as data (§6.2's `FormField`).
    fn config_spec(&self) -> Vec<FormField>;
    /// Everything the spec cannot express — that a named table exists, that a
    /// configured formula resolves in the scope this event gives it. Called on
    /// **save and on load**, never at fire time.
    async fn validate_config(&self, check: &ConfigCheck<'_>) -> Result<()> { Ok(()) }
    async fn run(&self, ctx: &mut ActionContext<'_>) -> Result<Json>;
}
```

Three commitments are expressed as types rather than as prose:

- **An action is one elementary step.** `run` returns a value and takes no branch: control
  flow is the workflow engine's (§10.3). The small built-in set GOALS asks for is a
  *consequence* of that split, not a separate decision.
- **Configuration is data.** An action declares its settings as `FormField`s, so the admin UI
  renders a form for an action it has never heard of, and save-time validation checks the
  values against the same declaration. This is the same vocabulary a framework (§13.3) and a
  file-store backend (§14.1) use.
- **The caller travels as JSON.** `ActionContext` exposes the `Event` (below), the
  configuration, the catalog, the JS evaluator and the firing chain — the caller as a role
  plus the user's fields, not a `sc_auth::User`, because that JSON object is exactly what the
  formula language binds `user` to and it keeps an action implementable from a guest language.

**Where an action's configuration is a formula**, it is the §7.3 language in a scope the event
defines: bare identifiers are the affected row's fields, `row`/`old`/`user`/`payload` are
ambient, and `row`/`old` are *out of scope* on an event that has no row (so naming `row` in a
`login` trigger is an unknown identifier, not a silent null). `sc-action` owns that scope
(`action_shape`) so an `only_if` and an action's settings cannot disagree about what is
in scope.

The five built-ins are `insert_row`, `update_rows`, `delete_rows`, `fetch` and `run_js_code`,
and they live in **`sc-core-actions`, above the row layer**. That placement is the design's
one real constraint on where an action may live: a trigger's write goes through `sc-api`'s
`rows` module, so it is coerced, validated, `File`-field-checked and *observed* exactly like an
API caller's write. A second write path would quietly skip all of it.

- `insert_row` / `update_rows` / `delete_rows` take a target table and formulas. The `where`
  of the latter two **selects** rows: translated into SQL when it translates, and falling back
  to fetch-then-filter through the reified evaluator when it does not (as ownership reads do).
  Matched rows are then written **one at a time by primary key**, which is what makes each
  affected row's own triggers fire with its own row payload. `delete_rows` requires a `where`:
  an omitted one would mean "delete everything", which is not something a missing setting
  should be able to cause.
- `fetch` sends an HTTP request built from the event and returns the parsed response — the
  response is the point, so a directly-run trigger can hand it back to its caller. Its timeout
  is bounded (60s max) because a trigger runs inside the write or request that fired it.
- `run_js_code` runs a JavaScript body on the server's isolate with `row`/`old`/`user`/
  `payload` in scope. Deliberately **bounded**: no host API, so the code cannot reach the
  catalog, the network or the disk. Catalog access from a guest language is `sc-code`'s
  milestone (§15); this is its seed, not a preview of it.

An action's writes carry **admin authority** on an RLS table (`ROLE_ADMIN` plus the event's
user): a trigger is the admin's configuration, and the audit row a user may not insert is
precisely the one the audit trigger exists to write.

### 10.2 Triggers

A trigger binds one event to one configured action:

```rust
pub struct Trigger {
    pub id: TriggerId,                 // UUID: it is stored metadata (§9)
    pub name: String,                  // unique; the key an app, an API path and Run use
    pub description: String,
    pub when: EventKind,               // insert/update/delete · none · login · startup ·
                                       // error · often/hourly/daily/weekly
    pub channel: Option<String>,       // the table, for a table event; nothing else has one
    pub only_if: Option<String>,       // a predicate over the affected row (table events)
    pub action: String,                // a registered action's name
    pub configuration: Attrs,          // that action's settings
    pub min_role: Option<u8>,          // the floor for running it through an app's API;
                                       // None = admin-only
    pub attributes: Attrs,             // sparse: `enabled`, and the periodic timing
    pub last_run_at: Option<DateTime<Utc>>, // the scheduler's record; never written by a save
}
```

**One trigger = one event + one action.** Not a list of actions: a sequence of steps is a
*workflow*, and conflating the two is what made v1's execution path hard to reason about. Two
triggers on the same event is how you get two things done today. `TriggerBody::Workflow` is
still the intended growth path (§10.3) and the record is shaped for it — `action` +
`configuration` become one variant of a body — but nothing else has to move.

**The event is separate from the trigger.** One insert on `books` is one `Event`; it may fire
three triggers or none, and nothing about the event changes either way. That split is what lets
the row layer emit without knowing whether anything listens, and what a workflow engine will
later listen to with a different body.

#### Storage and the live set

`_sc_triggers` follows §9: a trigger has nothing to introspect it from, so **its row is its
definition** (not an overlay). Reading is strict — a missing or ill-shaped column is an error
naming the trigger, never a silently defaulted field that would fire the wrong action.

`Triggers` is the **cached live set**: firing an event is a lookup, never a query (GOALS), so a
write that nothing observes does not pay for the feature. Loading **validates**, and a trigger
that fails is dropped from the live set *with its reason kept* — fail closed, exactly as an
invalid ownership formula grants nothing — while remaining stored, listed and editable, because
editing it is the repair. A *disabled* trigger is filtered at match time rather than dropped at
load, so it stays listed (that is how it gets switched back on).

Validation runs on **save** and again on **load**, in one function: the action resolves in the
registry, the configuration validates against its `config_spec`, `min_role` is on the 1–100
scale, a table event names a real table and a non-table event names none (*both* directions),
the periodic timing is in range and belongs to the kind, and the `only_if` parses and resolves
against that table's shape.

#### The fire path, and its choke point

A table write raises its event through a seam the **catalog** owns (`TableEvents`), because
`sc-action` is layer 6 and the row layer is layer 8 — the writer cannot name the dispatcher.
`TriggerDispatcher` implements it and a server installs it once at boot; until then a write is
simply unobserved, which is what makes a build tool or an admin script safe to run against the
same database.

`TableEvents::observes(table, op)` is a **synchronous predicate the row layer asks first**, and
it is the choke point that keeps the feature free for writes that do not use it: an update
fetches its pre-image only when something will read it.

Every other event is raised by something that already sits above `sc-action` and can simply
hold the dispatcher: `login` from the one place a session starts, `startup` from the boot path,
`error` from the four places an `Error` becomes a response (a 404 for an unrouted path is a
*rejection*, not a failure, and does not raise one), and `none` from whoever asks —
`run_trigger`, which is what the admin's Run button and an application's exposed endpoint both
call.

One dispatch, for each matching trigger in name order:

1. **Check the depth.** An action may write a row, which is an event, which may fire another
   trigger — a feature (denormalising into a second table is the archetype), so it is *bounded*
   rather than forbidden. An event carries the **chain of trigger names** that led to it and
   refuses to descend past `MAX_DEPTH` (5), naming the whole chain: a depth counter alone would
   catch the loop but leave the admin to find it.
2. **Evaluate the `only_if`**, reified, against the affected row. An evaluator error means the
   trigger does **not** run: "could not be decided" is not "yes".
3. **Run the action**, with the chain, so anything it writes knows how deep it is.

**One trigger's failure is one trigger's failure**: every run is independent, and none of it
reaches the write that caused it — which has already committed. A request that inserted a row
gets its row back even when the audit trigger it fired is misconfigured, and the reason is
reported rather than lost.

The `error` event is guarded against re-entrancy at exactly one place (the fire-and-forget
path), so an error raised while handling an error cannot become an infinite loop at the worst
possible moment.

#### `only_if`

A predicate over the affected row, in decision 7's scope: bare identifiers are the row's
fields, `row`/`old`/`user` are ambient, and the **operation flags (`_insert`, …) are refused by
name** — the trigger's own event *is* the operation, so `_insert` inside an insert trigger is a
tautology and inside a delete trigger a lie. It is always evaluated reified (there is no
statement for a translation to ride in on: the row is in hand, already written), and every
Ⱶ-path or Ↄ-relation it reads is resolved by the same prefetch an ownership check uses.

An `only_if` on a channel-less event is refused on save: there is no row to test, and accepting
a condition that can never be true is worse than saying so.

#### The periodic scheduler

`often` (every five minutes), `hourly`, `daily` and `weekly` — no cron expression, because a
cron string is a second language to learn, to validate and to render a form for. The timing is
three sparse attributes (`minute`, `hour`, `day_of_week`) and **everything is UTC**: a
server-side schedule has no user to have a timezone, and a local one would mean an hour that
happens twice a year and an hour that does not happen at all. One function (`Schedule::of`)
both validates and reads the timing, so what the admin is refused and what the scheduler
computes cannot drift apart; a timing value on a kind with no use for it is refused rather than
ignored.

`Scheduler` is one tokio task started by `serve` (and only by `serve`), waking on the minute
boundary and firing due triggers through the same `run_trigger` path a direct run takes. The
clock is a **parameter** — `tick(now)` — and the loop is the only place the time is read, which
is what lets the rules be tested against a table of instants instead of waited for.

- A trigger's clock starts from its persisted `last_run_at`, or from **now** for one that has
  never run — never from the epoch, which would make every newly created trigger instantly
  overdue.
- A run missed while the server was down is caught up **once**: due is
  `next_due(last_run) <= now`, and after one run `last_run` is now.
- Each firing runs in **its own task**, so one slow action delays neither the clock nor another
  trigger, and an occurrence that arrives while the last is still running is **dropped, not
  queued**.
- `last_run_at` is a column `save_trigger` never writes — only the scheduler does. An admin
  editing a trigger at 3pm must not thereby claim the daily job ran at 3pm, or that it never
  ran.
- **Disabling is not downtime**: a disabled trigger's clock still advances, so switching a
  nightly report off for a week and back on runs it *tonight*. Nobody chooses downtime, so that
  one is caught up; switching a trigger off is a decision to skip those runs.

#### Reaching a trigger from outside

An application declares the subset of triggers it exposes (§13.2), and each becomes one
`POST {mount}/actions/{name}` typed into the app's generated client as `runFoo(body)`. The body
is the event's payload and the action's result is the response. Authorization is the trigger's
own `min_role`, **defaulting to admin** when unset — a trigger whose access nobody has thought
about must not turn out to be public — and a trigger the app does not name has no endpoint at
all: a 404, not a 403, because exposing one is the application's decision.

#### What `TriggerBody::Workflow` will need

The pieces already in place: the event model, the registry, the live set, the fire path and its
depth bound, and `run_trigger` as the "somebody asked" entry point. What a workflow body adds
is **durability** — a run row with context and position, committed per step (§10.3) — which is
why it is a separate milestone rather than a fourth variant of this dispatch: a `Workflow` body
does not return a value at the end of `run`, it *suspends*.

### 10.3 Durable workflow engine

This is the part GOALS is most emphatic about ("v1 is a mess; match modern engines"). The
design draws on DBOS / Temporal / Restate:

```rust
pub struct Workflow {
    pub id: TriggerId,
    pub version: u32,                     // versioned; a suspended run keeps its version
    pub steps: Vec<Step>,
    pub error_policy: ErrorPolicy,        // default for the workflow
}

pub struct Step {
    pub name: String,
    pub action: TriggerBody,
    pub next: Formula,                    // evaluated against context; yields next step name
    pub error_policy: Option<ErrorPolicy>,// per-step override
}

pub enum ErrorPolicy {
    Retry { max: u32, backoff: Backoff }, // configurable backoff
    Handler { step: String },             // jump to a designated error-handling step
    Fail,
}

pub struct Run {
    pub id: RunId,
    pub workflow: TriggerId,
    pub workflow_version: u32,
    pub context: Value,                   // JSON, accumulates state
    pub state: RunState,                  // Running{step} | Suspended{waiting} | Done | Failed
    pub trace: bool,
}
```

**Execution guarantees (normative, from GOALS):**

- **Each step runs in one database transaction.** The step's effects and the advance of
  `Run.state`/`context` commit together.
- **At-least-once per step.** If the engine crashes mid-step, on recovery it *resumes the
  step it was running* (the run row records which step is in flight). Steps SHOULD therefore
  be idempotent; the engine provides the transactional commit of context+position to make
  most steps effectively once.
- **Error handling** per the policy above: a designated handler step, or bounded retries
  with configurable backoff, defaulting at the workflow level and overridable per step.
- **Durability**: a run persists context + position after every step, so it can suspend
  (e.g. awaiting user input) and resume across process restarts — the run outlives any
  request.

Control flow is data: the `next` formula sees every step name as an in-scope string
identifier (as in v1), and loops use an explicit `ForLoop` step type. Keeping control flow
in the engine (not in code actions) is what keeps step code clean.

The engine is driven by a **durable queue** on the bus: scheduled/periodic triggers, retry
timers, and resume-after-suspend all flow through it, so a multi-node deployment can pick up
any runnable step. The in-process and pg-NOTIFY bus drivers are enough for single-node;
redis/kafka drivers scale it out.

---

## 11. Agents and copilot (`sc-agent`, `sc-copilot`)

### 11.1 Agents

An **agent is a kind of `Action`** (so it is also a trigger body). It is configured by
enabling a set of **skills**, each with its own config. A skill is an elementary agent
capability — most expose a tool to the LLM loop, some change chat behaviour.

```rust
#[async_trait]
pub trait Skill: Send + Sync {
    fn name(&self) -> &str;
    fn config_spec(&self) -> Vec<FormField>;
    /// Tools this skill contributes to the inference loop (may be zero).
    fn tools(&self, cfg: &Attrs, cat: &Catalog) -> Vec<Tool>;
    /// Hook to alter chat behaviour / system prompt (e.g. model picker, preload data).
    fn on_turn(&self, turn: &mut Turn) -> Result<()> { Ok(()) }
}
```

Built-in skills mirror v1's `agents` plugin: query-a-table tool, HTTP-request tool, run
a guest function as a tool, generate-and-run code tool, long-term memory (backed by a
table), MCP client, model picker, preload data, use-any-action/workflow-as-a-tool,
subagent handoff, web search, plan approval.

Agents run either attached to events (with an initial prompt derived from the triggering
row) or through an Agent-chat view pattern (a ChatGPT-like UI with history and sharing).

**LLM provider note:** the inference loop targets the Claude API by default (latest models
— Opus/Sonnet/Haiku families), with an abstraction over providers so others can be added.

### 11.2 Copilot & AppConstructor

The copilot is itself an agent composed of app-building skills (build tables, views,
workflows). Two front-ends, as in v1: a plain chat interface, and the staged
**AppConstructor** (describe → clarify → research → requirements → plan → execute → user
feedback → self-heal). For users who prefer an external coding agent, the copilot can emit a
`SKILL.md` describing the app.

---

## 12. Admin UI, CSP, and the form runtime (`ui/admin`, `ui/form-runtime`)

The admin UI and applications enforce a **strict Content-Security-Policy** (no inline
scripts, no inline event handlers). v2 satisfies this **structurally through React** rather
than through a server-side HTML model: the admin UI is a **React + TypeScript SPA**
(`ui/admin`) that talks to the server exclusively over a **typed JSON API** (§13), and every
UI bundle is self-hosted with no inline handlers, so the CSP needs no `unsafe-inline`.

This replaces v1's server-string HTML **and the previously-planned `sc-markup` symbolic
tree + JS-extraction crate — both dropped.** The server renders no admin HTML beyond a
minimal bootstrap document that loads the SPA bundle; all data flows as JSON through the
generated typed client (§13.1), so the API and the UI cannot drift.

- **`ui/admin`** — React + TypeScript + **react-bootstrap** (Bootstrap 5.3) SPA, themed with
  **Tabler** ([tabler.io](https://tabler.io)). Tabler is a Bootstrap-5 admin UI kit, so it
  layers directly on the react-bootstrap decision rather than replacing it: Tabler supplies
  the design system (layout shell, navigation, cards, forms, icons, dashboard components) as
  the admin UI's look and feel, while react-bootstrap remains the component primitives. Served
  under a separate URL (subdomain or path) from user-facing routes. Only admins log in
  initially; later, admins may grant restricted access (e.g. app development only) to selected
  non-admins. Includes a much-improved **table editor** (Airtable-inspired), a **file
  manager**, and an **application manager** (§13.2) — creating an app, configuring its
  framework from that framework's declared settings, and building/mounting it are admin-UI
  operations, not code — all built against the typed API client.
- **`ui/form-runtime`** — the dynamic form framework (React + TypeScript), rebuilt cleanly
  from v1's messy client JS. Covers the requirements GOALS lists explicitly: conditional
  fields (shown based on other values), repeated sub-forms (order lines on an order),
  dynamically populated selects (options from the server or from client code, depending on
  other field values), dynamic attributes/contents, and client+server validation. Styling is
  **Bootstrap 5.3 via react-bootstrap**.
- **`ui/ide`** — the file-store IDE: the VS Code workbench embedded on its own admin route, for
  editing a store that holds an application's source. It is *not* part of the SPA, for reasons
  that are structural rather than stylistic (§12.1).
- **`ui/builder`** — the Craft.js + react-flow drag-and-drop builder is *not* built into the
  admin UI; it arrives with the Saltcorn-v1 view/page experience (post-MVP).

The XSS-safety story is now the ordinary React one — values are escaped by the framework and
`dangerouslySetInnerHTML` is banned by lint — pairing with the structural SQL-injection
safety in `sc-query` (§4).

### 12.1 The file-store IDE (`ui/ide`)

A file store holding a React front-end (§13.3) is a **software project**, and the file manager
in `ui/admin` treats it as a folder of files: one file open at a time, a plain textarea, no
project-wide anything. That is the wrong instrument for the primary use case. The requirement is
therefore stated at full strength: an admin edits a store **as if they had it open in desktop
VS Code** — a project tree on the left, tabs of editors in the main area, the command palette,
find-in-files, keybindings and settings — and from there formats JavaScript with **prettier**,
sees **TypeScript errors**, and **builds the application**.

**This is the real VS Code workbench, not an editor component.** The tree, the tabs, the
palette, the settings editor and the keyboard story are the deliverable, and every one of them
is code that already exists in VS Code. Writing them again around a bare editor is the failure
mode to avoid, so the decision is which distribution of VS Code to embed. Three were examined
(July 2026):

- **`@codingame/monaco-vscode-api`** — VS Code's own workbench, service by service, as npm
  packages, at **36.0.0 tracking VS Code 1.128.1** and published within the week. Its
  `workbench-service-override` renders the full workbench into a container element; its
  `files-service-override` takes a **custom `FileSystemProvider`**; extensions are registered
  from a **manifest object in the host page**, which can then call the `vscode` API directly.
- **`vscode-web`** — the static official web build, served under a route. **Dead**: last publish
  1.91.1 (July 2024), repository archived 2026-07-21. Its live neighbours are no better —
  upstream `openvscode-server` stopped being updated (deprecated by its packagers 2026-07-16),
  leaving `code-server`: a several-hundred-megabyte per-platform binary to install, supervise and
  reverse-proxy, for a product whose only Rust-side dependency today is `npm`.
- **`@typefox/monaco-editor-react`** — the right family (it wraps `monaco-vscode-api`) but one
  layer too low: it is documented as one editor and *one language client per component*, and it
  pins `@codingame/*@^25.1.2`, eleven majors of VS Code behind. Its genuinely useful part,
  `monaco-languageclient`, is a separate package used on its own merits (below).

**The decision is `@codingame/monaco-vscode-api`, and the deciding argument is where
customization lives.** In a statically served workbench, everything the host wants — a
filesystem, a formatter, a build button — must be packaged, bundled and served as a **web
extension**, three extension builds before anything works, and `prettier-vscode` is a *node*
extension that does not run in a web extension host at all. With `monaco-vscode-api` the
workbench runs in the host page's own JavaScript context: `registerExtension(manifest,
ExtensionHostKind.LocalProcess).setAsDefaultApi()` takes a manifest **object**, after which
`vscode.commands.registerCommand` and `vscode.languages.register*Provider` are ordinary function
calls in `ui/ide`. **No `.vsix` is built and no extension is packaged** — the "there will
probably have to be a VS Code extension" turns out to be a manifest literal.

**`ui/ide` is its own page, not part of the admin SPA.** VS Code is designed to be initialized
once per page and cannot be unloaded, the bundle is an order of magnitude larger than the SPA,
and the workbench owns its whole viewport. So it is a separate Vite project served under
`/ide/`, opened as `/ide/?store=<name>` — one document, no history fallback, no base-path games —
with the store name choosing the workspace folder, so the workbench's own storage keeps each
store's open tabs and layout. It is admin-only through the same session cookie as every other
admin surface, and it reuses the **generated typed client** (§13.1) rather than hand-written
`fetch` calls, so it cannot drift from the API either.

**It needs its own CSP.** The strict admin policy (`script-src 'self'; style-src 'self'`) is
satisfied structurally by React (above); the workbench injects styles and runs workers from
blobs, so `/ide/*` is served with a relaxed policy of its own — `style-src` allowing inline,
`worker-src blob:` — and the admin SPA's policy is left untouched. Serving the IDE from a
distinct route is what makes that containment possible, and is the second reason it is a
separate page.

The four capabilities then land as follows:

- **Files** — a `FileSystemProvider` registered with `registerFileSystemOverlay` over the file
  endpoints that already exist (`browseFiles`, `readFile`, `writeFile`, `makeDirectory`,
  `deleteFile`, `renameFile`, §9's per-file metadata beside them). No new server surface: the
  file manager's API *is* the IDE's filesystem, so a store of any backend — local, git, object —
  is editable. What the API did have to learn is that **a missing path is a 404, not a 500**:
  an editor asks whether a path exists by trying to read it — it stats before writing, and looks
  for optional files like `.vscode/settings.json` — so "not there" is a value it acts on rather
  than a failure to report (§16). There is no `stat` endpoint and no watcher, and neither is
  worth adding yet: a path is stat'ed by listing its parent, and a change made outside the IDE
  is seen when the explorer is refreshed.
- **Prettier** — `prettier/standalone` with its plugins, **in the browser**, registered as a
  `DocumentFormattingEditProvider` so format-on-save and the format command work as they do in
  desktop VS Code. The project's own `.prettierrc` (or `package.json`'s `prettier` key) is read
  through the filesystem provider and passed as options. Deliberately *not* the project's own
  installed prettier: a store need not have one, need not have `node_modules`, and need not have
  a local path at all — formatting should not be the capability that stops working on an object
  store.
- **Build** — a command contributed by the in-page manifest, calling `buildApplication` (§13.2)
  for the application whose derived `source.store` (§13.3) is this store; the SPA already
  receives that field, so the IDE filters the application list client-side and adds no endpoint.
  A build failure is an **Application** error carrying the bundler's and `tsc`'s diagnostics
  (§16), and those go into a `DiagnosticCollection` — so the Problems panel shows a failed
  build's errors, by file and line, in the tab the admin has open.
- **TypeScript errors** — a **language server on the server**, `typescript-language-server`
  spawned in the store's directory and bridged to `monaco-languageclient` over a WebSocket on an
  admin-authenticated route. The alternative, VS Code's own `typescript-language-features`
  extension in a web worker, is available as a package and would work through the filesystem
  provider — at the price of dragging the project's entire `node_modules` type surface across
  HTTP. Server-side keeps it where it already is, and type-checks against the real
  `tsconfig.json` and the really-installed dependencies. This is the one capability that needs
  `FileStore::local_path` (§14.1), so it follows the rule that method already established: a
  store with no local path cannot host a buildable app, and equally cannot host a language
  server. Such a store gets editing, formatting, grammars and syntax errors, and is told why it
  gets no semantics. Until the language server lands, the build's diagnostics are the
  type errors, which is why they are wired to the Problems panel and not to a toast.

---

## 13. HTTP endpoints, applications, frameworks, and APIs (`sc-api`, `sc-app`)

### 13.1 The endpoint model and typed API generation

The GOALS "HTTP server framework" requirement drives a **single machinery** shared by the
admin UI API and every application API:

- **Endpoints are Rust values, not just handler functions.** `sc-api` defines a reified
  representation of an endpoint — method, path (with typed path/query params), a typed
  request body, and a typed response — where argument and result types are described by a
  small schema enum. This mirrors the "data, not fluent calls" philosophy of `sc-query`
  (§4) at the HTTP layer.

```rust
pub struct Endpoint {
    pub method:  Method,
    pub path:    PathSpec,          // literal segments + typed params, e.g. /tables/{id}/rows
    pub input:   TypeSchema,        // query + body args
    pub output:  TypeSchema,        // result value
    pub auth:    AuthRequirement,   // role / ownership, enforced via §7
    pub handler: HandlerRef,        // Rust fn, or guest code / SQL for custom routes
}

pub enum TypeSchema {               // enough to describe args & results and emit TS types
    Value(ValueType),
    Struct(Vec<(String, TypeSchema)>),
    Array(Box<TypeSchema>),
    Optional(Box<TypeSchema>),
    // …
}
```

- **Dynamic routes.** Application APIs and custom user routes are registered at runtime and
  are **not known at compile time**, so the endpoint set is a runtime value — not everything
  can be statically typed in Rust. The `Endpoint`/`TypeSchema` representation is what lets a
  runtime-defined route still be fully described (and typed for consumers).
- **The admin UI API is compile-time-known, but expressed as the same fixed values.** Rather
  than a bespoke statically-typed router, the admin API is built as a set of constant
  `Endpoint` values fed through the identical machinery. This maximises code reuse and means
  the admin SPA consumes a generated typed client exactly as an application would.
- **TypeScript generation.** From the `Endpoint`/`TypeSchema` values, `sc-api` generates
  TypeScript **type declarations and a typed API-consumer library**. GOALS requires this for
  both the admin UI and per-application APIs, so `ui/admin` and every code-framework app get a
  type-checked client that cannot drift from the server contract.

### 13.2 Applications

```rust
pub struct Application {
    pub id: AppId,                       // UUID (§9 rule for stored metadata)
    pub name: String,
    pub description: String,
    pub subdomain: String,               // each app served on its own subdomain; unique
    pub framework: FrameworkRef,         // one primary UI framework
    pub extra_frameworks: Vec<FrameworkRef>, // may bring in others (see Open Questions)
    pub tables: Vec<TableId>,            // the subset of the data layer it can access
    pub file_stores: Vec<FileStoreId>,
    pub apis: Vec<ApiConfig>,            // any number, each on a sub-path
    pub static_dirs: Vec<StaticDir>,     // any number, each served at a sub-path
    pub csp: CspPolicy,                  // strict by default
    pub attributes: Attrs,               // sparse per-app values (§9 rule)
}

pub struct FrameworkRef {
    pub name: String,                    // the registered Framework's name
    pub config: Attrs,                   // framework-specific; validated against config_spec (§13.3)
}

/// A subdirectory of a file store served as static assets under the app.
pub struct StaticDir {
    pub mount: String,                   // sub-path within the app, e.g. /docs
    pub store: FileStoreId,
    pub path: String,                    // subdirectory within that store
}
```

**Multiple applications share one data layer**; each sees only its declared subset of
tables and file stores. This is v2's replacement for v1 schema-per-tenant multi-tenancy —
lighter-weight and driven by access subsets rather than separate schemas.

**An application is created and configured in the admin UI — never in Rust code and never
by a CLI flag.** The admin picks the framework, fills in that framework's settings (a React
app needs the file store, or the subdirectory of one, holding its code), sets the subdomain,
adds any number of APIs on sub-paths, and adds any number of statically-served
subdirectories. This is the whole configuration path; embedding `sc-server` in a bespoke
binary to declare an `Application` in Rust is not one. `sc-cli` may grow app commands for
scripted deployment, but the admin UI is the primary and complete surface.

**Applications are stored in `_sc_applications`** (§9) and so obey the §9 rules: UUID `id`,
`name`, `description`, `attributes`. Note what this is *not*: `_sc_tables`/`_sc_fields` are
**overlays** — introspection already yields the tables, so a row only adds to what the
database itself reports, and a legacy database needs zero metadata rows. An application has
no such underlying reality. It exists only as stored configuration, so its row is the
authoritative and only definition of it. That is why applications need stored metadata even
in the MVP, while the table/field overlays remain deferred (§17).

The `Application` value is pure data, and the stored row is that value serialised — one
column per field every app has (subdomain, framework, the subsets, the API/static-dir lists,
the CSP), with genuinely sparse values in `attributes`, per the §9 column-vs-attributes rule.
A framework's own settings live in `FrameworkRef.config` rather than the app's `attributes`,
because they belong to the framework, not the app: the admin UI renders a form for them from
that framework's `config_spec()` (§13.3), as it will for an action's configuration when the
actions registry arrives.

**Lifecycle: create → build → mount, without a restart.** GOALS is explicit that a full
restart should never be required and that only individual APIs and applications may need
one, so mounting is a runtime operation, not a boot-time one:

- **At boot**, `sc-server` loads every row of `_sc_applications` and mounts each app.
- **On create/edit**, the admin UI's call persists the row, then builds (for a framework with
  a build step, §13.3) and mounts or re-mounts *that app alone*. Other apps keep serving; the
  admin never goes away; the process does not restart.
- **On delete**, the app is unmounted and its row removed; its subdomain stops resolving.
- The mount registry is therefore **live**, not a value fixed at router construction: it is
  shared mutable state behind the router, keyed by subdomain.
- A **build failure leaves the previously mounted version serving** and surfaces the
  bundler's diagnostics to the admin (§16 error handling; the failure is an *Application*
  error — bad configuration or bad app code — not a *System* error).

Because a build runs a bundler, which is slow and can fail, "save the configuration" and
"build and mount it" are distinct operations with distinct outcomes: an app can be saved but
unbuilt, and the admin UI shows that state rather than pretending a save deployed anything.
A saved-but-unbuilt app is a normal state, not an error — it is what a newly created app is
until its first build.

### 13.3 Frameworks

```rust
/// Owns an application's primary UI.
#[async_trait]
pub trait Framework: Send + Sync {
    fn name(&self) -> &str;
    /// The settings this framework needs, so the admin UI can render a form for
    /// them without knowing anything about this framework (§13.2).
    fn config_spec(&self) -> Vec<FormField>;
    /// Serve the app's routes (bundled assets, SSR, or v1 view/page rendering).
    async fn handle(&self, req: AppRequest, cat: &Catalog) -> Result<AppResponse>;
    fn build(&self) -> Option<BuildSpec>;   // code frameworks have a build step
}
```

**A framework also declares how it presents itself.** The registry carries a `FrameworkInfo`
per framework — name, human label, and one sentence about who it is for — in the order an
admin should be offered them. This exists so the admin UI can show two frameworks as the
genuinely different propositions they are (one creates the project for you; the other hands
you the paths) while containing no knowledge of either: the alternative is a screen that
special-cases the name `react`, which stops working the moment a third framework, or one
supplied by a guest language through `sc-code`, arrives. The same reasoning applies to the
application's derived `source: { store, path }` in the admin API — the UI links into the file
manager at an app's source without knowing that `code` states it in five settings while
`react` derives it from one.

**`config_spec` is what makes "the admin picks a Framework" work.** GOALS requires that
different frameworks have different settings — a React app needs the file store or
subdirectory holding its code; a Saltcorn-v1 app needs none of that. The admin UI must render
a form for whichever framework the admin picked *without* a per-framework special case, and
a framework supplied by a guest language through `sc-code` must work the same way. So a
framework declares its settings as data, exactly as `Action` (§10.1), `Agent` (§11.1) and
`ModelProvider` (§14.2) declare theirs — one `FormField` vocabulary (§6.2), one way to render a
configuration form, for every configurable extension point, and the same one a row editor
already uses. Post-MVP this is `ui/form-runtime`
(§12); the MVP, which does not have it yet, renders the same `FormField` data with a plain
form and gains the runtime later without a contract change. `FrameworkRef.config` is
validated against the spec on save (`validate_framework_config`), so a misconfigured app is
rejected at the point the admin can fix it rather than at build or serve time, and the same
config resolves to the build step (`app_source_from_config`) — the store, sub-directories and
build command are stated in the config and nowhere else.

`config_spec` still takes no arguments, so a framework's settings remain **static data** — and
a setting whose choices depend on runtime state, such as the `store` setting, states *where its
options come from* rather than listing them. That is §6.2's `OptionsSource::ServerQuery`: the
spec declares `store` as the named query `file_stores`, and the server resolves it
(`sc_catalog::resolve_options`) at the two points a spec is used — when the admin API hands it
to the UI, and when a config is validated on save.

Resolving **server-side** is the part worth keeping: the admin UI receives a concrete option
list and needs no query evaluator, so this did not have to wait for the form runtime (§12), and
a spec supplied by a guest language through `sc-code` stays inert data rather than becoming
something the host must execute. A query is therefore a *name*, not an expression, and the set
of names is the server's to define. `OptionsSource::ClientCode` — for options that depend on
other values in the form, which cannot be pre-resolved — is what still waits for the runtime.

The consequence for an admin: an unknown store name is rejected **on save**, where they are
still looking at the form, instead of failing the build later. The list offered is every
*defined* store plus any connected without a definition, deliberately including stores that are
defined but currently unreachable (§14.1) — otherwise an unmounted disk would block editing
every application that uses it, including to repair it. Validation at *build* time checks only
the config's structure, since whether the store exists was settled on save and re-asking it
would make a build fail for a reason unrelated to building.

- **The `code` framework** — the generic code framework (React, Next.js, SvelteKit, React
  Native, anything that emits a static bundle): the app's source lives in a git repository
  that is (a subdir of) a selected file store, editable in an in-browser editor (ideally VS
  Code for the Web), with a build step. `sc-server` serves the bundled assets. The app talks
  to data only through the API providers. Its `config_spec` is where "which file store,
  which subdirectory, which build command" is declared — as the settings `store`, `source`,
  `output`, `command` and an optional `client`.
- **The `react` framework** — the same serving path with the settings replaced by
  conventions and the project created by the server. See "Two code frameworks" below.
- **Saltcorn-v1 framework**: the drag-and-drop views/pages experience, continuously
  improved, using `sc-viewpattern` + `ui/builder`. How its rendered output stays CSP-safe
  now that `sc-markup` is dropped is an open question (§18.5).

#### Two code frameworks, and why

`code` is the right shape for "any bundler, any layout" and the wrong shape for the common
case. It asks for five mutually-consistent settings and then assumes a project that already
exists — which the admin has to create over SSH, on a product whose premise is that they
never need one. `react` inverts that: **conventions instead of settings, and the server
creates the project.** It is not a second serving implementation — a built React app is a
static bundle with an SPA fallback, which is exactly what `CodeFramework::serve` already
does — the difference is entirely configuration and scaffolding.

Its `config_spec` is two settings: `store` and `project`. Everything `code` asks for is
derived: a project named `todo` has source `todo/`, output `todo/dist`, build command
`npm run build`, and its generated client and runtime under `todo/src/saltcorn/`. The same
`BuildSpec` comes out the other end (`app_source_from_config` resolves both frameworks), so
the build, mount and serve paths are shared, not forked — nothing downstream of that function
can tell which framework it is building.

`project` is the framework's setting rather than the application's `name` because it is a
directory on disk, while `name` is a renameable display string; it is also the only thing
`app_source_from_config` is given. It is constrained to a plain identifier (ASCII letters,
digits, `-`, `_`, leading alphanumeric), checked **on save** by the framework itself — §6.2's
vocabulary states presence, type and membership, not patterns, and growing it for one setting
would oblige every guest-language framework to be understood by it. Checking at save rather
than at build is §1.6's principle again: the admin hears about it while looking at the form,
and a traversal is refused as the setting they typed rather than as a build path caught
escaping the store.

A framework also supplies the **default CSP** for an app that does not state one
(`framework_default_csp`), because a framework that chooses the build tooling knows what that
tooling's output needs — `react` supplies the policy below, `code` and anything unrecognised
get the strict baseline. A stated policy always wins.

The opinions the scaffold hard-codes, and the reasoning that has to hold for them to stay
hard-coded:

- **Vite + React + TypeScript, no SSR.** Its output is real module scripts and stylesheet
  links with no inline script, so a scaffolded app's default CSP needs no exception. SSR is
  excluded on principle rather than by omission: this section serves *bundled assets*, and
  server rendering would put a Node process in every application's request path — a
  different serving model, not a different setting.
- **Routes declared as data in one file**, using `react-router`, rather than a file-system
  convention. File-system routing needs a build-time plugin scanning directories to generate
  the route module — a second convention to own and to debug through — and buys nothing when
  the scaffold generates the route list from the app's tables anyway.
- **The generated client plus generated typed hooks** (`useRows`, `useRow`, `useCreate`,
  `useUpdate`, `useDelete`), and no data-fetching dependency. Hand-rolled `useEffect` +
  `useState` around the client is the boilerplate this framework exists to delete. The hooks
  are generated from the same `EndpointSet` as the client (§13.1), so they are typed per
  table. A general-purpose query library would add a second mental model (query keys,
  invalidation strategy) for a cache whose keys are already known exactly: one per table,
  invalidated by table name on mutation.
- **Authenticated by default.** The scaffold ships an auth provider, a current-user hook and
  a login screen against the app's own `/api/login` / `/api/logout` / `/api/whoami`. A route
  opts out with a `public` flag. The default is this way round because forgetting to mark a
  route should produce a locked door, not an open one — and because client-side auth state
  is a UI convenience that is never the enforcement point: every request is authorized again
  by §7, which is what makes a wrong flag cosmetic rather than a hole.
- **Plain CSS, replaceable.** No CSS framework (a large dependency with its own version
  treadmill, when the value on offer is the data/auth/build path, not the look) and no
  CSS-in-JS (runtime `<style>` injection would force `style-src 'unsafe-inline'` into every
  scaffolded app's CSP). The scaffold writes the stylesheet once and never regenerates it;
  nothing in the runtime imports it.
- **The runtime is generated into the project, not an npm package.** This is the decision
  that cannot be walked back, and what settles it is that the runtime is *app-shaped*: the
  hooks worth having are typed per table, hence generated from this app's endpoints, which a
  registry package cannot contain — it could only ship generic untyped hooks, discarding the
  reason to have a hooks layer. So the usual objection to vendoring (instantly stale) does
  not apply: `src/saltcorn/**` is generated output refreshed on every build, like the client,
  and a server upgrade cannot leave it pinned behind. The accepted cost is that it is
  overwritten and so not hackable in place; everything outside it is the admin's and is never
  touched.

What unifies these: each is either **derived from the app's own schema** (routes, hooks,
client) or **a dependency not taken** (no data library, no CSS framework, no CSS-in-JS, no
SSR runtime). Generated things can be regenerated and need no version negotiation with the
server; things not depended on cannot drift out of step with it. That is the test a further
opinion has to pass — an opinion that can only be honoured by a package the admin must keep
in step with the server is the wrong opinion.

**Scaffolding.** The server writes the project itself, on the app's first save. This is what
makes `react` more than a settings preset: the alternative is the MVP's tutorial, which told
the admin to log into the host and run `npm create vite`, `npm install` and `git init` before
the settings meant anything — and an admin with no shell could not use the product at all.
What is generated:

- `package.json`, `vite.config.ts`, `tsconfig.json`, `index.html`, `.gitignore`, the entry
  point, the app shell, the login screen, the route list, a stylesheet, and **one page per
  table the app declares**, using that table's real columns.
- The runtime under `src/saltcorn/`: the typed client and the typed hooks, from the app's own
  `EndpointSet`.

Three rules it obeys. **It never overwrites**: scaffolding into a directory with anything in
it is refused, naming the directory, before a byte is written — a generator that clobbers is
worse than none, because the work it destroys is the admin's. **It generates against real
tables**, so the app comes up showing rows rather than a placeholder whose first job is to be
deleted. And **failures carry the tool's own output** (§16) — a failed `npm install` reports
the registry error, not that something failed.

Only `src/saltcorn/` is rewritten afterwards, on every build; everything else belongs to the
admin from the moment it exists. That split is what makes regeneration safe and is why adding
a table in the admin UI makes its hooks exist at the next build with nobody regenerating
anything by hand. The build also **installs dependencies** when `node_modules` is absent
(carried on the `BuildSpec` as an `InstallSpec`, so `code` apps — whose dependencies are the
admin's business — are unaffected), and the project's build script is `tsc --noEmit && vite
build`, so a client that no longer matches the app's calls fails the build with a type error
rather than producing a bundle that 404s at runtime.

### 13.4 API providers

```rust
#[async_trait]
pub trait ApiProvider: Send + Sync {
    fn name(&self) -> &str;               // rest | graphql | grpc | trpc | mcp
    fn mount(&self) -> String;            // sub-path within the application
    async fn handle(&self, req: ApiRequest, cat: &Catalog, user: &AuthUser) -> Result<ApiResponse>;
}
```

Per application, any number of API providers can be enabled, each on a sub-path: **REST,
GraphQL, gRPC, tRPC, MCP**. Each provider projects the application's shared `Endpoint` set
(§13.1) into its protocol. An application's API surface covers **tables and actions** (subject
to the permission settings of §7) **and custom routes** authored by the developer as guest
code (in a supported language) or as SQL queries. Quality bar: Hasura / PostgREST / Supabase.
All API access flows through the same authorization layer (§7), so an API caller sees exactly
the rows a user of that role/ownership would, and every provider participates in the shared
TypeScript consumer generation (§13.1).

### 13.5 Serving: TLS certificates and readiness notification

The HTTP server (`sc-server`) terminates TLS in-process so a deployment needs no external
reverse proxy (though one may still front it). TLS uses **rustls** (via `tokio-rustls` /
`axum-server`), keeping the stack pure-Rust and off OpenSSL, consistent with the dependency
posture of §16.

**Certificates** are obtained two ways, admin-selectable per deployment:

- **ACME (Let's Encrypt).** Certificates are provisioned and renewed automatically from an
  ACME CA using a pure-Rust ACME client (e.g. `rustls-acme` / `instant-acme`) — no external
  `certbot` process and no C dependency. The ACME account key and issued certificates are
  persisted (in the primary DB metadata or a file store) so renewals survive restarts and are
  shared across nodes. The CA directory URL is configurable so any ACME server, not only
  Let's Encrypt, can be used.
- **Manual.** An admin may instead paste/upload a certificate chain and private key. These are
  stored the same way and loaded at startup; no ACME traffic occurs in this mode.

Both modes feed the same rustls `ServerConfig`; switching modes does not change how the
listener is set up. Plain-HTTP serving (behind a trusted proxy, or for local development)
remains available.

**Readiness notification.** On systemd-managed Linux, `sc-server` sends `READY=1` via the
`sd_notify` protocol once it has bound its listener(s) and the catalog is initialised, so the
unit can use `Type=notify`. This must not add a build-time dependency on `libsystemd-dev`: the
protocol is just a datagram written to the unix socket named by the `$NOTIFY_SOCKET`
environment variable, so it is implemented with a pure-Rust helper (e.g. the `sd-notify` crate,
which has no C dependency) or a few lines writing to that socket directly. **The code compiles
on every target platform**; on non-Linux, or on Linux where `$NOTIFY_SOCKET` is unset (not run
under systemd `Type=notify`), the call is a no-op. `RELOADING=1` / `STOPPING=1` can be sent on
the corresponding lifecycle transitions by the same helper.

---

## 14. Files and models (`sc-files`, `sc-model`)

### 14.1 File stores

```rust
#[async_trait]
pub trait FileStore: Send + Sync {
    fn name(&self) -> &str;
    async fn read(&self, path: &str) -> Result<Bytes>;
    async fn write(&self, path: &str, data: Bytes) -> Result<()>;
    async fn list(&self, dir: &str) -> Result<Vec<Entry>>;
    fn is_git_repo(&self) -> bool;
    async fn get_meta(&self, path: &str) -> Result<FileMeta>;   // via xattrs
    async fn set_meta(&self, path: &str, m: &FileMeta) -> Result<()>;
}
```

Drivers: local directory, S3, git-recognised directory. **Access rules** are set per file
and per directory; to access a file, a user must have rights to **every directory in its
path** (path-cumulative authorization). Per-file metadata is xattrs, no DB rows (§9).

`local_path` is the deliberate exception to backend-agnosticism: `Ok(None)` means the backend has
no on-disk path, and the two jobs that cannot work without one are a framework's **build step**
(§13.3) and the IDE's **language server** (§12.1). Both must say so rather than pretend
otherwise.

### 14.2 Predictive models

```rust
#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn name(&self) -> &str;                       // scikit-learn | mc-stan | …
    fn config_spec(&self) -> Vec<FormField>;
    fn hyperparameters(&self) -> Vec<FormField>;
    /// Fit against a subset of a table's rows → a model instance (parameters inspectable).
    async fn fit(&self, data: RowStream, cfg: &Attrs, hp: &Attrs) -> Result<ModelInstance>;
    /// Apply a fitted instance to a new row → an outcome defined by the provider/config.
    async fn predict(&self, inst: &ModelInstance, row: &Row) -> Result<Value>;
}
```

A model is configured against a table; fitting produces a `ModelInstance` (stored in
`_sc_model_instances`) whose parameters may themselves be the point of interest, or which is
applied to new rows for prediction.

---

## 15. Code adapters and polyglot plugins (`sc-code`)

A `CodeAdapter` maintains an open interpreter for a guest language, inside which the catalog
entities are available. Adapters are initialised lazily (not every install needs Python).
Guest code can provide any extension point **except `DatabaseDriver`** (Rust-only).

```rust
#[async_trait]
pub trait CodeAdapter: Send + Sync {
    fn language(&self) -> &str;                       // "javascript" | "python" | …
    async fn call(&self, module: &str, func: &str, args: Vec<Value>) -> Result<Value>;
    /// Register a guest-provided extension (action, fieldview, skill, provider…).
    fn register(&self, decl: &GuestDecl) -> Result<Registration>;
}
```

- **JavaScript** adapter MUST be **API-compatible with Saltcorn v1** so v1 plugin/formula
  code can run. Initial focus is **JavaScript and Rust**; Python, Java, C#, Go follow, each
  with its own plugin mechanism sharing this adapter shape.
- Guest extensions appear in the catalog as ordinary `Box<dyn Trait>` implementations backed
  by a single Rust shim per adapter, so higher layers never know or care what language an
  extension is written in.

---

## 16. Cross-cutting concerns

**Error handling (principle 5).** `sc-error` defines one `Error` enum and `Result<T>`.
The variants are coarse and location-based (`NotFound`, `Invalid`, `Config`, `Database`,
`Query`, `Auth`, `File`, `Serde`, `Internal`, plus a `Context` variant that wraps a source),
with a `Context` extension trait (`.context()` / `.with_context()`) on both `Result` and
`Option` that preserves the underlying error as a `std::error::Error` source chain, and
`bail!` / `ensure!` macros. Library code MUST NOT `unwrap()`/`expect()` on fallible paths;
this is enforced mechanically — `[workspace.lints.clippy]` denies `unwrap_used`/`expect_used`,
each crate opts in via `[lints] workspace = true`, and `clippy.toml` exempts test code.
Errors carry context and either are handled or propagate to a crash with a clear message. No
`Result` is silently discarded.

**Error classification and logging (from GOALS).** Every `Error` carries a **kind** that
splits errors into two classes with different audiences and different remedies:

- **Application errors** — the fault is in *configuration authored by an app builder*: an
  invalid calculated-field equation, a malformed access formula, a bad action config, a
  workflow that references a missing field. Nothing is wrong with Saltcorn; the person
  building the app must fix their configuration. These map to the `Config`/`Invalid`/`Query`
  family (and to guest-code errors surfaced through `sc-code`).
- **System errors** — something crashed and there is *likely a bug in the Saltcorn code*
  (or the infrastructure it depends on): a driver failure, a panic caught at a boundary, an
  `Internal` invariant violation. These map to the `Database`/`Internal`/`Serde` family.

```rust
pub enum ErrorKind { Application, System }
impl Error { pub fn kind(&self) -> ErrorKind { /* per-variant classification */ } }
```

Regardless of whether an error is handled or propagates to a crash, it is **logged to an
error log in the primary database** — the `_sc_errors` table (§9). A log row records the
kind, the variant, the message and source chain, and context (application, request/route,
table, workflow run + step, user role) where available. The error log is a runtime record
stream, so like users/runs/files it is **not cached** and is written on a best-effort path
that must never itself mask the original failure (a logging failure is swallowed after being
reported, never allowed to replace the real error). Errors are also an `Event` (§10.2), so a
trigger MAY fire on them (e.g. alert an admin on a `System` error); the admin UI surfaces the
log with a filter on `kind` so operators can separate "my app is misconfigured" from "report
this bug". This is a cross-cutting concern layered on `sc-error`; it is **not required for the
MVP** but the `ErrorKind` split lands with `sc-error` from the start so classification is
never retrofitted.

**Message bus.** One `BusDriver` trait, several drivers: in-process (single node),
Postgres LISTEN/NOTIFY (simple, reuses the primary DB), and redis/kafka (scale-out). The bus
carries cache invalidation, the durable-workflow queue, real-time chat, real-time
collaboration, and server-driven UI pushes.

**Testing (principle 4).** Integration tests run against a **real Postgres** that is
reinitialised before each test. MVP test targets: table creation, field creation,
initialising the catalog against existing tables, row CRUD, user create/login/logout.

**Target platforms.** Linux, macOS, Windows, FreeBSD — constrains dependency choices,
especially the cross-platform xattr library and the native code adapters.

**Runtime and core dependencies.** The workspace is a single Cargo workspace on Rust
**edition 2024** with an MSRV of **1.85**. The async runtime is **tokio** (multi-threaded);
every async trait in this document is expressed with `async_trait` over it, and the
`saltcorn` binary's entry point is `#[tokio::main]`. The MVP Postgres driver is
**tokio-postgres** with **deadpool-postgres** for pooling — deliberately **not sqlx**:
`sc-query` already renders a `Statement` into `(sql, binds)` (§3), so sqlx's compile-time
query macros would add no value, whereas tokio-postgres offers native `$n` parameter binding
and row streaming that map directly onto the `RowStream` returned by `DatabaseDriver::query`
(§4). Core third-party dependencies are pinned once in the root `[workspace.dependencies]`
and inherited by member crates (`dep.workspace = true`): `tokio`, `async-trait`,
`serde`/`serde_json`, `uuid`, `argon2` (argon2id password hashing; pinned to the stable 0.5
line), `tokio-postgres` (with the `uuid`/`chrono`/`serde_json` `ToSql`/`FromSql` features),
`deadpool-postgres`, and `chrono`/`rust_decimal` (temporal and decimal backing for the
`Value` enum). This keeps versions coherent and the dependency graph minimal and acyclic
(principle 7). Formatting and linting are gated in CI (`cargo fmt --check`, `cargo clippy
--all-targets -D warnings`); tests run against a real Postgres.

The **HTTP server framework is axum** (on hyper + tower), pinned alongside `axum-extra`,
`tower`, `tower-http`, and `matchit`. axum is chosen over actix-web (which brings its own
`actix-rt` runtime) and Rocket (macro-driven routing) because it is maintained by the tokio
team, runs directly on the already-selected tokio/hyper stack with no second runtime, exposes
handlers as plain async fns, and is unopinionated enough to carry the reified `Endpoint`
registry (§13.1) rather than fighting a built-in router. The tower ecosystem provides exactly
the middleware this design calls for: `tower-http`'s `set-header` for strict CSP/security
headers and `ServeDir` for serving the built `ui/admin` SPA bundle. Crucially, **routes not
known at compile time** (§13.1: application and custom user routes) are matched with
`matchit` — axum's own path-router crate — used directly to dispatch the runtime `Endpoint`
set, while the compile-time-known admin API mounts through the same machinery. Session cookies
use `axum-extra`'s cookie jar; the session store stays in `sc-auth`. Streaming responses map
onto the same `futures::Stream` used for `RowStream`. **TLS is terminated in-process with
rustls** (no OpenSSL), fed by either ACME-provisioned or admin-supplied certificates, and the
server emits a systemd `sd_notify` readiness signal without any `libsystemd-dev` build
dependency — both detailed in §13.5.

**Security posture.** Strict CSP everywhere, satisfied structurally by the React UI bundles
(no inline handlers) rather than a server markup model; framework-level XSS escaping in the
React layer; structural SQL-injection safety in `sc-query`; per-CRUD authorization enforced
at the query layer or via RLS; passwords argon2id; optional OAuth2 IdP; new-device detection.

---

## 17. MVP scope (mapping the milestone to this design)

The MVP milestone from GOALS, expressed in the crates above. The admin UI is a **React +
TypeScript SPA** served by `sc-server` over a **typed JSON API**; there is no server-rendered
admin HTML (the earlier "web 1.0 admin" and `sc-markup` plan are dropped, §12).

| MVP requirement | Crates involved |
|---|---|
| Enum `Statement` for select/insert/update/delete | `sc-query` |
| Postgres driver (host/user/pass/db); run queries | `sc-db`, `sc-db-postgres` |
| Catalog initialised from a driver; introspect via information_schema; get/create table & field; **no stored metadata beyond information_schema and `_sc_applications`** *(true for the MVP; since superseded — see the note below)* | `sc-catalog` |
| Types: all **basic**, no rich types | `sc-types` |
| Users: create-first-user flow; login/logout | `sc-auth`, `sc-server` |
| Endpoint model (typed Rust values) + generated TypeScript API client | `sc-api` |
| Admin UI: typed JSON API + served React/TS SPA | `sc-server`, `sc-api`, `ui/admin` |
| CLI to run the server | `sc-cli` |
| File store connect + basic file manager + edit files | `sc-files`, `sc-server` |
| React app served entirely from the Saltcorn process, no DB access, living in a git-repo file store, with a build step | `sc-app`, `ui/` build path, `sc-server` |
| API to serve the React app; auth from the React app | `sc-api`, `sc-auth` |
| **Applications created and configured in the admin UI**; stored in `_sc_applications`; built and mounted with no process restart | `sc-app`, `sc-api`, `sc-server`, `ui/admin` |
| Tests against a real Postgres, reinitialised per test | `tests/` |

MVP explicitly excluded: multiple databases, rich types, stored table/field metadata
overlay, workflows, agents, models, and the drag-and-drop builder. The system is "useful" at
the end of the MVP.

Note the one deliberate exception the MVP made to "no stored metadata": `_sc_applications` was
in scope because an application has no other definition (§9), while the `_sc_tables`/`_sc_fields`
overlays stayed out because tables and fields work without them. "No stored metadata beyond
information_schema" was always a statement about *overlays*, not a ban on the `_sc_*` tables
whose subjects exist nowhere else.

**Since superseded.** Three post-MVP milestones later, the catalog's stored metadata is:
`_sc_applications`, `_sc_file_stores` and `_sc_triggers` (definitions — their subjects exist
nowhere else), `_sc_roles` (the authoritative role list, §7.4), and the
`_sc_tables`/`_sc_fields` **overlays**
(§9.1), which is what replaced the "information_schema only" invariant. What the invariant was
*for* — the zero-setup promise, "point Saltcorn at a legacy database and it just works" — still
holds, and the merge rule is what carries it now: a table or field with **no overlay row** comes
out of the catalog exactly as introspection built it, so a newly connected database still needs
zero metadata rows. The promise was always "zero rows *required*"; it stayed true when rows
became *possible*.

---

## 18. Open questions (from GOALS, unresolved)

These are deliberately not settled here; they need prototyping or a product decision:

1. **Mixing frameworks.** Can a single application mix Saltcorn-v1 views/pages with code
   pages? The design leaves room (`Application.extra_frameworks`) but the default stance is
   one primary framework per app. *Decision pending.*
2. **Email generation.** How v2 renders and sends email (v1 used MJML + nodemailer/Graph).
   Likely an `Action` plus a transport abstraction, but the renderer story under the new CSP
   markup model needs design.
3. **Auth features** beyond new-device recognition (step-up auth, passkeys, etc.).
4. **Formula language for table auth — RESOLVED: JavaScript.** The choice between JavaScript
   and CEL for ownership/ACL formulas is settled, and §7.3 is built on it. JavaScript wins
   because it is evaluated *two ways from one parse* — reified in a real V8 isolate and
   symbolically translated to a SQL predicate — and the reified engine being V8 means the
   parser must accept exactly what V8 accepts; one language, parsed once (via swc, Deno's
   parser family), beats maintaining two grammars. It also aligns with v1 compatibility,
   calculated fields and the future JS code adapter (§15), which all want JavaScript anyway.
   CEL's sandboxing advantage is answered by bounding the language at lowering time (a single
   pure expression; assignment, `new`, `this`, function/class expressions and a long list of
   constructs refused by name) and running the reified path in a no-extensions, no-ops
   `deno_core` isolate with a watchdog. **The question is closed, not deferred.**
5. **Server-rendered v1 views under strict CSP.** With the `sc-markup` symbolic-HTML model
   dropped (§12), how the Saltcorn-v1 view/page experience renders CSP-safe HTML — server
   templates with externalised JS, or React-rendered views driven by the builder — is an open
   design question (post-MVP).

---

*Next steps: stand up the `sc-error` → `sc-query` → `sc-db-postgres` → `sc-catalog` →
`sc-auth` → `sc-server` → `sc-cli` spine with the integration-test harness, then build the
MVP feature list in §17 against it.*
