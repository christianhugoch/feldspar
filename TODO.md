# Saltcorn v2 — MVP Implementation TODO

Ordered, checkable task list for the MVP milestone. Scope and rationale are in
[docs/GOALS.md](./docs/GOALS.md) (§ Milestones) and [docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md) (§17).

**MVP definition of done:** a single Postgres database, connected as both primary and only
data store. An admin can create the first user, log in/out, create tables and fields, edit
rows, and manage users — through a **React + TypeScript admin SPA** (`ui/admin`) served by the
Saltcorn process over a **typed JSON API**. A file store can be connected with a basic file
manager. One React app (no DB access, living in a git-repo file store, with a build step) is
**created and configured in the admin UI**, then served entirely from the Saltcorn process and
authenticates against the API. The admin API is a set of **typed endpoint values that also
generate a TypeScript consumer client** — no server-rendered admin HTML (the earlier web-1.0 /
`sc-markup` plan is dropped). All of it is covered by integration tests against a real Postgres
reinitialised per test.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## Phase 0 — Workspace & foundations

- [x] Create Cargo workspace (`Cargo.toml`) with the MVP crates
- [x] `sc-error`: single `Error` enum + `Result<T>` alias; context helpers; lint against `unwrap()`/`expect()` in libs (clippy config)
- [x] Repo hygiene: `rustfmt.toml`, `clippy.toml`, CI skeleton (fmt + clippy + test), `.gitignore` for Rust/node
- [x] Decide async runtime (tokio) and pin core deps (serde, uuid, argon2, sqlx-or-tokio-postgres)
- [x] Integration-test harness: spin up / reset a real Postgres per test (testcontainers or a reset fixture); shared helper crate under `tests/`

## Phase 1 — Query language (`sc-query`)

- [x] `Value` enum (Null/Bool/Int/Float/Text/Bytes/Json/Uuid/Date/Time/Timestamp/Decimal)
- [x] `Statement` AST: `Select` (from, columns, joins, filter, group, having, order, limit, offset), `Insert`, `Update`, `Delete`
- [x] `Expr` (Col, Lit, Param, Binary, Unary, Func, In, Json, Case) — enough for MVP CRUD + joins
- [x] `SqlDialect` trait: render `Statement` → (sql, binds); literals **always parameterised**
- [x] Unit tests: render each statement kind; verify parameterisation (no literal interpolation)

## Phase 2 — Database driver (`sc-db`, `sc-db-postgres`)

- [x] `DatabaseDriver` trait (introspect, query, apply_schema, begin/tx, capabilities, dialect)
- [x] `DbCapabilities` (row_level_security, composite_pk, listen_notify, returning, …)
- [x] Postgres `SqlDialect` implementation (quoting, `$n` placeholders, JSON operators, RETURNING)
- [x] Postgres connection + pooling; run `Statement` → `RowStream`
- [x] `introspect()` via `information_schema` → `PhysicalTable` (columns, types, PKs incl. composite, FKs)
- [x] `apply_schema`: create/drop table, add/drop column — **no auto `id` column** created
- [x] Transactions: `begin()` → commit/rollback; used by metadata mutations
- [x] Integration tests: create table, add fields, introspect existing tables, CRUD rows

## Phase 3 — Types (`sc-types`, MVP: basic only)

- [x] `Value`↔Postgres type mapping (basic types only; no rich types this milestone)
- [x] `TypeRef` / basic-type representation used by fields
- [x] Catch-all display/edit path (defer real `FieldView` trait richness to post-MVP)

## Phase 4 — Catalog (`sc-catalog`)

- [x] `BaseField` / `DataField` (name, label, type, attrs, required, unique, primary_key, kind)
- [x] `DataFieldKind`: Plain, Key (target table/field/summary), File (store/folder/mime) — Key/File may be stubbed for MVP UI but modeled now
- [x] `Table` (id, name, database, provider, fields, access, attributes)
- [x] `Catalog` initialised from a `DatabaseDriver`; **no stored metadata beyond information_schema** for MVP
- [x] Cache of tables/fields; get / create-table / create-field methods
- [x] `TableProvider` trait defined; trivial driver-backed provider implemented
- [x] Integration tests: init catalog from existing DB; create table + fields via catalog; reflect in introspection

## Phase 5 — Users & auth (`sc-auth`)

- [x] `users` table bootstrap: UUID PK (not deletable), `role` (1–100), argon2id password hash, initial `email` field
- [x] `User` struct (id, role, extra fields map)
- [x] Create-first-user flow (when no user exists, login redirects here)
- [x] Login / logout; session cookie; auth middleware — auth/session logic in `sc-auth`; HTTP cookie + middleware wiring is Phase 6 (`sc-server`)
- [x] Password hashing (argon2id) + verification
- [x] Role gate: admin-only login for the admin UI (per MVP)
- [x] Integration tests: create-first-user, login success/failure, logout, session expiry

## Phase 6 — Server: typed API + React admin SPA (`sc-server`, `sc-api`, `ui/admin`)

Reflects the GOALS pivot (design §12–§13): the admin UI is a React + TypeScript SPA over a
**typed JSON API**, not server-rendered "web 1.0" HTML. The endpoint model and TypeScript
consumer generation live in `sc-api`; `sc-server` mounts it and serves the built `ui/admin`
bundle.

**Reconcile existing code with the pivot first:**

- [x] Do **not** create an `sc-markup` crate — it is dropped from the design (§12); remove it from any planning notes
- [x] Update `sc-types` `src/catchall.rs` doc comments: they describe a `FieldView` "symbolic markup tree" that is no longer planned (fieldviews are now React components, §6.3)
- [x] Confirm the `sc-server` / `sc-api` / `sc-app` stub crates (currently ~20-line placeholders) carry no server-HTML / `sc-markup` assumptions before building on them

**Endpoint model & typed client (`sc-api`):**

- [x] `Endpoint` value (method, typed path/query params, `TypeSchema` input, `TypeSchema` output, `auth` requirement, handler ref) — design §13.1
- [x] `TypeSchema` enum sufficient to describe args/results and emit TS (Value types, struct, array, optional)
- [x] Register endpoints as runtime values (routes need not be known at compile time); admin API expressed as fixed `Endpoint` constants through the **same** machinery
- [x] TypeScript generator: emit type declarations + a typed API-consumer client from the `Endpoint` set
- [x] Tests: generated TS type-checks against the declared endpoints

**Server (`sc-server`):**

- [x] HTTP server bootstrap (**axum** on hyper/tower; design §16); config from CLI; graceful shutdown (tokio `signal`)
- [x] Mount the `sc-api` endpoint set as JSON routes; runtime/dynamic routes dispatched via `matchit`; auth enforced per `Endpoint.auth`
- [x] Session (via `axum-extra` cookie jar; store in `sc-auth`) + CSRF handling for the SPA; strict CSP headers via `tower-http` `set-header` (no `unsafe-inline`; bundle-only)
- [x] Serve the built `ui/admin` bundle via `tower-http` `ServeDir` + a minimal bootstrap document (no server-rendered admin HTML)
- [x] API endpoints: first-user, login, logout
- [x] API endpoints: list tables, create table, list a table's fields, create field
- [x] API endpoints: rows CRUD (list / create / edit / delete a row in a table)
- [x] API endpoints: list users, create user
- [x] Integration tests: drive each endpoint end-to-end against a real DB

**Admin SPA (`ui/admin`):**

- [x] Scaffold React + TypeScript + react-bootstrap SPA consuming the generated typed client
- [x] Screens: create-first-user, login/logout, tables list, table fields, row editor, users
- [x] Wire the `ui/admin` build into the server build so the bundle is served by `sc-server`

## Phase 7 — CLI (`sc-cli`)

- [x] `saltcorn serve` — run the server (db connection args, port, admin URL/subdomain)
- [x] DB connection config (host/user/pass/db) surfaced as flags/env
- [x] Helpful startup errors (no silent failures) when DB unreachable / misconfigured
- [x] Smoke test: `serve` boots against a test DB and answers a health route

## Phase 8 — Files (`sc-files`)

- [x] `FileStore` trait (read, write, list, is_git_repo, get/set xattr meta)
- [x] Local-directory driver
- [x] Cross-platform xattr access (Linux/macOS/Windows/FreeBSD) for per-file metadata
- [x] Connect a file store to the catalog (named)
- [x] Server routes: basic file manager (browse, upload, download, edit a text file)
- [x] Integration tests: connect store, list/read/write, round-trip xattr metadata

## Phase 9 — Applications & API for the React app (`sc-app`, `sc-api`)

- [x] `Application` model (name, subdomain, framework, tables subset, file stores, apis, csp)
- [x] `Framework` trait; code-framework implementation that serves bundled static assets
- [x] React app lives in a git-repo file store; **build step** wired (invoke bundler, output served by `sc-server`)
- [x] `ApiProvider` trait; minimal REST provider projecting the app's `Endpoint` set (design §13.1/§13.4) — tables + actions + custom code/SQL routes (custom routes MAY be stubbed for MVP)
- [x] Generate the app's TypeScript API-consumer client from its `Endpoint` set (same machinery as the admin API in Phase 6)
- [x] API auth: React app authenticates against `sc-auth` (token/session); authz honored
- [x] Serve the built React app from the Saltcorn process (no direct DB access from the app)
- [x] Integration tests: build serves assets; API auth round-trip; unauthorized request rejected


## Phase 10 — Applications in the admin UI (`_sc_applications`, `ui/admin`)

Phase 9 made an application *servable*, but the only way to define one is to embed
`sc-server` in a bespoke Rust binary and hand `build_router_with_apps` an `Application`
value — `saltcorn serve` mounts the admin only, which is why `--base-domain` currently does
nothing. GOALS is explicit that **applications are created in the admin UI** (§ "Applications
can be created in the admin UI"), so this phase closes the configuration path from database
to mounted app. Design: §13.2 (storage, lifecycle, no-restart mounting), §13.3
(`Framework::config_spec`), §9 (`_sc_applications`).

Two decisions worth reading before starting, both in §13.2:

- `_sc_applications` is **not an overlay** — an app has nothing to introspect, so its row is
  its only definition. This is the one stored-metadata table the MVP needs; the
  `_sc_tables`/`_sc_fields` overlays stay out of scope.
- Mounting is a **runtime** operation. Creating an app in the admin UI must build and mount
  it live: "full restart should never be required" (GOALS).

**Storage (`sc-app`):**

- [x] `_sc_applications` table + bootstrap (like `sc_auth::bootstrap`): §9 required columns (UUID `id`, `name`, `description`, `attributes`) plus subdomain, framework + config, table/store subsets, apis, static dirs, csp
- [x] `Application` gains `description`, `attributes`, `static_dirs: Vec<StaticDir>`; `AppId` becomes a UUID (it is a stored row now) and `subdomain` becomes the unique routing key
- [x] `FrameworkRef.config` becomes `Attrs` (JSON) rather than a string map, so it can hold what a framework's `config_spec` describes
- [x] Load/save/delete: `_sc_applications` row ⇄ `Application`; unique subdomain enforced in the database, not only in `AppMounts`
- [x] Integration tests: round-trip an app through the table; a duplicate subdomain is rejected; a legacy database with no `_sc_applications` bootstraps cleanly

**Framework settings as data (`sc-types`, `sc-app`):**

- [x] Settings-as-data vocabulary in `sc-types` — the design leans on it for fieldview attributes (§6.1), action config (§10.1), agents (§11.1) and models (§14.2). **Resolved: no `AttrSpec`; settings are declared as `FormField`s** (§6.2), which already covers name/label/type/required/default/options and is the standalone half of the `DataField`/`FormField` split. One vocabulary, one form, one type for the admin UI to render
- [x] Decide where `Attrs` lives: it was a `serde_json::Map` alias in `sc-catalog` (layer 4), but `sc-types` is layer 3 and cannot depend on it. Moved down to `sc-types` (re-exported from `sc-catalog`, so no call site changes) — it is the same underlying type either way. **`BaseField` moved down with it**, since `FormField` carries one; `DataField` stays in `sc-catalog`, where its `Key`/`File` kinds and `Column`/`ColumnDef` bridge need it
- [x] `Framework::config_spec() -> Vec<FormField>`; `CodeFramework` declares its own via `code_config_spec` (`store`, `source`, `output`, `command`, optional `client`); the trait method delegates so instance and free function cannot drift. **Resolved the open question**: `config_spec` takes no `&Catalog`, so the `store` setting is free-text (validated as text, resolved against the catalog at build time) rather than a pick-list of connected stores — §6.2's `OptionsSource::ServerQuery` is the post-MVP answer, documented in §13.3
- [x] Validate `FrameworkRef.config` against the spec **on save** (`validate_framework_config`, called from `save_application` for the primary and every extra framework), so a misconfigured app is rejected where the admin can fix it, not at build or serve time
- [x] Resolve an `AppSource`/`BuildSpec` *from* a stored app's framework config (`app_source_from_config`) — the Phase 9 tests (`build_app`, `app_client`, server `app_serving`) now state the build settings only in the framework config and resolve from it; no test hand-builds a `BuildSpec`
- [x] Tests: `sc-types` `FormField::validate`/`validate_attrs` and `BasicType::accepts_json`; `sc-app` resolver tests (valid config → build spec, missing/ill-typed/unknown setting → `Invalid` naming it, command split, source default); framework spec tests; integration test that a bad config is rejected on save

**Live mounting (`sc-server`):**

- [x] `AppMounts` becomes live shared state (mount/unmount/replace one app at a time) instead of a value frozen at `build_router_with_apps`
- [x] Boot: load every row of `_sc_applications` and mount each; a single app that fails to build must not stop the server or the other apps
- [x] Build + mount an app at runtime, with **no process restart**; a failed rebuild leaves the previously mounted version serving
- [x] `saltcorn serve` honours `--base-domain` for real: remove the README's "no effect yet" caveat once it does
- [x] Integration tests: create an app at runtime → it serves on its subdomain without a restart; edit → re-mount; delete → subdomain stops resolving; a failing build keeps the old bundle up and reports the bundler's diagnostics. (Driven through the live `AppMounts`/`build_and_mount` API, not a REST endpoint — the create/build HTTP endpoints are the next "Admin API" subphase)

**Admin API (`sc-api`, `sc-server`):**

- [x] Endpoints: list/create/update/delete applications; build (+ mount) an application; report build status/log (build success returns `{built, git_repo, log}`; a failed build is the §16 error path below, carrying the diagnostics)
- [x] Endpoint: list registered frameworks with their `config_spec`, so the UI can render a settings form for a framework it knows nothing about (`listFrameworks`, backed by `sc_app::registered_frameworks`)
- [x] Build errors are **Application errors**, not System errors (§16): the bundler's own output reaches the admin. Added `ErrorKind`/`Error::kind()` to `sc-error` (the §16 split, landed with `sc-error` as the design requires) and mapped unhandled Application errors to `422` (vs System→`500`) in the router
- [x] Integration tests: drive each endpoint end-to-end; non-admins are rejected (`crates/sc-server/tests/admin_applications_api.rs`)

**Admin SPA (`ui/admin`):**

- [ ] Applications screen: list, create, edit, delete
- [ ] Create/edit form: pick a framework → render its `config_spec` settings; subdomain; table + file-store subsets; APIs (provider + mount); static dirs; CSP. `ui/form-runtime` is out of MVP scope, so render the `FormField` spec with a plain form — the point is only that no screen knows a specific framework's settings
- [ ] Build button with its outcome surfaced (success, or the bundler's diagnostics), and a visible saved-but-unbuilt state
- [ ] A link to the app's own subdomain from the applications list

## Phase 11 — MVP hardening & acceptance

- [ ] End-to-end acceptance test walking the full MVP DoD user story
- [ ] Confirm every route/action has integration coverage; Postgres reset-per-test verified
- [ ] Manual pass: create-first-user → create table → add fields → edit rows → create user → login/out → connect file store → create a React app in the admin UI → serve it
- [x] README quickstart (build, run, connect DB, open admin)
- [ ] Tutorial: building a React app against an application's API (write once Phase 10 lands, so it can be admin-UI-first with no Rust)
- [ ] Tag MVP

---

## Explicitly OUT of MVP scope

Tracked so they aren't accidentally pulled in early:

- Multiple / non-primary databases
- Rich types & full `FieldView` trait (React-component fieldviews) — basic types only for MVP
- Stored `_sc_tables` / `_sc_fields` overlay metadata — the *overlays* only. `_sc_applications` **is** in scope (Phase 10): a table exists without a metadata row, an application does not exist without one (design §9/§13.2)
- Workflows & durable engine (`sc-workflow`), triggers, actions registry
- Agents / skills / copilot (`sc-agent`, `sc-copilot`)
- Predictive models (`sc-model`)
- Message bus & cross-process cache invalidation (`sc-bus`) — single process for MVP
- Drag-and-drop builder (`ui/builder`), dynamic form runtime, Saltcorn-v1 view patterns
- GraphQL / gRPC / tRPC / MCP API providers (REST only for MVP)
- Code adapters / polyglot plugins (`sc-code`)
- OAuth2 IdP, device recognition, RLS-based authz

**Dropped from the design entirely (not merely deferred):** the `sc-markup` symbolic-HTML /
CSP tree + JS-extraction crate. The admin UI is a React + TypeScript SPA and CSP is satisfied
structurally by the bundle; there is no server-side symbolic-HTML model. How Saltcorn-v1 views
render CSP-safe HTML post-MVP is an open design question (design §18.5).
