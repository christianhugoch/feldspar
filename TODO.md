# Saltcorn v2 — MVP Implementation TODO

Ordered, checkable task list for the MVP milestone. Scope and rationale are in
[docs/GOALS.md](./docs/GOALS.md) (§ Milestones) and [docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md) (§17).

**MVP definition of done:** a single Postgres database, connected as both primary and only
data store. An admin can create the first user, log in/out, create tables and fields, edit
rows, and manage users. A file store can be connected with a basic file manager. One React
app (no DB access, living in a git-repo file store, with a build step) is served entirely
from the Saltcorn process and authenticates against the API. Everything else is web 1.0
(server-rendered HTML, minimal client JS). All of it is covered by integration tests against
a real Postgres reinitialised per test.

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

- [ ] `users` table bootstrap: UUID PK (not deletable), `role` (1–100), argon2id password hash, initial `email` field
- [ ] `User` struct (id, role, extra fields map)
- [ ] Create-first-user flow (when no user exists, login redirects here)
- [ ] Login / logout; session cookie; auth middleware
- [ ] Password hashing (argon2id) + verification
- [ ] Role gate: admin-only login for the admin UI (per MVP)
- [ ] Integration tests: create-first-user, login success/failure, logout, session expiry

## Phase 6 — Server: admin UI, web 1.0 (`sc-server`)

- [ ] HTTP server bootstrap (axum/actix); config from CLI; graceful shutdown
- [ ] Session + CSRF middleware; strict-ish CSP headers
- [ ] Server-rendered HTML for admin (minimal markup helper; full `sc-markup` symbolic tree is post-MVP)
- [ ] Routes: first-user, login, logout
- [ ] Routes: list tables, create table, show table's fields, create field
- [ ] Routes: edit rows (list / create / edit / delete a row in a table)
- [ ] Routes: list users, create user
- [ ] Integration tests: drive each route end-to-end against a real DB

## Phase 7 — Files (`sc-files`)

- [ ] `FileStore` trait (read, write, list, is_git_repo, get/set xattr meta)
- [ ] Local-directory driver
- [ ] Cross-platform xattr access (Linux/macOS/Windows/FreeBSD) for per-file metadata
- [ ] Connect a file store to the catalog (named)
- [ ] Server routes: basic file manager (browse, upload, download, edit a text file)
- [ ] Integration tests: connect store, list/read/write, round-trip xattr metadata

## Phase 8 — Applications & API for the React app (`sc-app`, `sc-api`)

- [ ] `Application` model (name, subdomain, framework, tables subset, file stores, apis, csp)
- [ ] `Framework` trait; code-framework implementation that serves bundled static assets
- [ ] React app lives in a git-repo file store; **build step** wired (invoke bundler, output served by `sc-server`)
- [ ] `ApiProvider` trait; minimal REST provider (the app's data/auth surface for MVP)
- [ ] API auth: React app authenticates against `sc-auth` (token/session); authz honored
- [ ] Serve the built React app from the Saltcorn process (no direct DB access from the app)
- [ ] Integration tests: build serves assets; API auth round-trip; unauthorized request rejected

## Phase 9 — CLI (`sc-cli`)

- [ ] `saltcorn serve` — run the server (db connection args, port, admin URL/subdomain)
- [ ] DB connection config (host/user/pass/db) surfaced as flags/env
- [ ] Helpful startup errors (no silent failures) when DB unreachable / misconfigured
- [ ] Smoke test: `serve` boots against a test DB and answers a health route

## Phase 10 — MVP hardening & acceptance

- [ ] End-to-end acceptance test walking the full MVP DoD user story
- [ ] Confirm every route/action has integration coverage; Postgres reset-per-test verified
- [ ] Manual pass: create-first-user → create table → add fields → edit rows → create user → login/out → connect file store → serve React app
- [ ] README quickstart (build, run, connect DB, open admin)
- [ ] Tag MVP

---

## Explicitly OUT of MVP scope

Tracked so they aren't accidentally pulled in early:

- Multiple / non-primary databases
- Rich types & full `FieldView` trait; symbolic CSP markup tree (`sc-markup`) + JS extraction
- Stored `_sc_tables` / `_sc_fields` overlay metadata
- Workflows & durable engine (`sc-workflow`), triggers, actions registry
- Agents / skills / copilot (`sc-agent`, `sc-copilot`)
- Predictive models (`sc-model`)
- Message bus & cross-process cache invalidation (`sc-bus`) — single process for MVP
- Drag-and-drop builder (`ui/builder`), dynamic form runtime, Saltcorn-v1 view patterns
- GraphQL / gRPC / tRPC / MCP API providers (REST only for MVP)
- Code adapters / polyglot plugins (`sc-code`)
- OAuth2 IdP, device recognition, RLS-based authz
