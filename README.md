# Saltcorn Feldspar

Saltcorn Feldspar is a ground-up rewrite of Saltcorn in Rust. This repository currently implements the
**MVP milestone**: a single Postgres database, a create-first-user / login flow,
a table and field editor, a row editor, and user management — all driven from a
**React + TypeScript admin SPA** served by the `feldspar` process over a typed
JSON API.

This README is for **operators**: how to install the prerequisites, set up the
database, build the server, and run it. §2 is a start-to-finish recipe for a
production deployment on Debian 13; the sections after it explain each piece on its
own, for every other kind of box. (Design and planning docs live under
[`docs/`](docs/); the roadmap is in [`TODO.md`](TODO.md).)

---

## 1. What you get

- A single binary, `feldspar`, that serves the admin UI and its API.
- One Postgres database is the source of truth. The server **introspects** whatever
  tables already exist and, on first start, creates a `users` table if none is
  present. It does **not** create the database itself — you do that once (below).
- An admin logs in through the SPA, creates tables and fields, edits rows, and
  manages users.
- **Triggers can run code**, in JavaScript or in **Python** — a trigger body that reads
  and writes your tables, calls endpoints, writes files and runs other triggers. The
  JavaScript half is in every build; **Python needs a build that has it** (§3), and the
  shipped tarball does not. See [`docs/tutorial-triggers.md`](docs/tutorial-triggers.md)
  and [`docs/tutorial-python.md`](docs/tutorial-python.md).
- **An external coding agent can administer the installation**, over an MCP server this
  binary serves on one route — the schema, the triggers, the applications, under a
  bearer token an admin mints and can revoke. It is **off by default**; see
  [`docs/tutorial-mcp.md`](docs/tutorial-mcp.md).

The MVP is deliberately scoped: one database, basic column types only, no
workflows/agents/models, no file stores or applications yet. See the "Out of MVP
scope" section of [`TODO.md`](TODO.md).

---

## 2. Quick start: a production install on Debian 13

A complete deployment, from a clean Debian 13 ("trixie") machine to a service that
starts at boot: PostgreSQL, a Rust toolchain, a build from source, a
`feldspar.toml`, and a systemd unit. Each step links to the section that explains
it properly — read those when something does not fit your box. If you only want to
*try* Saltcorn, skip this and follow §3–§8 instead.

Everything below assumes a `sudo`-capable login, and uses `example.com` as the
domain applications will be served under.
[`docs/OPERATIONS.md`](docs/OPERATIONS.md) is the same ground as an operator's
manual — the two installation modes side by side, the upgrade path for each, the
configuration file and every environment variable, the reload signal, and running
a coding agent against the installation.

**The short way: download the built binary.** There is a prebuilt, statically
linked artifact of the latest build. It needs no Rust, no C toolchain and no
checkout: it is all of §2 with the build taken out.

```bash
curl -fLO https://feldspar-latest-static.saltcorn.com/feldspar.tar.gz
curl -fLO https://feldspar-latest-static.saltcorn.com/feldspar.tar.gz.sha256
sha256sum -c feldspar.tar.gz.sha256                     # optional, and quick
tar -xzf feldspar.tar.gz
sudo feldspar-*/install.sh                              # the tree at /opt/feldspar
sudo /opt/feldspar/setup-host.sh --domain example.com   # packages, database, unit
```

The tarball carries `setup-host.sh`, so the second command puts it at
`/opt/feldspar/setup-host.sh` and the third runs it from there — it finds the binary
beside it, and does everything §2.1–§2.6 does except build (PostgreSQL, the
`feldspar` service account, the role and database, `/etc/feldspar/feldspar.toml`,
the systemd unit, and `feldspar` on `PATH`). `--dry-run` prints all of it without
doing any of it, and `--help` lists the options. Then continue at §2.7, which is DNS,
the first admin user and TLS.

Those URLs carry no version: they are always the *latest* build, which is also how a
host is upgraded (§2.8). Two things the artifact does not have: **Python triggers**,
which need a dynamically linked build (§3), and support for native Node addons in
modules (§4.1). Everything else in this README applies to it unchanged.

**Or run it as a script.** `scripts/setup-host.sh` is this section — packages, the
service account, the role and database, `feldspar.toml`, the unit — and it can be
fetched and run on a bare host:

```bash
curl -fsSL https://raw.githubusercontent.com/saltcorn/feldspar/main/scripts/setup-host.sh \
  | sh -s -- --domain example.com            # builds from source here, as §2.4 does
```

That is the from-source route. The download above skips it: run from inside an
installed artifact, `setup-host.sh` finds the binary beside it and takes `--static`
as its default, so there is nothing to build and no flag to remember. The same
artifact can be built and pushed from a workstation instead of downloaded (§4.1) —

```bash
scripts/build-static.sh --deploy root@host   # build, copy, unpack, install
```

— and then `sudo /opt/feldspar/setup-host.sh --domain example.com` on the host.
Fetched on its own rather than out of a tarball, the script still needs the flag:

```bash
curl -fsSL https://raw.githubusercontent.com/saltcorn/feldspar/main/scripts/setup-host.sh \
  | sh -s -- --static --domain example.com
```

Either order works, `wget -qO-` does as well as curl, `--dry-run` prints every
command and file first, and `--help` lists the rest (an external database with
`--database-url`, a different listen address, a second run that keeps what is
already there). Read on to know what it did.

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
  `feldspar build-app` (§7), and whenever a module is installed. It never runs
  `node`: an application's bundle is built by npm, and a module runs on a
  JavaScript worker inside the `feldspar` process.

The build needs outbound network for cargo's crates and a prebuilt V8, but nothing
listens on the internet until §2.7.

### 2.2 A service account and a place to live

Run the server as its own unprivileged system user, and keep the checkout somewhere
that user can read:

```bash
sudo adduser --system --group --home /opt/feldspar feldspar
sudo install -d -o "$USER" -g "$USER" /opt/feldspar/src
```

The checkout is built and owned by *you* and is only ever read by the service; the
service's writable state (disk file stores, application source trees and their
`node_modules`, npm's cache) lives in `/var/lib/feldspar`, which the systemd unit in
§2.6 creates.

> **The built binary keeps a path back into its checkout.** `cargo build` records the
> absolute paths of `ui/admin/dist` and `ui/ide/dist` in the binary (§6), which is how
> `feldspar serve` serves the admin UI and the IDE with no flags at all. That path has
> to keep existing: build in the directory you intend to keep, not in `/tmp` or a home
> directory you will clean out. Copying or installing the *binary* elsewhere is fine —
> the recorded paths are absolute. If you do need to separate the two, pass
> `--static-dir` (§6).

### 2.3 The database

Create a role and a database it owns. Doing it over the Unix socket with **peer
authentication** means the service account connects as itself, and no database
password is written to disk anywhere:

```bash
sudo -u postgres createuser feldspar
sudo -u postgres createdb -O feldspar feldspar
```

Check it from the service account:

```bash
sudo -u feldspar psql -h /var/run/postgresql -d feldspar -c '\conninfo'
```

Saltcorn never creates or drops the database — this step is yours, once (§5). The
role must own the database, because the server creates the `users` table on first
start and the admin UI creates every table after that.

For a Postgres on another host, give the role a password instead
(`sudo -u postgres psql -c "CREATE ROLE feldspar LOGIN PASSWORD 'change-me'"`) and put
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
git clone <this-repo-url> /opt/feldspar/src
cd /opt/feldspar/src
cargo build --release -p sc-cli
sudo install -m 0755 target/release/feldspar /usr/local/bin/feldspar
feldspar                       # prints the usage summary and the config paths it searches
```

### 2.5 The configuration file

A service account has no home directory to keep a configuration file in, so put it in
the system location — `/etc/feldspar/feldspar.toml`, which is one of the paths
`feldspar` searches on Linux (§7):

```bash
sudo install -d -m 0755 /etc/feldspar
sudo tee /etc/feldspar/feldspar.toml >/dev/null <<'TOML'
default_environment = "production"

[environments.production]
host = "/var/run/postgresql"   # a leading `/` is a Unix socket directory
user = "feldspar"
database = "feldspar"

base_domain = "example.com"    # each application is served at <subdomain>.example.com
bind = "0.0.0.0:80"
TOML
sudo chown feldspar:feldspar /etc/feldspar/feldspar.toml
sudo chmod 600 /etc/feldspar/feldspar.toml
```

Notes on that file, all of which §7 covers in full:

- `base_domain` and `bind` mirror the flags of the same names, so `feldspar serve`
  needs neither on the command line — and a `feldspar build-app` run against the same
  environment writes the application's real URL into the documentation it generates.
- **Without `base_domain` no application is served at all**, only the admin UI: the
  server has no way to address an app.
- A remote database goes in as a URL instead of the socket parts:
  `url = "postgres://feldspar:change-me@db.internal:5432/feldspar"`.
- Add a `[environments.staging]` section when you have a second database, and select
  it with `feldspar serve --environment staging`.
- `chmod 600` because such a file may hold a password; the server warns on stderr when
  it is readable by anyone else.
- Leave `secure_cookies` unset for now. It is for a deployment behind a
  TLS-terminating proxy — with Saltcorn's own TLS (§2.7) the session cookies become
  `Secure` on their own.

### 2.6 The systemd unit

```bash
sudo tee /etc/systemd/system/feldspar.service >/dev/null <<'UNIT'
[Unit]
Description=Saltcorn Feldspar
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
Type=notify
WatchdogSec=30s
User=feldspar
Group=feldspar
ExecStart=/usr/local/bin/feldspar serve --environment production
Environment=FELDSPAR_CONFIG=/etc/feldspar/feldspar.toml
Environment=HOME=/var/lib/feldspar
Environment=SC_DATA_DIR=/var/lib/feldspar
StateDirectory=feldspar
WorkingDirectory=/var/lib/feldspar
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
ReadWritePaths=/var/lib/feldspar

[Install]
WantedBy=multi-user.target
UNIT

sudo systemctl daemon-reload
sudo systemctl enable --now feldspar
systemctl status feldspar
journalctl -u feldspar -f      # the boot log names the environment and the file it came from
```

Why each of the less obvious lines:

- **`Type=notify`.** The server tells systemd when it is *serving*: `systemctl start`
  returns once the port is bound and accepting, so a unit ordered `After=feldspar.service`
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
- **`FELDSPAR_CONFIG`** names the file outright instead of relying on the search paths,
  which depend on `HOME`.
- **`StateDirectory=feldspar`** creates `/var/lib/feldspar` owned by the service user,
  and **`ReadWritePaths`** makes it the one writable place under `ProtectSystem=strict`.
  Keep disk file stores inside it; a store pointed anywhere else will fail to write.
- **`HOME=/var/lib/feldspar`** gives npm a writable home for its cache when the server
  builds an application.
- **`SC_DATA_DIR=/var/lib/feldspar`** is where the server keeps directories it owns
  rather than the admin: git file stores are checked out into `git-stores/`, and the
  **Suggest a directory** button on a new local file store offers
  `local-stores/<store name>`. Naming it outright is what makes those suggestions land
  somewhere an operator would look — and somewhere `ReadWritePaths` allows — instead of
  the `HOME`-derived `~/.local/share/feldspar`.
- **`WorkingDirectory`** is what a relative file-store path resolves against.
- **`AmbientCapabilities=CAP_NET_BIND_SERVICE`** lets an unprivileged process bind 80
  and 443. Drop both capability lines if you bind a high port behind a reverse proxy.

Confirm the process is up (§9):

```bash
curl -s http://127.0.0.1/health      # {"status":"ok"}
```

If it is not, `journalctl -u feldspar` has the reason: the server exits immediately,
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
sudo -u feldspar pg_dump -h /var/run/postgresql feldspar > ~/feldspar-$(date +%F).sql

cd /opt/feldspar/src && git pull
cargo build --release -p sc-cli
sudo install -m 0755 target/release/feldspar /usr/local/bin/feldspar
sudo systemctl restart feldspar
```

A host installed from a static artifact is updated by installing a newer one. Only
the binary and the bundles change — `setup-host.sh` is not run again, the database,
`feldspar.toml` and the unit are left alone — and nothing restarts by itself. On the
host, from the same version-less URL as the install:

```bash
curl -fLO https://feldspar-latest-static.saltcorn.com/feldspar.tar.gz
tar -xzf feldspar.tar.gz
sudo feldspar-*/install.sh          # over the running install; ETXTBSY if it is up
sudo systemctl restart feldspar
```

`install.sh` copies over `/opt/feldspar/bin/feldspar`, and the kernel refuses to write
the executable of a live process, so stop the service first (`sudo systemctl stop
feldspar`) if that is where it fails. Or push a build from the workstation, which
stops and starts the unit around the install for you:

```bash
scripts/build-static.sh --deploy root@host
ssh root@host systemctl restart feldspar
```

The restart rebuilds and remounts every stored application; one that fails to build is
logged and skipped rather than taking the server with it (§7). To rebuild a single
application on its own, and to see the bundler's own diagnostics when it fails:

```bash
feldspar build-app blog --environment production
```

That command mounts nothing, so it is safe to run against the live deployment's
database (§7).

---

## 3. Prerequisites

| Component | Version | Needed for |
|---|---|---|
| **Rust** (with `cargo`) | 1.85+ (edition 2024) | building the `feldspar` binary |
| **PostgreSQL** | 13 or newer (16 recommended) | the primary data store — *or* SQLite, see §5 Option C |
| **libclang** (`libclang-dev`) | any recent | building the module runtime (`deno_runtime` → `bindgen`); build time only |
| **npm** (and the Node.js it ships with) | Node 18+ | building the admin UI bundle (optional; see §6), **and** *installing* modules (Settings → Modules) |
| **CPython** + `pip`, and `python3-dev` to build against | 3.11+ | **only** for a server that runs Python trigger bodies or installs Python modules — and only in a build that has the `python` feature (below) |

Install Rust via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustc --version   # must be >= 1.85
```

npm is required for two things, both optional. Without it the server still runs
and its JSON API works, but the browser UI will be a blank bootstrap page (see §6
and §10 Troubleshooting), and **modules** — Saltcorn v1 plugins, which are npm
packages — cannot be installed.

**Some modules come with Saltcorn.** A few are written and maintained in this
repository (`plugins/`), ship inside the release tarball, and appear on the
Modules tab as a catalog with an Install button each — an RSS feed table provider
and a Markdown renderer today. They are still modules: nothing is loaded until an
admin installs one. What is *not* shipped is what they depend on, which npm or
pip fetches at that moment — so a server that installs none of them downloads
nothing. See [`plugins/README.md`](plugins/README.md) for what is there and how
to add one.

**`node` is not a runtime requirement.** npm is the *installer*; a module then runs
on a JavaScript worker inside the `feldspar` process itself, on the same V8 the
server already links for code bodies. A server whose modules are already installed
— a container image built elsewhere, a deployment that never adds one — needs no
JavaScript toolchain on its `PATH` at all. What that worker may reach is a
permission set an admin grants on the Modules tab, closed until they do; the
`npm install` that put the package there is not sandboxed. See
[`docs/tutorial-modules.md`](docs/tutorial-modules.md), and
[`docs/tutorial-table-providers.md`](docs/tutorial-table-providers.md) for a module
that supplies a **table** rather than an action.

### Python, which is a build and not a flag

A trigger body can be Python, and a module can be a `pip`-installable Python
distribution ([`docs/tutorial-python.md`](docs/tutorial-python.md)). That half of the
server is **off unless the binary was built with it**, and there is no flag that turns
it on afterwards:

```bash
sudo apt install python3-dev                       # the libpython this links against
cargo build --release -p sc-cli --features python  # the only way to get Python support
```

The reason is linkage rather than policy. Python is *embedded*: the build links
`libpython3.x.so` into the binary, so a binary built with the feature **will not start
at all** on a host that has no matching library — a dynamic-linker error before the
server prints anything — and **the packaged artifact of §4.1 cannot carry it**, being a
static binary with no shared-library dependencies by design. So the shipped tarball has
no Python, and a Python-capable server is a separate, dynamically-linked build. One such
build serves every CPython from **3.11** up (`abi3`), as long as the matching
`libpython` is present on the host.

At run time such a server wants two more things, both only when Python is actually used:
a `python3` on its `PATH` with `pip` available (that is what *installs* a Python module,
into a virtual environment the server owns and creates — `--python-dir`), and nothing
else. The interpreter that runs your code is the one linked in, and it is not started
until the first Python body or module asks for it. `--python off` keeps it that way for
good.

**The interpreter `pip` runs under must match the one linked in**, to the minor version.
`--python-bin` names it (default `python3`); on a mismatch the server refuses to put the
environment on its import path and says so on Settings → Development, naming both
versions — because a compiled extension built against one version and loaded into
another does not reliably fail, it reads the wrong memory in silence.

**And there is no sandbox for Python.** A JavaScript module gets a permission set; a
Python module runs inside the server with the server's own privileges, because CPython
has no equivalent to grant. The Modules tab says so where the other language's
permissions form is. Install what you trust; writing a Python trigger body is an
administrator's capability in the same way `db.sql` already is.

Settings → Development reports which of four states this process is in — not built with
Python, built but turned off, built and not started yet, or running with its version —
plus the environment, what is installed in it, and how many runs are in flight.

---

## 4. Get the code and do a first build

```bash
git clone <this-repo-url> feldspar
cd feldspar

# Compile the server binary (without the web UI bundle for now — see §6).
cargo build --release -p sc-cli
```

The binary is produced at `target/release/feldspar` (or `target/debug/feldspar`
for a plain `cargo build`). You can also run it through cargo with
`cargo run --release -p sc-cli -- <args>`.

**Add `--features python`** to that command line if this server is to run Python trigger
bodies or install Python modules (§3). It is a build-time decision and the only one:
there is no flag that adds Python to a binary built without it, and a build without it
still *offers* the `run_python_code` action — it fails at fire time saying the server was
built without Python support, so a trigger's configuration keeps its meaning across
deployments.

### 4.1 Building for a machine that will not have a toolchain

Everything above builds *here*, and the binary it produces belongs here: it is
linked against this machine's glibc and carries this checkout's paths to the admin
UI and IDE bundles. To deploy to a VM without repeating §2.1 and §3 on it, build a
packaged artifact instead:

```bash
scripts/build-static.sh                    # dist/feldspar-<version>-<target>.tar.gz
scripts/build-static.sh --deploy root@vm   # ...and install it on that machine
scripts/build-static.sh --release          # ...and publish it to the R2 bucket
scripts/build-static.sh --help             # targets, install prefix, options
```

The binary inside is statically linked (`+crt-static`), so it has no shared-library
dependencies and no interpreter: the same tarball runs on Debian, Ubuntu, RHEL and
on Alpine. **That is also why it has no Python** — embedding CPython means linking
`libpython`, which a static binary cannot do (and could not `dlopen` a C-extension wheel
even if it did), so a Python-capable server is the separate dynamically-linked build of
§3 rather than this artifact. It carries the admin SPA and the IDE beside it, an `install.sh` that
puts the tree at `/opt/feldspar` — the prefix compiled into the binary, which
`--prefix` changes at build time — and a copy of `setup-host.sh`, installed at
`/opt/feldspar/setup-host.sh`. So a machine with nothing on it takes three commands
and no checkout: unpack, `install.sh`, `setup-host.sh` (§2.2). The last of those is
run once and is deliberately not on `PATH`.

The destination then needs no Rust, no `libclang` and no C toolchain. It still needs
a database, and it still needs `npm` if applications will be *built* on it (§7).

`--deploy [user@]host` finishes the job over ssh: the tarball is copied to the host,
unpacked in `/tmp` (`--remote-tmp` elsewhere), installed with its own `install.sh`
under `sudo` unless the login is root, and the staging copy removed again. The host
is anything ssh accepts, a `~/.ssh/config` alias included, and `--ssh-opt` (repeatable,
one argv element each, e.g. `--ssh-opt -p2222`) passes options through. It is checked
that the host answers *before* the build starts rather than after it. Nothing is
restarted: an already-running server keeps serving the binary it started with until
you restart the unit (`sudo systemctl restart feldspar`, §2.6).

The host still needs a database, a service account and a unit, which is the other
half of the two-step install: `scripts/setup-host.sh --static` on the host, in
either order with the deploy (§2).

`--release` publishes instead of deploying: the tarball and its `.sha256` go to the
Cloudflare R2 bucket `feldspar-latest-static` (`--bucket` for another) through
`npx wrangler r2 object put --remote`, as `feldspar.tar.gz` and
`feldspar.tar.gz.sha256`. That bucket is what §2 downloads:

```
https://feldspar-latest-static.saltcorn.com/feldspar.tar.gz
https://feldspar-latest-static.saltcorn.com/feldspar.tar.gz.sha256
```

The object names carry no version — this is the *latest*
build, and a machine fetching it has no version to ask for — so each release
overwrites the last and the download URL never changes. The published checksum file
names `feldspar.tar.gz` rather than the versioned file it was computed over, so
`sha256sum -c` works on what was downloaded. It needs a `wrangler` login with access
to the bucket; that `npx` is on `PATH` is checked before the build starts.

**Name resolution does not go through glibc.** A statically linked binary that calls
`getaddrinfo` gets glibc's NSS machinery, which `dlopen`s a shared object for every
module on the `hosts:` line of `/etc/nsswitch.conf` — `myhostname` is on Debian's and
Ubuntu's by default — and each of those links the *shared* glibc, so the first hostname
the process resolves loads a second `libc.so.6` beside the static one and the process
dies. This binary therefore resolves names itself (`crates/sc-dns`): the linker puts a
`hickory-resolver` implementation of `getaddrinfo` in front of glibc's, reading
`/etc/resolv.conf` and `/etc/hosts` with no `dlopen` anywhere. Two things follow for a
deployment: `/etc/nsswitch.conf` no longer affects how this process resolves anything,
and a name it must reach has to be in DNS or in `/etc/hosts` — mDNS (`.local`),
`myhostname`'s synthesis of the local hostname, and LDAP/sssd hosts do not apply to it.

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
CREATE ROLE feldspar WITH LOGIN PASSWORD 'change-me';
CREATE DATABASE feldspar OWNER feldspar;
SQL
```

Verify you can connect as that role:

```bash
psql "postgres://feldspar:change-me@localhost:5432/feldspar" -c '\conninfo'
```

### Option B — Docker

```bash
docker run --name feldspar-db \
  -e POSTGRES_USER=feldspar \
  -e POSTGRES_PASSWORD=change-me \
  -e POSTGRES_DB=feldspar \
  -p 5432:5432 \
  -d postgres:16
```

That gives you `postgres://feldspar:change-me@localhost:5432/feldspar`.

### Option C — a SQLite file, and nothing else

```bash
target/release/feldspar serve --sqlite ./app.sqlite
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
below), embeds the resulting paths in the binary, and `feldspar serve` then serves
the UI with no `--static-dir` needed:

```bash
cargo build --release -p sc-cli     # builds the Rust binary *and* both front ends
target/release/feldspar serve ...   # serves the admin UI, no flags
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
> `feldspar serve`.** Putting it on the run command has **no effect** either way:
> a binary built without the bundle stays without it, and the browser gets a blank
> page (see §10).

Such a binary still serves the whole JSON API; only the browser UI is missing, and
you can supply it at run time instead — build the SPA yourself and point the server
at the output with `--static-dir`:

```bash
cd ui/admin && npm ci && npm run build   # outputs ui/admin/dist (index.html + hashed assets/)
cd ../..

target/release/feldspar serve --static-dir ui/admin/dist ...   # see §7
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

The main command is `feldspar serve`. It takes **database flags** and **server
flags**; database settings may also come from environment variables or from a
configuration file. (The other command is `feldspar build-app`, below.)

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
| `--sqlite <path>` | `FELDSPAR_SQLITE` | `sqlite` | — |

`--sqlite` names the other kind of database: a **SQLite file** rather than a
Postgres server, with no host, no role and nothing to start. The file is created
if it is not there, which is what makes `feldspar serve --sqlite ./app.sqlite` a
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
their connection parameters can live together in a `feldspar.toml`, one section
each, and the command line picks one:

```toml
default_environment = "production"

[environments.production]
host = "db.internal"
port = 5432
user = "feldspar"
password = "change-me"
database = "feldspar"

[environments.staging]
url = "postgres://feldspar:change-me@staging.internal:5432/feldspar"

[environments.test]
database = "feldspar_test"
test_template = "sc_template"   # read by `cargo test`, not by the server
```

```bash
feldspar serve                          # the file's default_environment
feldspar serve --environment staging    # or --env staging, or FELDSPAR_ENV=staging
feldspar serve --environment test
```

`environments` is an ordinary table: define as many as you have databases, named
whatever you like. With no `default_environment` and nothing named on the command
line, the environment used is `production`. The `test` environment has one extra
reader: the integration-test harness takes its connection (and `test_template`)
from there, so a developer machine needs no test-specific environment variables —
see "For developers" below.

| Flag | Environment fallback | Meaning |
|---|---|---|
| `--environment <name>` | `FELDSPAR_ENV` | which `[environments.<name>]` section to connect with |
| `--config <path>` | `FELDSPAR_CONFIG` | read this file instead of searching for one |

Without `--config`, the file is looked for in the platform's configuration
directories, user first:

| | user | system |
|---|---|---|
| Linux/BSD | `$XDG_CONFIG_HOME/feldspar/feldspar.toml` (else `~/.config/feldspar/feldspar.toml`) | `/etc/feldspar/feldspar.toml` |
| macOS | `~/Library/Application Support/feldspar/feldspar.toml` | `/etc/feldspar/feldspar.toml` |
| Windows | `%APPDATA%\feldspar\feldspar.toml` | `%PROGRAMDATA%\feldspar\feldspar.toml` |

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
> staging. Explicit `--db-*` flags still win over everything. `feldspar serve`
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
| `--python <auto\|off>` | whether this process starts its Python interpreter (never in a binary built without the `python` feature) | `auto` |
| `--python-max-inflight <n>` | Python runs resident at once | `32` |
| `--python-max-stuck <n>` | runs that never returned before Python is refused until a restart | `8` |
| `--python-dir <dir>` | the virtual environment Python modules install into | the platform's data directory |
| `--python-bin <path>` | the interpreter `pip` runs under | `python3` |

Unknown flags in either group are rejected with a clear error rather than ignored.

> **The two code-pool flags size concurrency, not parallelism.** A code body
> suspended on a database call costs a pending promise rather than a thread, so
> one isolate serves hundreds of bodies at once: `--code-workers` buys CPU
> parallelism (what a body that *computes* wants) and `--code-max-inflight` buys
> occupancy (memory: a resident run holds its scope, its bindings and its last
> read). The defaults serve 512 bodies at once and queue the rest, with the queue
> time counted inside each run's own `timeout_ms`. Past that the ceiling is the
> database connection pool, which is where it belongs.

> **`--base-domain` mounts your applications.** With it set, `feldspar serve` loads
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
feldspar build-app <subdomain> [database flags] [--file-store NAME=PATH]
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

### Reading and setting configuration values

```bash
feldspar get-cfg [KEY] [database flags]
feldspar set-cfg KEY [VALUE] [database flags]      # no VALUE: read it from stdin
```

The settings an admin edits under **Settings** — the TLS mode and certificate, the
SMTP transport, the logging switches — are rows in the primary database, not a file.
These two commands are the terminal's way in, and they need the database and nothing
else: no running server, no session, no browser.

```bash
feldspar set-cfg smtp_host smtp.example.com
feldspar set-cfg smtp_port 587
feldspar set-cfg ssl_certificate < fullchain.pem     # multi-line values on stdin
port=$(feldspar get-cfg https_port)                  # one value, ready to capture
feldspar get-cfg                                     # every setting, key=value
```

- **The value's type comes from the key**, not from how it was typed: `8443` is a
  number because `https_port` is declared one, and it is checked against that
  declaration before anything is written — so `set-cfg https_port yes` is a message
  and not a stored string. A yes/no setting takes `true`/`false`, and also the
  `yes`/`no`, `on`/`off`, `1`/`0` a shell script tends to produce.
- **Without a value, `set-cfg` reads stdin**, which is how a PEM block is set without
  quoting a certificate into a shell. The one trailing newline a pipe adds is not
  stored; everything inside the value is.
- **`get-cfg KEY` prints the value and nothing else** — no quotes around a string, and
  no SQL echo even when Settings → Development has it switched on — so it can be
  captured. With no key it prints every declared setting as `key=value`, one line
  each, showing what the server acts on: the stored value where there is one, the
  declared default where there is not.
- **Secrets are redacted in the listing** (`smtp_password=••••••••`), because a listing
  ends up in scrollback and in CI logs. Naming the key prints it in full — that caller
  asked for that value, and the command already holds the database.
- **Nothing is restarted.** When a setting takes effect is the setting's own business:
  the logging switches are immediate, the SMTP transport is read per message, and the
  TLS settings are read at boot.

### The administration MCP server

An external coding agent — Claude Code or anything else that speaks MCP over streamable
HTTP — can read and change this installation's **configuration half**: the tables and
their fields, the access rules, the triggers, the workflows, the agents, and the
applications with their custom SQL queries. It is served by this same process on
`POST /mcp`, and it projects the same administrative surface the admin SPA uses, under
the same authorization: a token names a **user**, and every call is authorized exactly as
that person's own admin session would be.

**It is off by default, and off means absent** — the route answers `404` and does not so
much as read the token table. Two settings turn it on, both in **Settings → Development**
and both effective on the next request, with no restart:

```bash
feldspar set-cfg mcp_enabled true          # serve POST /mcp at all
feldspar set-cfg mcp_loopback_only false   # accept a peer that is not on this machine
```

`mcp_loopback_only` defaults to **on**: the usual arrangement is an agent running beside
the server or reaching it down a tunnel the developer made, and an installation that will
never be administered from elsewhere should be able to say so in a checkbox rather than in
a reverse proxy.

The credential is minted in the same Settings → Development screen — a label, an expiry,
and six grants (create, change, drop, access changes, and whether the token may work on
triggers and on applications) — and it is shown **once**, with the `claude mcp add` line
built around it. The server keeps only a SHA-256 hash, so a lost token is revoked rather
than recovered. Two things worth knowing before minting one: **a token is an
administrator**, bounded only by the grants ticked when it was made, and **revoking it is
the only way to take it back**.

`Authorization: Bearer` is the only credential the route accepts — a session cookie on it
is ignored, not honoured, which is why no page an admin visits can reach this surface
through the session they are logged into.

[`docs/tutorial-mcp.md`](docs/tutorial-mcp.md) walks the whole of it: turning it on,
minting a token, the one registration line, and a session that adds a field, saves a
trigger and rebuilds the application the schema change affected.

### Examples

Local development, connecting with a URL and serving the pre-built bundle
(Option A from §6 — the simplest path):

```bash
target/release/feldspar serve \
  --database-url postgres://feldspar:change-me@localhost:5432/feldspar \
  --static-dir ui/admin/dist \
  --bind 127.0.0.1:3032
```

Individual DB parts, listening on all interfaces:

```bash
target/release/feldspar serve \
  --db-host localhost --db-user feldspar --db-password change-me --db-name feldspar \
  --static-dir ui/admin/dist \
  --bind 0.0.0.0:8080
```

With the bundle baked into the binary (the default build, §6 — `--static-dir` is
then unnecessary):

```bash
cargo build --release -p sc-cli
target/release/feldspar serve \
  --database-url postgres://feldspar:change-me@localhost:5432/feldspar \
  --bind 127.0.0.1:3032
```

Using environment variables (handy for systemd/containers), behind a TLS proxy:

```bash
export DATABASE_URL=postgres://feldspar:change-me@localhost:5432/feldspar
target/release/feldspar serve --bind 127.0.0.1:3032 --secure-cookies
```

One box, three databases: the parameters in `~/.config/feldspar/feldspar.toml`
(or `/etc/feldspar/feldspar.toml` for a service account), the choice on the
command line:

```bash
target/release/feldspar serve --environment staging --bind 127.0.0.1:3001
target/release/feldspar build-app blog --environment staging
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
- **A Python trigger says the server was built without Python support.** It was: the
  `python` feature is a build-time decision and no flag substitutes for it (§3). Rebuild
  with `cargo build --release -p sc-cli --features python`, or check Settings →
  Development, which names which of the four states this process is in and what to do
  about each.
- **The server will not start: `error while loading shared libraries: libpython3.x.so`.**
  This binary was built with the `python` feature and the host has no matching
  `libpython`. Install it (Debian: `python3-dev`, or the `libpython3.x` runtime package),
  or run a binary built without the feature. `abi3` means any CPython 3.11+ will do, but
  the *soname* is version-specific, so it must be the one the build linked against or a
  symlink to a compatible one.
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

The workspace's default features leave Python out, so those three commands need no
Python toolchain. The adapter's own suite does — it embeds an interpreter:

```bash
cargo test -p sc-python --features python-host    # needs python3-dev, and pip for the
                                                  # environment tests
```

Integration tests run against a **real Postgres**, reinitialised per test. Point
them at a database in either of two ways:

- `DATABASE_URL` (the same variable the server uses). CI sets
  `postgres://saltcorn:saltcorn@localhost:5432/saltcorn_test` against a
  `postgres:16` service.
- the `test` environment of `feldspar.toml` — the same file `feldspar serve`
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
**every one** of the workspace's test binaries, so `cargo test --workspace` is a
burst of very large, very parallel links. Three things keep that from taking the
machine — or CI's disk — with it:

- **One integration-test binary per crate**, not one per file. Cargo makes a
  target of every `crates/<crate>/tests/*.rs`, which for 180 files meant 180
  links of a ~400 MB binary and ~70 GB of `target/`; CI ran out of disk in the
  middle of one and `rust-lld` died with `signal 7 [Bus error]` rather than
  saying so. Each crate now has a `tests/it.rs` that pulls its test files in as
  modules, and `autotests = false` in its `Cargo.toml` so cargo does not also
  build them separately. That is ~55 binaries and ~7 GB.

  **Adding a test file means adding two lines to that crate's `tests/it.rs`** —
  a `#[path]` and a `mod`. A file that is not listed there is not compiled, and
  nothing will tell you so. Three consequences of being a module rather than a
  crate root: a shared `tests/common/` is declared once in `it.rs` and reached as
  `crate::common`, not with a second `mod common;`; a `use` of a module in the
  same file needs `self::`; and a test that mutates process-global state (an
  environment variable, `PATH`, a signal handler) must stay its own target — see
  the `[[test]]` entries in `crates/sc-server/Cargo.toml`.

- **A debug-info budget**, in the workspace `Cargo.toml`. Dependencies are built
  with no debug info and workspace crates with line tables only, which is what a
  test backtrace actually reads. This is the setting that matters: it took a test
  binary from ~440 MB to ~150 MB and the whole `--workspace` build from ~20 GB of
  peak memory to ~3 GB when it landed. Both numbers have since grown with the
  dependency tree — the npm module runtime links the whole of `deno_runtime`, and
  the largest test binary is back around 440 MB with the budget in place, of
  which only ~47 MB is debug info — so a cold `cargo test --workspace` no longer
  fits in the wrapper's default 12 GB at `-j8`. Run it with `-j4`, or raise the
  ceiling. Full DWARF for a dependency, on the rare occasion of stepping into
  one, is a one-off flag:

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

- **`-j 4` for the workspace test build.** Since the module runtime landed, each
  test binary maps a much larger set of rlibs at link time, and the default
  parallelism under the wrapper's 10 GB `MemoryHigh` puts all of them in
  continuous reclaim — a build that makes no progress rather than one that fails.
  Either cap the jobs or raise the ceiling:

  ```bash
  ./scripts/cargo-guarded.sh test -p sc-server --no-run -j 4
  SC_BUILD_MEM_HIGH=16G ./scripts/cargo-guarded.sh test -p sc-server --no-run
  ```

Note also that `target/` is not garbage-collected by cargo: every rebuild leaves
the previous hashed test binaries behind, and at a few hundred MB each that
reaches hundreds of GB over weeks. `cargo clean` periodically, or
[`cargo-sweep`](https://github.com/holmgr/cargo-sweep) to drop only stale files.
