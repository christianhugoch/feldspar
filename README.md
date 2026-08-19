# Saltcorn v2

A ground-up rewrite of Saltcorn in Rust. This repository currently implements the
**MVP milestone**: a single Postgres database, a create-first-user / login flow,
a table and field editor, a row editor, and user management — all driven from a
**React + TypeScript admin SPA** served by the `saltcorn` process over a typed
JSON API.

This README is for **operators**: how to install the prerequisites, set up the
database, build the server, and run it. (Design and planning docs live under
[`docs/`](docs/); the roadmap is in [`TODO.md`](TODO.md).)

---

## 1. What you get

- A single binary, `saltcorn`, that serves the admin UI and its API.
- One Postgres database is the source of truth. The server **introspects** whatever
  tables already exist and, on first start, creates a `users` table if none is
  present. It does **not** create the database itself — you do that once (below).
- An admin logs in through the SPA, creates tables and fields, edits rows, and
  manages users.

The MVP is deliberately scoped: one database, basic column types only, no
workflows/agents/models, no file stores or applications yet. See the "Out of MVP
scope" section of [`TODO.md`](TODO.md).

---

## 2. Prerequisites

| Component | Version | Needed for |
|---|---|---|
| **Rust** (with `cargo`) | 1.85+ (edition 2024) | building the `saltcorn` binary |
| **PostgreSQL** | 13 or newer (16 recommended) | the primary data store |
| **Node.js + npm** | Node 18+ | *only* to build the admin UI bundle (optional; see §5) |

Install Rust via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustc --version   # must be >= 1.85
```

Node is required **only** if you want the admin web UI. Without it the server
still runs and its JSON API works, but the browser UI will be a blank bootstrap
page (see §5 and §9 Troubleshooting).

---

## 3. Get the code and do a first build

```bash
git clone <this-repo-url> saltcorn
cd saltcorn

# Compile the server binary (without the web UI bundle for now — see §5).
cargo build --release -p sc-cli
```

The binary is produced at `target/release/saltcorn` (or `target/debug/saltcorn`
for a plain `cargo build`). You can also run it through cargo with
`cargo run --release -p sc-cli -- <args>`.

---

## 4. Database setup

Saltcorn needs one Postgres database and a role that **owns** it (so it can create
the `users` table and any tables you add from the admin UI).

### Option A — local PostgreSQL

Assuming a local Postgres you can administer (as the `postgres` superuser):

```bash
# Create a dedicated role and database. Choose your own password.
sudo -u postgres psql <<'SQL'
CREATE ROLE saltcorn WITH LOGIN PASSWORD 'change-me';
CREATE DATABASE saltcorn OWNER saltcorn;
SQL
```

Verify you can connect as that role:

```bash
psql "postgres://saltcorn:change-me@localhost:5432/saltcorn" -c '\conninfo'
```

### Option B — Docker

```bash
docker run --name saltcorn-db \
  -e POSTGRES_USER=saltcorn \
  -e POSTGRES_PASSWORD=change-me \
  -e POSTGRES_DB=saltcorn \
  -p 5432:5432 \
  -d postgres:16
```

That gives you `postgres://saltcorn:change-me@localhost:5432/saltcorn`.

### Notes

- **The server never creates or drops the database.** Create it once as above.
- The connecting role must be able to `CREATE TABLE` in the database (owning it is
  the simplest way). On startup the server creates a `users` table if one is not
  already present.
- Any tables that already exist in the database are picked up automatically — no
  import or registration step.

---

## 5. Building the admin web UI

The admin SPA lives in [`ui/admin`](ui/admin) and is compiled to a static bundle
that the server serves. **`cargo build` builds it for you**: `sc-cli`'s build
script runs `npm ci && npm run build` in `ui/admin` (and in [`ui/ide`](ui/ide),
below), embeds the resulting paths in the binary, and `saltcorn serve` then serves
the UI with no `--static-dir` needed:

```bash
cargo build --release -p sc-cli     # builds the Rust binary *and* both front ends
target/release/saltcorn serve ...   # serves the admin UI, no flags
```

The price is that a build needs a Node toolchain and takes as long as `npm ci` does.

### Turning the UI build off

Set **`SC_BUILD_ADMIN`** to `0`, `false`, `False` or `FALSE` on the **build** and
the script is skipped, leaving a Rust-only build that needs no JS toolchain — what
CI's clippy and test jobs do, and what you want on a machine without npm:

```bash
SC_BUILD_ADMIN=0 cargo build --release -p sc-cli   # Rust only, no bundles
```

Any other value (including `1` and `true`) builds the UI, as does leaving it unset.

> **`SC_BUILD_ADMIN` is a _build-time_ variable, read by `cargo build` — not by
> `saltcorn serve`.** Putting it on the run command has **no effect** either way:
> a binary built without the bundle stays without it, and the browser gets a blank
> page (see §9).

Such a binary still serves the whole JSON API; only the browser UI is missing, and
you can supply it at run time instead — build the SPA yourself and point the server
at the output with `--static-dir`:

```bash
cd ui/admin && npm ci && npm run build   # outputs ui/admin/dist (index.html + hashed assets/)
cd ../..

target/release/saltcorn serve --static-dir ui/admin/dist ...   # see §6
```

That is also the quickest loop when you are *working on* the UI: what you serve is
exactly the `dist` you point at, and rebuilding it does not rebuild the Rust binary.
The bundle's entry points carry a content hash in their names, and the server serves
the bundle's own `index.html` (including for client-routed deep links), so a rebuild
is picked up by a plain reload — no cache to disable. With neither the embedded
bundle nor `--static-dir`, the browser shows a document saying the admin UI is not
built. See §9 Troubleshooting.

### The file-store IDE (`ui/ide`)

A second bundle built alongside the SPA: the **VS Code workbench**, for editing a file
store that holds an application's source — a project tree, editor tabs, the command
palette (design §12.1). It is served at `/ide/?store=<name>`, is **admin-only**, and is
reached from an "Edit code" button in the file store list, the file manager, and an
application's row.

**There is nothing to configure.** The default build builds it along with the SPA and the
binary serves both; a binary built with `SC_BUILD_ADMIN=0` finds `ui/ide/dist` in the
checkout it was compiled from, so a development server serves the IDE with no flag
either. The one thing it needs is to have been built:

```bash
cd ui/ide && npm ci && npm run build     # only if you built with SC_BUILD_ADMIN=0
```

It is a separate page rather than a screen in the admin SPA because VS Code initializes
once per page and owns the whole viewport.

**TypeScript errors, completions and go-to-definition** come from a
`typescript-language-server` the server starts in the store's own directory, so they need
two things that are the *project's*, not the IDE's: an on-disk store (an object store can
be edited and formatted, but not type-checked), and dependencies installed — which the
first Build does. The tool itself is found in the project's `node_modules/.bin` or on the
server's `PATH`:

```bash
npm install -g typescript-language-server    # or add it to the project's devDependencies
```

Without it — or without either of the other two — the IDE says so once, in a notification,
and everything else about the workbench goes on working.

---

## 6. Running the server

The main command is `saltcorn serve`. It takes **database flags** and **server
flags**; database settings may also come from environment variables or from a
configuration file. (The other command is `saltcorn build-app`, below.)

### Database connection

Provide **either** a full connection URL **or** the individual parts. A URL, when
present, wins. Anything a flag does not set is taken from the environment, and
anything the environment does not set is taken from the configuration file below.

| Flag | Environment fallback | Config file key | Default |
|---|---|---|---|
| `--database-url <url>` | `DATABASE_URL` | `url` | — |
| `--db-host <host>` | `PGHOST` | `host` | `localhost` |
| `--db-port <port>` | `PGPORT` | `port` | `5432` |
| `--db-user <user>` | `PGUSER` | `user` | (libpq default) |
| `--db-password <pw>` | `PGPASSWORD` | `password` | — |
| `--db-name <name>` | `PGDATABASE` | `database` | (libpq default) |

### Environments, and the configuration file

A deployment usually has more than one database — production, staging, test — so
their connection parameters can live together in a `saltcorn.toml`, one section
each, and the command line picks one:

```toml
default_environment = "production"

[environments.production]
host = "db.internal"
port = 5432
user = "saltcorn"
password = "change-me"
database = "saltcorn"

[environments.staging]
url = "postgres://saltcorn:change-me@staging.internal:5432/saltcorn"

[environments.test]
database = "saltcorn_test"
test_template = "sc_template"   # read by `cargo test`, not by the server
```

```bash
saltcorn serve                          # the file's default_environment
saltcorn serve --environment staging    # or --env staging, or SALTCORN_ENV=staging
saltcorn serve --environment test
```

`environments` is an ordinary table: define as many as you have databases, named
whatever you like. With no `default_environment` and nothing named on the command
line, the environment used is `production`. The `test` environment has one extra
reader: the integration-test harness takes its connection (and `test_template`)
from there, so a developer machine needs no test-specific environment variables —
see "For developers" below.

| Flag | Environment fallback | Meaning |
|---|---|---|
| `--environment <name>` | `SALTCORN_ENV` | which `[environments.<name>]` section to connect with |
| `--config <path>` | `SALTCORN_CONFIG` | read this file instead of searching for one |

Without `--config`, the file is looked for in the platform's configuration
directories, user first:

| | user | system |
|---|---|---|
| Linux/BSD | `$XDG_CONFIG_HOME/saltcorn/saltcorn.toml` (else `~/.config/saltcorn/saltcorn.toml`) | `/etc/saltcorn/saltcorn.toml` |
| macOS | `~/Library/Application Support/saltcorn/saltcorn.toml` | `/etc/saltcorn/saltcorn.toml` |
| Windows | `%APPDATA%\saltcorn\saltcorn.toml` | `%PROGRAMDATA%\saltcorn\saltcorn.toml` |

No file at all is fine — that is the environment-variable deployment. But a file
that does not parse, a key that is not recognised, a `--config` path that does not
exist, or an `--environment` the file does not define are all startup errors, not
things stepped over: the alternative is connecting to a database you did not mean.
The file holds passwords, so keep it `chmod 600` — the server warns on stderr if
other users can read it.

> **Naming an environment outranks `DATABASE_URL` and `PG*`.** Normally the
> environment wins and the file fills in what it leaves unset. But
> `--environment staging` is an instruction: for that run the section is
> authoritative and the ambient variables are ignored entirely, so an operator who
> asks for staging on a box where `DATABASE_URL` points at production gets
> staging. Explicit `--db-*` flags still win over everything. `saltcorn serve`
> prints which environment it connected with, from which file.

### Server options

| Flag | Meaning | Default |
|---|---|---|
| `--bind <addr>` | address:port to listen on | `127.0.0.1:3032` |
| `--static-dir <dir>` | directory holding the built admin bundle | (the embedded bundle, unless built with `SC_BUILD_ADMIN=0`) |
| `--session-ttl-hours <n>` | session lifetime | `24` |
| `--secure-cookies` | set the `Secure` attribute on session/CSRF cookies (use behind HTTPS) | off |
| `--base-domain <domain>` | domain that applications are served under: an app with subdomain `blog` is served at `blog.<domain>` | none (app routing off) |
| `--code-workers <n>` | V8 isolates serving `run_js_code` trigger bodies | `2` |
| `--code-max-inflight <n>` | runs each of those isolates keeps resident at once | `256` |

Unknown flags in either group are rejected with a clear error rather than ignored.

> **The two code-pool flags size concurrency, not parallelism.** A code body
> suspended on a database call costs a pending promise rather than a thread, so
> one isolate serves hundreds of bodies at once: `--code-workers` buys CPU
> parallelism (what a body that *computes* wants) and `--code-max-inflight` buys
> occupancy (memory: a resident run holds its scope, its bindings and its last
> read). The defaults serve 512 bodies at once and queue the rest, with the queue
> time counted inside each run's own `timeout_ms`. Past that the ceiling is the
> database connection pool, which is where it belongs.

> **`--base-domain` mounts your applications.** With it set, `saltcorn serve` loads
> every `_sc_applications` row at boot, builds each, and serves it at
> `<subdomain>.<base-domain>`; an app that fails to build is logged and skipped, not
> fatal, and can be fixed and rebuilt without a restart. Without a base domain the
> server has no way to address an app, so it serves the admin only.

### HTTPS

TLS is **not** a flag: it is configured in the admin UI under **Settings → SSL / TLS
certificates**, stored in `_sc_config`, and read at boot. Two sources, and both serve
the admin UI and every application:

| `ssl_mode` | What happens |
|---|---|
| `off` (default) | plain HTTP — right for local development and for a deployment behind a TLS-terminating proxy |
| `letsencrypt` | certificates obtained and renewed from an ACME CA. Give it a contact email; the CA directory URL is a setting, so a staging directory or a private CA is a value in a text box |
| `custom` | paste a PEM certificate chain and private key. Refused on save if they are not valid PEM or do not match each other |

With TLS on, the server binds **two** listeners: the `--bind` address, which answers
plain HTTP, and the `https_port` setting (default 443) beside it. The plain one
redirects to HTTPS by default (`redirect_http_to_https`); switch it off to serve the
application on both. Session cookies become `Secure` automatically.

ACME notes:

- Validation uses the **TLS-ALPN-01** challenge, inside the TLS handshake — so the CA
  must reach `https_port` on a public address (443 for Let's Encrypt), and there is no
  HTTP challenge route to keep clear.
- The certificate covers the base domain, every mounted application's subdomain, and
  anything listed in `ssl_extra_domains`. **Adding an application adds a name at the
  next restart**, which is when the order is built.
- The account key and the issued certificates are cached in the database
  (`_sc_acme_cache`), so a renewal survives a restart and a second node serves what the
  first one ordered instead of ordering its own.
- Try `https://acme-staging-v02.api.letsencrypt.org/directory` first. Its certificates
  are untrusted; its rate limits are not.

Settings that do not serve are refused where they are typed, and a stored setting that
cannot serve **stops the boot** rather than silently falling back to plain HTTP — an
admin who configured TLS and got HTTP would not find out from the server.

### Building one application from the command line

```bash
saltcorn build-app <subdomain> [database flags] [--file-store NAME=PATH]
```

Builds the application served at that subdomain — regenerating its typed client,
installing its dependencies if needed, and running its build command — and prints
the tool output as it goes, failing with the bundler's own diagnostics. It is the
same build the admin UI's **Build** button runs, so reach for it in a deploy
script, in CI, or when a failed build has left the app unreachable in a browser.

It **mounts nothing**: no application is served by this process, so running it
against a live deployment's database cannot disturb what that server is serving.

> **Building your first application?**
> [`docs/tutorial-react-todo.md`](docs/tutorial-react-todo.md) walks through a React
> to-do app end to end, entirely in the browser: the server creates the project,
> generates a typed client and hooks for your tables, installs its dependencies and
> builds it. For a project React's conventions do not fit — another bundler, an
> existing app, Next.js — [`docs/tutorial-code-framework.md`](docs/tutorial-code-framework.md)
> covers the generic `code` framework, where you bring the project and state its paths.

### Examples

Local development, connecting with a URL and serving the pre-built bundle
(Option A from §5 — the simplest path):

```bash
target/release/saltcorn serve \
  --database-url postgres://saltcorn:change-me@localhost:5432/saltcorn \
  --static-dir ui/admin/dist \
  --bind 127.0.0.1:3032
```

Individual DB parts, listening on all interfaces:

```bash
target/release/saltcorn serve \
  --db-host localhost --db-user saltcorn --db-password change-me --db-name saltcorn \
  --static-dir ui/admin/dist \
  --bind 0.0.0.0:8080
```

With the bundle baked into the binary (the default build, §5 — `--static-dir` is
then unnecessary):

```bash
cargo build --release -p sc-cli
target/release/saltcorn serve \
  --database-url postgres://saltcorn:change-me@localhost:5432/saltcorn \
  --bind 127.0.0.1:3032
```

Using environment variables (handy for systemd/containers), behind a TLS proxy:

```bash
export DATABASE_URL=postgres://saltcorn:change-me@localhost:5432/saltcorn
target/release/saltcorn serve --bind 127.0.0.1:3032 --secure-cookies
```

One box, three databases: the parameters in `~/.config/saltcorn/saltcorn.toml`
(or `/etc/saltcorn/saltcorn.toml` for a service account), the choice on the
command line:

```bash
target/release/saltcorn serve --environment staging --bind 127.0.0.1:3001
target/release/saltcorn build-app blog --environment staging
```

On start the server prints the address it is listening on. If the database is
unreachable or misconfigured it exits immediately with an error naming the target
(the password is never printed) — it will not boot in a half-working state.

Stop the server with Ctrl-C; it also handles `SIGTERM` (what an orchestrator
sends) and shuts down gracefully.

---

## 7. First run: create the admin user

1. Start the server (§6).
2. Open the admin URL in a browser, e.g. `http://127.0.0.1:3032`.
3. The SPA detects that no user exists yet and shows a **create-first-user**
   screen. Enter an email and password; that first user is created as **role 1
   (admin)** and logged straight in.
4. From there you can create tables and fields, edit rows, and add more users.

If you did not build the web UI, you can drive the same flow over the API:

```bash
# Is there a user yet?
curl -s http://127.0.0.1:3032/api/auth/status

# Create the first admin (only works while no user exists).
curl -s -X POST http://127.0.0.1:3032/api/first-user \
  -H 'content-type: application/json' \
  -d '{"email":"admin@example.com","password":"a-strong-password"}'
```

---

## 8. Health check

The server exposes an unauthenticated liveness route for load balancers,
orchestrators, and smoke tests:

```bash
curl -s http://127.0.0.1:3032/health
# {"status":"ok"}
```

A `200` here means the process booted and is accepting requests.

---

## 9. Troubleshooting

- **`error: connecting to database ...` on startup.** The database is unreachable
  or the credentials/host/port/name are wrong. The message names the target
  (without the password). Confirm you can `psql` to the same URL, and that the
  database exists (§4) — the server does not create it.
- **Startup error mentioning the `users` table or permissions.** The connecting
  role cannot create tables. Make it the owner of the database (§4).
- **The page says "The Saltcorn admin UI is not built".** The server has no admin
  bundle to serve. That means the binary was built with `SC_BUILD_ADMIN` set to
  `0`/`false` — note it is a **build-time** variable, so unsetting it on the run
  command changes nothing (see §5). Fix it either way:
  - quickest: restart with `--static-dir ui/admin/dist` (after `npm run build` in
    `ui/admin`), or
  - rebuild the binary with `cargo build --release -p sc-cli` and `SC_BUILD_ADMIN`
    unset, then run without `--static-dir`.
  Confirm with `curl -i http://localhost:3032/` — a working setup returns a document
  linking `/assets/index-<hash>.js`, and that URL returns
  `content-type: text/javascript`.
- **Login/session doesn't stick behind HTTPS.** Add `--secure-cookies` so the
  cookies are sent over TLS. Conversely, do **not** use `--secure-cookies` for
  plain-HTTP local development, or the browser will drop the cookies.

---

## 10. For developers

Run the workspace checks and tests:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

Integration tests run against a **real Postgres**, reinitialised per test. Point
them at a database in either of two ways:

- `DATABASE_URL` (the same variable the server uses). CI sets
  `postgres://saltcorn:saltcorn@localhost:5432/saltcorn_test` against a
  `postgres:16` service.
- the `test` environment of `saltcorn.toml` — the same file `saltcorn serve`
  reads, on the same search paths. Written down there once, `cargo test` needs no
  environment at all; `DATABASE_URL` still overrides it when set.

```toml
[environments.test]
host = "/var/run/postgresql"     # a leading `/` is a Unix socket directory
user = "dev"
database = "saltcorn_test"       # only ever the maintenance connection
test_template = "sc_template"    # optional: what per-test databases are cloned from
```

The named database is only the connection per-test databases are **created and
dropped from** — no test writes to it. `test_template` (or `SC_TEST_TEMPLATE`,
which overrides it) names the database each per-test database is cloned from; it
must be **empty**, since every test inherits whatever is in it. Leave it unset,
as CI does, to use Postgres's own `template1`; set it on a machine whose
`template1` has a stale collation version, which makes `CREATE DATABASE` fail.

See [`docs/TECHNICAL_DESIGN.md`](docs/TECHNICAL_DESIGN.md) §16 for the testing
approach.

### Build resource use

A static V8 (`deno_core`, behind `sc-expr`'s `eval` feature) is linked into
**every one** of the workspace's ~110 integration-test binaries, so
`cargo test --workspace` is a burst of very large, very parallel links. Two
things keep that from taking the machine with it:

- **A debug-info budget**, in the workspace `Cargo.toml`. Dependencies are built
  with no debug info and workspace crates with line tables only, which is what a
  test backtrace actually reads. This is the setting that matters: it takes a
  test binary from ~440 MB to ~150 MB and the whole `--workspace` build from
  ~20 GB of peak memory to ~3 GB. Full DWARF for a dependency, on the rare
  occasion of stepping into one, is a one-off flag:

  ```bash
  cargo test --config 'profile.dev.package."*".debug=true' -p <crate>
  ```

- **`scripts/cargo-guarded.sh`**, an optional wrapper that runs cargo in its own
  memory-capped systemd scope. Use it in place of `cargo` for long runs:

  ```bash
  ./scripts/cargo-guarded.sh test --workspace
  ```

  This matters on desktop Linux running `systemd-oomd`, which kills the heaviest
  **cgroup** rather than the heaviest process when the session comes under memory
  pressure. A shell, cargo and every rustc share the terminal's cgroup, so an
  unguarded build that overruns is executed as "close the terminal window",
  taking the scrollback that would have explained it. The wrapper gives the build
  its own cgroup so only the build can be killed. It falls back to plain `cargo`
  where systemd is not available.

Note also that `target/` is not garbage-collected by cargo: every rebuild leaves
the previous hashed test binaries behind, and at ~150 MB each across ~110 targets
that reaches hundreds of GB over weeks. `cargo clean` periodically, or
[`cargo-sweep`](https://github.com/holmgr/cargo-sweep) to drop only stale files.
