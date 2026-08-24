# Saltcorn v2

A ground-up rewrite of Saltcorn in Rust. This repository currently implements the
**MVP milestone**: a single Postgres database, a create-first-user / login flow,
a table and field editor, a row editor, and user management — all driven from a
**React + TypeScript admin SPA** served by the `saltcorn` process over a typed
JSON API.

This README is for **operators**: how to install the prerequisites, set up the
database, build the server, and run it. §2 is a start-to-finish recipe for a
production deployment on Debian 13; the sections after it explain each piece on its
own, for every other kind of box. (Design and planning docs live under
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

## 2. Quick start: a production install on Debian 13

A complete deployment, from a clean Debian 13 ("trixie") machine to a service that
starts at boot: PostgreSQL, a Rust toolchain, a build from source, a
`saltcorn.toml`, and a systemd unit. Each step links to the section that explains
it properly — read those when something does not fit your box. If you only want to
*try* Saltcorn, skip this and follow §3–§8 instead.

Everything below assumes a `sudo`-capable login, and uses `example.com` as the
domain applications will be served under.

### 2.1 Packages

```bash
sudo apt update
sudo apt install -y build-essential pkg-config git curl ca-certificates \
                    libclang-dev postgresql postgresql-client nodejs npm
```

- **`build-essential` / `pkg-config`** — a C toolchain and linker for the Rust build.
  No TLS libraries are needed: the server links rustls, not OpenSSL.
- **`libclang-dev`** — a **build-time** requirement of the module runtime. The server
  links `deno_runtime`, several of whose extensions reach `rusqlite`'s session feature
  and so `bindgen`, which needs libclang. There is no feature knob that turns it off,
  and it is needed only to build: the binary it produces does not use libclang.
- **`postgresql`** on trixie is PostgreSQL 17; the package starts the server and
  enables it at boot for you.
- **`nodejs` / `npm`** — trixie's packages are new enough (§3 asks for Node 18+).
  They are needed **twice**: once at build time, for the two front-end bundles (§6),
  and again at run time — the server itself runs `npm` whenever an application is
  installed or built, from the admin UI's **Build** button or from
  `saltcorn build-app` (§7), and whenever a module is installed. It never runs
  `node`: an application's bundle is built by npm, and a module runs on a
  JavaScript worker inside the `saltcorn` process.

The build needs outbound network for cargo's crates and a prebuilt V8, but nothing
listens on the internet until §2.7.

### 2.2 A service account and a place to live

Run the server as its own unprivileged system user, and keep the checkout somewhere
that user can read:

```bash
sudo adduser --system --group --home /opt/saltcorn saltcorn
sudo install -d -o "$USER" -g "$USER" /opt/saltcorn/src
```

The checkout is built and owned by *you* and is only ever read by the service; the
service's writable state (disk file stores, application source trees and their
`node_modules`, npm's cache) lives in `/var/lib/saltcorn`, which the systemd unit in
§2.6 creates.

> **The built binary keeps a path back into its checkout.** `cargo build` records the
> absolute paths of `ui/admin/dist` and `ui/ide/dist` in the binary (§6), which is how
> `saltcorn serve` serves the admin UI and the IDE with no flags at all. That path has
> to keep existing: build in the directory you intend to keep, not in `/tmp` or a home
> directory you will clean out. Copying or installing the *binary* elsewhere is fine —
> the recorded paths are absolute. If you do need to separate the two, pass
> `--static-dir` (§6).

### 2.3 The database

Create a role and a database it owns. Doing it over the Unix socket with **peer
authentication** means the service account connects as itself, and no database
password is written to disk anywhere:

```bash
sudo -u postgres createuser saltcorn
sudo -u postgres createdb -O saltcorn saltcorn
```

Check it from the service account:

```bash
sudo -u saltcorn psql -h /var/run/postgresql -d saltcorn -c '\conninfo'
```

Saltcorn never creates or drops the database — this step is yours, once (§5). The
role must own the database, because the server creates the `users` table on first
start and the admin UI creates every table after that.

For a Postgres on another host, give the role a password instead
(`sudo -u postgres psql -c "CREATE ROLE saltcorn LOGIN PASSWORD 'change-me'"`) and put
a `url` in the configuration file below rather than a socket path.

### 2.4 Rust, and the build

Install rustup as your own login user — the toolchain is only needed to build, and
the service never uses it:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
rustc --version    # must be >= 1.85, for edition 2024
```

Then clone and build. This compiles the workspace in release mode *and* runs
`npm ci && npm run build` in both `ui/admin` and `ui/ide` (§6), so it takes a while
and wants a few GB of RAM — on a small VM, add swap first (§11 explains why the
build is heavy):

```bash
git clone <this-repo-url> /opt/saltcorn/src
cd /opt/saltcorn/src
cargo build --release -p sc-cli
sudo install -m 0755 target/release/saltcorn /usr/local/bin/saltcorn
saltcorn                       # prints the usage summary and the config paths it searches
```

### 2.5 The configuration file

A service account has no home directory to keep a configuration file in, so put it in
the system location — `/etc/saltcorn/saltcorn.toml`, which is one of the paths
`saltcorn` searches on Linux (§7):

```bash
sudo install -d -m 0755 /etc/saltcorn
sudo tee /etc/saltcorn/saltcorn.toml >/dev/null <<'TOML'
default_environment = "production"

[environments.production]
host = "/var/run/postgresql"   # a leading `/` is a Unix socket directory
user = "saltcorn"
database = "saltcorn"

base_domain = "example.com"    # each application is served at <subdomain>.example.com
bind = "0.0.0.0:80"
TOML
sudo chown saltcorn:saltcorn /etc/saltcorn/saltcorn.toml
sudo chmod 600 /etc/saltcorn/saltcorn.toml
```

Notes on that file, all of which §7 covers in full:

- `base_domain` and `bind` mirror the flags of the same names, so `saltcorn serve`
  needs neither on the command line — and a `saltcorn build-app` run against the same
  environment writes the application's real URL into the documentation it generates.
- **Without `base_domain` no application is served at all**, only the admin UI: the
  server has no way to address an app.
- A remote database goes in as a URL instead of the socket parts:
  `url = "postgres://saltcorn:change-me@db.internal:5432/saltcorn"`.
- Add a `[environments.staging]` section when you have a second database, and select
  it with `saltcorn serve --environment staging`.
- `chmod 600` because such a file may hold a password; the server warns on stderr when
  it is readable by anyone else.
- Leave `secure_cookies` unset for now. It is for a deployment behind a
  TLS-terminating proxy — with Saltcorn's own TLS (§2.7) the session cookies become
  `Secure` on their own.

### 2.6 The systemd unit

```bash
sudo tee /etc/systemd/system/saltcorn.service >/dev/null <<'UNIT'
[Unit]
Description=Saltcorn
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
Type=notify
WatchdogSec=30s
User=saltcorn
Group=saltcorn
ExecStart=/usr/local/bin/saltcorn serve --environment production
Environment=SALTCORN_CONFIG=/etc/saltcorn/saltcorn.toml
Environment=HOME=/var/lib/saltcorn
StateDirectory=saltcorn
WorkingDirectory=/var/lib/saltcorn
Restart=on-failure
RestartSec=5s

# Bind ports 80/443 without running as root.
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

# Everything read-only except the state directory.
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/saltcorn

[Install]
WantedBy=multi-user.target
UNIT

sudo systemctl daemon-reload
sudo systemctl enable --now saltcorn
systemctl status saltcorn
journalctl -u saltcorn -f      # the boot log names the environment and the file it came from
```

Why each of the less obvious lines:

- **`Type=notify`.** The server tells systemd when it is *serving*: `systemctl start`
  returns once the port is bound and accepting, so a unit ordered `After=saltcorn.service`
  never races the listener. Until then `systemctl status` shows what the boot is doing —
  connecting to the database, building each application — and a boot step that legitimately
  takes minutes (an application's first `npm install`) asks systemd for more time rather
  than needing a large `TimeoutStartSec`. `SIGTERM` is handled, so `systemctl stop` and
  `systemctl restart` are graceful shutdowns, and the unit shows `deactivating` while
  in-flight requests drain.
- **`WatchdogSec=30s`** is optional and cheap: the server pings every 15 seconds from an
  async task, so a process whose runtime has stopped scheduling — a deadlock, a blocking
  call that has eaten every worker — is killed and restarted by `Restart=on-failure`
  instead of sitting there accepting connections it will never answer. Raise it or drop
  the line on a machine where a 30-second stall is normal.
- **`--environment production`**, even though it is the file's default: *naming* an
  environment makes that section outrank any ambient `DATABASE_URL`/`PG*` in the unit's
  environment, so the service cannot be pointed at the wrong database by accident (§7).
- **`SALTCORN_CONFIG`** names the file outright instead of relying on the search paths,
  which depend on `HOME`.
- **`StateDirectory=saltcorn`** creates `/var/lib/saltcorn` owned by the service user,
  and **`ReadWritePaths`** makes it the one writable place under `ProtectSystem=strict`.
  Keep disk file stores inside it; a store pointed anywhere else will fail to write.
- **`HOME=/var/lib/saltcorn`** gives npm a writable home for its cache when the server
  builds an application.
- **`WorkingDirectory`** is what a relative file-store path resolves against.
- **`AmbientCapabilities=CAP_NET_BIND_SERVICE`** lets an unprivileged process bind 80
  and 443. Drop both capability lines if you bind a high port behind a reverse proxy.

Confirm the process is up (§9):

```bash
curl -s http://127.0.0.1/health      # {"status":"ok"}
```

If it is not, `journalctl -u saltcorn` has the reason: the server exits immediately,
naming the target, rather than booting half-working (§10).

### 2.7 First user, DNS, TLS

1. **DNS.** Point `example.com` at the box, and a wildcard `*.example.com` beside it —
   each application is a subdomain of the base domain (§7).
2. **The admin user.** Open `http://example.com`, and the SPA offers a
   create-first-user screen. That first account is an admin (§8). Do this before the
   site is reachable from the internet, or do it over an SSH tunnel: the screen is
   open to whoever reaches it first.
3. **TLS**, in the admin UI under **Settings → SSL / TLS certificates**, not on the
   command line (§7). Choose `letsencrypt`, give a contact address, and restart the
   service; the server then binds 443 beside the plain listener and redirects to it.
   Validation is TLS-ALPN-01, so port 443 must be reachable from the internet, and the
   certificate covers the base domain plus every mounted application's subdomain —
   **adding an application adds a name at the next restart**. Try the Let's Encrypt
   *staging* directory first if you are iterating.
4. **Firewall.** Only 80 and 443 need to be open. Postgres stays on its Unix socket,
   and nothing else listens.

### 2.8 Updating

The project is a **prototype**: it does not migrate databases created by older builds,
so take a dump before you upgrade one.

```bash
sudo -u saltcorn pg_dump -h /var/run/postgresql saltcorn > ~/saltcorn-$(date +%F).sql

cd /opt/saltcorn/src && git pull
cargo build --release -p sc-cli
sudo install -m 0755 target/release/saltcorn /usr/local/bin/saltcorn
sudo systemctl restart saltcorn
```

The restart rebuilds and remounts every stored application; one that fails to build is
logged and skipped rather than taking the server with it (§7). To rebuild a single
application on its own, and to see the bundler's own diagnostics when it fails:

```bash
saltcorn build-app blog --environment production
```

That command mounts nothing, so it is safe to run against the live deployment's
database (§7).

---

## 3. Prerequisites

| Component | Version | Needed for |
|---|---|---|
| **Rust** (with `cargo`) | 1.85+ (edition 2024) | building the `saltcorn` binary |
| **PostgreSQL** | 13 or newer (16 recommended) | the primary data store — *or* SQLite, see §5 Option C |
| **libclang** (`libclang-dev`) | any recent | building the module runtime (`deno_runtime` → `bindgen`); build time only |
| **npm** (and the Node.js it ships with) | Node 18+ | building the admin UI bundle (optional; see §6), **and** *installing* modules (Settings → Modules) |

Install Rust via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustc --version   # must be >= 1.85
```

npm is required for two things, both optional. Without it the server still runs
and its JSON API works, but the browser UI will be a blank bootstrap page (see §6
and §10 Troubleshooting), and **modules** — Saltcorn v1 plugins, which are npm
packages — cannot be installed.

**`node` is not a runtime requirement.** npm is the *installer*; a module then runs
on a JavaScript worker inside the `saltcorn` process itself, on the same V8 the
server already links for code bodies. A server whose modules are already installed
— a container image built elsewhere, a deployment that never adds one — needs no
JavaScript toolchain on its `PATH` at all. What that worker may reach is a
permission set an admin grants on the Modules tab, closed until they do; the
`npm install` that put the package there is not sandboxed. See
[`docs/tutorial-modules.md`](docs/tutorial-modules.md).

---

## 4. Get the code and do a first build

```bash
git clone <this-repo-url> saltcorn
cd saltcorn

# Compile the server binary (without the web UI bundle for now — see §6).
cargo build --release -p sc-cli
```

The binary is produced at `target/release/saltcorn` (or `target/debug/saltcorn`
for a plain `cargo build`). You can also run it through cargo with
`cargo run --release -p sc-cli -- <args>`.

---

## 5. Database setup

Saltcorn needs one Postgres database and a role that **owns** it (so it can create
the `users` table and any tables you add from the admin UI) — or, on a machine
where a database server is more setup than the whole installation is worth, a
SQLite file (Option C).

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

### Option C — a SQLite file, and nothing else

```bash
target/release/saltcorn serve --sqlite ./app.sqlite
```

No server, no role, nothing to create: the file is the database, and it is created
on first start. This is the whole of the setup on a laptop, a Raspberry Pi or a
demo box, and the resulting installation can be copied, backed up or handed over
as one file. SQLite is a full backend — composite keys, foreign keys, `RETURNING`,
transactions and the schema editor all work — with two things genuinely absent,
which Saltcorn adapts to rather than pretending otherwise: row-level security
(there are no policies, so authorization is enforced above the database) and
`LISTEN`/`NOTIFY` (the message bus does not use the database). Row constraints and
full-text indexes are generated as Postgres SQL and are Postgres-only for now.

A SQLite file can also be attached to a *Postgres* deployment as a second database:
put it in a file store and add it under **Tables → Connections**, choosing
**SQLite file**.

### Notes

- **The server never creates or drops the database.** Create it once as above.
  (A SQLite file is the exception: naming one creates it, because naming it is
  the whole of the setup.)
- The connecting role must be able to `CREATE TABLE` in the database (owning it is
  the simplest way). On startup the server creates a `users` table if one is not
  already present.
- Any tables that already exist in the database are picked up automatically — no
  import or registration step.

---

## 6. Building the admin web UI

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
> page (see §10).

Such a binary still serves the whole JSON API; only the browser UI is missing, and
you can supply it at run time instead — build the SPA yourself and point the server
at the output with `--static-dir`:

```bash
cd ui/admin && npm ci && npm run build   # outputs ui/admin/dist (index.html + hashed assets/)
cd ../..

target/release/saltcorn serve --static-dir ui/admin/dist ...   # see §7
```

That is also the quickest loop when you are *working on* the UI: what you serve is
exactly the `dist` you point at, and rebuilding it does not rebuild the Rust binary.
The bundle's entry points carry a content hash in their names, and the server serves
the bundle's own `index.html` (including for client-routed deep links), so a rebuild
is picked up by a plain reload — no cache to disable. With neither the embedded
bundle nor `--static-dir`, the browser shows a document saying the admin UI is not
built. See §10 Troubleshooting.

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

## 7. Running the server

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
| `--sqlite <path>` | `SALTCORN_SQLITE` | `sqlite` | — |

`--sqlite` names the other kind of database: a **SQLite file** rather than a
Postgres server, with no host, no role and nothing to start. The file is created
if it is not there, which is what makes `saltcorn serve --sqlite ./app.sqlite` a
complete installation on a laptop or a Raspberry Pi. It is exclusive with the
Postgres parameters — an environment that names both is refused rather than
quietly preferring one — and a `--database-url` typed on the command line beside
a `sqlite` in the configuration file wins, because a flag always outranks the
file.

SQLite is a full backend, not a demo mode: composite keys, foreign keys,
`RETURNING`, transactions and the schema editor all work. Two things are
genuinely absent, and Saltcorn adapts rather than pretending: **row-level
security** (SQLite has no policies, so authorization is enforced above the
database and the tables list says `rls_available: false`) and **`LISTEN`/
`NOTIFY`** (so the message bus does not use the database).

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
(Option A from §6 — the simplest path):

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

With the bundle baked into the binary (the default build, §6 — `--static-dir` is
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

## 8. First run: create the admin user

1. Start the server (§7).
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

## 9. Health check

The server exposes an unauthenticated liveness route for load balancers,
orchestrators, and smoke tests:

```bash
curl -s http://127.0.0.1:3032/health
# {"status":"ok"}
```

A `200` here means the process booted and is accepting requests.

---

## 10. Troubleshooting

- **`error: connecting to database ...` on startup.** The database is unreachable
  or the credentials/host/port/name are wrong. The message names the target
  (without the password). Confirm you can `psql` to the same URL, and that the
  database exists (§5) — the server does not create it.
- **Startup error mentioning the `users` table or permissions.** The connecting
  role cannot create tables. Make it the owner of the database (§5).
- **The page says "The Saltcorn admin UI is not built".** The server has no admin
  bundle to serve. That means the binary was built with `SC_BUILD_ADMIN` set to
  `0`/`false` — note it is a **build-time** variable, so unsetting it on the run
  command changes nothing (see §6). Fix it either way:
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

## 11. For developers

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

- **`-j 4` for `sc-server`'s test build.** Since the module runtime landed, each of
  `sc-server`'s ~47 test binaries maps a much larger set of rlibs at link time, and
  the default parallelism under the wrapper's 10 GB `MemoryHigh` puts all of them in
  continuous reclaim — a build that makes no progress rather than one that fails.
  Either cap the jobs or raise the ceiling:

  ```bash
  ./scripts/cargo-guarded.sh test -p sc-server --no-run -j 4
  SC_BUILD_MEM_HIGH=16G ./scripts/cargo-guarded.sh test -p sc-server --no-run
  ```

Note also that `target/` is not garbage-collected by cargo: every rebuild leaves
the previous hashed test binaries behind, and at ~150 MB each across ~110 targets
that reaches hundreds of GB over weeks. `cargo clean` periodically, or
[`cargo-sweep`](https://github.com/holmgr/cargo-sweep) to drop only stale files.
